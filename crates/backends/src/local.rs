//! `LocalPosixBackend`: the local filesystem.
//!
//! Listing: one blocking task reads the directory (`readdir` is cheap) and hands
//! chunks of entries to a small pool of scoped threads that `fstatat` them in
//! parallel, so the first `first_batch` entries reach the UI before the rest is even
//! read. Every loop checks the `CancellationToken`.

use std::ffi::CString;
use std::fs::{self, DirEntry};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use core_vfs::provider::DirWatch;
use core_vfs::{
    BackendKind, CancellationToken, EntrySink, FileEntry, FileKind, ListOptions, ListSummary, Scheme, TransferControl,
    TransferKind, TransferProgress, TransferRequest, TransferState, VfsCapabilities, VfsError, VfsMetadata,
    VfsProvider, VfsResult, VfsUri, VolumeInfo, WatchEvent,
};
use notify::{RecursiveMode, Watcher};
use tokio::sync::mpsc;

/// Threads that stat entries in parallel during a listing.
const STAT_WORKERS: usize = 4;
/// Copy buffer.
const CHUNK: usize = 1 << 20;
/// Minimum spacing of progress reports.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

#[derive(Debug, Default, Clone)]
pub struct LocalPosixBackend;

impl LocalPosixBackend {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

fn local(uri: &VfsUri) -> VfsResult<PathBuf> {
    if uri.scheme() == Scheme::Trash {
        let trash = home_trash()?;
        let files = trash.join("files");
        let rel = uri.path().trim_start_matches('/');
        if rel.is_empty() {
            if !files.is_dir() {
                let _ = fs::create_dir_all(&files);
            }
            Ok(files)
        } else {
            Ok(files.join(rel))
        }
    } else {
        uri.to_local_path().ok_or_else(|| VfsError::NoBackend(uri.display()))
    }
}

fn io_err<'a>(op: &'static str, p: &'a Path) -> impl Fn(std::io::Error) -> VfsError + 'a {
    move |e| VfsError::from_io(op, &e, &p.display().to_string())
}

/// Run blocking filesystem work off the async runtime threads.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> VfsResult<T> + Send + 'static) -> VfsResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| VfsError::Transport(format!("worker task failed: {e}")))?
}

/// Build an entry from `lstat` data (`md`), following a symlink once to learn
/// whether it points at a directory.
pub(crate) fn entry_from_metadata(name: String, uri: VfsUri, path: &Path, md: &fs::Metadata) -> FileEntry {
    let ft = md.file_type();
    let kind = if ft.is_dir() {
        FileKind::Directory
    } else if ft.is_symlink() {
        FileKind::Symlink
    } else if ft.is_file() {
        FileKind::File
    } else {
        FileKind::Special
    };
    let target_is_dir = kind == FileKind::Symlink && fs::metadata(path).is_ok_and(|m| m.is_dir());
    FileEntry {
        name,
        uri,
        kind,
        target_is_dir,
        size: if kind == FileKind::Directory { 0 } else { md.len() },
        modified: md.modified().ok(),
        mode: md.mode() & 0o7777,
        vfs: VfsMetadata::default(),
    }
}

fn entry_from_dirent(de: &DirEntry, dir: &VfsUri) -> Option<FileEntry> {
    // VfsUri is UTF-8-only. Never turn invalid bytes into a different,
    // potentially existing filename that a destructive action could target.
    let name = de.file_name().into_string().ok()?;
    let uri = dir.join(&name).ok()?;
    let md = de.metadata().ok()?; // fstatat(AT_SYMLINK_NOFOLLOW) relative to the dir fd
    Some(entry_from_metadata(name, uri, &de.path(), &md))
}

