//! Errors. The variants after `Io` map 1:1 onto gungnird reply statuses, so the
//! Gungnir backend never has to squash a daemon answer into a vaguer error.

use thiserror::Error;

pub type VfsResult<T> = Result<T, VfsError>;

/// gungnird reply status codes (protocol v2). Mirrors `md.rs` (`ST_*`, 0..=13) and
/// `ops.rs` (`ST_CANCELLED`, `ST_OS_ERROR`, `ST_WOULD_RECURSE`, 14..=16) in
/// `../gungnir-vfs/src/daemon-rust/src`.
pub mod gungnir_status {
    pub const OK: u32 = 0;
    pub const NOT_FOUND: u32 = 1;
    pub const BUSY: u32 = 2;
    pub const TIMEOUT: u32 = 3;
    pub const SUSPECT_MOUNT: u32 = 4;
    pub const NO_SUCH_FILE: u32 = 5;
    pub const BAD_REQUEST: u32 = 6;
    pub const KEY_TOO_LONG: u32 = 7;
    pub const VALUE_TOO_LARGE: u32 = 8;
    pub const TOO_MANY_KEYS: u32 = 9;
    pub const BAD_UTF8: u32 = 10;
    pub const IO_ERROR: u32 = 11;
    pub const DB_ERROR: u32 = 12;
    pub const UNSUPPORTED: u32 = 13;
    pub const CANCELLED: u32 = 14;
    pub const OS_ERROR: u32 = 15;
    pub const WOULD_RECURSE: u32 = 16;
    pub const MOUNT_INSIDE: u32 = 17;
    pub const NOT_EMPTY: u32 = 18;
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum VfsError {
    #[error("invalid URI: {0}")]
    InvalidUri(String),
    #[error("no backend handles {0}")]
    NoBackend(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("already exists: {0}")]
    AlreadyExists(String),
    #[error("not a directory: {0}")]
    NotADirectory(String),
    #[error("directory not empty: {0}")]
    NotEmpty(String),
    #[error("{op}: {msg} (errno {errno})")]
    Io { op: &'static str, errno: i32, msg: String },
    #[error("transport: {0}")]
    Transport(String),
    #[error("gungnird is not available: {0}")]
    DaemonUnavailable(String),

    // gungnird statuses.
    #[error("not found: {0}")]
    NotFound(String),
    #[error("busy, try again")]
    Busy,
    #[error("timed out")]
    Timeout,
    #[error("the mount is hung or gone (reported by the gungnird mount health monitor)")]
    SuspectMount,
    #[error("no such file: {0}")]
    NoSuchFile(String),
    #[error("bad request")]
    BadRequest,
    #[error("metadata key too long")]
    KeyTooLong,
    #[error("metadata value too large")]
    ValueTooLarge,
    #[error("too many metadata keys on this file")]
    TooManyKeys,
    #[error("invalid UTF-8")]
    BadUtf8,
    #[error("daemon I/O error")]
    DaemonIo,
    #[error("daemon database error")]
    Database,
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("cancelled")]
    Cancelled,
    #[error("operation would recurse into a directory")]
    WouldRecurse,
}

impl VfsError {
    /// Map a gungnird reply status (plus the errno of `OS_ERROR`) to an error;
    /// `None` for `OK`. `what` names the path for the path-carrying variants.
    #[must_use]
    pub fn from_gungnir_status(status: u32, errno: i32, what: &str) -> Option<Self> {
        use gungnir_status as s;
        Some(match status {
            s::OK => return None,
            s::NOT_FOUND => Self::NotFound(what.to_owned()),
            s::BUSY => Self::Busy,
            s::TIMEOUT => Self::Timeout,
            s::SUSPECT_MOUNT => Self::SuspectMount,
            s::NO_SUCH_FILE => Self::NoSuchFile(what.to_owned()),
            s::BAD_REQUEST => Self::BadRequest,
            s::KEY_TOO_LONG => Self::KeyTooLong,
            s::VALUE_TOO_LARGE => Self::ValueTooLarge,
            s::TOO_MANY_KEYS => Self::TooManyKeys,
            s::BAD_UTF8 => Self::BadUtf8,
            s::IO_ERROR => Self::DaemonIo,
            s::DB_ERROR => Self::Database,
            s::UNSUPPORTED => Self::Unsupported("the running gungnird does not support this request".into()),
            s::CANCELLED => Self::Cancelled,
            s::OS_ERROR => Self::from_errno("gungnird", errno, what),
            s::WOULD_RECURSE => Self::WouldRecurse,
            s::MOUNT_INSIDE => Self::Unsupported("it contains a mounted filesystem; unmount it and try again".into()),
            s::NOT_EMPTY => Self::Unsupported("some items could not be removed from the trash".into()),
            other => Self::Transport(format!("unknown gungnird status {other}")),
        })
    }

