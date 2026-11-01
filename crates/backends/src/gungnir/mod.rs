//! `GungnirVfsBackend`: the live gungnird backend.
//!
//! gungnird (protocol v2, milestones 1-8) runs on this machine and serves local
//! paths: it has no remote nodes and no directory listing of its own. So this
//! backend serves `gungnir:///<absolute path>` (node empty or `localhost`), lists
//! through the local filesystem, and adds what gungnird provides:
//!
//! - **Mount health gate** (M7): paths on mounts the daemon reports hung or gone are
//!   refused at once with `SuspectMount`, before any syscall touches them.
//! - **`metadata::*`** (M5/M6): entries are identified and their keys listed (within a
//!   time budget) and shown as the `tagged` VFS status; Inspect shows the identity
//!   (`fs_uuid`, subvolume, inode, generation, btime), every key and value, and the
//!   mount's health. Renames are reported with `moved` so path hints follow.
//! - **File operations** (M8): copy, move and delete run inside gungnird (io_uring or
//!   its thread engine) with progress events and cancellation.
//!
//! The link reconnects lazily (at most every 2 s) if gungnird restarts.

pub mod link;
pub mod proto;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use core_vfs::provider::DirWatch;
use core_vfs::{
    BackendKind, CancellationToken, EntrySink, FileEntry, ListOptions, ListSummary, MountInfo, MountStatus, Scheme,
    TransferControl, TransferKind, TransferProgress, TransferRequest, TransferState, VfsCapabilities, VfsError,
    VfsProvider, VfsResult, VfsStatus, VfsUri, VolumeInfo,
};
use tokio::sync::mpsc;

use crate::local::{LocalPosixBackend, unique_name};
use link::{HealthTable, Link, OpEvent};
use proto::{Health, Ident, Value};

/// Per-request budget (gungnird's own resolver deadline is 250 ms).
const CALL_TIMEOUT: Duration = Duration::from_millis(400);
/// Handshake budget (the GIO module uses 500 ms).
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const RECONNECT_EVERY: Duration = Duration::from_secs(2);
/// Metadata enrichment budget per listing, and entries looked up at most.
const ENRICH_BUDGET: Duration = Duration::from_millis(250);
const ENRICH_MAX: usize = 2_000;
/// Values fetched per tagged entry.
const VALUES_PER_ENTRY: usize = 8;

pub struct GungnirVfsBackend {
    socket: PathBuf,
    link: Mutex<Option<Arc<Link>>>,
    last_attempt: Mutex<Option<Instant>>,
    local: LocalPosixBackend,
    /// A listing's pages read a snapshot kept per connection, and this one is shared.
    trash_listing: tokio::sync::Mutex<()>,
}

/// `gungnir:///p` and `gungnir://localhost/p` -> `/p`; `file://` passes through.
fn local_path(uri: &VfsUri) -> VfsResult<PathBuf> {
    match uri.scheme() {
        Scheme::File => Ok(PathBuf::from(uri.path())),
        Scheme::Gungnir if uri.authority().is_empty() || uri.authority() == "localhost" => {
            Ok(PathBuf::from(uri.path()))
        }
        Scheme::Trash => {
            let trash = crate::local::home_trash()?;
            let files = trash.join("files");
            let rel = uri.path().trim_start_matches('/');
            if rel.is_empty() {
                if !files.is_dir() {
                    let _ = std::fs::create_dir_all(&files);
                }
                Ok(files)
            } else {
                Ok(files.join(rel))
            }
        }
        Scheme::Recent => Err(VfsError::Unsupported("recent:// not backed by local path".into())),
        Scheme::Network => Err(VfsError::Unsupported("network:// not backed by local path".into())),
        Scheme::Gungnir => Err(VfsError::Unsupported(format!(
            "gungnird serves this machine only; node {:?} needs a newer Gungnir",
            uri.authority()
        ))),
        Scheme::Sftp | Scheme::Smb => {
            Err(VfsError::Unsupported(format!("remote uri {} is not handled by gungnird", uri.display())))
        }
    }
}

fn gungnir_uri(local: &VfsUri) -> VfsResult<VfsUri> {
    VfsUri::gungnir("", local.path())
}

fn file_uri(p: &Path) -> VfsResult<VfsUri> {
    VfsUri::from_local_path(p)
}

fn is_local_gungnir(uri: &VfsUri) -> bool {
    uri.scheme() == Scheme::Gungnir && (uri.authority().is_empty() || uri.authority() == "localhost")
}

