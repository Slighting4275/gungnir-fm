//! `MockGungnirBackend`: synthetic `gungnir://` nodes for building and stress-testing
//! the UI while gungnird has no remote nodes.
//!
//! Nodes and shares (`gungnir://<node>/<share>/...`):
//! - `odin` (Online): `asgard` (with `stress-50k`, 50,000 files) and `valhalla`
//!   (with `stress-100k`, 100,000 files). Stress directories are generated on first use.
//! - `huginn` (Syncing): `memory`.
//! - `muninn` (Disconnected): `thought`; every request fails fast with
//!   `SuspectMount`, as gungnird does for a hung mount.
//!
//! Every request waits a simulated IPC latency; listings stream in batches with a
//! per-batch delay; transfers advance at a simulated throughput and can be paused.
//! Everything lives in memory and is deterministic (seeded LCG).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use core_vfs::{
    BackendKind, CancellationToken, EntrySink, FileEntry, FileKind, ListOptions, ListSummary, MountInfo, MountStatus,
    Scheme, TransferControl, TransferKind, TransferProgress, TransferRequest, TransferState, VfsCapabilities, VfsError,
    VfsMetadata, VfsProvider, VfsResult, VfsStatus, VfsUri, VolumeInfo,
};

#[derive(Debug, Clone, Copy)]
pub struct MockConfig {
    /// Round trip of every request.
    pub latency: Duration,
    /// Delay between listing batches.
    pub batch_latency: Duration,
    /// Simulated transfer speed.
    pub bytes_per_sec: u64,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self { latency: Duration::from_millis(12), batch_latency: Duration::from_millis(2), bytes_per_sec: 350_000_000 }
    }
}

impl MockConfig {
    /// No delays: unit tests.
    #[must_use]
    pub fn instant() -> Self {
        Self { latency: Duration::ZERO, batch_latency: Duration::ZERO, bytes_per_sec: u64::MAX }
    }
}

const NODES: &[(&str, MountStatus, &[&str])] = &[
    ("odin", MountStatus::Online, &["asgard", "valhalla"]),
    ("huginn", MountStatus::Syncing, &["memory"]),
    ("muninn", MountStatus::Disconnected, &["thought"]),
];
const STRESS: &[(&str, &str, usize)] =
    &[("odin", "/asgard/stress-50k", 50_000), ("odin", "/valhalla/stress-100k", 100_000)];
const CAPACITY: u64 = 120_000_000_000_000;

#[derive(Debug, Clone)]
struct Item {
    kind: FileKind,
    size: u64,
    mtime: SystemTime,
    mode: u32,
    status: VfsStatus,
    /// Seed of the synthetic attributes (built on demand: cheap 100k-entry dirs).
    seed: u64,
}

impl Item {
    fn attrs(&self, node: &str) -> Vec<(String, String)> {
        let r = self.seed;
        let mut a = vec![
            ("gungnir::node".to_owned(), node.to_owned()),
            (
                "gungnir::replicas".to_owned(),
                if r.is_multiple_of(3) { format!("{node},huginn") } else { node.to_owned() },
            ),
            (
                "gungnir::content-hash".to_owned(),
                format!("{:016x}{:016x}", r.wrapping_mul(0x9e37_79b9_7f4a_7c15), r ^ 0xdead_beef),
            ),
        ];
        if self.status == VfsStatus::Tagged {
            a.push(("metadata::emblems".to_owned(), "important".to_owned()));
        }
        a
    }
}

type Key = (String, String); // (node, path)

#[derive(Default)]
struct Fs {
    items: HashMap<Key, Item>,
    children: HashMap<Key, Vec<String>>,
    seed: u64,
}

impl Fs {
    fn rand(&mut self) -> u64 {
        self.seed = self.seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.seed >> 33
    }

