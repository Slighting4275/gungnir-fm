//! gungnird protocol v2, client side. Every constant and layout here mirrors
//! `../gungnir-vfs/src/daemon-rust/src/ipc.rs` (frames, shared memory, rings),
//! `md.rs` (metadata requests, identities, values), `health.rs` (mount health) and
//! `ops.rs` (file operations). The daemon is the authority; this file must change
//! whenever those do (and the protocol `VERSION` is bumped for layout changes).
//!
//! The peer's memory is untrusted: sizes, seals, header, ring indices and lengths
//! are all validated, and every parse is bounds-checked (errors, never panics).

use std::ffi::OsStr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use core_vfs::{VfsError, VfsResult};

// ---- ipc.rs ---------------------------------------------------------------------------

/// `b"GNGR"` as a little-endian u32.
pub const MAGIC: u32 = 0x5247_4E47;
pub const VERSION: u16 = 2;
pub const KIND_HELLO: u16 = 1;
pub const KIND_ACK: u16 = 2;
pub const KIND_REJECT: u16 = 3;
pub const STATUS_OK: u32 = 0;
pub const STATUS_BAD_VERSION: u32 = 2;
pub const FRAME_LEN: usize = 16;
pub const SLOT_COUNT: u32 = 32;
pub const SLOT_SIZE: usize = 4096;
pub const SLOT_HEADER: usize = 16;
pub const PAYLOAD_MAX: usize = SLOT_SIZE - SLOT_HEADER;
pub const SHM_SIZE: usize = 4096 + 2 * SLOT_COUNT as usize * SLOT_SIZE;
/// Requests the daemon reads per connection before it waits for replies to drain.
pub const MAX_INFLIGHT: usize = SLOT_COUNT as usize / 2;
pub const MSG_PING: u32 = 1;
pub const MSG_PONG: u32 = 2;

#[derive(Debug, Clone, Copy)]
pub struct Ring {
    head: usize,
    tail: usize,
    slots: usize,
}

/// Client -> daemon.
pub const RING_C2D: Ring = Ring { head: 64, tail: 128, slots: 4096 };
/// Daemon -> client.
pub const RING_D2C: Ring = Ring { head: 192, tail: 256, slots: 4096 + SLOT_COUNT as usize * SLOT_SIZE };

// ---- md.rs ----------------------------------------------------------------------------

pub const MSG_MD_IDENTIFY: u32 = 16;
pub const MSG_MD_GET: u32 = 17;
pub const MSG_MD_SET: u32 = 18;
pub const MSG_MD_UNSET: u32 = 19;
pub const MSG_MD_LIST: u32 = 20;
pub const MSG_MD_REMOVED: u32 = 21;
pub const MSG_MD_MOVED: u32 = 22;
pub const MSG_MD_REPLY: u32 = 32;
pub const MAX_UUID_LEN: usize = 64;
pub const MAX_KEY_LEN: usize = 256;
pub const MAX_VALUE_LEN: usize = 3072;
pub const MAX_STRINGV_ITEMS: usize = 64;
const VALUE_STRING: u8 = 1;
const VALUE_STRINGV: u8 = 2;

// ---- health.rs ------------------------------------------------------------------------

pub const MSG_HEALTH_QUERY: u32 = 23;
pub const MSG_HEALTH_SUBSCRIBE: u32 = 24;
pub const MSG_HEALTH_EVENT: u32 = 33;
pub const QUERY_BY_PATH: u8 = 0;
pub const EV_FIRST: u8 = 1;
pub const EV_LAST: u8 = 2;
pub const EV_INCOMPLETE: u8 = 4;

// ---- ops.rs ---------------------------------------------------------------------------

pub const MSG_VFS_LIST: u32 = 40;
pub const MSG_VFS_DELETE: u32 = 41;
pub const MSG_VFS_RESTORE: u32 = 42;
pub const MSG_TRASH_EMPTY: u32 = 43;
/// The daemon is alive and working on the request with this seq.
pub const MSG_VFS_BEAT: u32 = 44;
pub const MSG_VFS_STAT: u32 = 45;
pub const MSG_VFS_CHILDREN: u32 = 46;
pub const MSG_VFS_STAMP: u32 = 47;
pub const SCHEME_TRASH: u8 = 0;
pub const SCHEME_RECENT: u8 = 1;
pub const VFS_KIND_FILE: u8 = 1;
pub const VFS_KIND_DIR: u8 = 2;
pub const VFS_KIND_SYMLINK: u8 = 3;

pub const MSG_OP_START: u32 = 25;
pub const MSG_OP_CANCEL: u32 = 26;
pub const MSG_OP_PROGRESS: u32 = 34;
pub const MSG_OP_DONE: u32 = 35;
pub const OP_COPY: u8 = 1;
pub const OP_MOVE: u8 = 2;
pub const OP_DELETE: u8 = 3;
pub const OP_DELETE_TREE: u8 = 4;
pub const OP_FSYNC: u8 = 5;
pub const FLAG_OVERWRITE: u32 = 1;
pub const FLAG_NOFOLLOW_SYMLINKS: u32 = 4;
pub const FLAG_ALL_METADATA: u32 = 8;
/// `PATH_MAX` without the NUL.
pub const MAX_PATH: usize = 4095;

fn proto(what: &str) -> VfsError {
    VfsError::Transport(format!("gungnird protocol: {what}"))
}

// ---- setup frame ----------------------------------------------------------------------

/// `magic u32 | version u16 | kind u16 | status u32 | shm_size u32`, little-endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub magic: u32,
    pub version: u16,
    pub kind: u16,
    pub status: u32,
    pub shm_size: u32,
}

impl Frame {
    #[must_use]
    pub fn hello() -> Self {
        Self { magic: MAGIC, version: VERSION, kind: KIND_HELLO, status: STATUS_OK, shm_size: 0 }
    }