impl GungnirVfsBackend {
    /// A backend for the daemon at `socket`; connects on first use.
    #[must_use]
    pub fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            trash_listing: tokio::sync::Mutex::new(()),
            link: Mutex::new(None),
            last_attempt: Mutex::new(None),
            local: LocalPosixBackend::new(),
        }
    }

    /// Connect now; fails if gungnird is not running (used to pick live vs. mock).
    ///
    /// # Errors
    /// `DaemonUnavailable` or a handshake error.
    pub async fn connect(socket: PathBuf) -> VfsResult<Self> {
        let b = Self::new(socket);
        b.link().await?;
        Ok(b)
    }

    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The live link, reconnecting (rate-limited) if the daemon went away.
    async fn link(&self) -> VfsResult<Arc<Link>> {
        if let Some(l) = self.link.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
            && l.alive()
        {
            return Ok(Arc::clone(l));
        }
        {
            let mut last = self.last_attempt.lock().unwrap_or_else(PoisonError::into_inner);
            if last.is_some_and(|t| t.elapsed() < RECONNECT_EVERY) {
                return Err(VfsError::DaemonUnavailable(format!("{} (retrying every 2 s)", self.socket.display())));
            }
            *last = Some(Instant::now());
        }
        let sock = self.socket.clone();
        let l = tokio::task::spawn_blocking(move || Link::connect(&sock, CONNECT_TIMEOUT))
            .await
            .map_err(|e| VfsError::Transport(e.to_string()))??;
        *self.link.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&l));
        Ok(l)
    }

    /// The cached health snapshot (empty and untrusted without a link).
    #[must_use]
    pub fn health_table(&self) -> HealthTable {
        self.link.lock().unwrap_or_else(PoisonError::into_inner).as_ref().map(|l| l.health()).unwrap_or_default()
    }

    /// Refuse paths on hung or gone mounts without touching them.
    fn gate(&self, p: &Path) -> VfsResult<()> {
        match self.health_table().lookup(p) {
            Some((h, _)) if h.blocks() => Err(VfsError::SuspectMount),
            _ => Ok(()),
        }
    }

    /// Round trip of a ping (diagnostics).
    ///
    /// # Errors
    /// Link errors.
    pub async fn ping(&self) -> VfsResult<Duration> {
        let l = self.link().await?;
        let t = Instant::now();
        let echo = l.call(proto::MSG_PING, b"gungnir-fm".to_vec(), CALL_TIMEOUT).await?;
        if echo != b"gungnir-fm" {
            return Err(VfsError::Transport("pong payload differs from the ping".into()));
        }
        Ok(t.elapsed())
    }

    /// # Errors
    /// The daemon's status or link errors.
    pub async fn identify(&self, p: &Path) -> VfsResult<Ident> {
        self.gate(p)?;
        let body = self.link().await?.call(proto::MSG_MD_IDENTIFY, proto::req::path(p)?, CALL_TIMEOUT).await?;
        let mut r = proto::Reader(&body);
        let id = Ident::read(&mut r)?;
        r.end()?;
        Ok(id)
    }

    /// Every `metadata::` key of a file, following the pagination.
    ///
    /// # Errors
    /// The daemon's status or link errors.
    pub async fn list_keys(&self, id: &Ident) -> VfsResult<Vec<String>> {
        let l = self.link().await?;
        let mut keys: Vec<String> = Vec::new();
        loop {
            let cursor = keys.last().cloned().unwrap_or_default();
            let body = match l.call(proto::MSG_MD_LIST, proto::req::ident_key(id, &cursor)?, CALL_TIMEOUT).await {
                Ok(b) => b,
                Err(VfsError::NotFound(_)) => return Ok(keys),
                Err(e) => return Err(e),
            };
            let (more, page) = proto::parse_list(&body)?;
            let n = page.len();
            keys.extend(page);
            if !more {
                return Ok(keys);
            }
            if n == 0 {
                return Err(VfsError::Transport("metadata list made no progress".into()));
            }
        }
    }

    /// `Ok(None)` if the key is not set.
    ///
    /// # Errors
    /// The daemon's status or link errors.
    pub async fn get(&self, id: &Ident, key: &str) -> VfsResult<Option<Value>> {
        match self.link().await?.call(proto::MSG_MD_GET, proto::req::ident_key(id, key)?, CALL_TIMEOUT).await {
            Ok(b) => Ok(Some(Value::decode(&b)?)),
            Err(VfsError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Set `metadata::<key>` (key without the `metadata::` prefix).
    ///
    /// # Errors
    /// The daemon's status or link errors.
    pub async fn set(&self, id: &Ident, key: &str, v: &Value) -> VfsResult<()> {
        self.link().await?.call(proto::MSG_MD_SET, proto::req::set(id, key, v)?, CALL_TIMEOUT).await.map(drop)
    }

    /// # Errors
    /// The daemon's status or link errors.
    pub async fn health_of(&self, p: &Path) -> VfsResult<proto::HealthReport> {
        let body =
            self.link().await?.call(proto::MSG_HEALTH_QUERY, proto::req::health_by_path(p)?, CALL_TIMEOUT).await?;
        proto::parse_health_report(&body)
    }

    /// Run one daemon op to completion, forwarding progress; cancels it in the daemon
    /// when `cancel` fires.
    async fn run_op(
        &self,
        op: u8,
        flags: u32,
        src: &Path,
        dest: Option<&Path>,
        cancel: &CancellationToken,
        mut on_progress: impl FnMut(u64, u64),
    ) -> VfsResult<proto::OpDone> {
        self.gate(src)?;
        if let Some(d) = dest {
            self.gate(d)?;
        }
        let l = self.link().await?;
        let (id, mut events) = l.start_op(proto::req::op_start(op, flags, 0, src, dest)?, CALL_TIMEOUT).await?;
        let mut cancel_sent = false;
        loop {
            tokio::select! {
                ev = events.recv() => match ev {
                    Some(OpEvent::Progress { bytes_done, bytes_total, .. }) => on_progress(bytes_done, bytes_total),
                    Some(OpEvent::Done(d)) => {
                        let what = src.display().to_string();
                        return match VfsError::from_gungnir_status(d.status, d.errno, &what) {
                            None => Ok(d),
                            Some(e) => Err(e),
                        };
                    }
                    None => return Err(VfsError::DaemonUnavailable("connection lost during a file operation".into())),
                },
                () = cancel.cancelled(), if !cancel_sent => {
                    cancel_sent = true;
                    let _ = l.call(proto::MSG_OP_CANCEL, id.to_le_bytes().to_vec(), CALL_TIMEOUT).await;
                }
            }
        }
    }

    /// Look up `metadata::*` of a batch concurrently (the link pipelines up to 16).
    async fn enrich(&self, batch: &mut [FileEntry], deadline: Instant) {
        let mut set = tokio::task::JoinSet::new();
        let paths: Vec<(usize, PathBuf)> =
            batch.iter().enumerate().filter_map(|(i, e)| local_path(&e.uri).ok().map(|p| (i, p))).collect();
        for (i, p) in paths {
            if Instant::now() >= deadline {
                break;
            }
            let Ok(l) = self.link().await else { return };
            set.spawn(async move {
                let body = l.call(proto::MSG_MD_IDENTIFY, proto::req::path(&p).ok()?, CALL_TIMEOUT).await.ok()?;
                let id = Ident::read(&mut proto::Reader(&body)).ok()?;
                let lb = l.call(proto::MSG_MD_LIST, proto::req::ident_key(&id, "").ok()?, CALL_TIMEOUT).await.ok()?;
                let (_, keys) = proto::parse_list(&lb).ok()?;
                if keys.is_empty() {
                    return None;
                }
                let mut attrs = Vec::new();
                for k in keys.iter().take(VALUES_PER_ENTRY) {
                    let v = l.call(proto::MSG_MD_GET, proto::req::ident_key(&id, k).ok()?, CALL_TIMEOUT).await.ok();
                    let shown = v.and_then(|b| Value::decode(&b).ok()).map(|v| v.display()).unwrap_or_default();
                    attrs.push((format!("metadata::{k}"), shown));
                }
                Some((i, attrs))
            });
            while set.len() >= proto::MAX_INFLIGHT / 2 {
                if let Some(Ok(Some((i, attrs)))) = set.join_next().await {
                    batch[i].vfs.status = VfsStatus::Tagged;
                    batch[i].vfs.attributes = attrs;
                }
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let _ = tokio::time::timeout(left.max(Duration::from_millis(50)), async {
            while let Some(r) = set.join_next().await {
                if let Ok(Some((i, attrs))) = r {
                    batch[i].vfs.status = VfsStatus::Tagged;
                    batch[i].vfs.attributes = attrs;
                }
            }
        })
        .await;
    }

    async fn transfer_inner(&self, req: &TransferRequest, ctl: &TransferControl) -> VfsResult<()> {
        let dest_dir = local_path(&req.dest_dir)?;
        self.gate(&dest_dir)?;
        let mut sources = Vec::new();
        for s in &req.sources {
            let p = local_path(s)?;
            self.gate(&p)?;
            if dest_dir.starts_with(&p) {
                return Err(VfsError::WouldRecurse);
            }
            sources.push(p);
        }
        // Plan: (source file, destination file) pairs plus directories to create, so
        // gungnird copies every file with progress (it refuses directories).
        let overwrite = req.overwrite;
        let mut plan = {
            let (sources, dest_dir) = (sources.clone(), dest_dir.clone());
            tokio::task::spawn_blocking(move || plan_tree(&sources, &dest_dir, overwrite))
                .await
                .map_err(|e| VfsError::Transport(e.to_string()))??
        };
        let mut pr = TransferProgress {
            id: req.id,
            bytes_done: 0,
            bytes_total: plan.bytes,
            files_done: 0,
            files_total: plan.files.len() as u64,
            current: String::new(),
            state: TransferState::Running,
        };
        let _ = ctl.progress.try_send(pr.clone());
        let flags = proto::FLAG_ALL_METADATA
            | proto::FLAG_NOFOLLOW_SYMLINKS
            | if overwrite { proto::FLAG_OVERWRITE } else { 0 };

        // A move inside one filesystem is a rename of each top-level source.
        if req.kind == TransferKind::Move {
            let mut leftover = Vec::new();
            for (src, dst) in &plan.tops {
                pr.current = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                match self
                    .run_op(
                        proto::OP_MOVE,
                        flags | 16, /* NO_FALLBACK_FOR_MOVE */
                        src,
                        Some(dst),
                        &ctl.cancel,
                        |_, _| {},
                    )
                    .await
                {
                    Ok(_) => {
                        pr.bytes_done += plan
                            .files
                            .iter()
                            .filter(|(path, _, _)| path.starts_with(src))
                            .map(|(_, _, size)| *size)
                            .sum::<u64>();
                        pr.files_done += plan.files.iter().filter(|(path, _, _)| path.starts_with(src)).count() as u64;
                    }
                    Err(
                        VfsError::WouldRecurse | VfsError::Io { errno: libc::EXDEV, .. } | VfsError::Unsupported(_),
                    ) => leftover.push(src.clone()),
                    Err(e) => return Err(e),
                }
            }
            if leftover.is_empty() {
                pr.bytes_done = pr.bytes_total;
                pr.files_done = pr.files_total;
                pr.state = TransferState::Done;
                let _ = ctl.progress.try_send(pr);
                return Ok(());
            }
            // A mixed batch may contain successful renames and cross-device
            // fallbacks. Never try to copy/delete the already-moved sources again.
            let remaining_tops: Vec<_> = plan.tops.iter().filter(|(src, _)| leftover.contains(src)).cloned().collect();
            plan.files.retain(|(src, _, _)| leftover.iter().any(|top| src.starts_with(top)));
            plan.dirs.retain(|dst| remaining_tops.iter().any(|(_, top)| dst.starts_with(top)));
            plan.tops = remaining_tops;
        }

        for d in &plan.dirs {
            if ctl.cancel.is_cancelled() {
                return Err(VfsError::Cancelled);
            }
            self.gate(d)?;
            let d2 = d.clone();
            tokio::task::spawn_blocking(move || match std::fs::create_dir(&d2) {
                Err(e)
                    if overwrite
                        && e.kind() == std::io::ErrorKind::AlreadyExists
                        && d2.symlink_metadata().is_ok_and(|md| md.is_dir()) =>
                {
                    Ok(())
                }
                result => result,
            })
            .await
            .map_err(|e| VfsError::Transport(e.to_string()))?
            .map_err(|e| VfsError::from_io("mkdir", &e, &d.display().to_string()))?;
        }
        for (src, dst, size) in &plan.files {
            if ctl.cancel.is_cancelled() {
                return Err(VfsError::Cancelled);
            }
            pr.current = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let base = pr.bytes_done;
            let progress = ctl.progress.clone();
            let mut tick = pr.clone();
            self.run_op(proto::OP_COPY, flags, src, Some(dst), &ctl.cancel, |done, _| {
                tick.bytes_done = base + done;
                let _ = progress.try_send(tick.clone());
            })
            .await?;
            pr.bytes_done = base + size;
            pr.files_done += 1;
            let _ = ctl.progress.try_send(pr.clone());
        }
        if req.kind == TransferKind::Move {
            for (src, _) in &plan.tops {
                if std::fs::symlink_metadata(src).is_ok() {
                    let op = if src.is_dir() { proto::OP_DELETE_TREE } else { proto::OP_DELETE };
                    self.run_op(op, 0, src, None, &ctl.cancel, |_, _| {}).await?;
                }
            }
        }
        pr.state = TransferState::Done;
        pr.current.clear();
        let _ = ctl.progress.try_send(pr);
        Ok(())
    }
}

struct Plan {
    /// Top-level (source, destination) pairs.
    tops: Vec<(PathBuf, PathBuf)>,
    dirs: Vec<PathBuf>,
    files: Vec<(PathBuf, PathBuf, u64)>,
    bytes: u64,
}

fn plan_tree(sources: &[PathBuf], dest_dir: &Path, overwrite: bool) -> VfsResult<Plan> {
    fn walk(src: &Path, dst: &Path, plan: &mut Plan) -> VfsResult<()> {
        let md =
            std::fs::symlink_metadata(src).map_err(|e| VfsError::from_io("lstat", &e, &src.display().to_string()))?;
        if md.is_dir() {
            plan.dirs.push(dst.to_owned());
            let mut kids: Vec<_> = std::fs::read_dir(src)
                .map_err(|e| VfsError::from_io("opendir", &e, &src.display().to_string()))?
                .collect::<Result<_, _>>()
                .map_err(|e| VfsError::from_io("readdir", &e, &src.display().to_string()))?;
            kids.sort_by_key(std::fs::DirEntry::file_name);
            for k in kids {
                walk(&k.path(), &dst.join(k.file_name()), plan)?;
            }
        } else {
            plan.bytes += md.len();
            plan.files.push((src.to_owned(), dst.to_owned(), md.len()));
        }
        Ok(())
    }
    let canonical_dest = std::fs::canonicalize(dest_dir)
        .map_err(|e| VfsError::from_io("realpath", &e, &dest_dir.display().to_string()))?;
    for source in sources {
        let md = std::fs::symlink_metadata(source)
            .map_err(|e| VfsError::from_io("lstat", &e, &source.display().to_string()))?;
        if md.is_dir() {
            let canonical_source = std::fs::canonicalize(source)
                .map_err(|e| VfsError::from_io("realpath", &e, &source.display().to_string()))?;
            if canonical_dest.starts_with(canonical_source) {
                return Err(VfsError::WouldRecurse);
            }
        }
    }
    let mut plan = Plan { tops: Vec::new(), dirs: Vec::new(), files: Vec::new(), bytes: 0 };
    for s in sources {
        let name = s.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let target = if overwrite { name } else { unique_name(dest_dir, &name, |p| p.symlink_metadata().is_ok()) };
        let dst = dest_dir.join(target);
        plan.tops.push((s.clone(), dst.clone()));
        walk(s, &dst, &mut plan)?;
    }
    Ok(plan)
}

fn health_status(h: Health) -> MountStatus {
    match h {
        Health::Healthy => MountStatus::Online,
        Health::Slow => MountStatus::Degraded,
        Health::Hung | Health::Gone => MountStatus::Disconnected,
    }
}

/// The daemon's request bounds for a trash list or stat, and the silence allowed during a
/// long delete or restore, which sends beats.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
const OP_SILENCE: Duration = Duration::from_secs(65);
const STAMP_EVERY: Duration = Duration::from_secs(2);
const BUSY_RETRIES: u32 = 100;
const INNER_READ_ONLY: &str = "only a whole item can be changed; restore the folder first";

/// A daemon that is not there, or older than the trash service: local trash code serves.
fn falls_back(e: &VfsError) -> bool {
    match e {
        VfsError::DaemonUnavailable(_) => true,
        VfsError::Unsupported(m) => m == "the running gungnird does not support this request",
        _ => false,
    }
}

/// The path of a `trash:` URI, without its leading slash.
fn trash_rel(uri: &VfsUri) -> &str {
    uri.path().trim_matches('/')
}

/// The daemon's name for a `trash:` URI. A home item's URI is its plain entry name, the
/// same as the local trash code uses, so both always mean the same file.
fn trash_name(uri: &VfsUri) -> String {
    let rel = trash_rel(uri);
    if proto::is_volume_item(rel.split('/').next().unwrap_or_default()) { rel.to_owned() } else { format!("h-{rel}") }
}

fn trash_entry(row: proto::VfsRow) -> VfsResult<FileEntry> {
    let name = String::from_utf8(row.name).map_err(|_| VfsError::BadUtf8)?;
    let rel = name.strip_prefix("h-").unwrap_or(&name);
    let uri = VfsUri::trash(rel)?;
    let last = |b: &[u8]| {
        let s = String::from_utf8_lossy(b).into_owned();
        s.rsplit('/').next().unwrap_or_default().to_owned()
    };
    // The original name; a degraded item has only its entry's.
    let shown = if row.orig.is_empty() {
        let tail = rel.rsplit('/').next().unwrap_or(rel);
        tail.split_once('-').filter(|_| proto::is_volume_item(tail)).map_or(tail, |(_, e)| e).to_owned()
    } else {
        last(&row.orig)
    };
    let kind = match row.kind {
        proto::VFS_KIND_FILE => core_vfs::FileKind::File,
        proto::VFS_KIND_DIR => core_vfs::FileKind::Directory,
        proto::VFS_KIND_SYMLINK => core_vfs::FileKind::Symlink,
        _ => core_vfs::FileKind::Special,
    };
    let modified = u64::try_from(row.mtime).ok().map(|s| std::time::UNIX_EPOCH + Duration::from_secs(s));
    Ok(FileEntry {
        name: shown,
        uri,
        kind,
        target_is_dir: false,
        size: row.size,
        modified,
        mode: 0,
        vfs: core_vfs::VfsMetadata::default(),
    })
}

async fn send_trash_rows(
    rows: Vec<proto::VfsRow>,
    partial: bool,
    opts: ListOptions,
    sink: &EntrySink,
    cancel: &CancellationToken,
) -> VfsResult<ListSummary> {
    let (mut sent, mut skipped) = (0u64, u64::from(partial));
    let mut batch = Vec::with_capacity(opts.first_batch);
    let mut limit = opts.first_batch.max(1);
    for row in rows {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        match trash_entry(row) {
            Ok(e) => batch.push(e),
            Err(_) => skipped += 1,
        }
        if batch.len() >= limit {
            sent += batch.len() as u64;
            sink.send(std::mem::take(&mut batch)).await.map_err(|_| VfsError::Cancelled)?;
            limit = opts.batch.max(1);
        }
    }
    if !batch.is_empty() {
        sent += batch.len() as u64;
        sink.send(batch).await.map_err(|_| VfsError::Cancelled)?;
    }
    Ok(ListSummary { entries: sent, skipped })
}

/// A change in the trash, from the daemon's stamp: it covers every volume's trash, and the
/// local watcher only the home one.
fn watch_trash(link: Arc<Link>, rt: &tokio::runtime::Handle) -> DirWatch {
    struct Stop(tokio::task::JoinHandle<()>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let (tx, events) = tokio::sync::mpsc::channel(8);
    let task = rt.spawn(async move {
        let stamp =
            |l: Arc<Link>| async move { l.call(proto::MSG_VFS_STAMP, vec![proto::SCHEME_TRASH], LIST_TIMEOUT).await };
        let mut last = stamp(Arc::clone(&link)).await.ok();
        loop {
            tokio::time::sleep(STAMP_EVERY).await;
            if !link.alive() {
                return;
            }
            // Busy or slow says nothing about the trash.
            let Ok(now) = stamp(Arc::clone(&link)).await else { continue };
            if last.as_ref() != Some(&now) {
                last = Some(now);
                if tx.send(core_vfs::WatchEvent::Changed).await.is_err() {
                    return;
                }
            }
        }
    });
    DirWatch { events, guard: Box::new(Stop(task)) }
}

impl GungnirVfsBackend {
    /// A trash request, tried again while every daemon request slot is taken.
    async fn trash_call(&self, kind: u32, payload: Vec<u8>, timeout: Duration) -> VfsResult<Vec<u8>> {
        let link = self.link().await?;
        let mut tries = 0;
        loop {
            match link.call(kind, payload.clone(), timeout).await {
                Err(VfsError::Busy) if tries < BUSY_RETRIES => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                r => return r,
            }
        }
    }

    /// Every row of the trash, or of a folder inside it, and whether that may not be all.
    async fn trash_rows(&self, dir: &VfsUri) -> VfsResult<(Vec<proto::VfsRow>, bool)> {
        let _one_at_a_time = self.trash_listing.lock().await;
        let inner = !trash_rel(dir).is_empty();
        let name = trash_name(dir);
        let (mut rows, mut partial, mut cursor) = (Vec::new(), false, 0u32);
        loop {
            let (kind, payload) = if inner {
                (proto::MSG_VFS_CHILDREN, proto::req::vfs_children(cursor, name.as_bytes())?)
            } else {
                (proto::MSG_VFS_LIST, proto::req::vfs_list(cursor))
            };
            let body = self.trash_call(kind, payload, LIST_TIMEOUT).await?;
            let (next, part, mut page) = proto::parse_page(&body)?;
            partial |= part;
            rows.append(&mut page);
            if next == 0 {
                return Ok((rows, partial));
            }
            if next <= cursor {
                return Err(VfsError::Transport("bad trash page".into()));
            }
            cursor = next;
        }
    }
}

#[async_trait]
impl VfsProvider for GungnirVfsBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::GungnirLive
    }

    fn capabilities(&self) -> VfsCapabilities {
        VfsCapabilities {
            writable: true,
            watch: true,
            trash: true,
            pause_transfers: false,
            vfs_metadata: true,
            mount_health: true,
        }
    }

    fn handles(&self, uri: &VfsUri) -> bool {
        is_local_gungnir(uri) || uri.scheme() == Scheme::Trash
    }

    fn can_transfer(&self, src: &VfsUri, dest: &VfsUri) -> bool {
        let mappable = |u: &VfsUri| u.scheme() == Scheme::File || is_local_gungnir(u);
        mappable(src) && mappable(dest) && (is_local_gungnir(src) || is_local_gungnir(dest))
    }

    async fn list_dir(
        &self,
        dir: &VfsUri,
        opts: ListOptions,
        sink: EntrySink,
        cancel: CancellationToken,
    ) -> VfsResult<ListSummary> {
        if dir.scheme() == Scheme::Trash {
            return match self.trash_rows(dir).await {
                Err(e) if falls_back(&e) => self.local.list_dir(dir, opts, sink, cancel).await,
                Err(e) => Err(e),
                Ok((rows, partial)) => send_trash_rows(rows, partial, opts, &sink, &cancel).await,
            };
        }
        let path = local_path(dir)?;
        self.gate(&path)?;
        let (tx, mut rx) = mpsc::channel::<Vec<FileEntry>>(4);
        let local_dir = file_uri(&path)?;
        let lister = self.local.list_dir(&local_dir, opts, tx, cancel.clone());
        let deadline = Instant::now() + ENRICH_BUDGET;
        let forward = async {
            let mut looked_up = 0usize;
            let daemon_up = self.link().await.is_ok();
            while let Some(mut batch) = rx.recv().await {
                for e in &mut batch {
                    if let Ok(u) = gungnir_uri(&e.uri) {
                        e.uri = u;
                    }
                }
                if daemon_up && looked_up < ENRICH_MAX && Instant::now() < deadline {
                    let n = batch.len().min(ENRICH_MAX - looked_up);
                    looked_up += n;
                    self.enrich(&mut batch[..n], deadline).await;
                }
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => return Err(VfsError::Cancelled),
                    result = sink.send(batch) => {
                        if result.is_err() { return Err(VfsError::Cancelled); }
                    }
                }
            }
            Ok(())
        };
        let (listed, forwarded) = tokio::join!(lister, forward);
        forwarded?;
        listed
    }

    async fn stat(&self, uri: &VfsUri) -> VfsResult<FileEntry> {
        if uri.scheme() == Scheme::Trash && !trash_rel(uri).is_empty() {
            let asked = async {
                let body = self
                    .trash_call(proto::MSG_VFS_STAT, proto::req::vfs_item(trash_name(uri).as_bytes())?, LIST_TIMEOUT)
                    .await?;
                let (_, _, rows) = proto::parse_page(&body)?;
                rows.into_iter().next().ok_or(VfsError::BadRequest).and_then(trash_entry)
            };
            return match asked.await {
                Err(e) if falls_back(&e) => self.local.stat(uri).await,
                r => r,
            };
        }
        if uri.scheme() == Scheme::Trash {
            return self.local.stat(uri).await;
        }
        let path = local_path(uri)?;
        self.gate(&path)?;
        let mut e = self.local.stat(&file_uri(&path)?).await?;
        e.uri = gungnir_uri(&e.uri)?;
        Ok(e)
    }

    async fn create_from_template(&self, source: &VfsUri, destination: &VfsUri) -> VfsResult<()> {
        let path = local_path(destination)?;
        self.gate(&path)?;
        self.local.create_from_template(source, &file_uri(&path)?).await
    }

    async fn create_file(&self, uri: &VfsUri) -> VfsResult<()> {
        let path = local_path(uri)?;
        self.gate(&path)?;
        self.local.create_file(&file_uri(&path)?).await
    }

    async fn create_dir(&self, uri: &VfsUri) -> VfsResult<()> {
        let path = local_path(uri)?;
        self.gate(&path)?;
        self.local.create_dir(&file_uri(&path)?).await
    }

    async fn rename(&self, uri: &VfsUri, new_name: &str) -> VfsResult<VfsUri> {
        let path = local_path(uri)?;
        self.gate(&path)?;
        let to = self.local.rename(&file_uri(&path)?, new_name).await?;
        // Keep gungnird's path hints in step (best effort, like GIO's local_file_moved).
        if let (Ok(l), Ok(p)) = (self.link().await, proto::req::moved(&path, Path::new(to.path()))) {
            let _ = l.call(proto::MSG_MD_MOVED, p, CALL_TIMEOUT).await;
        }
        gungnir_uri(&to)
    }

    async fn delete(&self, uris: &[VfsUri], cancel: CancellationToken) -> VfsResult<()> {
        for u in uris {
            if u.scheme() == Scheme::Trash {
                if cancel.is_cancelled() {
                    return Err(VfsError::Cancelled);
                }
                if trash_rel(u).contains('/') {
                    return Err(VfsError::Unsupported(INNER_READ_ONLY.into()));
                }
                let payload = proto::req::vfs_item(trash_name(u).as_bytes())?;
                match self.trash_call(proto::MSG_VFS_DELETE, payload, OP_SILENCE).await {
                    Err(e) if falls_back(&e) => self.local.delete(std::slice::from_ref(u), cancel.clone()).await?,
                    r => {
                        r?;
                    }
                }
                continue;
            }
            let p = local_path(u)?;
            let md = {
                self.gate(&p)?;
                let p2 = p.clone();
                tokio::task::spawn_blocking(move || std::fs::symlink_metadata(p2))
                    .await
                    .map_err(|e| VfsError::Transport(e.to_string()))?
                    .map_err(|e| VfsError::from_io("lstat", &e, &p.display().to_string()))?
            };
            let op = if md.is_dir() { proto::OP_DELETE_TREE } else { proto::OP_DELETE };
            self.run_op(op, 0, &p, None, &cancel, |_, _| {}).await?;
        }
        Ok(())
    }

    async fn trash(&self, uris: &[VfsUri]) -> VfsResult<()> {
        let mut local = Vec::with_capacity(uris.len());
        for u in uris {
            let p = local_path(u)?;
            self.gate(&p)?;
            local.push(file_uri(&p)?);
        }
        // Metadata is keyed by inode, so it stays with the file in the trash.
        self.local.trash(&local).await
    }

    async fn restore(&self, uris: &[VfsUri]) -> VfsResult<()> {
        for u in uris {
            if u.scheme() != Scheme::Trash {
                return Err(VfsError::BadRequest);
            }
            if trash_rel(u).contains('/') {
                return Err(VfsError::Unsupported(INNER_READ_ONLY.into()));
            }
            let payload = proto::req::vfs_restore(trash_name(u).as_bytes())?;
            match self.trash_call(proto::MSG_VFS_RESTORE, payload, OP_SILENCE).await {
                Err(e) if falls_back(&e) => self.local.restore(std::slice::from_ref(u)).await?,
                r => {
                    r?;
                }
            }
        }
        Ok(())
    }

    async fn empty_trash(&self) -> VfsResult<()> {
        match self.trash_call(proto::MSG_TRASH_EMPTY, Vec::new(), OP_SILENCE).await {
            Err(e) if falls_back(&e) => self.local.empty_trash().await,
            r => r.map(drop),
        }
    }

    async fn transfer(&self, req: TransferRequest, ctl: TransferControl) -> VfsResult<()> {
        let r = self.transfer_inner(&req, &ctl).await;
        if let Err(e) = &r {
            let state =
                if *e == VfsError::Cancelled { TransferState::Cancelled } else { TransferState::Failed(e.to_string()) };
            let _ = ctl
                .progress
                .send(TransferProgress {
                    id: req.id,
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
        let p = local_path(uri)?;
        self.gate(&p)?;
        tokio::task::spawn_blocking(move || crate::local::statvfs(&p))
            .await
            .map_err(|e| VfsError::Transport(e.to_string()))?
    }

    async fn mounts(&self) -> VfsResult<Vec<MountInfo>> {
        let root = VfsUri::gungnir("", "/")?;
        let Ok(l) = self.link().await else {
            return Ok(vec![MountInfo {
                label: "gungnird (this machine)".into(),
                uri: root,
                status: MountStatus::Disconnected,
                detail: format!("no daemon at {}", self.socket.display()),
            }]);
        };
        let table = l.health();
        let mut out = vec![MountInfo {
            label: "gungnird (this machine)".into(),
            uri: root,
            status: MountStatus::Online,
            detail: format!("{} · health epoch {}", self.socket.display(), table.epoch),
        }];
        for (id, h, mp) in &table.mounts {
            let Some(s) = mp.to_str() else { continue };
            out.push(MountInfo {
                label: s.to_owned(),
                uri: VfsUri::gungnir("", s)?,
                status: health_status(*h),
                detail: format!("mount {id}: {h:?}"),
            });
        }
        Ok(out)
    }

    async fn inspect(&self, uri: &VfsUri) -> VfsResult<Vec<(String, String)>> {
        let p = local_path(uri)?;
        let mut out = vec![
            ("URI".to_owned(), uri.to_uri_string()),
            ("Daemon socket".to_owned(), self.socket.display().to_string()),
        ];
        match self.health_of(&p).await {
            Ok(h) => {
                out.push(("Mount".to_owned(), format!("{} (id {})", h.mount_point.display(), h.mnt_id)));
                out.push((
                    "Mount health".to_owned(),
                    format!("{:?}{}", h.state, if h.probed { "" } else { " (local, not probed)" }),
                ));
                if let Some(us) = h.latency_us {
                    out.push(("Probe latency".to_owned(), format!("{us} µs")));
                }
            }
            Err(e) => out.push(("Mount health".to_owned(), e.to_string())),
        }
        let id = match self.identify(&p).await {
            Ok(id) => id,
            Err(e) => {
                out.push(("Identity".to_owned(), e.to_string()));
                return Ok(out);
            }
        };
        out.push(("fs_uuid".to_owned(), id.fs_uuid.clone()));
        out.push(("Subvolume".to_owned(), id.subvol_id.to_string()));
        out.push(("Inode".to_owned(), id.inode_num.to_string()));
        out.push(("Generation".to_owned(), id.generation.to_string()));
        out.push(("Birth time".to_owned(), format!("{}.{:09}", id.btime_sec, id.btime_nsec)));
        let keys = self.list_keys(&id).await?;
        if keys.is_empty() {
            out.push(("metadata::*".to_owned(), "(none)".to_owned()));
        }
        for k in keys {
            let v = self.get(&id, &k).await?.map(|v| v.display()).unwrap_or_default();
            out.push((format!("metadata::{k}"), v));
        }
        Ok(out)
    }

    fn watch(&self, dir: &VfsUri) -> VfsResult<Option<DirWatch>> {
        if dir.scheme() == Scheme::Trash {
            let live = self.link.lock().unwrap_or_else(PoisonError::into_inner).clone();
            return match (live.filter(|l| l.alive()), tokio::runtime::Handle::try_current()) {
                (Some(link), Ok(rt)) => Ok(Some(watch_trash(link, &rt))),
                _ => self.local.watch(dir),
            };
        }
        let p = local_path(dir)?;
        self.gate(&p)?;
        self.local.watch(&file_uri(&p)?)
    }
}