/// The blocking body of `list_dir`.
fn list_blocking(
    path: &Path,
    dir: &VfsUri,
    opts: ListOptions,
    sink: &EntrySink,
    cancel: &CancellationToken,
) -> VfsResult<ListSummary> {
    let rd = fs::read_dir(path).map_err(io_err("opendir", path))?;
    let summary = Mutex::new(ListSummary::default());
    let send = |mut batch: Vec<FileEntry>, skipped: u64| -> VfsResult<()> {
        {
            let mut s = summary.lock().unwrap_or_else(PoisonError::into_inner);
            s.entries += batch.len() as u64;
            s.skipped += skipped;
        }
        if batch.is_empty() {
            return Ok(());
        }
        loop {
            if cancel.is_cancelled() {
                return Err(VfsError::Cancelled);
            }
            match sink.try_send(batch) {
                Ok(()) => return Ok(()),
                Err(mpsc::error::TrySendError::Closed(_)) => return Err(VfsError::Cancelled),
                Err(mpsc::error::TrySendError::Full(pending)) => {
                    batch = pending;
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    };
    let stat_chunk = |chunk: Vec<DirEntry>| -> VfsResult<()> {
        let n = chunk.len();
        let batch: Vec<FileEntry> = chunk.iter().filter_map(|de| entry_from_dirent(de, dir)).collect();
        let skipped = (n - batch.len()) as u64;
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        send(batch, skipped)
    };

    let mut rd = rd.filter_map(Result::ok);
    // First batch inline: on screen as soon as possible.
    let first: Vec<DirEntry> = rd.by_ref().take(opts.first_batch.max(1)).collect();
    let more = first.len() == opts.first_batch.max(1);
    stat_chunk(first)?;
    if !more {
        return Ok(*summary.lock().unwrap_or_else(PoisonError::into_inner));
    }

    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<DirEntry>>(STAT_WORKERS * 2);
    let rx = Arc::new(Mutex::new(rx));
    let result: VfsResult<()> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..STAT_WORKERS)
            .map(|_| {
                let rx = Arc::clone(&rx);
                let stat_chunk = &stat_chunk;
                s.spawn(move || -> VfsResult<()> {
                    loop {
                        let next = rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
                        match next {
                            Ok(chunk) => stat_chunk(chunk)?,
                            Err(_) => return Ok(()),
                        }
                    }
                })
            })
            .collect();
        // Only workers own the receiver, so a failed worker cannot leave the feeder blocked.
        drop(rx);
        let mut chunk = Vec::with_capacity(opts.batch);
        let mut feed = || -> VfsResult<()> {
            for de in rd.by_ref() {
                chunk.push(de);
                if chunk.len() >= opts.batch.max(1) {
                    if cancel.is_cancelled() {
                        return Err(VfsError::Cancelled);
                    }
                    tx.send(std::mem::take(&mut chunk)).map_err(|_| VfsError::Cancelled)?;
                }
            }
            if !chunk.is_empty() {
                tx.send(std::mem::take(&mut chunk)).map_err(|_| VfsError::Cancelled)?;
            }
            Ok(())
        };
        let fed = feed();
        drop(tx);
        let mut out = fed;
        for w in workers {
            let r = w.join().unwrap_or(Err(VfsError::Transport("stat worker panicked".into())));
            if out.is_ok() {
                out = r;
            }
        }
        out
    });
    result?;
    Ok(*summary.lock().unwrap_or_else(PoisonError::into_inner))
}

fn cstring(p: &Path) -> VfsResult<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| VfsError::InvalidUri(format!("{} contains NUL", p.display())))
}

/// `renameat2(RENAME_NOREPLACE)`: never clobbers an existing destination.
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    let (a, b) = (
        CString::new(from.as_os_str().as_bytes()).map_err(|_| std::io::ErrorKind::InvalidInput)?,
        CString::new(to.as_os_str().as_bytes()).map_err(|_| std::io::ErrorKind::InvalidInput)?,
    );
    // SAFETY: both strings are valid NUL-terminated paths for the duration of the call.
    let r = unsafe { libc::renameat2(libc::AT_FDCWD, a.as_ptr(), libc::AT_FDCWD, b.as_ptr(), libc::RENAME_NOREPLACE) };
    if r == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

/// `name`, else `name (copy).ext`, `name (copy 2).ext`, ... : the first free name in `dir`.
pub(crate) fn unique_name(dir: &Path, name: &str, exists: impl Fn(&Path) -> bool) -> String {
    if !exists(&dir.join(name)) {
        return name.to_owned();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    for n in 1u32.. {
        let cand = if n == 1 { format!("{stem} (copy){ext}") } else { format!("{stem} (copy {n}){ext}") };
        if !exists(&dir.join(&cand)) {
            return cand;
        }
    }
    name.to_owned()
}

struct Copier<'a> {
    ctl: &'a TransferControl,
    progress: TransferProgress,
    last_report: Instant,
    buf: Vec<u8>,
}