    #[must_use]
    pub fn encode(&self) -> [u8; FRAME_LEN] {
        let mut b = [0u8; FRAME_LEN];
        b[0..4].copy_from_slice(&self.magic.to_le_bytes());
        b[4..6].copy_from_slice(&self.version.to_le_bytes());
        b[6..8].copy_from_slice(&self.kind.to_le_bytes());
        b[8..12].copy_from_slice(&self.status.to_le_bytes());
        b[12..16].copy_from_slice(&self.shm_size.to_le_bytes());
        b
    }

    #[must_use]
    pub fn decode(b: &[u8; FRAME_LEN]) -> Self {
        Self {
            magic: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            version: u16::from_le_bytes([b[4], b[5]]),
            kind: u16::from_le_bytes([b[6], b[7]]),
            status: u32::from_le_bytes([b[8], b[9], b[10], b[11]]),
            shm_size: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
        }
    }
}

/// The daemon's socket, first match wins (absolute values only), exactly as
/// `ipc::socket_path`: `$GUNGNIR_SOCKET`, `$XDG_RUNTIME_DIR/gungnir/gungnird.sock`,
/// `$HOME/.cache/gungnir/gungnird.sock`.
///
/// # Errors
/// `DaemonUnavailable` if none applies or the path does not fit `sun_path`.
pub fn socket_path(sock: Option<&OsStr>, runtime: Option<&OsStr>, home: Option<&OsStr>) -> VfsResult<PathBuf> {
    let abs = |v: Option<&OsStr>| v.map(PathBuf::from).filter(|p| p.is_absolute());
    let p = abs(sock)
        .or_else(|| abs(runtime).map(|r| r.join("gungnir/gungnird.sock")))
        .or_else(|| abs(home).map(|h| h.join(".cache/gungnir/gungnird.sock")))
        .ok_or_else(|| VfsError::DaemonUnavailable("no socket path (set XDG_RUNTIME_DIR or HOME)".into()))?;
    // sun_path is 108 bytes including the NUL.
    if p.as_os_str().len() >= 108 {
        return Err(VfsError::DaemonUnavailable(format!("socket path too long: {}", p.display())));
    }
    Ok(p)
}