    fn add(&mut self, node: &str, path: &str, kind: FileKind, size: u64) {
        let r = self.rand();
        let status = match (node, r % 13) {
            ("huginn", 0..=4) => VfsStatus::Syncing,
            (_, 0) => VfsStatus::Conflict,
            (_, 1 | 2) => VfsStatus::RemoteOnly,
            (_, 3 | 4) => VfsStatus::Cached,
            (_, 5) => VfsStatus::Tagged,
            _ => VfsStatus::Synced,
        };
        let mtime = UNIX_EPOCH + Duration::from_secs(1_700_000_000 + r % 60_000_000);
        let item =
            Item { kind, size, mtime, mode: if kind == FileKind::Directory { 0o755 } else { 0o644 }, status, seed: r };
        let (parent, name) = match path.rsplit_once('/') {
            Some(("", n)) => ("/".to_owned(), n.to_owned()),
            Some((p, n)) => (p.to_owned(), n.to_owned()),
            None => return,
        };
        if kind == FileKind::Directory {
            self.children.entry((node.to_owned(), path.to_owned())).or_default();
        }
        self.children.entry((node.to_owned(), parent)).or_default().push(name);
        self.items.insert((node.to_owned(), path.to_owned()), item);
    }

    fn seeded() -> Self {
        let mut fs = Fs { seed: 0x0067_756e_676e_6972 /* "gungnir" */, ..Fs::default() };
        for (node, _, shares) in NODES {
            fs.children.entry(((*node).to_owned(), "/".to_owned())).or_default();
            for share in *shares {
                fs.add(node, &format!("/{share}"), FileKind::Directory, 0);
            }
        }
        let dirs = [
            ("odin", "/asgard/runes"),
            ("odin", "/asgard/sagas"),
            ("odin", "/asgard/project_demo"),
            ("odin", "/asgard/project_demo/release_v1"),
            ("odin", "/asgard/stress-50k"),
            ("odin", "/valhalla/einherjar"),
            ("odin", "/valhalla/stress-100k"),
            ("huginn", "/memory/2026"),
            ("muninn", "/thought/archive"),
        ];
        for (n, d) in dirs {
            fs.add(n, d, FileKind::Directory, 0);
        }
        let files: &[(&str, &str, u64)] = &[
            ("odin", "/asgard/README.md", 4_812),
            ("odin", "/asgard/mead-hall.png", 2_412_330),
            ("odin", "/asgard/yggdrasil.svg", 88_120),
            ("odin", "/asgard/runes/elder-futhark.txt", 1_204),
            ("odin", "/asgard/runes/younger-futhark.txt", 980),
            ("odin", "/asgard/sagas/volsunga.epub", 1_830_211),
            ("odin", "/asgard/sagas/egils-saga.pdf", 6_120_004),
            ("odin", "/asgard/project_demo/release_v1/track_01.flac", 48_211_777),
            ("odin", "/asgard/project_demo/release_v1/track_02.flac", 51_998_032),
            ("odin", "/asgard/project_demo/release_v1/track_03.flac", 44_120_512),
            ("odin", "/asgard/project_demo/release_v1/cover.jpg", 1_204_334),
            ("odin", "/asgard/project_demo/archive_alpha.tar.zst", 2_811_334_002),
            ("odin", "/valhalla/einherjar/roster.csv", 22_001),
            ("odin", "/valhalla/backup_01.iso", 4_311_220_100),
            ("huginn", "/memory/2026/journal.md", 31_442),
            ("huginn", "/memory/2026/backup.tar.zst", 912_330_441),
            ("muninn", "/thought/archive/old.tar", 120_000_000),
        ];
        for (n, f, s) in files {
            fs.add(n, f, FileKind::File, *s);
        }
        fs
    }

    /// Generate a stress directory the first time it is opened.
    fn ensure_stress(&mut self, node: &str, path: &str) {
        let Some(&(_, _, count)) = STRESS.iter().find(|(n, p, _)| *n == node && *p == path) else { return };
        let key = (node.to_owned(), path.to_owned());
        if self.children.get(&key).is_some_and(|c| c.len() >= count) {
            return;
        }
        const EXT: [&str; 8] = ["flac", "png", "rs", "md", "mkv", "zst", "pdf", "txt"];
        for i in 0..count {
            let r = self.rand();
            let ext = EXT[usize::try_from(r % 8).unwrap_or(0)];
            let name = format!("rune-{i:06}.{ext}");
            self.add(node, &format!("{path}/{name}"), FileKind::File, r % 900_000_000);
        }
    }

