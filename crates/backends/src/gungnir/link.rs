//! One connection to gungnird, driven by a dedicated `gungnir-link` thread.
//!
//! The ring protocol waits on futexes, which must never happen on a tokio worker or
//! the UI thread. Callers hand requests to the link thread over a channel and await a
//! `oneshot`; the thread pushes them into the client->daemon ring (at most
//! [`MAX_INFLIGHT`] unanswered), waits on the reply ring in bounded slices, routes
//! replies by `seq`, op progress by op id, and assembles mount-health snapshots.
//! Every request has a deadline; a closed control socket fails everything pending
//! with `DaemonUnavailable` and marks the link dead (the backend reconnects).

use std::collections::{HashMap, VecDeque};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc as std_mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use core_vfs::{VfsError, VfsResult};
use tokio::sync::{mpsc, oneshot, watch};

use super::proto::{self, Health, MAX_INFLIGHT, Msg, RING_C2D, RING_D2C, Shm};

/// Longest single futex sleep of the link thread (also the liveness poll period).
const SLICE: Duration = Duration::from_millis(25);

/// What a running daemon op reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpEvent {
    Progress { bytes_done: u64, bytes_total: u64, files_done: u64 },
    Done(proto::OpDone),
}

/// The latest complete health snapshot: every unhealthy mount plus every mount at or
/// below one, so a longest-prefix lookup on it equals one on the full table.
#[derive(Debug, Clone, Default)]
pub struct HealthTable {
    pub epoch: u64,
    /// `false` before the first snapshot and after an incomplete one: no verdicts.
    pub trusted: bool,
    pub mounts: Vec<proto::HealthEntry>,
}

impl HealthTable {
    /// Lexical longest-prefix lookup; never touches the path.
    #[must_use]
    pub fn lookup(&self, p: &Path) -> Option<(Health, &Path)> {
        if !self.trusted {
            return None;
        }
        self.mounts
            .iter()
            .filter(|(_, _, mp)| p.starts_with(mp))
            .max_by_key(|(_, _, mp)| mp.as_os_str().len())
            .map(|(_, h, mp)| (*h, mp.as_path()))
    }
}

enum Cmd {
    Call {
        kind: u32,
        payload: Vec<u8>,
        deadline: Instant,
        reply: oneshot::Sender<VfsResult<Vec<u8>>>,
        /// Hand back the whole reply (`status | body`) for the caller to read.
        raw: bool,
    },
    StartOp {
        payload: Vec<u8>,
        deadline: Instant,
        started: oneshot::Sender<VfsResult<u64>>,
        events: mpsc::UnboundedSender<OpEvent>,
    },
}

enum Pending {
    Call {
        reply: oneshot::Sender<VfsResult<Vec<u8>>>,
        deadline: Instant,
        raw: bool,
    },
    /// Before the start reply: `started` is Some. After: waits for MSG_OP_DONE.
    Op {
        started: Option<oneshot::Sender<VfsResult<u64>>>,
        events: mpsc::UnboundedSender<OpEvent>,
        deadline: Instant,
    },
}

/// Longest silence during a request that sends beats.
const BEAT_SILENCE: Duration = Duration::from_secs(65);

struct Shared {
    alive: AtomicBool,
    health: Mutex<HealthTable>,
    health_tx: watch::Sender<u64>,
}

/// A live connection. Dropping the last handle stops the thread.
pub struct Link {
    tx: Mutex<Option<std_mpsc::Sender<Cmd>>>,
    shm: Arc<Shm>,
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
    pub socket: PathBuf,
}

impl Link {
    /// Handshake (blocking, bounded by `timeout`) and start the link thread. Call from
    /// a blocking context.
    ///
    /// # Errors
    /// `DaemonUnavailable` or a transport error.
    pub fn connect(sock: &Path, timeout: Duration) -> VfsResult<Arc<Self>> {
        let (fd, shm) = proto::handshake(sock, timeout)?;
        let shm = Arc::new(shm);
        let (health_tx, _) = watch::channel(0);
        let shared =
            Arc::new(Shared { alive: AtomicBool::new(true), health: Mutex::new(HealthTable::default()), health_tx });
        let (tx, rx) = std_mpsc::channel();
        let thread = {
            let (shm, shared) = (Arc::clone(&shm), Arc::clone(&shared));
            std::thread::Builder::new()
                .name("gungnir-link".into())
                .spawn(move || Driver::new(fd, shm, shared, rx).run())
                .map_err(|e| VfsError::Transport(format!("spawn gungnir-link: {e}")))?
        };
        Ok(Arc::new(Self {
            tx: Mutex::new(Some(tx)),
            shm,
            shared,
            thread: Mutex::new(Some(thread)),
            socket: sock.to_owned(),
        }))
    }