/// [`socket_path`] from the process environment.
///
/// # Errors
/// As [`socket_path`].
pub fn socket_path_from_env() -> VfsResult<PathBuf> {
    socket_path(
        std::env::var_os("GUNGNIR_SOCKET").as_deref(),
        std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

// ---- shared memory --------------------------------------------------------------------

/// One message slot read from the daemon.
#[derive(Debug, Clone)]
pub struct Msg {
    pub kind: u32,
    pub seq: u64,
    pub payload: Vec<u8>,
}

/// The client's mapping of the daemon's sealed memfd. Every access is atomic: the
/// daemon (another process) may write any of it at any time.
pub struct Shm {
    ptr: NonNull<u8>,
}

// SAFETY: plain shared memory accessed only through atomics.
unsafe impl Send for Shm {}
// SAFETY: as above.
unsafe impl Sync for Shm {}

fn os_err<T>(what: &str) -> VfsResult<T> {
    let e = std::io::Error::last_os_error();
    Err(VfsError::from_io("gungnird ipc", &e, what))
}

impl Shm {
    /// Check size, seals and header of a received memfd, then map it.
    ///
    /// # Errors
    /// Transport errors for anything that is not a sealed v2 ring region.
    pub fn map(fd: RawFd) -> VfsResult<Self> {
        // SAFETY: `st` is POD; fstat fills it on success.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: valid fd and out-pointer.
        if unsafe { libc::fstat(fd, &raw mut st) } < 0 {
            return os_err("fstat memfd");
        }
        if usize::try_from(st.st_size).ok() != Some(SHM_SIZE) {
            return Err(proto("shared memory has the wrong size"));
        }
        // SAFETY: F_GET_SEALS takes no argument.
        let seals = unsafe { libc::fcntl(fd, libc::F_GET_SEALS) };
        let need = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW;
        if seals < 0 || seals & need != need {
            return Err(proto("shared memory is not size-sealed"));
        }
        // SAFETY: mapping a sealed memfd of exactly SHM_SIZE bytes; checked below.
        let p = unsafe {
            libc::mmap(std::ptr::null_mut(), SHM_SIZE, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0)
        };
        if p == libc::MAP_FAILED {
            return os_err("mmap ring");
        }
        let shm = NonNull::new(p.cast::<u8>()).map(|ptr| Self { ptr }).ok_or_else(|| proto("mmap returned NULL"))?;
        let ok = shm.word(0).load(Ordering::Acquire) == MAGIC
            && shm.word(4).load(Ordering::Relaxed) == u32::from(VERSION)
            && shm.word(8).load(Ordering::Relaxed) == SLOT_COUNT
            && shm.word(12).load(Ordering::Relaxed) == SLOT_SIZE as u32;
        if !ok {
            return Err(proto("bad shared memory header"));
        }
        Ok(shm)
    }

    fn word(&self, off: usize) -> &AtomicU32 {
        debug_assert!(off.is_multiple_of(4) && off + 4 <= SHM_SIZE);
        // SAFETY: in bounds and 4-aligned within a page-aligned mapping; atomic access only.
        unsafe { &*self.ptr.as_ptr().add(off).cast::<AtomicU32>() }
    }

    fn slot(&self, ring: Ring, index: u32) -> &[AtomicU64; SLOT_SIZE / 8] {
        let off = ring.slots + (index % SLOT_COUNT) as usize * SLOT_SIZE;
        // SAFETY: in bounds and 8-aligned (slot offsets are multiples of 4096).
        unsafe { &*self.ptr.as_ptr().add(off).cast::<[AtomicU64; SLOT_SIZE / 8]>() }
    }

    /// Producer side of `ring`: append one message and wake the consumer.
    /// `Ok(false)` if the ring is full.
    ///
    /// # Errors
    /// Corrupt indices or an oversized payload.
    pub fn push(&self, ring: Ring, kind: u32, seq: u64, payload: &[u8]) -> VfsResult<bool> {
        if payload.len() > PAYLOAD_MAX {
            return Err(proto("payload too large"));
        }
        let head_w = self.word(ring.head);
        let head = head_w.load(Ordering::Relaxed);
        let used = head.wrapping_sub(self.word(ring.tail).load(Ordering::Acquire));
        if used > SLOT_COUNT {
            return Err(proto("ring indices are corrupt"));
        }
        if used == SLOT_COUNT {
            return Ok(false);
        }
        let mut slot = vec![0u8; SLOT_HEADER + payload.len().next_multiple_of(8)];
        slot[0..4].copy_from_slice(&kind.to_le_bytes());
        slot[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        slot[8..16].copy_from_slice(&seq.to_le_bytes());
        slot[SLOT_HEADER..SLOT_HEADER + payload.len()].copy_from_slice(payload);
        let (words, _) = slot.as_chunks::<8>();
        for (w, chunk) in self.slot(ring, head).iter().zip(words) {
            w.store(u64::from_le_bytes(*chunk), Ordering::Relaxed);
        }
        head_w.store(head.wrapping_add(1), Ordering::Release);
        futex_wake(head_w);
        Ok(true)
    }

    /// Consumer side of `ring`: take one message, `None` if empty.
    ///
    /// # Errors
    /// Corrupt indices or lengths.
    pub fn pop(&self, ring: Ring) -> VfsResult<Option<Msg>> {
        let tail_w = self.word(ring.tail);
        let tail = tail_w.load(Ordering::Relaxed);
        let used = self.word(ring.head).load(Ordering::Acquire).wrapping_sub(tail);
        if used > SLOT_COUNT {
            return Err(proto("ring indices are corrupt"));
        }
        if used == 0 {
            return Ok(None);
        }
        let words = self.slot(ring, tail);
        let h0 = words[0].load(Ordering::Relaxed).to_le_bytes();
        let h1 = words[1].load(Ordering::Relaxed).to_le_bytes();
        let kind = u32::from_le_bytes([h0[0], h0[1], h0[2], h0[3]]);
        let len = u32::from_le_bytes([h0[4], h0[5], h0[6], h0[7]]) as usize;
        if len > PAYLOAD_MAX {
            return Err(proto("message length out of range"));
        }
        let mut payload = Vec::with_capacity(len.next_multiple_of(8));
        for w in words.iter().skip(SLOT_HEADER / 8).take(len.div_ceil(8)) {
            payload.extend_from_slice(&w.load(Ordering::Relaxed).to_le_bytes());
        }
        payload.truncate(len);
        tail_w.store(tail.wrapping_add(1), Ordering::Release);
        Ok(Some(Msg { kind, seq: u64::from_le_bytes(h1), payload }))
    }

    /// Wait (futex, bounded) until `ring` is non-empty.
    pub fn wait_readable(&self, ring: Ring, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let head_w = self.word(ring.head);
        loop {
            let head = head_w.load(Ordering::Acquire);
            if head != self.word(ring.tail).load(Ordering::Relaxed) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            futex_wait(head_w, head, deadline - now);
        }
    }

    /// Wake a thread sleeping in [`Shm::wait_readable`] on `ring` without writing
    /// anything (used to hand it new work).
    pub fn nudge(&self, ring: Ring) {
        futex_wake(self.word(ring.head));
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly what `map` mapped.
        unsafe { libc::munmap(self.ptr.as_ptr().cast(), SHM_SIZE) };
    }
}

/// Process-shared futex wait (no `FUTEX_PRIVATE_FLAG`), as gungnird requires.
fn futex_wait(word: &AtomicU32, expected: u32, timeout: Duration) {
    let ts = libc::timespec {
        tv_sec: libc::time_t::try_from(timeout.as_secs()).unwrap_or(libc::time_t::MAX),
        tv_nsec: libc::c_long::from(timeout.subsec_nanos()),
    };
    // SAFETY: valid futex word and timespec; every outcome is re-checked by the caller.
    unsafe {
        libc::syscall(libc::SYS_futex, word.as_ptr(), libc::FUTEX_WAIT, expected, &raw const ts, 0usize, 0u32);
    }
}

fn futex_wake(word: &AtomicU32) {
    // SAFETY: valid futex word.
    unsafe {
        libc::syscall(libc::SYS_futex, word.as_ptr(), libc::FUTEX_WAKE, i32::MAX);
    }
}

// ---- socket setup ---------------------------------------------------------------------

/// Connect without blocking (a full backlog is "unavailable", not a hang), check the
/// peer is our own uid, send hello, and receive the ack with its memfd.
///
/// # Errors
/// `DaemonUnavailable` if nothing listens; transport errors for a bad handshake.
pub fn handshake(sock: &Path, timeout: Duration) -> VfsResult<(OwnedFd, Shm)> {
    let unavailable = |what: &str| VfsError::DaemonUnavailable(format!("{}: {what}", sock.display()));
    // SAFETY: plain socket creation.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK, 0) };
    if raw < 0 {
        return os_err("socket");
    }
    // SAFETY: `raw` is a fresh fd we own.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: zeroed sockaddr_un is valid; we fill family and path.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = sock.as_os_str().as_bytes();
    if bytes.len() >= addr.sun_path.len() {
        return Err(unavailable("socket path too long"));
    }
    for (d, s) in addr.sun_path.iter_mut().zip(bytes) {
        *d = *s as libc::c_char;
    }
    // SAFETY: valid fd and address of the given length.
    let r = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&raw const addr).cast(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if r < 0 {
        let e = std::io::Error::last_os_error();
        return Err(unavailable(&e.to_string()));
    }
    // Peer credentials: only a daemon of our own effective uid is trusted.
    // SAFETY: POD out-struct of the advertised size.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: valid fd, option and buffer.
    if unsafe {
        libc::getsockopt(fd.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &raw mut len)
    } < 0
    {
        return os_err("SO_PEERCRED");
    }
    // SAFETY: geteuid cannot fail.
    if cred.uid != unsafe { libc::geteuid() } {
        return Err(proto("daemon runs as another user"));
    }
    let deadline = Instant::now() + timeout;
    write_all(&fd, &Frame::hello().encode(), deadline)?;
    let (frame, memfd) = recv_frame_with_fd(&fd, deadline)?;
    if frame.magic != MAGIC {
        return Err(proto("bad magic in reply"));
    }
    if frame.kind == KIND_REJECT {
        return Err(if frame.status == STATUS_BAD_VERSION {
            proto(&format!("daemon refused protocol version {VERSION}"))
        } else {
            proto(&format!("hello rejected with status {}", frame.status))
        });
    }
    if frame.kind != KIND_ACK || frame.status != STATUS_OK {
        return Err(proto("unexpected reply to hello"));
    }
    let memfd = memfd.ok_or_else(|| proto("ack without a memfd"))?;
    let shm = Shm::map(memfd.as_raw_fd())?;
    Ok((fd, shm))
}

fn poll_fd(fd: &OwnedFd, events: libc::c_short, deadline: Instant) -> VfsResult<()> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(VfsError::Timeout);
        }
        let mut p = libc::pollfd { fd: fd.as_raw_fd(), events, revents: 0 };
        let ms = libc::c_int::try_from(left.as_millis().max(1)).unwrap_or(libc::c_int::MAX);
        // SAFETY: one valid pollfd.
        let r = unsafe { libc::poll(&raw mut p, 1, ms) };
        if r > 0 {
            return Ok(());
        }
        if r < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return os_err("poll");
        }
    }
}