    fn entry(&self, node: &str, path: &str) -> Option<FileEntry> {
        let it = self.items.get(&(node.to_owned(), path.to_owned()))?;
        let uri = VfsUri::gungnir(node, path).ok()?;
        Some(FileEntry {
            name: uri.file_name().unwrap_or(node).to_owned(),
            uri,
            kind: it.kind,
            target_is_dir: false,
            size: it.size,
            modified: Some(it.mtime),
            mode: it.mode,
            vfs: VfsMetadata { status: it.status, attributes: it.attrs(node) },
        })
    }

    /// Paths of `path` and everything below it (pre-order).
    fn subtree(&self, node: &str, path: &str) -> Vec<String> {
        let mut out = vec![path.to_owned()];
        let mut i = 0;
        while i < out.len() {
            let p = out[i].clone();
            if let Some(kids) = self.children.get(&(node.to_owned(), p.clone())) {
                out.extend(kids.iter().map(|k| if p == "/" { format!("/{k}") } else { format!("{p}/{k}") }));
            }
            i += 1;
        }
        out
    }

    fn detach(&mut self, node: &str, path: &str) {
        if let Some((parent, name)) = path.rsplit_once('/') {
            let parent = if parent.is_empty() { "/" } else { parent };
            if let Some(kids) = self.children.get_mut(&(node.to_owned(), parent.to_owned())) {
                kids.retain(|k| k != name);
            }
        }
    }

    fn remove_tree(&mut self, node: &str, path: &str) {
        for p in self.subtree(node, path) {
            let k = (node.to_owned(), p);
            self.items.remove(&k);
            self.children.remove(&k);
        }
        self.detach(node, path);
    }

    /// Copy the subtree at (`sn`, `sp`) to (`dn`, `dp`); returns bytes copied.
    fn copy_tree(&mut self, sn: &str, sp: &str, dn: &str, dp: &str) -> u64 {
        let mut bytes = 0;
        for p in self.subtree(sn, sp) {
            let Some(item) = self.items.get(&(sn.to_owned(), p.clone())).cloned() else { continue };
            let rel = &p[sp.len()..];
            let target = format!("{dp}{rel}");
            bytes += item.size;
            let kind = item.kind;
            let (parent, name) =
                target.rsplit_once('/').map(|(a, b)| (if a.is_empty() { "/" } else { a }, b)).unwrap_or(("/", ""));
            self.children.entry((dn.to_owned(), parent.to_owned())).or_default().push(name.to_owned());
            if kind == FileKind::Directory {
                self.children.entry((dn.to_owned(), target.clone())).or_default();
            }
            let mut item = item;
            item.status = VfsStatus::Syncing;
            self.items.insert((dn.to_owned(), target), item);
        }
        bytes
    }

    fn exists(&self, node: &str, path: &str) -> bool {
        self.items.contains_key(&(node.to_owned(), path.to_owned()))
            || (path == "/" && NODES.iter().any(|(n, ..)| *n == node))
    }

    fn is_dir(&self, node: &str, path: &str) -> bool {
        path == "/"
            || self.items.get(&(node.to_owned(), path.to_owned())).is_some_and(|i| i.kind == FileKind::Directory)
    }
}

/// See the module docs.
pub struct MockGungnirBackend {
    cfg: MockConfig,
    fs: Arc<Mutex<Fs>>,
}

impl Default for MockGungnirBackend {
    fn default() -> Self {
        Self::new(MockConfig::default())
    }
}

fn join_path(dir: &str, name: &str) -> String {
    if dir == "/" { format!("/{name}") } else { format!("{dir}/{name}") }
}

impl MockGungnirBackend {
    #[must_use]
    pub fn new(cfg: MockConfig) -> Self {
        Self { cfg, fs: Arc::new(Mutex::new(Fs::seeded())) }
    }

