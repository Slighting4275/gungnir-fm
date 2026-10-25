//! Built-in remote filesystem backend supporting SFTP/SSH (`sftp://`, `ssh://`)
//! and SMB/CIFS (`smb://`, `cifs://`).
//!
//! Communicates with remote servers via GIO/GVFS (GNOME Virtual File System) subsystem,
//! streaming directory batches into [`core_vfs::EntrySink`].

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use core_vfs::error::{VfsError, VfsResult};
use core_vfs::model::{
    BackendKind, FileEntry, FileKind, ListSummary, TransferProgress, TransferRequest, VfsCapabilities, VfsMetadata,
    VolumeInfo,
};
use core_vfs::provider::{EntrySink, ListOptions, TransferControl, VfsProvider};
use core_vfs::uri::{Scheme, VfsUri};
use tokio_util::sync::CancellationToken;

static TEST_MODE: AtomicBool = AtomicBool::new(false);

/// Enable hermetic test mode for unit tests without network access.
pub fn set_remote_test_mode(enabled: bool) {
    TEST_MODE.store(enabled, Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy)]
pub struct RemoteVfsBackend {
    kind: BackendKind,
}

impl RemoteVfsBackend {
    #[must_use]
    pub fn sftp() -> Self {
        Self { kind: BackendKind::Sftp }
    }

    #[must_use]
    pub fn smb() -> Self {
        Self { kind: BackendKind::Smb }
    }
}

#[async_trait]
impl VfsProvider for RemoteVfsBackend {
    fn kind(&self) -> BackendKind {
        self.kind
    }

    fn capabilities(&self) -> VfsCapabilities {
        VfsCapabilities {
            writable: true,
            watch: false,
            trash: false,
            pause_transfers: false,
            vfs_metadata: false,
            mount_health: false,
        }
    }

    fn handles(&self, uri: &VfsUri) -> bool {
        match self.kind {
            BackendKind::Sftp => uri.scheme() == Scheme::Sftp,
            BackendKind::Smb => uri.scheme() == Scheme::Smb,
            _ => false,
        }
    }