fn write_all(fd: &OwnedFd, mut b: &[u8], deadline: Instant) -> VfsResult<()> {
    while !b.is_empty() {
        poll_fd(fd, libc::POLLOUT, deadline)?;
        // SAFETY: valid fd and buffer.
        let n = unsafe { libc::send(fd.as_raw_fd(), b.as_ptr().cast(), b.len(), libc::MSG_NOSIGNAL) };
        match usize::try_from(n) {
            Ok(n) => b = &b[n..],
            Err(_) if std::io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN) => {}
            Err(_) => return os_err("send hello"),
        }
    }
    Ok(())
}

#[repr(C, align(8))]
struct CmsgBuf([u8; 64]);

fn recv_frame_with_fd(fd: &OwnedFd, deadline: Instant) -> VfsResult<(Frame, Option<OwnedFd>)> {
    let mut data = [0u8; FRAME_LEN];
    let mut cbuf = CmsgBuf([0u8; 64]);
    let n = loop {
        poll_fd(fd, libc::POLLIN, deadline)?;
        let mut iov = libc::iovec { iov_base: data.as_mut_ptr().cast(), iov_len: data.len() };
        // SAFETY: zeroed msghdr is valid; we point it at live buffers below.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &raw mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cbuf.0.as_mut_ptr().cast();
        msg.msg_controllen = cbuf.0.len() as _;
        // SAFETY: msghdr points at valid buffers for the duration of the call.
        let n = unsafe { libc::recvmsg(fd.as_raw_fd(), &raw mut msg, libc::MSG_CMSG_CLOEXEC) };
        if n < 0 {
            let e = std::io::Error::last_os_error().raw_os_error();
            if e == Some(libc::EAGAIN) || e == Some(libc::EINTR) {
                continue;
            }
            return os_err("recvmsg");
        }
        let mut fds = Vec::new();
        // SAFETY: walking the control buffer the kernel just filled, with the libc macros.
        unsafe {
            let mut c = libc::CMSG_FIRSTHDR(&raw const msg);
            while !c.is_null() {
                if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                    let data_ptr = libc::CMSG_DATA(c);
                    let header = data_ptr.offset_from(c.cast::<u8>()).unsigned_abs();
                    let count = ((*c).cmsg_len as usize).saturating_sub(header) / std::mem::size_of::<RawFd>();
                    for i in 0..count {
                        fds.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(data_ptr.cast::<RawFd>().add(i))));
                    }
                }
                c = libc::CMSG_NXTHDR(&raw const msg, c);
            }
        }
        if msg.msg_flags & libc::MSG_CTRUNC != 0 {
            return Err(proto("control message truncated"));
        }
        if fds.len() > 1 {
            return Err(proto("more than one fd passed"));
        }
        break (n, fds.pop());
    };
    if usize::try_from(n.0).ok() != Some(FRAME_LEN) {
        return Err(proto("short frame"));
    }
    Ok((Frame::decode(&data), n.1))
}

/// True once the daemon closed the control socket (zero-timeout poll).
#[must_use]
pub fn peer_gone(fd: &OwnedFd) -> bool {
    let mut p = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN | libc::POLLRDHUP, revents: 0 };
    // SAFETY: one valid pollfd, zero timeout.
    let r = unsafe { libc::poll(&raw mut p, 1, 0) };
    r > 0 && p.revents & (libc::POLLHUP | libc::POLLRDHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

// ---- payloads -------------------------------------------------------------------------

/// Bounds-checked reader over a daemon payload.
pub struct Reader<'a>(pub &'a [u8]);

impl<'a> Reader<'a> {
    /// # Errors
    /// Short payload.
    pub fn take(&mut self, n: usize) -> VfsResult<&'a [u8]> {
        if self.0.len() < n {
            return Err(proto("short payload"));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }

    fn arr<const N: usize>(&mut self) -> VfsResult<[u8; N]> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }

    /// # Errors
    /// Short payload.
    pub fn u8(&mut self) -> VfsResult<u8> {
        Ok(self.arr::<1>()?[0])
    }

    /// # Errors
    /// Short payload.
    pub fn u16(&mut self) -> VfsResult<u16> {
        Ok(u16::from_le_bytes(self.arr()?))
    }

    /// # Errors
    /// Short payload.
    pub fn u32(&mut self) -> VfsResult<u32> {
        Ok(u32::from_le_bytes(self.arr()?))
    }

    /// # Errors
    /// Short payload.
    pub fn u64(&mut self) -> VfsResult<u64> {
        Ok(u64::from_le_bytes(self.arr()?))
    }

    /// # Errors
    /// Short payload.
    pub fn i64(&mut self) -> VfsResult<i64> {
        Ok(i64::from_le_bytes(self.arr()?))
    }

    /// `len u16 | bytes`.
    ///
    /// # Errors
    /// Short payload.
    pub fn blob16(&mut self) -> VfsResult<&'a [u8]> {
        let n = usize::from(self.u16()?);
        self.take(n)
    }

    /// # Errors
    /// Trailing bytes.
    pub fn end(&self) -> VfsResult<()> {
        if self.0.is_empty() { Ok(()) } else { Err(proto("trailing bytes")) }
    }
}

/// A file identity as gungnird returns it from identify (`md::Ident`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ident {
    pub fs_uuid: String,
    pub subvol_id: u64,
    pub inode_num: u64,
    pub generation: u64,
    pub btime_sec: i64,
    pub btime_nsec: u32,
}

impl Ident {
    /// `uuid_len u8 | fs_uuid | subvol u64 | inode u64 | generation u64 | btime_sec i64 | btime_nsec u32`.
    ///
    /// # Errors
    /// Malformed identity.
    pub fn read(r: &mut Reader<'_>) -> VfsResult<Self> {
        let n = usize::from(r.u8()?);
        if n == 0 || n > MAX_UUID_LEN {
            return Err(proto("bad fs_uuid length"));
        }
        let raw = r.take(n)?;
        if !raw.iter().all(u8::is_ascii_graphic) {
            return Err(proto("fs_uuid is not printable"));
        }
        Ok(Self {
            fs_uuid: String::from_utf8_lossy(raw).into_owned(),
            subvol_id: r.u64()?,
            inode_num: r.u64()?,
            generation: r.u64()?,
            btime_sec: r.i64()?,
            btime_nsec: r.u32()?,
        })
    }

    /// # Errors
    /// A `fs_uuid` gungnird would refuse.
    pub fn write(&self, out: &mut Vec<u8>) -> VfsResult<()> {
        let u = self.fs_uuid.as_bytes();
        if u.is_empty() || u.len() > MAX_UUID_LEN || !u.iter().all(u8::is_ascii_graphic) {
            return Err(VfsError::BadRequest);
        }
        out.push(u.len() as u8);
        out.extend_from_slice(u);
        for v in [self.subvol_id, self.inode_num, self.generation] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.btime_sec.to_le_bytes());
        out.extend_from_slice(&self.btime_nsec.to_le_bytes());
        Ok(())
    }
}

/// A `metadata::*` value: GIO `string` or `stringv`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    String(String),
    Stringv(Vec<String>),
}

impl Value {
    /// # Errors
    /// Values gungnird would refuse.
    pub fn encode(&self) -> VfsResult<Vec<u8>> {
        fn put(out: &mut Vec<u8>, s: &str) -> VfsResult<()> {
            if s.contains('\0') {
                return Err(VfsError::BadRequest);
            }
            out.extend_from_slice(&u16::try_from(s.len()).map_err(|_| VfsError::ValueTooLarge)?.to_le_bytes());
            out.extend_from_slice(s.as_bytes());
            Ok(())
        }
        let mut out = Vec::new();
        match self {
            Value::String(s) => {
                out.push(VALUE_STRING);
                put(&mut out, s)?;
            }
            Value::Stringv(v) => {
                if v.len() > MAX_STRINGV_ITEMS {
                    return Err(VfsError::ValueTooLarge);
                }
                out.push(VALUE_STRINGV);
                out.extend_from_slice(&(v.len() as u16).to_le_bytes());
                for s in v {
                    put(&mut out, s)?;
                }
            }
        }
        if out.len() > MAX_VALUE_LEN { Err(VfsError::ValueTooLarge) } else { Ok(out) }
    }

    /// # Errors
    /// Malformed value.
    pub fn decode(b: &[u8]) -> VfsResult<Self> {
        let text = |s: &[u8]| String::from_utf8(s.to_vec()).map_err(|_| VfsError::BadUtf8);
        let mut r = Reader(b);
        let v = match r.u8()? {
            VALUE_STRING => Value::String(text(r.blob16()?)?),
            VALUE_STRINGV => {
                let n = usize::from(r.u16()?);
                if n > MAX_STRINGV_ITEMS {
                    return Err(VfsError::ValueTooLarge);
                }
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(text(r.blob16()?)?);
                }
                Value::Stringv(v)
            }
            _ => return Err(proto("unknown value type")),
        };
        r.end()?;
        Ok(v)
    }

    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Value::String(s) => s.clone(),
            Value::Stringv(v) => format!("[{}]", v.join(", ")),
        }
    }
}

/// An absolute path gungnird accepts: `/`-rooted, no NUL, within `MAX_PATH`.
///
/// # Errors
/// `BadRequest`.
pub fn path_bytes(p: &Path) -> VfsResult<&[u8]> {
    let b = p.as_os_str().as_bytes();
    if b.first() != Some(&b'/') || b.contains(&0) || b.len() > MAX_PATH {
        return Err(VfsError::BadRequest);
    }
    Ok(b)
}

fn put16(out: &mut Vec<u8>, b: &[u8]) -> VfsResult<()> {
    out.extend_from_slice(&u16::try_from(b.len()).map_err(|_| VfsError::BadRequest)?.to_le_bytes());
    out.extend_from_slice(b);
    Ok(())
}

