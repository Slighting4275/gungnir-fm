//! Session-local history for individual renames. Identity checks never follow symlinks.

use std::collections::VecDeque;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::time::SystemTime;

use core_vfs::{VfsError, VfsProvider, VfsResult, VfsUri};

const LIMIT: usize = 100;

#[derive(Debug, Clone)]
pub struct Identity {
    device: u64,
    inode: u64,
    created: SystemTime,
}

impl Identity {
    fn capture(path: &std::path::Path) -> std::io::Result<Option<Self>> {
        let md = std::fs::symlink_metadata(path)?;
        // Inode numbers can be reused after deletion. Birth time distinguishes
        // the replacement without holding deleted files open in history.
        Ok(md.created().ok().map(|created| Self { device: md.dev(), inode: md.ino(), created }))
    }

    fn matches(&self, other: &Self) -> bool {
        self.device == other.device && self.inode == other.inode && self.created == other.created
    }
}

#[derive(Debug, Clone)]
pub struct Record {
    pub before: VfsUri,
    pub after: VfsUri,
    pub identity: Identity,
}

#[derive(Debug, Default)]
pub struct History {
    undo: VecDeque<Record>,
    redo: VecDeque<Record>,
}

impl History {
    pub fn record(&mut self, before: VfsUri, after: VfsUri, identity: Option<Identity>) {
        self.redo.clear();
        if let Some(identity) = identity {
            self.undo.push_back(Record { before, after, identity });
            if self.undo.len() > LIMIT {
                self.undo.pop_front();
            }
        }
    }

    pub fn undo(&self) -> Option<&Record> {
        self.undo.back()
    }

    pub fn redo(&self) -> Option<&Record> {
        self.redo.back()
    }

    pub fn finish_undo(&mut self) {
        if let Some(record) = self.undo.pop_back() {
            self.redo.push_back(record);
        }
    }

    pub fn finish_redo(&mut self) {
        if let Some(record) = self.redo.pop_back() {
            self.undo.push_back(record);
        }
    }
}

/// Resolve identity on a worker, then use the backend's non-replacing rename.
/// External filesystem mutations can still race the check and rename syscall.
pub async fn apply(
    provider: Arc<dyn VfsProvider>,
    source: VfsUri,
    name: String,
    expected: Option<Identity>,
) -> VfsResult<(VfsUri, Option<Identity>)> {
    let identity = if let Some(path) = source.to_local_path() {
        let current = tokio::task::spawn_blocking(move || Identity::capture(&path))
            .await
            .map_err(|e| VfsError::Unsupported(e.to_string()))?
            .map_err(|e| VfsError::from_io("rename", &e, &source.display()))?;
        if let Some(expected) = expected
            && current.as_ref().is_none_or(|current| !expected.matches(current))
        {
            return Err(VfsError::Unsupported("The item was replaced; its saved rename cannot be applied".into()));
        }
        current
    } else {
        None
    };
    let destination = provider.rename(&source, &name).await?;
    Ok((destination, identity))
}