impl Copier<'_> {
    fn report(&mut self, force: bool) {
        if force || self.last_report.elapsed() >= PROGRESS_EVERY {
            self.last_report = Instant::now();
            // try_send: a busy UI drops intermediate reports instead of stalling the copy.
            let _ = self.ctl.progress.try_send(self.progress.clone());
        }
    }

    /// Honour pause and cancel between chunks.
    fn checkpoint(&mut self) -> VfsResult<()> {
        if self.ctl.cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        if *self.ctl.paused.borrow() {
            self.progress.state = TransferState::Paused;
            self.report(true);
            while *self.ctl.paused.borrow() {
                if self.ctl.cancel.is_cancelled() {
                    return Err(VfsError::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            self.progress.state = TransferState::Running;
            self.report(true);
        }
        Ok(())
    }

    fn copy_tree(&mut self, src: &Path, dst: &Path) -> VfsResult<()> {
        self.checkpoint()?;
        let md = fs::symlink_metadata(src).map_err(io_err("lstat", src))?;
        self.progress.current = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let ft = md.file_type();
        if ft.is_symlink() {
            let target = fs::read_link(src).map_err(io_err("readlink", src))?;
            std::os::unix::fs::symlink(&target, dst).map_err(io_err("symlink", dst))?;
            self.progress.files_done += 1;
        } else if ft.is_dir() {
            fs::create_dir(dst).map_err(io_err("mkdir", dst))?;
            let mut children: Vec<_> = fs::read_dir(src)
                .map_err(io_err("opendir", src))?
                .collect::<Result<_, _>>()
                .map_err(io_err("readdir", src))?;
            children.sort_by_key(DirEntry::file_name);
            for c in children {
                self.copy_tree(&c.path(), &dst.join(c.file_name()))?;
            }
            fs::set_permissions(dst, fs::Permissions::from_mode(md.mode() & 0o7777)).map_err(io_err("chmod", dst))?;
            set_times(dst, &md);
        } else if ft.is_file() {
            self.copy_file(src, dst, &md)?;
            self.progress.files_done += 1;
        } else {
            return Err(VfsError::Unsupported(format!("{}: special files are not copied", src.display())));
        }
        self.report(false);
        Ok(())
    }

    fn copy_file(&mut self, src: &Path, dst: &Path, md: &fs::Metadata) -> VfsResult<()> {
        let mut input = fs::File::open(src).map_err(io_err("open", src))?;
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(md.mode() & 0o7777)
            .open(dst)
            .map_err(io_err("create", dst))?;
        let res = (|| -> VfsResult<()> {
            loop {
                self.checkpoint()?;
                let n = input.read(&mut self.buf).map_err(io_err("read", src))?;
                if n == 0 {
                    break;
                }
                out.write_all(&self.buf[..n]).map_err(io_err("write", dst))?;
                self.progress.bytes_done += n as u64;
                self.report(false);
            }
            out.sync_data().map_err(io_err("fsync", dst))?;
            set_times(dst, md);
            Ok(())
        })();
        if res.is_err() {
            drop(out);
            let _ = fs::remove_file(dst); // never leave a partial destination
        }
        res
    }
}

/// Keep the source's access and modification times (best effort).
fn set_times(dst: &Path, md: &fs::Metadata) {
    let ts = |s: i64, ns: i64| libc::timespec { tv_sec: s as libc::time_t, tv_nsec: ns as libc::c_long };
    let times = [ts(md.atime(), md.atime_nsec()), ts(md.mtime(), md.mtime_nsec())];
    if let Ok(c) = cstring(dst) {
        // SAFETY: valid path and a two-element timespec array, as utimensat requires.
        unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
    }
}

/// Bytes and files below `p` (symlinks not followed).
fn measure(p: &Path, cancel: &CancellationToken) -> VfsResult<(u64, u64)> {
    if cancel.is_cancelled() {
        return Err(VfsError::Cancelled);
    }
    let md = fs::symlink_metadata(p).map_err(io_err("lstat", p))?;
    if md.is_dir() {
        let mut acc = (0, 0);
        for c in fs::read_dir(p).map_err(io_err("opendir", p))? {
            let c = c.map_err(io_err("readdir", p))?;
            let (b, f) = measure(&c.path(), cancel)?;
            acc = (acc.0 + b, acc.1 + f);
        }
        Ok(acc)
    } else {
        Ok((if md.is_file() { md.len() } else { 0 }, 1))
    }
}

fn create_transfer_stage(dir: &Path, id: u64) -> VfsResult<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    for n in 0..10_000 {
        let stage = dir.join(format!(".gungnir-transfer-{}-{id}-{n}", std::process::id()));
        match fs::DirBuilder::new().mode(0o700).create(&stage) {
            Ok(()) => return Ok(stage),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_err("mkdir", &stage)(e)),
        }
    }
    Err(VfsError::AlreadyExists("transfer staging directory".into()))
}