/// `key: len u16 | bytes` (empty allowed: the list cursor "from the start").
///
/// # Errors
/// `KeyTooLong`.
pub fn put_key(out: &mut Vec<u8>, key: &str) -> VfsResult<()> {
    if key.len() > MAX_KEY_LEN {
        return Err(VfsError::KeyTooLong);
    }
    put16(out, key.as_bytes())
}

/// Request payloads.
pub mod req {
    use super::*;

    fn blob16(out: &mut Vec<u8>, b: &[u8]) -> VfsResult<()> {
        let n = u16::try_from(b.len()).map_err(|_| VfsError::BadRequest)?;
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(b);
        Ok(())
    }

    #[must_use]
    pub fn vfs_list(cursor: u32) -> Vec<u8> {
        let mut out = vec![SCHEME_TRASH];
        out.extend_from_slice(&cursor.to_le_bytes());
        out
    }

    #[must_use]
    pub fn recent_list(cursor: u32) -> Vec<u8> {
        let mut out = vec![SCHEME_RECENT];
        out.extend_from_slice(&cursor.to_le_bytes());
        out
    }

    /// # Errors
    /// Name too long.
    pub fn vfs_children(cursor: u32, name: &[u8]) -> VfsResult<Vec<u8>> {
        let mut out = cursor.to_le_bytes().to_vec();
        blob16(&mut out, name)?;
        Ok(out)
    }

    /// For stat and delete: `scheme u8 | name len16`.
    ///
    /// # Errors
    /// Name too long.
    pub fn vfs_item(name: &[u8]) -> VfsResult<Vec<u8>> {
        let mut out = vec![SCHEME_TRASH];
        blob16(&mut out, name)?;
        Ok(out)
    }

    /// For recent stat and delete: `scheme u8 (1) | name len16`.
    ///
    /// # Errors
    /// Name too long.
    pub fn recent_item(name: &[u8]) -> VfsResult<Vec<u8>> {
        let mut out = vec![SCHEME_RECENT];
        blob16(&mut out, name)?;
        Ok(out)
    }

    /// Restore to the item's own original path.
    ///
    /// # Errors
    /// Name too long.
    pub fn vfs_restore(name: &[u8]) -> VfsResult<Vec<u8>> {
        let mut out = Vec::new();
        blob16(&mut out, name)?;
        blob16(&mut out, b"")?;
        Ok(out)
    }

    /// # Errors
    /// Invalid path.
    pub fn path(p: &Path) -> VfsResult<Vec<u8>> {
        Ok(path_bytes(p)?.to_vec())
    }

    /// # Errors
    /// Invalid paths.
    pub fn moved(src: &Path, dest: &Path) -> VfsResult<Vec<u8>> {
        let mut out = Vec::new();
        put16(&mut out, path_bytes(src)?)?;
        put16(&mut out, path_bytes(dest)?)?;
        Ok(out)
    }

    /// # Errors
    /// Invalid identity or key.
    pub fn ident_key(id: &Ident, key: &str) -> VfsResult<Vec<u8>> {
        let mut out = Vec::new();
        id.write(&mut out)?;
        put_key(&mut out, key)?;
        Ok(out)
    }

    /// # Errors
    /// Invalid identity, key or value.
    pub fn set(id: &Ident, key: &str, v: &Value) -> VfsResult<Vec<u8>> {
        let mut out = ident_key(id, key)?;
        out.extend_from_slice(&v.encode()?);
        Ok(out)
    }

    /// Health query by path.
    ///
    /// # Errors
    /// Invalid path.
    pub fn health_by_path(p: &Path) -> VfsResult<Vec<u8>> {
        let mut out = vec![QUERY_BY_PATH];
        out.extend_from_slice(path_bytes(p)?);
        Ok(out)
    }

    /// `op u8 | flags u32 | timeout_ms u32 | src path16 | dest path16`.
    ///
    /// # Errors
    /// Invalid paths.
    pub fn op_start(op: u8, flags: u32, timeout_ms: u32, src: &Path, dest: Option<&Path>) -> VfsResult<Vec<u8>> {
        let mut out = vec![op];
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&timeout_ms.to_le_bytes());
        put16(&mut out, path_bytes(src)?)?;
        match dest {
            Some(d) => put16(&mut out, path_bytes(d)?)?,
            None => out.extend_from_slice(&0u16.to_le_bytes()),
        }
        Ok(out)
    }
}

/// Split a `MSG_MD_REPLY` payload: the body on status 0, else the mapped error.
///
/// # Errors
/// The daemon's status, or a transport error for a malformed reply.
pub fn reply_body(payload: &[u8], what: &str) -> VfsResult<Vec<u8>> {
    let mut r = Reader(payload);
    let st = r.u32()?;
    let errno = if st == OS_ERROR_STATUS { r.u32().map_or(0, |e| e as i32) } else { 0 };
    match VfsError::from_gungnir_status(st, errno, what) {
        None => Ok(r.0.to_vec()),
        Some(e) => Err(e),
    }
}

/// The reply to a trash empty (`MSG_TRASH_EMPTY`): `status u32 | deleted u32 | failed u32 |
/// len u16 | names` whatever the status (gungnird `vfssvc.rs` `empty`), never errno-first.
/// Items left, an item that would not go or an OS error come back as `NotEmpty` with the
/// counts; hung volumes skipped stay `SuspectMount`; a reply without counts (busy,
/// cancelled, an older daemon) is mapped from its status alone.
///
/// # Errors
/// What the empty left undone, or a transport error for a malformed reply.
pub fn parse_empty(payload: &[u8]) -> VfsResult<()> {
    use core_vfs::error::gungnir_status as s;
    let mut r = Reader(payload);
    let st = r.u32()?;
    if st == s::OK {
        return Ok(());
    }
    let counts = (|| Some((r.u32().ok()?, r.u32().ok()?)))();
    let Some((deleted, failed)) = counts else {
        return Err(VfsError::from_gungnir_status(st, 0, "trash")
            .unwrap_or(VfsError::Transport(format!("gungnird status {st}"))));
    };
    match st {
        s::SUSPECT_MOUNT if failed == 0 => Err(VfsError::SuspectMount),
        s::NOT_EMPTY | s::OS_ERROR | s::IO_ERROR | s::MOUNT_INSIDE | s::SUSPECT_MOUNT => {
            let what = match failed {
                0 => "some trash items could not be removed".to_owned(),
                1 => "1 trash item could not be removed".to_owned(),
                n => format!("{n} trash items could not be removed"),
            };
            Err(VfsError::NotEmpty(format!("{what} ({deleted} deleted)")))
        }
        other => Err(VfsError::from_gungnir_status(other, 0, "trash")
            .unwrap_or(VfsError::Transport(format!("gungnird status {other}")))),
    }
}

