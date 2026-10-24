//! Data model shared by backends and the UI controller.

use std::time::SystemTime;

use crate::uri::VfsUri;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileKind {
    Directory,
    File,
    Symlink,
    /// FIFO, socket, device.
    Special,
}

/// Per-entry VFS state shown in the "VFS Status" column and as a badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub enum VfsStatus {
    /// Plain local file, nothing to report.
    #[default]
    Local,
    /// Gungnir: content is current on this node.
    Synced,
    /// Gungnir: a transfer or sync is in progress.
    Syncing,
    /// Gungnir: a local copy of remote content.
    Cached,
    /// Gungnir: content lives on another node only.
    RemoteOnly,
    /// Gungnir: diverging versions.
    Conflict,
    /// gungnird holds `metadata::*` keys for this file.
    Tagged,
    /// The mount holding the entry is hung or gone (gungnird health monitor).
    Unreachable,
}

impl VfsStatus {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            VfsStatus::Local => "",
            VfsStatus::Synced => "synced",
            VfsStatus::Syncing => "syncing",
            VfsStatus::Cached => "cached",
            VfsStatus::RemoteOnly => "remote",
            VfsStatus::Conflict => "conflict",
            VfsStatus::Tagged => "tagged",
            VfsStatus::Unreachable => "unreachable",
        }
    }

    /// Stable small integer for the UI (`theme.slint` maps it to a badge colour).
    #[must_use]
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// Backend-specific metadata of one entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VfsMetadata {
    pub status: VfsStatus,
    /// `metadata::*` keys and values (Gungnir), or synthetic attributes (mock).
    pub attributes: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub uri: VfsUri,
    pub kind: FileKind,
    /// For a symlink: whether it points at a directory (it is then navigable).
    pub target_is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Unix mode bits (`st_mode & 0o7777`).
    pub mode: u32,
    pub vfs: VfsMetadata,
}

impl FileEntry {
    #[must_use]
    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }

    /// Opens as a folder: a directory or a symlink to one.
    #[must_use]
    pub fn is_navigable(&self) -> bool {
        self.kind == FileKind::Directory || (self.kind == FileKind::Symlink && self.target_is_dir)
    }
}

/// What a backend can do; the UI greys out the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct VfsCapabilities {
    pub writable: bool,
    pub watch: bool,
    pub trash: bool,
    pub pause_transfers: bool,
    /// Entries carry `VfsMetadata::attributes` worth inspecting.
    pub vfs_metadata: bool,
    /// Mounts report a health state.
    pub mount_health: bool,
}

/// Which backend serves the active location (status bar badge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Posix,
    GungnirMock,
    GungnirLive,
}

impl BackendKind {
    #[must_use]
    pub fn badge(self) -> &'static str {
        match self {
            BackendKind::Posix => "POSIX",
            BackendKind::GungnirMock => "GUNGNIR-VFS · MOCK",
            BackendKind::GungnirLive => "GUNGNIR-VFS",
        }
    }
}

/// Connection state of a sidebar mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MountStatus {
    #[default]
    Online,
    Syncing,
    /// Answering, but slowly (gungnird `Health::Slow`).
    Degraded,
    /// gungnird `Health::Hung` / `Health::Gone`, or the node is unreachable.
    Disconnected,
}

impl MountStatus {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            MountStatus::Online => "Online",
            MountStatus::Syncing => "Syncing",
            MountStatus::Degraded => "Slow",
            MountStatus::Disconnected => "Disconnected",
        }
    }

    #[must_use]
    pub fn code(self) -> i32 {
        self as i32
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountInfo {
    pub label: String,
    pub uri: VfsUri,
    pub status: MountStatus,
    /// Extra detail for the tooltip line (latency, health source, ...).
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VolumeInfo {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    Copy,
    Move,
}

impl TransferKind {
    #[must_use]
    pub fn verb(self) -> &'static str {
        match self {
            TransferKind::Copy => "Copying",
            TransferKind::Move => "Moving",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRequest {
    pub id: u64,
    pub kind: TransferKind,
    pub sources: Vec<VfsUri>,
    /// Existing directory the sources land in, keeping their names.
    pub dest_dir: VfsUri,
    pub overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferState {
    Queued,
    Running,
    Paused,
    Done,
    Cancelled,
    Failed(String),
}

/// One progress report; backends send them at most every ~100 ms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferProgress {
    pub id: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
    /// Name of the file being worked on.
    pub current: String,
    pub state: TransferState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ListSummary {
    pub entries: u64,
    /// Entries the backend could not stat (permission races, vanished files).
    pub skipped: u64,
}

/// A change below a watched directory; the controller reloads (debounced).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    Changed,
    /// The watched directory itself vanished.
    Removed,
}