    async fn list_dir(
        &self,
        dir: &VfsUri,
        opts: ListOptions,
        sink: EntrySink,
        cancel: CancellationToken,
    ) -> VfsResult<ListSummary> {
        let uri_str = dir.to_uri_string();
        let target_dir = dir.clone();

        if TEST_MODE.load(Ordering::Relaxed) {
            return list_mock_remote(&target_dir, opts, sink, cancel).await;
        }

        tokio::task::spawn_blocking(move || {
            let output = Command::new("gio")
                .args(["list", "-l", "--", &uri_str])
                .output()
                .map_err(|e| VfsError::Transport(format!("gio list failed: {e}")))?;

            if !output.status.success() {
                let err_msg = String::from_utf8_lossy(&output.stderr);
                return Err(VfsError::Transport(format!("cannot open {uri_str}: {err_msg}")));
            }

            let text = String::from_utf8_lossy(&output.stdout);
            let mut entries = Vec::new();
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // gio list -l format: "<name>\t<size>\t(<type>)"
                let parts: Vec<&str> = trimmed.split('\t').collect();
                if parts.is_empty() {
                    continue;
                }
                let name = parts[0].trim().to_string();
                if name.is_empty() || name == "." || name == ".." {
                    continue;
                }

                let size = parts.get(1).and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
                let type_str = parts.get(2).unwrap_or(&"");
                let is_dir = type_str.contains("directory");
                let kind = if is_dir { FileKind::Directory } else { FileKind::File };
                let child_uri = match target_dir.join(&name) {
                    Ok(u) => u,
                    Err(_) => continue,
                };

                entries.push(FileEntry {
                    name,
                    uri: child_uri,
                    kind,
                    target_is_dir: is_dir,
                    size,
                    modified: None,
                    mode: if is_dir { 0o755 } else { 0o644 },
                    vfs: VfsMetadata::default(),
                });
            }

            let total = entries.len();
            let mut first = true;
            let mut pos = 0;
            while pos < entries.len() {
                if cancel.is_cancelled() {
                    return Err(VfsError::Cancelled);
                }
                let chunk_size = if first { opts.first_batch } else { opts.batch };
                first = false;
                let end = (pos + chunk_size).min(entries.len());
                let batch = entries[pos..end].to_vec();
                pos = end;
                if sink.blocking_send(batch).is_err() {
                    return Err(VfsError::Cancelled);
                }
            }

            let total_u64 = u64::try_from(total).unwrap_or(0);
            Ok(ListSummary { entries: total_u64, skipped: 0 })
        })
        .await
        .map_err(|e| VfsError::Transport(e.to_string()))?
    }

    async fn stat(&self, uri: &VfsUri) -> VfsResult<FileEntry> {
        let uri_str = uri.to_uri_string();
        let target_uri = uri.clone();
        let name = uri.file_name().unwrap_or("").to_string();

        if TEST_MODE.load(Ordering::Relaxed) {
            let is_dir = uri.is_root() || !name.contains('.');
            return Ok(FileEntry {
                name,
                uri: target_uri,
                kind: if is_dir { FileKind::Directory } else { FileKind::File },
                target_is_dir: is_dir,
                size: if is_dir { 4096 } else { 1024 },
                modified: None,
                mode: if is_dir { 0o755 } else { 0o644 },
                vfs: VfsMetadata::default(),
            });
        }

        tokio::task::spawn_blocking(move || {
            let output = Command::new("gio")
                .args(["info", "--", &uri_str])
                .output()
                .map_err(|e| VfsError::Transport(format!("gio info failed: {e}")))?;

            if !output.status.success() {
                return Err(VfsError::Transport(format!("stat failed for {uri_str}")));
            }

            let text = String::from_utf8_lossy(&output.stdout);
            let is_dir = text.contains("type: directory");
            let mut size = 0;
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("size:") {
                    size = rest.trim().parse::<u64>().unwrap_or(0);
                    break;
                }
            }

            Ok(FileEntry {
                name,
                uri: target_uri,
                kind: if is_dir { FileKind::Directory } else { FileKind::File },
                target_is_dir: is_dir,
                size,
                modified: None,
                mode: if is_dir { 0o755 } else { 0o644 },
                vfs: VfsMetadata::default(),
            })
        })
        .await
        .map_err(|e| VfsError::Transport(e.to_string()))?
    }

    async fn create_dir(&self, uri: &VfsUri) -> VfsResult<()> {
        let uri_str = uri.to_uri_string();
        if TEST_MODE.load(Ordering::Relaxed) {
            return Ok(());
        }
        tokio::task::spawn_blocking(move || {
            let status = Command::new("gio")
                .args(["mkdir", "--", &uri_str])
                .status()
                .map_err(|e| VfsError::Transport(e.to_string()))?;
            if status.success() { Ok(()) } else { Err(VfsError::Transport(format!("failed to create dir {uri_str}"))) }
        })
        .await
        .map_err(|e| VfsError::Transport(e.to_string()))?
    }

    async fn rename(&self, uri: &VfsUri, new_name: &str) -> VfsResult<VfsUri> {
        let uri_str = uri.to_uri_string();
        let new_name_owned = new_name.to_string();
        let parent = uri.parent().ok_or_else(|| VfsError::InvalidUri("cannot rename root".into()))?;
        let new_uri = parent.join(new_name)?;

        if TEST_MODE.load(Ordering::Relaxed) {
            return Ok(new_uri);
        }

        tokio::task::spawn_blocking(move || {
            let status = Command::new("gio")
                .args(["rename", "--", &uri_str, &new_name_owned])
                .status()
                .map_err(|e| VfsError::Transport(e.to_string()))?;
            if status.success() { Ok(new_uri) } else { Err(VfsError::Transport(format!("failed to rename {uri_str}"))) }
        })
        .await
        .map_err(|e| VfsError::Transport(e.to_string()))?
    }

    async fn delete(&self, uris: &[VfsUri], _cancel: CancellationToken) -> VfsResult<()> {
        if TEST_MODE.load(Ordering::Relaxed) {
            return Ok(());
        }
        let uri_strings: Vec<String> = uris.iter().map(VfsUri::to_uri_string).collect();
        tokio::task::spawn_blocking(move || {
            for s in &uri_strings {
                let _ = Command::new("gio").args(["remove", "--", s]).status();
            }
            Ok(())
        })
        .await
        .map_err(|e| VfsError::Transport(e.to_string()))?
    }

    async fn transfer(&self, req: TransferRequest, ctl: TransferControl) -> VfsResult<()> {
        let dest_str = req.dest_dir.to_uri_string();
        let src_strings: Vec<String> = req.sources.iter().map(VfsUri::to_uri_string).collect();
        let is_move = matches!(req.kind, core_vfs::model::TransferKind::Move);

        if TEST_MODE.load(Ordering::Relaxed) {
            let _ = ctl
                .progress
                .send(TransferProgress {
                    id: 0,
                    bytes_done: 100,
                    bytes_total: 100,
                    files_done: req.sources.len() as u64,
                    files_total: req.sources.len() as u64,
                    current: String::new(),
                    state: core_vfs::model::TransferState::Done,
                })
                .await;
            return Ok(());
        }

        tokio::task::spawn_blocking(move || {
            let cmd_name = if is_move { "move" } else { "copy" };
            for s in &src_strings {
                if ctl.cancel.is_cancelled() {
                    return Err(VfsError::Cancelled);
                }
                let status = Command::new("gio")
                    .args([cmd_name, "--", s, &dest_str])
                    .status()
                    .map_err(|e| VfsError::Transport(e.to_string()))?;
                if !status.success() {
                    return Err(VfsError::Transport(format!("transfer failed from {s} to {dest_str}")));
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| VfsError::Transport(e.to_string()))?
    }

    async fn volume(&self, uri: &VfsUri) -> VfsResult<VolumeInfo> {
        let uri_str = uri.to_uri_string();
        if TEST_MODE.load(Ordering::Relaxed) {
            return Ok(VolumeInfo { total_bytes: 1024 * 1024 * 1024 * 100, free_bytes: 1024 * 1024 * 1024 * 50 });
        }

        tokio::task::spawn_blocking(move || {
            let output = Command::new("gio").args(["info", "-f", "--", &uri_str]).output().ok();
            let mut free = 0;
            let mut total = 0;
            if let Some(out) = output {
                let text = String::from_utf8_lossy(&out.stdout);
                for line in text.lines() {
                    if let Some(rest) = line.strip_prefix("filesystem::free:") {
                        free = rest.trim().parse::<u64>().unwrap_or(0);
                    } else if let Some(rest) = line.strip_prefix("filesystem::size:") {
                        total = rest.trim().parse::<u64>().unwrap_or(0);
                    }
                }
            }
            Ok(VolumeInfo { total_bytes: total, free_bytes: free })
        })
        .await
        .map_err(|e| VfsError::Transport(e.to_string()))?
    }

    async fn inspect(&self, uri: &VfsUri) -> VfsResult<Vec<(String, String)>> {
        let uri_str = uri.to_uri_string();
        if TEST_MODE.load(Ordering::Relaxed) {
            return Ok(vec![
                ("URI".to_string(), uri_str),
                ("Backend".to_string(), self.kind.badge().to_string()),
                ("Protocol".to_string(), uri.scheme().as_str().to_string()),
            ]);
        }

        tokio::task::spawn_blocking(move || {
            let output = Command::new("gio")
                .args(["info", "--", &uri_str])
                .output()
                .map_err(|e| VfsError::Transport(format!("gio info failed: {e}")))?;

            let text = String::from_utf8_lossy(&output.stdout);
            let mut rows = vec![("URI".to_string(), uri_str), ("Backend".to_string(), "Remote GIO".to_string())];
            for line in text.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    rows.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            Ok(rows)
        })
        .await
        .map_err(|e| VfsError::Transport(e.to_string()))?
    }
}

async fn list_mock_remote(
    dir: &VfsUri,
    _opts: ListOptions,
    sink: EntrySink,
    _cancel: CancellationToken,
) -> VfsResult<ListSummary> {
    let mock_names = ["project_remote", "archive.tar.gz", "readme.txt", "video_stream.mp4"];
    let mut entries = Vec::new();
    for name in mock_names {
        let is_dir = !name.contains('.');
        let uri = dir.join(name)?;
        entries.push(FileEntry {
            name: name.to_string(),
            uri,
            kind: if is_dir { FileKind::Directory } else { FileKind::File },
            target_is_dir: is_dir,
            size: if is_dir { 4096 } else { 128_000 },
            modified: None,
            mode: if is_dir { 0o755 } else { 0o644 },
            vfs: VfsMetadata::default(),
        });
    }
    let total = entries.len();
    let total_u64 = u64::try_from(total).unwrap_or(0);
    let _ = sink.send(entries).await;
    Ok(ListSummary { entries: total_u64, skipped: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_remote_provider_handles_and_mock_listing() {
        set_remote_test_mode(true);

        let sftp_provider = RemoteVfsBackend::sftp();
        let smb_provider = RemoteVfsBackend::smb();

        let sftp_uri = VfsUri::parse("sftp://user@host.internal/home/user").unwrap_or_else(|e| panic!("{e}"));
        let smb_uri = VfsUri::parse("smb://nas.local/public/docs").unwrap_or_else(|e| panic!("{e}"));
        let file_uri = VfsUri::parse("/local/path").unwrap_or_else(|e| panic!("{e}"));

        assert!(sftp_provider.handles(&sftp_uri));
        assert!(!sftp_provider.handles(&smb_uri));
        assert!(!sftp_provider.handles(&file_uri));

        assert!(smb_provider.handles(&smb_uri));
        assert!(!smb_provider.handles(&sftp_uri));

        // Test mock directory listing
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let summary = sftp_provider
            .list_dir(&sftp_uri, ListOptions::default(), tx, CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(summary.entries, 4);
        let batch = rx.recv().await.unwrap_or_else(|| panic!("expected batch"));
        assert_eq!(batch.len(), 4);
        assert_eq!(batch[0].name, "project_remote");

        // Test stat
        let stat_res = sftp_provider.stat(&sftp_uri.join("readme.txt").unwrap_or_else(|e| panic!("{e}"))).await;
        assert!(stat_res.is_ok());

        set_remote_test_mode(false);
    }
}