/// One trash item from gungnird (`vfssvc.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfsRow {
    /// The URI name: `h-<entry>`, or `<hex of the trash directory>-<entry>`, then any path
    /// inside a trashed folder.
    pub name: Vec<u8>,
    pub orig: Vec<u8>,
    pub target: Vec<u8>,
    pub kind: u8,
    pub size: u64,
    pub mtime: i64,
}

/// A page of items: `next u32 (0: the end) | count u16 | partial u8 | rows`.
///
/// # Errors
/// Malformed body.
pub fn parse_page(body: &[u8]) -> VfsResult<(u32, bool, Vec<VfsRow>)> {
    let mut r = Reader(body);
    let next = r.u32()?;
    let count = r.u16()?;
    let partial = r.u8()? != 0;
    let mut rows = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let name = r.blob16()?.to_vec();
        let orig = r.blob16()?.to_vec();
        let target = r.blob16()?.to_vec();
        let _deleted = r.blob16()?;
        let _link = r.blob16()?;
        let kind = r.u8()?;
        let size = r.u64()?;
        let mtime = r.i64()?;
        rows.push(VfsRow { name, orig, target, kind, size, mtime });
    }
    Ok((next, partial, rows))
}

/// Whether the first part of a `trash:` path names an item on another volume: the hex of
/// an absolute trash directory, then `-`. Home items are their plain entry names.
#[must_use]
pub fn is_volume_item(part: &str) -> bool {
    part.split_once('-').is_some_and(|(hex, _)| {
        hex.len() >= 4
            && hex.len().is_multiple_of(2)
            && hex.starts_with("2f")
            && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Its body starts with the errno.
const OS_ERROR_STATUS: u32 = 15;

/// List reply body: `more u8 | count u16 | count x key`.
///
/// # Errors
/// Malformed body.
pub fn parse_list(body: &[u8]) -> VfsResult<(bool, Vec<String>)> {
    let mut r = Reader(body);
    let more = r.u8()? != 0;
    let n = r.u16()?;
    let mut keys = Vec::with_capacity(usize::from(n));
    for _ in 0..n {
        keys.push(String::from_utf8(r.blob16()?.to_vec()).map_err(|_| VfsError::BadUtf8)?);
    }
    r.end()?;
    Ok((more, keys))
}

/// gungnird `health::Health`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Healthy,
    Slow,
    Hung,
    Gone,
}

impl Health {
    #[must_use]
    pub fn from_code(c: u8) -> Option<Self> {
        Some(match c {
            0 => Health::Healthy,
            1 => Health::Slow,
            2 => Health::Hung,
            3 => Health::Gone,
            _ => return None,
        })
    }

    /// Paths on such a mount must fail fast.
    #[must_use]
    pub fn blocks(self) -> bool {
        matches!(self, Health::Hung | Health::Gone)
    }
}

/// Health query reply: `mnt_id u64 | state u8 | probed u8 | latency_us u32 | path16`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    pub mnt_id: u64,
    pub state: Health,
    pub probed: bool,
    /// `None` until the first probe answered.
    pub latency_us: Option<u32>,
    pub mount_point: PathBuf,
}

/// # Errors
/// Malformed body.
pub fn parse_health_report(body: &[u8]) -> VfsResult<HealthReport> {
    let mut r = Reader(body);
    let mnt_id = r.u64()?;
    let state = Health::from_code(r.u8()?).ok_or_else(|| proto("bad health state"))?;
    let probed = r.u8()? != 0;
    let lat = r.u32()?;
    let mp = PathBuf::from(OsStr::from_bytes(r.blob16()?));
    r.end()?;
    Ok(HealthReport { mnt_id, state, probed, latency_us: (lat != u32::MAX).then_some(lat), mount_point: mp })
}

/// A health snapshot entry: `(mnt_id, state, mount point)`.
pub type HealthEntry = (u64, Health, PathBuf);

/// One health event slot: `epoch u64 | flags u8 | count u16 | (mnt_id u64 | state u8 | path16)*`.
///
/// # Errors
/// Malformed event.
pub fn parse_health_event(p: &[u8]) -> VfsResult<(u64, u8, Vec<HealthEntry>)> {
    let mut r = Reader(p);
    let epoch = r.u64()?;
    let flags = r.u8()?;
    let n = r.u16()?;
    let mut out = Vec::with_capacity(usize::from(n));
    for _ in 0..n {
        let id = r.u64()?;
        let st = Health::from_code(r.u8()?).ok_or_else(|| proto("bad health state"))?;
        out.push((id, st, PathBuf::from(OsStr::from_bytes(r.blob16()?))));
    }
    r.end()?;
    Ok((epoch, flags, out))
}

/// `MSG_OP_PROGRESS`: `op_id | bytes_done | bytes_total | files_done` (all u64).
///
/// # Errors
/// Malformed payload.
pub fn parse_progress(p: &[u8]) -> VfsResult<(u64, u64, u64, u64)> {
    let mut r = Reader(p);
    let v = (r.u64()?, r.u64()?, r.u64()?, r.u64()?);
    r.end()?;
    Ok(v)
}

/// `MSG_OP_DONE`: `op_id u64 | status u32 | errno i32 | bytes_done u64 | files_done u64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpDone {
    pub op_id: u64,
    pub status: u32,
    pub errno: i32,
    pub bytes_done: u64,
    pub files_done: u64,
}