fn exchange_paths(a: &Path, b: &Path) -> std::io::Result<()> {
    let a = CString::new(a.as_os_str().as_bytes()).map_err(|_| std::io::ErrorKind::InvalidInput)?;
    let b = CString::new(b.as_os_str().as_bytes()).map_err(|_| std::io::ErrorKind::InvalidInput)?;
    // SAFETY: both paths are valid NUL-terminated strings. The exchange is atomic.
    let result =
        unsafe { libc::renameat2(libc::AT_FDCWD, a.as_ptr(), libc::AT_FDCWD, b.as_ptr(), libc::RENAME_EXCHANGE) };
    if result == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

fn transfer_blocking(req: &TransferRequest, ctl: &TransferControl) -> VfsResult<()> {
    let dest_dir = local(&req.dest_dir)?;
    let sources: Vec<PathBuf> = req.sources.iter().map(local).collect::<VfsResult<_>>()?;
    let canonical_dest = fs::canonicalize(&dest_dir).map_err(io_err("realpath", &dest_dir))?;
    let mut totals = (0, 0);
    for s in &sources {
        let md = fs::symlink_metadata(s).map_err(io_err("lstat", s))?;
        if md.is_dir() && canonical_dest.starts_with(fs::canonicalize(s).map_err(io_err("realpath", s))?) {
            return Err(VfsError::WouldRecurse);
        }
        let (b, f) = measure(s, &ctl.cancel)?;
        totals = (totals.0 + b, totals.1 + f);
    }
    let mut c = Copier {
        ctl,
        progress: TransferProgress {
            id: req.id,
            bytes_done: 0,
            bytes_total: totals.0,
            files_done: 0,
            files_total: totals.1,
            current: String::new(),
            state: TransferState::Running,
        },
        last_report: Instant::now(),
        buf: vec![0; CHUNK],
    };
    c.report(true);
    for src in &sources {
        let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let target_name =
            if req.overwrite { name.clone() } else { unique_name(&dest_dir, &name, |p| p.symlink_metadata().is_ok()) };
        let dst = dest_dir.join(&target_name);
        // Stage replacements on the destination filesystem. Failure/cancellation must
        // leave the existing destination intact, including when a folder copy fails.
        if req.overwrite && dst.symlink_metadata().is_ok() {
            let src_md = fs::symlink_metadata(src).map_err(io_err("lstat", src))?;
            let dst_md = fs::symlink_metadata(&dst).map_err(io_err("lstat", &dst))?;
            if src_md.dev() == dst_md.dev() && src_md.ino() == dst_md.ino() {
                return Err(VfsError::BadRequest);
            }
            let stage = create_transfer_stage(&dest_dir, req.id)?;
            let staged = stage.join("item");
            let result = c.copy_tree(src, &staged).and_then(|()| {
                c.checkpoint()?;
                exchange_paths(&staged, &dst).map_err(io_err("replace", &dst))
            });
            // After the exchange the stage holds the old destination; before it,
            // it holds only the partial copy. Never remove the destination on error.
            let _ = remove_any(&stage);
            result?;
            if req.kind == TransferKind::Move {
                remove_any(src)?;
            }
            continue;
        }
        c.checkpoint()?;
        if req.kind == TransferKind::Move {
            match rename_noreplace(src, &dst) {
                Ok(()) => {
                    let (b, f) = measure(&dst, &ctl.cancel).unwrap_or((0, 1));
                    c.progress.bytes_done += b;
                    c.progress.files_done += f;
                    c.report(false);
                    continue;
                }
                Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {} // copy, then delete
                Err(e) => return Err(VfsError::from_io("rename", &e, &src.display().to_string())),
            }
        }
        if let Err(e) = c.copy_tree(src, &dst) {
            if dst.symlink_metadata().is_ok() && !matches!(e, VfsError::AlreadyExists(_)) {
                let _ = remove_any(&dst);
            }
            return Err(e);
        }
        if req.kind == TransferKind::Move {
            remove_any(src)?;
        }
    }
    c.progress.state = TransferState::Done;
    c.progress.current.clear();
    c.report(true);
    Ok(())
}

fn remove_any(p: &Path) -> VfsResult<()> {
    let md = fs::symlink_metadata(p).map_err(io_err("lstat", p))?;
    if md.is_dir() {
        fs::remove_dir_all(p).map_err(io_err("rmdir", p))?; // never follows symlinks
    } else {
        fs::remove_file(p).map_err(io_err("unlink", p))?;
    }
    if let Some(name) = p.file_name()
        && let Some(parent) = p.parent()
        && home_trash().is_ok_and(|trash| parent == trash.join("files"))
        && let Some(trash) = parent.parent()
    {
        let info = trash.join("info").join(format!("{}.trashinfo", name.to_string_lossy()));
        let _ = fs::remove_file(&info);
    }
    Ok(())
}

fn trash_decode(encoded: &str) -> Option<PathBuf> {
    let mut bytes = Vec::new();
    let mut chars = encoded.as_bytes().iter();
    while let Some(&b) = chars.next() {
        if b == b'%' {
            let h1 = *chars.next()?;
            let h2 = *chars.next()?;
            let hex_bytes = [h1, h2];
            let hex_str = std::str::from_utf8(&hex_bytes).ok()?;
            let val = u8::from_str_radix(hex_str, 16).ok()?;
            bytes.push(val);
        } else {
            bytes.push(b);
        }
    }
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

fn restore_one(trash: &Path, name: &str) -> VfsResult<PathBuf> {
    let files = trash.join("files").join(name);
    let info = trash.join("info").join(format!("{name}.trashinfo"));
    if !files.exists() && fs::symlink_metadata(&files).is_err() {
        return Err(VfsError::NotFound(format!("trash item {name}")));
    }
    let info_content = fs::read_to_string(&info).map_err(io_err("read", &info))?;
    let mut orig_path: Option<PathBuf> = None;
    for line in info_content.lines() {
        if let Some(rest) = line.strip_prefix("Path=") {
            orig_path = trash_decode(rest.trim());
            break;
        }
    }
    let orig = orig_path.filter(|path| path.is_absolute()).ok_or(VfsError::BadRequest)?;
    if let Some(parent) = orig.parent()
        && !parent.is_dir()
    {
        fs::create_dir_all(parent).map_err(io_err("mkdir", parent))?;
    }
    rename_noreplace(&files, &orig).map_err(io_err("restore", &orig))?;
    let _ = fs::remove_file(&info);
    Ok(orig)
}

fn empty_trash_dir(trash: &Path) -> VfsResult<()> {
    let files_dir = trash.join("files");
    if !files_dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(&files_dir).map_err(io_err("opendir", &files_dir))? {
        let entry = entry.map_err(io_err("readdir", &files_dir))?;
        remove_any(&entry.path())?;
        let info = trash.join("info").join(format!("{}.trashinfo", entry.file_name().to_string_lossy()));
        match fs::remove_file(&info) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err("unlink", &info)(e)),
        }
    }
    Ok(())
}

