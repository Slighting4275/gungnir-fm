//! Shared helpers for the integration tests: private temp dirs under the
//! isolated sandbox `$TMPDIR/gungnir-test-sandbox` (never a home folder) and listing
//! collection.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use core_vfs::{CancellationToken, FileEntry, ListOptions, VfsProvider, VfsResult, VfsUri};
use tokio::sync::mpsc;

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join("gungnir-test-sandbox");
        std::fs::create_dir_all(&base).unwrap_or_else(|e| panic!("{e}"));
        let base = std::fs::canonicalize(base).unwrap_or_else(|e| panic!("{e}"));
        let p = base.join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir(&p).unwrap_or_else(|e| panic!("{e}"));
        Self(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn uri(&self, rel: &str) -> VfsUri {
        let p = if rel.is_empty() { self.0.clone() } else { self.0.join(rel) };
        VfsUri::from_local_path(&p).unwrap_or_else(|e| panic!("{e}"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// List `dir` fully; returns batch sizes and all entries.
pub async fn list(b: &dyn VfsProvider, dir: &VfsUri, opts: ListOptions) -> VfsResult<(Vec<usize>, Vec<FileEntry>)> {
    let (tx, mut rx) = mpsc::channel(4);
    let fut = b.list_dir(dir, opts, tx, CancellationToken::new());
    let collect = async {
        let (mut sizes, mut all) = (Vec::new(), Vec::new());
        while let Some(batch) = rx.recv().await {
            sizes.push(batch.len());
            all.extend(batch);
        }
        (sizes, all)
    };
    let (r, out) = tokio::join!(fut, collect);
    r.map(|_| out)
}