/// # Errors
/// Malformed payload.
pub fn parse_done(p: &[u8]) -> VfsResult<OpDone> {
    let mut r = Reader(p);
    let d = OpDone {
        op_id: r.u64()?,
        status: r.u32()?,
        errno: r.u32()? as i32,
        bytes_done: r.u64()?,
        files_done: r.u64()?,
    };
    r.end()?;
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_gungnird() {
        assert_eq!(SHM_SIZE, 266_240, "AGENTS.md: shared memory is 266240 bytes");
        assert_eq!(RING_D2C.slots, 135_168);
        assert_eq!(PAYLOAD_MAX, 4080);
        assert_eq!(&MAGIC.to_le_bytes(), b"GNGR");
        let f = Frame::hello();
        assert_eq!(Frame::decode(&f.encode()), f);
        assert_eq!(f.encode()[..8], [b'G', b'N', b'G', b'R', 2, 0, 1, 0]);
    }

    #[test]
    fn socket_path_order() -> VfsResult<()> {
        let os = |s: &str| Some(OsStr::new(s).to_owned());
        let p = socket_path(os("/s/g.sock").as_deref(), os("/run/user/1000").as_deref(), os("/home/n").as_deref())?;
        assert_eq!(p, PathBuf::from("/s/g.sock"));
        let p = socket_path(os("rel").as_deref(), os("/run/user/1000").as_deref(), None)?;
        assert_eq!(p, PathBuf::from("/run/user/1000/gungnir/gungnird.sock"));
        let p = socket_path(None, None, os("/home/n").as_deref())?;
        assert_eq!(p, PathBuf::from("/home/n/.cache/gungnir/gungnird.sock"));
        assert!(socket_path(None, os("x").as_deref(), None).is_err());
        Ok(())
    }

    #[test]
    fn identity_and_value_round_trip() -> VfsResult<()> {
        let id = Ident {
            fs_uuid: "abcd-1".into(),
            subvol_id: 5,
            inode_num: u64::MAX,
            generation: 7,
            btime_sec: -3,
            btime_nsec: 9,
        };
        let mut b = Vec::new();
        id.write(&mut b)?;
        assert_eq!(b.len(), 1 + 6 + 8 * 4 + 4);
        let mut r = Reader(&b);
        assert_eq!(Ident::read(&mut r)?, id);
        r.end()?;
        for v in [Value::String("Viking".into()), Value::Stringv(vec!["a".into(), "".into()])] {
            assert_eq!(Value::decode(&v.encode()?)?, v);
        }
        assert!(Value::String("x".repeat(4000)).encode().is_err());
        Ok(())
    }

    #[test]
    fn op_start_layout() -> VfsResult<()> {
        let b = req::op_start(OP_DELETE, 0, 0, Path::new("/a"), None)?;
        assert_eq!(b, [3, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, b'/', b'a', 0, 0]);
        assert!(req::op_start(OP_COPY, 0, 0, Path::new("rel"), None).is_err());
        Ok(())
    }

    #[test]
    fn empty_replies_are_read_as_counts_not_errno() {
        let reply = |st: u32, deleted: u32, failed: u32, names: &[u8]| {
            let mut v = st.to_le_bytes().to_vec();
            v.extend_from_slice(&deleted.to_le_bytes());
            v.extend_from_slice(&failed.to_le_bytes());
            v.extend_from_slice(&u16::try_from(names.len()).unwrap_or(0).to_le_bytes());
            v.extend_from_slice(names);
            v
        };
        assert_eq!(parse_empty(&reply(0, 3, 0, b"")), Ok(()));
        // OS error (15) with 5 deleted: once misread as errno 5, EIO.
        assert_eq!(
            parse_empty(&reply(15, 5, 1, b"")),
            Err(VfsError::NotEmpty("1 trash item could not be removed (5 deleted)".into()))
        );
        assert_eq!(
            parse_empty(&reply(18, 0, 2, b"")),
            Err(VfsError::NotEmpty("2 trash items could not be removed (0 deleted)".into()))
        );
        assert_eq!(
            parse_empty(&reply(18, 4, 0, b"")),
            Err(VfsError::NotEmpty("some trash items could not be removed (4 deleted)".into()))
        );
        assert_eq!(parse_empty(&reply(4, 7, 0, b"/mnt/nfs")), Err(VfsError::SuspectMount));
        // No counts: the status alone (busy is retried by the caller).
        assert_eq!(parse_empty(&2u32.to_le_bytes()), Err(VfsError::Busy));
        assert!(parse_empty(&[1, 0]).is_err());
    }

    #[test]
    fn reply_status_maps() {
        assert_eq!(reply_body(&[0, 0, 0, 0, 9], "x"), Ok(vec![9]));
        assert_eq!(reply_body(&[4, 0, 0, 0], "x"), Err(VfsError::SuspectMount));
        assert!(reply_body(&[1], "x").is_err());
    }

    #[test]
    fn health_event_parse() -> VfsResult<()> {
        let mut p = 7u64.to_le_bytes().to_vec();
        p.push(EV_FIRST | EV_LAST);
        p.extend_from_slice(&1u16.to_le_bytes());
        p.extend_from_slice(&42u64.to_le_bytes());
        p.push(2);
        p.extend_from_slice(&4u16.to_le_bytes());
        p.extend_from_slice(b"/mnt");
        let (epoch, flags, e) = parse_health_event(&p)?;
        assert_eq!((epoch, flags), (7, 3));
        assert_eq!(e, vec![(42, Health::Hung, PathBuf::from("/mnt"))]);
        assert!(parse_health_event(&p[..p.len() - 1]).is_err());
        Ok(())
    }
}
