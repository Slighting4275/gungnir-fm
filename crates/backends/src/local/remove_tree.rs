//! Descriptor-relative local deletion with a bounded number of open directories.

use std::ffi::{CStr, CString};
use std::fs::Metadata;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rustix::fd::{AsFd, BorrowedFd};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat, fstat, open, openat, statat, unlinkat};
use rustix::io::Errno;

const OPEN_LEVELS: usize = 16;
const DIRECTORY: OFlags = OFlags::RDONLY.union(OFlags::DIRECTORY).union(OFlags::CLOEXEC);
const NOFOLLOW: AtFlags = AtFlags::SYMLINK_NOFOLLOW;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(stat: &Stat) -> Self {
        // stat's field widths vary by target.
        #[allow(clippy::useless_conversion)]
        Self { device: u64::from(stat.st_dev), inode: u64::from(stat.st_ino) }
    }

    fn verify(self, stat: &Stat) -> rustix::io::Result<()> {
        if self == Self::of(stat) { Ok(()) } else { Err(Errno::BUSY) }
    }
}

struct Level {
    name: CString,
    identity: Identity,
    stream: Option<Dir>,
}

fn open_directory(parent: BorrowedFd<'_>, name: &CStr, identity: Identity) -> rustix::io::Result<Dir> {
    let fd = openat(parent, name, DIRECTORY | OFlags::NOFOLLOW, Mode::empty())?;
    identity.verify(&fstat(&fd)?)?;
    Dir::new(fd)
}

fn unlink_directory(parent: BorrowedFd<'_>, level: &Level) -> rustix::io::Result<()> {
    level.identity.verify(&statat(parent, level.name.as_c_str(), NOFOLLOW)?)?;
    unlinkat(parent, level.name.as_c_str(), AtFlags::REMOVEDIR)
}

pub(super) fn remove(path: &Path, original: &Metadata) -> io::Result<()> {
    let name = path.file_name().ok_or(Errno::INVAL)?;
    let root_parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    // The containing directory only needs search permission on Linux.
    #[cfg(target_os = "linux")]
    let anchor_flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let anchor_flags = DIRECTORY;
    let root_parent = open(root_parent, anchor_flags, Mode::empty())?;
    let name = CString::new(name.as_bytes()).map_err(|_| Errno::INVAL)?;
    let identity = Identity { device: original.dev(), inode: original.ino() };
    let root = open_directory(root_parent.as_fd(), &name, identity)?;
    let mut ancestors = Vec::<Level>::new();
    let mut current = Level { name, identity, stream: Some(root) };

    loop {
        let stream = current.stream.as_mut().ok_or(Errno::BADF)?;
        if let Some(entry) = stream.read().transpose()? {
            let name = entry.file_name();
            if name == c"." || name == c".." {
                continue;
            }
            let parent = stream.fd()?;
            let stat = match statat(parent, name, NOFOLLOW) {
                Err(Errno::NOENT) => continue,
                other => other?,
            };
            if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
                match unlinkat(parent, name, AtFlags::empty()) {
                    Err(Errno::NOENT) => {}
                    other => other?,
                }
                continue;
            }
            let identity = Identity::of(&stat);
            let child = open_directory(parent, name, identity)?;
            if let Some(index) = ancestors.len().checked_sub(OPEN_LEVELS - 1) {
                ancestors[index].stream = None;
            }
            ancestors.push(current);
            current = Level { name: name.to_owned(), identity, stream: Some(child) };
        } else if let Some(mut parent) = ancestors.pop() {
            // Reopen a closed level through the child's real parent and verify its
            // identity before using it. A moved child can lead to a different parent.
            let parent_stream = match parent.stream.as_mut() {
                Some(stream) => stream,
                None => parent.stream.insert(open_directory(stream.fd()?, c"..", parent.identity)?),
            };
            match unlink_directory(parent_stream.fd()?, &current) {
                Err(Errno::NOENT) => {}
                other => other?,
            }
            current = parent;
        } else {
            unlink_directory(root_parent.as_fd(), &current)?;
            return Ok(());
        }
    }
}
