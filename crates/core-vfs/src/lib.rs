//! The contract every Gungnir FM storage backend implements.
//!
//! - [`VfsProvider`]: the async trait (listing streamed in batches, file operations,
//!   transfers with progress, mounts, volume info, change watching).
//! - [`model`]: `FileEntry`, `VfsMetadata`, `VfsCapabilities`, `TransferProgress`, ...
//! - [`uri`]: `file://` and `gungnir://` URIs.
//! - [`error`]: `VfsError`, whose variants follow the reply statuses of gungnird
//!   (`../gungnir-vfs/src/daemon-rust/src/md.rs` and `ops.rs`).
//! - [`sort`] and [`format`]: ordering and display helpers the UI runs off its thread.

pub mod error;
pub mod format;
pub mod model;
pub mod pattern;
pub mod provider;
pub mod sort;
pub mod uri;

pub use error::{VfsError, VfsResult};
pub use model::*;
pub use provider::{EntrySink, ListOptions, TransferControl, VfsProvider, VfsRegistry};
pub use sort::{SortColumn, SortKey};
pub use tokio_util::sync::CancellationToken;
pub use uri::{Scheme, VfsUri};