    /// Map an OS errno to the closest variant.
    #[must_use]
    pub fn from_errno(op: &'static str, errno: i32, what: &str) -> Self {
        match errno {
            libc_errno::ENOENT => Self::NotFound(what.to_owned()),
            libc_errno::EACCES | libc_errno::EPERM => Self::PermissionDenied(what.to_owned()),
            libc_errno::EEXIST => Self::AlreadyExists(what.to_owned()),
            libc_errno::ENOTDIR => Self::NotADirectory(what.to_owned()),
            libc_errno::ENOTEMPTY => Self::NotEmpty(what.to_owned()),
            libc_errno::ETIMEDOUT => Self::Timeout,
            _ => Self::Io { op, errno, msg: format!("{what}: {}", std::io::Error::from_raw_os_error(errno)) },
        }
    }

    /// From a `std::io::Error`, keeping the errno when there is one.
    #[must_use]
    pub fn from_io(op: &'static str, e: &std::io::Error, what: &str) -> Self {
        match e.raw_os_error() {
            Some(errno) => Self::from_errno(op, errno, what),
            None => match e.kind() {
                std::io::ErrorKind::NotFound => Self::NotFound(what.to_owned()),
                std::io::ErrorKind::PermissionDenied => Self::PermissionDenied(what.to_owned()),
                std::io::ErrorKind::AlreadyExists => Self::AlreadyExists(what.to_owned()),
                _ => Self::Io { op, errno: 0, msg: format!("{what}: {e}") },
            },
        }
    }
}

/// The few errno values the mapping needs (Linux numbering), so this crate does not
/// depend on `libc`.
mod libc_errno {
    pub const EPERM: i32 = 1;
    pub const ENOENT: i32 = 2;
    pub const EACCES: i32 = 13;
    pub const EEXIST: i32 = 17;
    pub const ENOTDIR: i32 = 20;
    pub const ENOTEMPTY: i32 = 39;
    pub const ETIMEDOUT: i32 = 110;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gungnir_status_maps() {
        assert_eq!(VfsError::from_gungnir_status(0, 0, "x"), None);
        for st in 1..=16 {
            let e = VfsError::from_gungnir_status(st, 2, "/x");
            assert!(matches!(e, Some(ref e) if !matches!(e, VfsError::Transport(_))), "status {st}");
        }
        assert_eq!(VfsError::from_gungnir_status(15, 2, "/x"), Some(VfsError::NotFound("/x".into())));
        assert!(matches!(VfsError::from_gungnir_status(99, 0, ""), Some(VfsError::Transport(_))));
    }

    #[test]
    fn io_errors_keep_their_errno() {
        let e = std::io::Error::from_raw_os_error(13);
        assert_eq!(VfsError::from_io("open", &e, "/root"), VfsError::PermissionDenied("/root".into()));
        let e = std::io::Error::from_raw_os_error(5);
        assert!(matches!(VfsError::from_io("read", &e, "/x"), VfsError::Io { errno: 5, .. }));
    }
}
