//! The backend contract and the scheme router.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::error::{VfsError, VfsResult};
use crate::model::{
    BackendKind, FileEntry, ListSummary, MountInfo, TransferProgress, TransferRequest, VfsCapabilities, VolumeInfo,
    WatchEvent,
};
use crate::uri::VfsUri;

/// Batches of entries flow through this bounded channel, so a slow UI applies
/// back-pressure to the scan instead of buffering 100k entries twice.
pub type EntrySink = mpsc::Sender<Vec<FileEntry>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListOptions {
    /// Size of the first batch (shown at once).
    pub first_batch: usize,
    /// Size of every later batch.
    pub batch: usize,
}

impl Default for ListOptions {
    fn default() -> Self {
        Self { first_batch: 200, batch: 2_000 }
    }
}

/// What a running transfer reports to and hears from the controller.
pub struct TransferControl {
    pub progress: mpsc::Sender<TransferProgress>,
    pub cancel: CancellationToken,
    /// `true` while paused (only where `VfsCapabilities::pause_transfers`).
    pub paused: watch::Receiver<bool>,
}

/// A live directory watch: events arrive until it is dropped.
pub struct DirWatch {
    pub events: mpsc::Receiver<WatchEvent>,
    /// Keeps the backend's watcher alive.
    pub guard: Box<dyn Send>,
}

/// A storage backend. Every method is async and must never block the calling
/// runtime thread: blocking syscalls go to `spawn_blocking` or a dedicated thread.
/// Hidden entries are always listed; the controller filters them (`Ctrl+H` toggles
/// without a reload).
#[async_trait]
pub trait VfsProvider: Send + Sync + 'static {
    fn kind(&self) -> BackendKind;

    fn capabilities(&self) -> VfsCapabilities;

    /// Whether this backend serves `uri`.
    fn handles(&self, uri: &VfsUri) -> bool;

    /// Stream the children of `dir` into `sink`: `opts.first_batch` entries first,
    /// then batches of `opts.batch`. Stops early with [`VfsError::Cancelled`] once
    /// `cancel` fires.
    async fn list_dir(
        &self,
        dir: &VfsUri,
        opts: ListOptions,
        sink: EntrySink,
        cancel: CancellationToken,
    ) -> VfsResult<ListSummary>;

    async fn stat(&self, uri: &VfsUri) -> VfsResult<FileEntry>;

    async fn create_dir(&self, uri: &VfsUri) -> VfsResult<()>;

    /// Rename within the same directory; returns the new URI.
    async fn rename(&self, uri: &VfsUri, new_name: &str) -> VfsResult<VfsUri>;

    /// Delete permanently (directories recursively).
    async fn delete(&self, uris: &[VfsUri], cancel: CancellationToken) -> VfsResult<()>;

    /// Move to the trash (freedesktop.org Trash spec for local files).
    async fn trash(&self, uris: &[VfsUri]) -> VfsResult<()> {
        let _ = uris;
        Err(VfsError::Unsupported(format!("{} has no trash", self.kind().badge())))
    }

    /// Restore trashed files back to their original location.
    async fn restore(&self, uris: &[VfsUri]) -> VfsResult<()> {
        let _ = uris;
        Err(VfsError::Unsupported(format!("{} cannot restore", self.kind().badge())))
    }

    /// Empty the trash.
    async fn empty_trash(&self) -> VfsResult<()> {
        Err(VfsError::Unsupported(format!("{} cannot empty trash", self.kind().badge())))
    }

    /// Whether [`VfsProvider::transfer`] of `src` into `dest` can run here. The
    /// controller asks the destination's backend.
    fn can_transfer(&self, src: &VfsUri, dest: &VfsUri) -> bool {
        self.handles(src) && self.handles(dest)
    }

    /// Copy or move `req.sources` into `req.dest_dir`, reporting progress.
    async fn transfer(&self, req: TransferRequest, ctl: TransferControl) -> VfsResult<()>;

    async fn volume(&self, uri: &VfsUri) -> VfsResult<VolumeInfo>;

    /// Locations for the sidebar (Gungnir shares, with their status).
    async fn mounts(&self) -> VfsResult<Vec<MountInfo>> {
        Ok(Vec::new())
    }

    /// Everything the backend knows about one entry ("Gungnir VFS Inspect").
    async fn inspect(&self, uri: &VfsUri) -> VfsResult<Vec<(String, String)>>;

    /// Start watching `dir`; `Ok(None)` if this backend cannot watch.
    fn watch(&self, dir: &VfsUri) -> VfsResult<Option<DirWatch>> {
        let _ = dir;
        Ok(None)
    }
}

/// Routes a URI to the first provider that handles it.
#[derive(Clone, Default)]
pub struct VfsRegistry {
    providers: Vec<Arc<dyn VfsProvider>>,
}

impl VfsRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, p: Arc<dyn VfsProvider>) {
        self.providers.push(p);
    }

    /// # Errors
    /// [`VfsError::NoBackend`] if nothing handles `uri`.
    pub fn resolve(&self, uri: &VfsUri) -> VfsResult<Arc<dyn VfsProvider>> {
        self.providers.iter().find(|p| p.handles(uri)).cloned().ok_or_else(|| VfsError::NoBackend(uri.display()))
    }

    #[must_use]
    pub fn providers(&self) -> &[Arc<dyn VfsProvider>] {
        &self.providers
    }
}