/// Home trash (`$XDG_DATA_HOME/Trash`, else `~/.local/share/Trash`).
pub fn home_trash() -> VfsResult<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or_else(|| VfsError::Unsupported("no HOME for the trash".into()))?;
    Ok(base.join("Trash"))
}

/// Percent-encode a path for a `.trashinfo` `Path=` line.
fn trash_encode(p: &Path) -> String {
    let mut out = String::new();
    for &c in p.as_os_str().as_bytes() {
        if c.is_ascii_alphanumeric() || b"/-._~".contains(&c) {
            out.push(char::from(c));
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

fn trash_one(trash: &Path, src: &Path) -> VfsResult<()> {
    let files = trash.join("files");
    let info = trash.join("info");
    for d in [&files, &info] {
        fs::create_dir_all(d).map_err(io_err("mkdir", d))?;
    }
    let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // Reserve the name by creating the .trashinfo exclusively (Trash spec 1.0).
    for n in 1u32..10_000 {
        let cand = if n == 1 { name.clone() } else { format!("{name}.{n}") };
        let info_path = info.join(format!("{cand}.trashinfo"));
        let mut f = match fs::OpenOptions::new().write(true).create_new(true).open(&info_path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(VfsError::from_io("create", &e, &info_path.display().to_string())),
        };
        let body = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            trash_encode(src),
            crate::time::trash_date(SystemTime::now())
        );
        if let Err(e) = f.write_all(body.as_bytes()) {
            let _ = fs::remove_file(&info_path);
            return Err(io_err("write", &info_path)(e));
        }
        match rename_noreplace(src, &files.join(&cand)) {
            Ok(()) => return Ok(()),
            Err(e) => {
                let _ = fs::remove_file(&info_path);
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    continue;
                }
                return if e.raw_os_error() == Some(libc::EXDEV) {
                    Err(VfsError::Unsupported(format!(
                        "{} is on another filesystem than the trash; use Shift+Delete to delete it permanently",
                        src.display()
                    )))
                } else {
                    Err(VfsError::from_io("rename", &e, &src.display().to_string()))
                };
            }
        };
    }
    Err(VfsError::AlreadyExists(format!("trash name for {}", src.display())))
}

#[async_trait]
impl VfsProvider for LocalPosixBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Posix
    }

    fn capabilities(&self) -> VfsCapabilities {
        VfsCapabilities {
            writable: true,
            watch: true,
            trash: true,
            pause_transfers: true,
            vfs_metadata: false,
            mount_health: false,
        }
    }

    fn handles(&self, uri: &VfsUri) -> bool {
        matches!(uri.scheme(), Scheme::File | Scheme::Trash)
    }

    async fn list_dir(
        &self,
        dir: &VfsUri,
        opts: ListOptions,
        sink: EntrySink,
        cancel: CancellationToken,
    ) -> VfsResult<ListSummary> {
        let path = local(dir)?;
        let dir = dir.clone();
        blocking(move || list_blocking(&path, &dir, opts, &sink, &cancel)).await
    }

    async fn stat(&self, uri: &VfsUri) -> VfsResult<FileEntry> {
        let path = local(uri)?;
        let uri = uri.clone();
        blocking(move || {
            let md = fs::symlink_metadata(&path).map_err(io_err("lstat", &path))?;
            let name = uri.file_name().unwrap_or("/").to_owned();
            Ok(entry_from_metadata(name, uri, &path, &md))
        })
        .await
    }

    async fn create_file(&self, uri: &VfsUri) -> VfsResult<()> {
        if uri.scheme() != Scheme::File {
            return Err(VfsError::Unsupported("Restore items before editing the Trash".into()));
        }
        let path = local(uri)?;
        tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
            .map(|_| ())
            .map_err(io_err("create", &path))
    }

    async fn create_dir(&self, uri: &VfsUri) -> VfsResult<()> {
        if uri.scheme() != Scheme::File {
            return Err(VfsError::Unsupported("Cannot create folders in the Trash".into()));
        }
        let path = local(uri)?;
        tokio::fs::create_dir(&path).await.map_err(io_err("mkdir", &path))
    }

    async fn rename(&self, uri: &VfsUri, new_name: &str) -> VfsResult<VfsUri> {
        if uri.scheme() != Scheme::File {
            return Err(VfsError::Unsupported("Restore items before renaming them".into()));
        }
        let parent = uri.parent().ok_or(VfsError::BadRequest)?;
        let to = parent.join(new_name)?;
        let (a, b) = (local(uri)?, local(&to)?);
        blocking(move || rename_noreplace(&a, &b).map_err(io_err("rename", &a))).await?;
        Ok(to)
    }

    async fn delete(&self, uris: &[VfsUri], cancel: CancellationToken) -> VfsResult<()> {
        let paths: Vec<PathBuf> = uris.iter().map(local).collect::<VfsResult<_>>()?;
        blocking(move || {
            for p in &paths {
                if cancel.is_cancelled() {
                    return Err(VfsError::Cancelled);
                }
                remove_any(p)?;
            }
            Ok(())
        })
        .await
    }

    async fn trash(&self, uris: &[VfsUri]) -> VfsResult<()> {
        if uris.iter().any(|uri| uri.scheme() != Scheme::File) {
            return Err(VfsError::BadRequest);
        }
        let paths: Vec<PathBuf> = uris.iter().map(local).collect::<VfsResult<_>>()?;
        blocking(move || {
            let trash = home_trash()?;
            paths.iter().try_for_each(|p| trash_one(&trash, p))
        })
        .await
    }

    async fn restore(&self, uris: &[VfsUri]) -> VfsResult<()> {
        let items: Vec<String> = uris
            .iter()
            .map(|u| {
                if u.scheme() != Scheme::Trash || !u.parent().is_some_and(|parent| parent.is_root()) {
                    return Err(VfsError::BadRequest);
                }
                u.file_name().ok_or(VfsError::BadRequest).map(str::to_owned)
            })
            .collect::<VfsResult<_>>()?;
        blocking(move || {
            let trash = home_trash()?;
            for item in items {
                restore_one(&trash, &item)?;
            }
            Ok(())
        })
        .await
    }

    async fn empty_trash(&self) -> VfsResult<()> {
        blocking(move || {
            let trash = home_trash()?;
            empty_trash_dir(&trash)
        })
        .await
    }

    fn can_transfer(&self, src: &VfsUri, dest: &VfsUri) -> bool {
        src.scheme() == Scheme::File && dest.scheme() == Scheme::File
    }

    async fn transfer(&self, req: TransferRequest, ctl: TransferControl) -> VfsResult<()> {
        if req.dest_dir.scheme() != Scheme::File || req.sources.iter().any(|uri| uri.scheme() != Scheme::File) {
            return Err(VfsError::Unsupported("Use Trash or Restore instead of transferring Trash paths".into()));
        }
        let progress = ctl.progress.clone();
        let id = req.id;
        let r = blocking(move || transfer_blocking(&req, &ctl)).await;
        if let Err(e) = &r {
            let state =
                if *e == VfsError::Cancelled { TransferState::Cancelled } else { TransferState::Failed(e.to_string()) };
            let _ = progress
                .send(TransferProgress {
                    id,
                    bytes_done: 0,
                    bytes_total: 0,
                    files_done: 0,
                    files_total: 0,
                    current: String::new(),
                    state,
                })
                .await;
        }
        r
    }

    async fn volume(&self, uri: &VfsUri) -> VfsResult<VolumeInfo> {
        let path = local(uri)?;
        blocking(move || statvfs(&path)).await
    }

    async fn inspect(&self, uri: &VfsUri) -> VfsResult<Vec<(String, String)>> {
        let path = local(uri)?;
        blocking(move || {
            let md = fs::symlink_metadata(&path).map_err(io_err("lstat", &path))?;
            let mut out = vec![
                ("Path".to_owned(), path.display().to_string()),
                ("Device".to_owned(), format!("{}:{}", libc::major(md.dev()), libc::minor(md.dev()))),
                ("Inode".to_owned(), md.ino().to_string()),
                ("Links".to_owned(), md.nlink().to_string()),
                ("Owner".to_owned(), format!("uid {} gid {}", md.uid(), md.gid())),
                ("Mode".to_owned(), format!("{:o}", md.mode() & 0o7777)),
                ("Size".to_owned(), format!("{} bytes", md.len())),
            ];
            if md.file_type().is_symlink()
                && let Ok(t) = fs::read_link(&path)
            {
                out.push(("Link target".to_owned(), t.display().to_string()));
            }
            out.extend(xattrs(&path).into_iter().map(|(k, v)| (format!("xattr {k}"), v)));
            Ok(out)
        })
        .await
    }

    fn watch(&self, dir: &VfsUri) -> VfsResult<Option<DirWatch>> {
        let path = local(dir)?;
        // Capacity 1 + try_send: bursts collapse into one pending "Changed".
        let (tx, rx) = mpsc::channel(1);
        let watched = path.clone();
        let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let ev = match res {
                Ok(ev) if ev.kind.is_remove() && ev.paths.iter().any(|p| p == &watched) => WatchEvent::Removed,
                Ok(ev) if ev.kind.is_access() => return,
                Ok(_) | Err(_) => WatchEvent::Changed,
            };
            let _ = tx.try_send(ev);
        })
        .map_err(|e| VfsError::Unsupported(format!("watch: {e}")))?;
        w.watch(&path, RecursiveMode::NonRecursive)
            .map_err(|e| VfsError::Unsupported(format!("watch {}: {e}", path.display())))?;
        Ok(Some(DirWatch { events: rx, guard: Box::new(Arc::new(Mutex::new(w))) }))
    }
}