    #[must_use]
    pub fn alive(&self) -> bool {
        self.shared.alive.load(Ordering::Acquire)
    }

    fn send(&self, cmd: Cmd) -> VfsResult<()> {
        let tx = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        let tx = tx.as_ref().ok_or_else(|| VfsError::DaemonUnavailable("link closed".into()))?;
        tx.send(cmd).map_err(|_| VfsError::DaemonUnavailable("link thread stopped".into()))?;
        self.shm.nudge(RING_D2C); // wake the thread from its futex wait
        Ok(())
    }

    /// One request, one reply body (status 0) or the mapped error.
    ///
    /// # Errors
    /// The daemon's status, `Timeout`, or `DaemonUnavailable`.
    pub async fn call(&self, kind: u32, payload: Vec<u8>, timeout: Duration) -> VfsResult<Vec<u8>> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Call { kind, payload, deadline: Instant::now() + timeout, reply, raw: false })?;
        rx.await.map_err(|_| VfsError::DaemonUnavailable("link dropped the request".into()))?
    }

    /// [`call`](Self::call), but the whole reply payload (`status | body`) comes back
    /// unread, for a reply whose body has its own layout whatever the status (the
    /// trash empty).
    ///
    /// # Errors
    /// `Timeout` or `DaemonUnavailable`.
    pub async fn call_raw(&self, kind: u32, payload: Vec<u8>, timeout: Duration) -> VfsResult<Vec<u8>> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Call { kind, payload, deadline: Instant::now() + timeout, reply, raw: true })?;
        rx.await.map_err(|_| VfsError::DaemonUnavailable("link dropped the request".into()))?
    }

    /// Start a daemon file op; returns its id and the stream of its events (ending
    /// with exactly one `Done`).
    ///
    /// # Errors
    /// The daemon's start status, `Timeout`, or `DaemonUnavailable`.
    pub async fn start_op(
        &self,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> VfsResult<(u64, mpsc::UnboundedReceiver<OpEvent>)> {
        let (started, rx) = oneshot::channel();
        let (events, ev_rx) = mpsc::unbounded_channel();
        self.send(Cmd::StartOp { payload, deadline: Instant::now() + timeout, started, events })?;
        let id = rx.await.map_err(|_| VfsError::DaemonUnavailable("link dropped the request".into()))??;
        Ok((id, ev_rx))
    }

    #[must_use]
    pub fn health(&self) -> HealthTable {
        self.shared.health.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Bumped on every new health snapshot.
    #[must_use]
    pub fn health_changes(&self) -> watch::Receiver<u64> {
        self.shared.health_tx.subscribe()
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.tx.lock().unwrap_or_else(PoisonError::into_inner).take();
        self.shm.nudge(RING_D2C);
        if let Some(t) = self.thread.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = t.join(); // bounded: the thread re-checks its channel every SLICE
        }
    }
}

struct Driver {
    fd: OwnedFd,
    shm: Arc<Shm>,
    shared: Arc<Shared>,
    rx: std_mpsc::Receiver<Cmd>,
    seq: u64,
    pending: HashMap<u64, Pending>,
    /// Daemon op id -> the seq of its start request.
    ops: HashMap<u64, u64>,
    /// Requests waiting for a free ring slot / in-flight budget.
    queue: VecDeque<(u64, u32, Vec<u8>)>,
    /// Seqs pushed to the daemon and not yet answered (the in-flight budget).
    sent: std::collections::HashSet<u64>,
    health_parts: Vec<proto::HealthEntry>,
    health_epoch: u64,
    health_incomplete: bool,
}