    fn fs(&self) -> MutexGuard<'_, Fs> {
        self.fs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Simulated round trip; returns early with `Cancelled`.
    async fn rtt(&self, d: Duration, cancel: Option<&CancellationToken>) -> VfsResult<()> {
        if d.is_zero() {
            return Ok(());
        }
        match cancel {
            Some(c) => tokio::select! {
                () = tokio::time::sleep(d) => Ok(()),
                () = c.cancelled() => Err(VfsError::Cancelled),
            },
            None => {
                tokio::time::sleep(d).await;
                Ok(())
            }
        }
    }

    /// Validate scheme and node, and fail fast on a disconnected node.
    fn node<'u>(&self, uri: &'u VfsUri) -> VfsResult<&'u str> {
        let node = uri.authority();
        let Some((_, status, _)) = NODES.iter().find(|(n, ..)| *n == node) else {
            return Err(VfsError::NotFound(format!("gungnir node {node:?} (mock nodes: odin, huginn, muninn)")));
        };
        if *status == MountStatus::Disconnected {
            return Err(VfsError::SuspectMount);
        }
        Ok(node)
    }
}

#[async_trait]
impl VfsProvider for MockGungnirBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::GungnirMock
    }

    fn capabilities(&self) -> VfsCapabilities {
        VfsCapabilities {
            writable: true,
            watch: false,
            trash: false,
            pause_transfers: true,
            vfs_metadata: true,
            mount_health: true,
        }
    }

    fn handles(&self, uri: &VfsUri) -> bool {
        uri.scheme() == Scheme::Gungnir
    }

    async fn list_dir(
        &self,
        dir: &VfsUri,
        opts: ListOptions,
        sink: EntrySink,
        cancel: CancellationToken,
    ) -> VfsResult<ListSummary> {
        let node = self.node(dir)?.to_owned();
        self.rtt(self.cfg.latency, Some(&cancel)).await?;
        // Generating a stress directory and snapshotting names is CPU work: keep it off
        // the async worker threads.
        let (fs, n2, p2) = (Arc::clone(&self.fs), node.clone(), dir.path().to_owned());
        let kids: Vec<String> = tokio::task::spawn_blocking(move || {
            let mut fs = fs.lock().unwrap_or_else(PoisonError::into_inner);
            fs.ensure_stress(&n2, &p2);
            if !fs.is_dir(&n2, &p2) {
                let u = VfsUri::gungnir(&n2, &p2).map(|u| u.display()).unwrap_or_default();
                return Err(if fs.exists(&n2, &p2) { VfsError::NotADirectory(u) } else { VfsError::NotFound(u) });
            }
            Ok(fs.children.get(&(n2.clone(), p2.clone())).cloned().unwrap_or_default())
        })
        .await
        .map_err(|e| VfsError::Transport(e.to_string()))??;
        let total = kids.len() as u64;
        let mut rest = kids.iter();
        let mut size = opts.first_batch.max(1);
        loop {
            let batch: Vec<FileEntry> = {
                let fs = self.fs();
                rest.by_ref().take(size).filter_map(|k| fs.entry(&node, &join_path(dir.path(), k))).collect()
            };
            if batch.is_empty() {
                break;
            }
            if cancel.is_cancelled() {
                return Err(VfsError::Cancelled);
            }
            sink.send(batch).await.map_err(|_| VfsError::Cancelled)?;
            self.rtt(self.cfg.batch_latency, Some(&cancel)).await?;
            size = opts.batch.max(1);
        }
        Ok(ListSummary { entries: total, skipped: 0 })
    }

    async fn stat(&self, uri: &VfsUri) -> VfsResult<FileEntry> {
        let node = self.node(uri)?.to_owned();
        self.rtt(self.cfg.latency, None).await?;
        if uri.is_root() {
            return Ok(FileEntry {
                name: node.clone(),
                uri: uri.clone(),
                kind: FileKind::Directory,
                target_is_dir: false,
                size: 0,
                modified: None,
                mode: 0o755,
                vfs: VfsMetadata::default(),
            });
        }
        self.fs().entry(&node, uri.path()).ok_or_else(|| VfsError::NotFound(uri.display()))
    }

    async fn create_dir(&self, uri: &VfsUri) -> VfsResult<()> {
        let node = self.node(uri)?.to_owned();
        self.rtt(self.cfg.latency, None).await?;
        let mut fs = self.fs();
        let parent = uri.parent().ok_or(VfsError::BadRequest)?;
        if !fs.is_dir(&node, parent.path()) {
            return Err(VfsError::NotFound(parent.display()));
        }
        if fs.exists(&node, uri.path()) {
            return Err(VfsError::AlreadyExists(uri.display()));
        }
        fs.add(&node, uri.path(), FileKind::Directory, 0);
        Ok(())
    }

    async fn rename(&self, uri: &VfsUri, new_name: &str) -> VfsResult<VfsUri> {
        let node = self.node(uri)?.to_owned();
        let to = uri.parent().ok_or(VfsError::BadRequest)?.join(new_name)?;
        self.rtt(self.cfg.latency, None).await?;
        let mut fs = self.fs();
        if !fs.exists(&node, uri.path()) || uri.path().matches('/').count() < 2 {
            return Err(VfsError::NotFound(uri.display()));
        }
        if fs.exists(&node, to.path()) {
            return Err(VfsError::AlreadyExists(to.display()));
        }
        fs.copy_tree(&node, uri.path(), &node, to.path());
        fs.remove_tree(&node, uri.path());
        if let Some(it) = fs.items.get_mut(&(node, to.path().to_owned())) {
            it.status = VfsStatus::Synced;
        }
        Ok(to)
    }

    async fn delete(&self, uris: &[VfsUri], cancel: CancellationToken) -> VfsResult<()> {
        for u in uris {
            let node = self.node(u)?.to_owned();
            self.rtt(self.cfg.latency, Some(&cancel)).await?;
            let mut fs = self.fs();
            if u.path().matches('/').count() < 2 {
                return Err(VfsError::PermissionDenied(format!("{} is a share root", u.display())));
            }
            if !fs.exists(&node, u.path()) {
                return Err(VfsError::NotFound(u.display()));
            }
            fs.remove_tree(&node, u.path());
        }
        Ok(())
    }

    async fn transfer(&self, req: TransferRequest, ctl: TransferControl) -> VfsResult<()> {
        let r = self.transfer_inner(&req, &ctl).await;
        let state = match &r {
            Ok(()) => TransferState::Done,
            Err(VfsError::Cancelled) => TransferState::Cancelled,
            Err(e) => TransferState::Failed(e.to_string()),
        };
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
        r
    }

    async fn volume(&self, uri: &VfsUri) -> VfsResult<VolumeInfo> {
        let node = self.node(uri)?.to_owned();
        let used: u64 = self.fs().items.iter().filter(|((n, _), _)| *n == node).map(|(_, i)| i.size).sum();
        Ok(VolumeInfo { total_bytes: CAPACITY, free_bytes: CAPACITY.saturating_sub(used) })
    }

    async fn mounts(&self) -> VfsResult<Vec<MountInfo>> {
        let mut out = Vec::new();
        for (node, status, shares) in NODES {
            for share in *shares {
                out.push(MountInfo {
                    label: format!("{node}/{share}"),
                    uri: VfsUri::gungnir(node, share)?,
                    status: *status,
                    detail: format!("mock node {node}, {}", status.label()),
                });
            }
        }
        Ok(out)
    }

    async fn inspect(&self, uri: &VfsUri) -> VfsResult<Vec<(String, String)>> {
        let e = self.stat(uri).await?;
        let mut out = vec![
            ("URI".to_owned(), e.uri.to_uri_string()),
            ("Backend".to_owned(), "MockGungnirBackend (synthetic)".to_owned()),
            ("VFS status".to_owned(), e.vfs.status.label().to_owned()),
        ];
        out.extend(e.vfs.attributes);
        Ok(out)
    }
}