pub(crate) fn statvfs(path: &Path) -> VfsResult<VolumeInfo> {
    let c = cstring(path)?;
    // SAFETY: `st` is plain old data, fully written by a successful statvfs.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid path and out-pointer.
    if unsafe { libc::statvfs(c.as_ptr(), &raw mut st) } != 0 {
        return Err(VfsError::from_io("statvfs", &std::io::Error::last_os_error(), &path.display().to_string()));
    }
    let frsize = st.f_frsize as u64;
    Ok(VolumeInfo { total_bytes: st.f_blocks as u64 * frsize, free_bytes: st.f_bavail as u64 * frsize })
}

/// Extended attributes, values shown as text when they are UTF-8 (best effort).
fn xattrs(path: &Path) -> Vec<(String, String)> {
    let Ok(c) = cstring(path) else { return Vec::new() };
    let mut names = vec![0u8; 4096];
    // SAFETY: buffer pointer and length match.
    let n = unsafe { libc::llistxattr(c.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
    let Ok(n) = usize::try_from(n) else { return Vec::new() };
    names.truncate(n);
    let mut out = Vec::new();
    for name in names.split(|&b| b == 0).filter(|s| !s.is_empty()) {
        let Ok(cn) = CString::new(name) else { continue };
        let mut val = vec![0u8; 1024];
        // SAFETY: valid strings and a buffer of the given length.
        let m = unsafe { libc::lgetxattr(c.as_ptr(), cn.as_ptr(), val.as_mut_ptr().cast(), val.len()) };
        let v = usize::try_from(m).map_or_else(
            |_| "(unreadable)".to_owned(),
            |m| {
                val.truncate(m);
                String::from_utf8(val).unwrap_or_else(|v| format!("{} bytes", v.as_bytes().len()))
            },
        );
        out.push((String::from_utf8_lossy(name).into_owned(), v));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod trash_tests {
    use super::*;

    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!("gungnir-trash-unit-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn orphaned_trash_name_uses_next_available_name() {
        let t = Sandbox::new("orphan");
        let trash = t.0.join("Trash");
        fs::create_dir_all(trash.join("files")).unwrap();
        fs::write(trash.join("files/item"), b"existing orphan").unwrap();
        let source = t.0.join("item");
        fs::write(&source, b"new item").unwrap();
        trash_one(&trash, &source).unwrap();
        assert_eq!(fs::read(trash.join("files/item")).unwrap(), b"existing orphan");
        assert_eq!(fs::read(trash.join("files/item.2")).unwrap(), b"new item");
        assert!(!trash.join("info/item.trashinfo").exists());
        assert!(trash.join("info/item.2.trashinfo").exists());
        assert_eq!(restore_one(&trash, "item.2").unwrap(), source);
    }

    #[test]
    fn restore_refuses_relative_metadata_paths_without_moving_item() {
        let t = Sandbox::new("relative");
        let trash = t.0.join("Trash");
        fs::create_dir_all(trash.join("files")).unwrap();
        fs::create_dir_all(trash.join("info")).unwrap();
        fs::write(trash.join("files/item"), b"keep").unwrap();
        fs::write(trash.join("info/item.trashinfo"), b"[Trash Info]\nPath=relative-parent/item\n").unwrap();
        assert_eq!(restore_one(&trash, "item"), Err(VfsError::BadRequest));
        assert!(trash.join("files/item").exists());
        assert!(trash.join("info/item.trashinfo").exists());
    }

    #[test]
    fn failed_restore_collision_keeps_both_files_and_metadata() {
        let t = Sandbox::new("collision");
        let trash = t.0.join("Trash");
        let source = t.0.join("item");
        fs::write(&source, b"old").unwrap();
        trash_one(&trash, &source).unwrap();
        fs::write(&source, b"new").unwrap();
        assert!(matches!(restore_one(&trash, "item"), Err(VfsError::AlreadyExists(_))));
        assert_eq!(fs::read(&source).unwrap(), b"new");
        assert_eq!(fs::read(trash.join("files/item")).unwrap(), b"old");
        assert!(trash.join("info/item.trashinfo").exists());
    }

    #[tokio::test]
    async fn restore_accepts_only_top_level_trash_uris() {
        let backend = LocalPosixBackend::new();
        for uri in [
            VfsUri::parse("/this-is-not-a-trash-entry").unwrap(),
            VfsUri::trash("/").unwrap(),
            VfsUri::trash("folder/nested").unwrap(),
        ] {
            assert_eq!(backend.restore(&[uri]).await, Err(VfsError::BadRequest));
        }
        assert!(backend.rename(&VfsUri::trash("item").unwrap(), "changed").await.is_err());
        assert!(backend.create_dir(&VfsUri::trash("orphan").unwrap()).await.is_err());
        assert!(!backend.can_transfer(&VfsUri::parse("/item").unwrap(), &VfsUri::trash("/").unwrap()));
    }
}