impl Driver {
    fn new(fd: OwnedFd, shm: Arc<Shm>, shared: Arc<Shared>, rx: std_mpsc::Receiver<Cmd>) -> Self {
        Self {
            fd,
            shm,
            shared,
            rx,
            seq: 0,
            pending: HashMap::new(),
            ops: HashMap::new(),
            queue: VecDeque::new(),
            sent: std::collections::HashSet::new(),
            health_parts: Vec::new(),
            health_epoch: 0,
            health_incomplete: false,
        }
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn run(mut self) {
        // Subscribe to mount-health events first; the reply carries the epoch.
        let s = self.next_seq();
        let (reply, _ignored) = oneshot::channel();
        self.pending.insert(s, Pending::Call { reply, deadline: Instant::now() + Duration::from_secs(5), raw: false });
        self.queue.push_back((s, proto::MSG_HEALTH_SUBSCRIBE, Vec::new()));
        let mut last_liveness = Instant::now();
        let err = loop {
            // New work.
            loop {
                match self.rx.try_recv() {
                    Ok(cmd) => self.accept(cmd),
                    Err(std_mpsc::TryRecvError::Empty) => break,
                    Err(std_mpsc::TryRecvError::Disconnected) => {
                        self.shared.alive.store(false, Ordering::Release);
                        return; // handle dropped: shut down quietly
                    }
                }
            }
            if let Err(e) = self.flush_queue() {
                break e;
            }
            // Replies and events.
            loop {
                match self.shm.pop(RING_D2C) {
                    Ok(Some(m)) => self.dispatch(m),
                    Ok(None) => break,
                    Err(e) => return self.fail_all(e),
                }
            }
            self.expire();
            if last_liveness.elapsed() >= SLICE {
                last_liveness = Instant::now();
                if proto::peer_gone(&self.fd) {
                    break VfsError::DaemonUnavailable("gungnird closed the connection".into());
                }
            }
            let _ = self.shm.wait_readable(RING_D2C, SLICE);
        };
        self.fail_all(err);
    }

    fn accept(&mut self, cmd: Cmd) {
        let s = self.next_seq();
        match cmd {
            Cmd::Call { kind, payload, deadline, reply, raw } => {
                self.pending.insert(s, Pending::Call { reply, deadline, raw });
                self.queue.push_back((s, kind, payload));
            }
            Cmd::StartOp { payload, deadline, started, events } => {
                self.pending.insert(s, Pending::Op { started: Some(started), events, deadline });
                self.queue.push_back((s, proto::MSG_OP_START, payload));
            }
        }
    }

    fn flush_queue(&mut self) -> VfsResult<()> {
        while self.sent.len() < MAX_INFLIGHT {
            let Some((seq, kind, payload)) = self.queue.pop_front() else { break };
            if !self.pending.contains_key(&seq) {
                continue; // expired while queued
            }
            if !self.shm.push(RING_C2D, kind, seq, &payload)? {
                self.queue.push_front((seq, kind, payload)); // ring full: retry next slice
                break;
            }
            self.sent.insert(seq);
        }
        Ok(())
    }

    fn dispatch(&mut self, m: Msg) {
        match m.kind {
            // A long trash delete or restore is alive: the silence bound starts over.
            proto::MSG_VFS_BEAT => {
                if let Some(Pending::Call { deadline, .. }) = self.pending.get_mut(&m.seq) {
                    *deadline = Instant::now() + BEAT_SILENCE;
                }
            }
            proto::MSG_MD_REPLY | proto::MSG_PONG => {
                self.sent.remove(&m.seq);
                let Some(p) = self.pending.remove(&m.seq) else { return }; // late reply
                match p {
                    Pending::Call { reply, raw, .. } => {
                        // A pong echoes the ping payload; everything else is `status | body`.
                        let body = if m.kind == proto::MSG_PONG || raw {
                            Ok(m.payload)
                        } else {
                            proto::reply_body(&m.payload, "request")
                        };
                        let _ = reply.send(body);
                    }
                    Pending::Op { started, events, .. } => {
                        let res = proto::reply_body(&m.payload, "op").and_then(|b| {
                            let mut r = proto::Reader(&b);
                            let id = r.u64()?;
                            r.end()?;
                            Ok(id)
                        });
                        match res {
                            Ok(id) => {
                                self.ops.insert(id, m.seq);
                                // Ops run for as long as the daemon allows (its own deadline).
                                let far = Instant::now() + Duration::from_secs(25 * 3600);
                                self.pending.insert(m.seq, Pending::Op { started: None, events, deadline: far });
                                if let Some(s) = started {
                                    let _ = s.send(Ok(id));
                                }
                            }
                            Err(e) => {
                                if let Some(s) = started {
                                    let _ = s.send(Err(e));
                                }
                            }
                        }
                    }
                }
            }
            proto::MSG_OP_PROGRESS => {
                let Ok((id, bytes_done, bytes_total, files_done)) = proto::parse_progress(&m.payload) else { return };
                if let Some(Pending::Op { events, .. }) = self.ops.get(&id).and_then(|s| self.pending.get(s)) {
                    let _ = events.send(OpEvent::Progress { bytes_done, bytes_total, files_done });
                }
            }
            proto::MSG_OP_DONE => {
                let Ok(done) = proto::parse_done(&m.payload) else { return };
                self.ops.remove(&done.op_id);
                if let Some(Pending::Op { events, .. }) = self.pending.remove(&m.seq) {
                    let _ = events.send(OpEvent::Done(done));
                }
            }
            proto::MSG_HEALTH_EVENT => self.health_event(&m.payload),
            _ => {} // unknown kinds from a newer daemon are ignored
        }
    }

    fn health_event(&mut self, payload: &[u8]) {
        let Ok((epoch, flags, entries)) = proto::parse_health_event(payload) else {
            self.health_parts.clear();
            return;
        };
        if flags & proto::EV_FIRST != 0 {
            self.health_parts.clear();
            self.health_epoch = epoch;
            self.health_incomplete = false;
        } else if epoch != self.health_epoch {
            return; // a slot of a superseded snapshot
        }
        self.health_parts.extend(entries);
        self.health_incomplete |= flags & proto::EV_INCOMPLETE != 0;
        if flags & proto::EV_LAST != 0 {
            let table =
                HealthTable { epoch, trusted: !self.health_incomplete, mounts: std::mem::take(&mut self.health_parts) };
            *self.shared.health.lock().unwrap_or_else(PoisonError::into_inner) = table;
            self.shared.health_tx.send_replace(epoch);
        }
    }

    fn expire(&mut self) {
        let now = Instant::now();
        let expired: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, p)| match p {
                Pending::Call { deadline, .. } | Pending::Op { deadline, .. } => *deadline <= now,
            })
            .map(|(s, _)| *s)
            .collect();
        for s in expired {
            // The slot stays counted until its (late) reply arrives, if it was sent.
            match self.pending.remove(&s) {
                Some(Pending::Call { reply, .. }) => {
                    let _ = reply.send(Err(VfsError::Timeout));
                }
                Some(Pending::Op { started: Some(st), .. }) => {
                    let _ = st.send(Err(VfsError::Timeout));
                }
                Some(Pending::Op { started: None, events, .. }) => {
                    drop(events);
                }
                None => {}
            }
        }
    }

    fn fail_all(&mut self, e: VfsError) {
        self.shared.alive.store(false, Ordering::Release);
        {
            let mut h = self.shared.health.lock().unwrap_or_else(PoisonError::into_inner);
            h.trusted = false;
        }
        self.shared.health_tx.send_modify(|v| *v += 1);
        for (_, p) in self.pending.drain() {
            match p {
                Pending::Call { reply, .. } => {
                    let _ = reply.send(Err(e.clone()));
                }
                Pending::Op { started: Some(st), .. } => {
                    let _ = st.send(Err(e.clone()));
                }
                Pending::Op { started: None, .. } => {}
            }
        }
        // Answer whatever is still arriving, until the handle is dropped.
        while let Ok(cmd) = self.rx.recv() {
            match cmd {
                Cmd::Call { reply, .. } => {
                    let _ = reply.send(Err(e.clone()));
                }
                Cmd::StartOp { started, .. } => {
                    let _ = started.send(Err(e.clone()));
                }
            }
        }
    }
}