impl MockGungnirBackend {
    async fn transfer_inner(&self, req: &TransferRequest, ctl: &TransferControl) -> VfsResult<()> {
        let dn = self.node(&req.dest_dir)?.to_owned();
        let mut plan = Vec::new();
        let (mut total_bytes, mut total_files) = (0u64, 0u64);
        {
            let fs = self.fs();
            if !fs.is_dir(&dn, req.dest_dir.path()) {
                return Err(VfsError::NotADirectory(req.dest_dir.display()));
            }
            for s in &req.sources {
                let sn = self.node(s)?.to_owned();
                if req.dest_dir.starts_with(s) {
                    return Err(VfsError::WouldRecurse);
                }
                if !fs.exists(&sn, s.path()) {
                    return Err(VfsError::NotFound(s.display()));
                }
                for p in fs.subtree(&sn, s.path()) {
                    if let Some(i) = fs.items.get(&(sn.clone(), p)) {
                        total_bytes += i.size;
                        total_files += 1;
                    }
                }
                plan.push((sn, s.clone()));
            }
        }
        let mut pr = TransferProgress {
            id: req.id,
            bytes_done: 0,
            bytes_total: total_bytes,
            files_done: 0,
            files_total: total_files,
            current: String::new(),
            state: TransferState::Running,
        };
        let _ = ctl.progress.try_send(pr.clone());
        let tick = Duration::from_millis(100);
        let per_tick = (self.cfg.bytes_per_sec / 10).max(1);
        for (sn, src) in plan {
            let name = src.file_name().unwrap_or("share").to_owned();
            let target = {
                let fs = self.fs();
                let base = req.dest_dir.path();
                let mut cand = name.clone();
                let mut n = 1;
                while fs.exists(&dn, &join_path(base, &cand)) && !req.overwrite {
                    cand = if n == 1 { format!("{name} (copy)") } else { format!("{name} (copy {n})") };
                    n += 1;
                }
                join_path(base, &cand)
            };
            let size: u64 = {
                let fs = self.fs();
                fs.subtree(&sn, src.path())
                    .iter()
                    .filter_map(|p| fs.items.get(&(sn.clone(), p.clone())))
                    .map(|i| i.size)
                    .sum()
            };
            pr.current.clone_from(&name);
            let mut left = if req.kind == TransferKind::Move && sn == dn { 0 } else { size };
            while left > 0 {
                if ctl.cancel.is_cancelled() {
                    return Err(VfsError::Cancelled);
                }
                if *ctl.paused.borrow() {
                    pr.state = TransferState::Paused;
                    let _ = ctl.progress.try_send(pr.clone());
                    let mut paused = ctl.paused.clone();
                    while *paused.borrow() {
                        tokio::select! {
                            r = paused.changed() => if r.is_err() { break },
                            () = ctl.cancel.cancelled() => return Err(VfsError::Cancelled),
                        }
                    }
                    pr.state = TransferState::Running;
                }
                let step = left.min(per_tick);
                if self.cfg.bytes_per_sec != u64::MAX {
                    self.rtt(tick, Some(&ctl.cancel)).await?;
                }
                left -= step;
                pr.bytes_done += step;
                let _ = ctl.progress.try_send(pr.clone());
            }
            let mut fs = self.fs();
            if req.overwrite && fs.exists(&dn, &target) {
                fs.remove_tree(&dn, &target);
            }
            let files = fs.subtree(&sn, src.path()).len() as u64;
            fs.copy_tree(&sn, src.path(), &dn, &target);
            if req.kind == TransferKind::Move {
                fs.remove_tree(&sn, src.path());
                pr.bytes_done += if sn == dn { size } else { 0 };
            }
            pr.files_done += files;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::{mpsc, watch};

    fn u(s: &str) -> VfsUri {
        VfsUri::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    async fn list(b: &MockGungnirBackend, dir: &str) -> VfsResult<(Vec<usize>, Vec<FileEntry>)> {
        let (tx, mut rx) = mpsc::channel(8);
        let dir = u(dir);
        let fut = b.list_dir(&dir, ListOptions::default(), tx, CancellationToken::new());
        let collect = async {
            let mut sizes = Vec::new();
            let mut all = Vec::new();
            while let Some(batch) = rx.recv().await {
                sizes.push(batch.len());
                all.extend(batch);
            }
            (sizes, all)
        };
        let (r, out) = tokio::join!(fut, collect);
        r.map(|_| out)
    }

    #[tokio::test]
    async fn stress_dir_streams_50k_in_batches() -> VfsResult<()> {
        let b = MockGungnirBackend::new(MockConfig::instant());
        let (sizes, all) = list(&b, "gungnir://odin/asgard/stress-50k").await?;
        assert_eq!(all.len(), 50_000);
        assert_eq!(sizes[0], 200, "first batch is small, for instant feedback");
        assert!(sizes[1..].iter().all(|&s| s <= 2_000));
        assert!(all.iter().all(|e| e.uri.scheme() == Scheme::Gungnir));
        Ok(())
    }

    #[tokio::test]
    async fn disconnected_node_fails_fast() {
        let b = MockGungnirBackend::default();
        let t = std::time::Instant::now();
        assert_eq!(list(&b, "gungnir://muninn/thought").await.err(), Some(VfsError::SuspectMount));
        assert!(t.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn cancellation_stops_a_listing() {
        let b = MockGungnirBackend::new(MockConfig {
            latency: Duration::from_millis(1),
            batch_latency: Duration::from_millis(20),
            ..MockConfig::instant()
        });
        let (tx, mut rx) = mpsc::channel(1);
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let dir = u("gungnir://odin/valhalla/stress-100k");
        let task = async { b.list_dir(&dir, ListOptions::default(), tx, cancel).await };
        let consumer = async {
            let first = rx.recv().await.map(|v| v.len());
            c2.cancel();
            while rx.recv().await.is_some() {}
            first
        };
        let (r, first) = tokio::join!(task, consumer);
        assert_eq!(first, Some(200));
        assert_eq!(r, Err(VfsError::Cancelled));
    }

    #[tokio::test]
    async fn mkdir_rename_copy_move_delete() -> VfsResult<()> {
        let b = MockGungnirBackend::new(MockConfig::instant());
        b.create_dir(&u("gungnir://odin/asgard/new")).await?;
        assert_eq!(
            b.create_dir(&u("gungnir://odin/asgard/new")).await,
            Err(VfsError::AlreadyExists("gungnir://odin/asgard/new".into()))
        );
        let renamed = b.rename(&u("gungnir://odin/asgard/new"), "forge").await?;
        assert_eq!(renamed, u("gungnir://odin/asgard/forge"));
        let (ptx, mut prx) = mpsc::channel(64);
        let (_ptx, paused) = watch::channel(false);
        let ctl = TransferControl { progress: ptx, cancel: CancellationToken::new(), paused };
        let req = TransferRequest {
            id: 1,
            kind: TransferKind::Copy,
            sources: vec![u("gungnir://odin/asgard/runes")],
            dest_dir: u("gungnir://huginn/memory"),
            overwrite: false,
        };
        b.transfer(req, ctl).await?;
        let mut last = None;
        while let Ok(p) = prx.try_recv() {
            last = Some(p.state);
        }
        assert_eq!(last, Some(TransferState::Done));
        let (_, mem) = list(&b, "gungnir://huginn/memory/runes").await?;
        assert_eq!(mem.len(), 2);
        b.delete(&[u("gungnir://huginn/memory/runes")], CancellationToken::new()).await?;
        assert!(b.stat(&u("gungnir://huginn/memory/runes")).await.is_err());
        assert!(b.delete(&[u("gungnir://odin/asgard")], CancellationToken::new()).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn mounts_cover_every_status() -> VfsResult<()> {
        let m = MockGungnirBackend::default().mounts().await?;
        for s in [MountStatus::Online, MountStatus::Syncing, MountStatus::Disconnected] {
            assert!(m.iter().any(|x| x.status == s));
        }
        Ok(())
    }
}
