//! Storage backends for Gungnir FM.
//!
//! - [`LocalPosixBackend`]: the daily-driver local filesystem (`file://`).
//! - [`MockGungnirBackend`]: synthetic `gungnir://` nodes and shares with simulated
//!   latency, sync states and 50,000/100,000-entry stress directories.
//! - [`GungnirVfsBackend`]: a live client of `gungnird` speaking its protocol v2
//!   (memfd ring pair, futex wakeups, `AF_UNIX` setup), mirroring the wire format in
//!   `../gungnir-vfs/src/daemon-rust/src/{ipc,md,health,ops}.rs`.

pub mod archive;
pub mod gungnir;
pub mod local;
pub mod mock;
pub mod remote;
pub mod time;

pub use archive::{ArchiveError, ArchiveFormat, ArchiveOptions, ArchiveStats, ExtractStats};
pub use gungnir::GungnirVfsBackend;
pub use local::LocalPosixBackend;
pub use mock::MockGungnirBackend;
pub use remote::RemoteVfsBackend;
