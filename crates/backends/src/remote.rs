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

/// Creates a `gio` Command that clears `GIO_USE_VFS` so child CLI calls talk to
/// system GVfs (network:///, sftp://, smb://) rather than inheriting GungnirVfs.
fn gio_cmd() -> Command {
    let mut cmd = Command::new("gio");
    cmd.env_remove("GIO_USE_VFS").env("LC_ALL", "C");
    cmd
}

async fn remove_with_program(program: &std::ffi::OsStr, uris: &[VfsUri], cancel: CancellationToken) -> VfsResult<()> {
    if cancel.is_cancelled() {
        return Err(VfsError::Cancelled);
    }
    for uri in uris {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        let mut child = tokio::process::Command::new(program)
            .env_remove("GIO_USE_VFS")
            .env("LC_ALL", "C")
            .args(["remove", "--", &uri.to_uri_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| VfsError::Transport(e.to_string()))?;
        let status = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(VfsError::Cancelled);
            }
            status = child.wait() => status.map_err(|e| VfsError::Transport(e.to_string()))?,
        };
        if !status.success() {
            return Err(VfsError::Transport(format!("gio remove failed for {}: {status}", uri.display())));
        }
    }
    Ok(())
}

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

    #[must_use]
    pub fn network() -> Self {
        Self { kind: BackendKind::Network }
    }
}

#[async_trait]
impl VfsProvider for RemoteVfsBackend {
    fn kind(&self) -> BackendKind {
        self.kind
    }

    fn capabilities(&self) -> VfsCapabilities {
        VfsCapabilities {
            writable: self.kind != BackendKind::Network,
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
            BackendKind::Network => uri.scheme() == Scheme::Network,
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
            let output = gio_cmd()
                .args(["list", "-l", "--", &uri_str])
                .output()
                .map_err(|e| VfsError::Transport(format!("gio list failed: {e}")))?;

            if !output.status.success() {
                let err_msg = String::from_utf8_lossy(&output.stderr);
                if target_dir.scheme() == Scheme::Network {
                    // When offline, no network daemon is active, or discovery returns empty/unsupported,
                    // return a clean empty list rather than failing the directory view with an error banner.
                    return Ok(ListSummary { entries: 0, skipped: 0 });
                }
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
                let chunk_size = if first { opts.first_batch.max(1) } else { opts.batch.max(1) };
                first = false;
                let end = (pos + chunk_size).min(entries.len());
                let mut batch = entries[pos..end].to_vec();
                pos = end;
                loop {
                    if cancel.is_cancelled() {
                        return Err(VfsError::Cancelled);
                    }
                    match sink.try_send(batch) {
                        Ok(()) => break,
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return Err(VfsError::Cancelled),
                        Err(tokio::sync::mpsc::error::TrySendError::Full(pending)) => {
                            batch = pending;
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                    }
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
        let name = if uri.scheme() == Scheme::Network && uri.is_root() {
            "Network".to_string()
        } else {
            uri.file_name().unwrap_or("").to_string()
        };

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
            let output = gio_cmd()
                .args(["info", "--", &uri_str])
                .output()
                .map_err(|e| VfsError::Transport(format!("gio info failed: {e}")))?;

            if !output.status.success() {
                if target_uri.scheme() == Scheme::Network && target_uri.is_root() {
                    return Ok(FileEntry {
                        name,
                        uri: target_uri,
                        kind: FileKind::Directory,
                        target_is_dir: true,
                        size: 0,
                        modified: None,
                        mode: 0o755,
                        vfs: VfsMetadata::default(),
                    });
                }
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
            let status =
                gio_cmd().args(["mkdir", "--", &uri_str]).status().map_err(|e| VfsError::Transport(e.to_string()))?;
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
            let status = gio_cmd()
                .args(["rename", "--", &uri_str, &new_name_owned])
                .status()
                .map_err(|e| VfsError::Transport(e.to_string()))?;
            if status.success() { Ok(new_uri) } else { Err(VfsError::Transport(format!("failed to rename {uri_str}"))) }
        })
        .await
        .map_err(|e| VfsError::Transport(e.to_string()))?
    }

    async fn delete(&self, uris: &[VfsUri], cancel: CancellationToken) -> VfsResult<()> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        if TEST_MODE.load(Ordering::Relaxed) {
            return Ok(());
        }
        remove_with_program(std::ffi::OsStr::new("gio"), uris, cancel).await
    }

    async fn transfer(&self, req: TransferRequest, ctl: TransferControl) -> VfsResult<()> {
        let dest_str = req.dest_dir.to_uri_string();
        let src_strings: Vec<String> = req.sources.iter().map(VfsUri::to_uri_string).collect();
        let is_move = matches!(req.kind, core_vfs::model::TransferKind::Move);

        if TEST_MODE.load(Ordering::Relaxed) {
            let _ = ctl
                .progress
                .send(TransferProgress {
                    id: req.id,
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
                let status = gio_cmd()
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
        if TEST_MODE.load(Ordering::Relaxed) || uri.scheme() == Scheme::Network {
            return Ok(VolumeInfo { total_bytes: 0, free_bytes: 0 });
        }

        tokio::task::spawn_blocking(move || {
            let output = gio_cmd().args(["info", "-f", "--", &uri_str]).output().ok();
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
        let is_network = uri.scheme() == Scheme::Network;
        if TEST_MODE.load(Ordering::Relaxed) {
            return Ok(vec![
                ("URI".to_string(), uri_str),
                ("Backend".to_string(), self.kind.badge().to_string()),
                ("Protocol".to_string(), uri.scheme().as_str().to_string()),
            ]);
        }

        tokio::task::spawn_blocking(move || {
            let output = gio_cmd()
                .args(["info", "--", &uri_str])
                .output()
                .map_err(|e| VfsError::Transport(format!("gio info failed: {e}")))?;

            if !output.status.success() {
                if is_network {
                    return Ok(vec![
                        ("URI".to_string(), uri_str),
                        ("Backend".to_string(), "Network Places (FreeDesktop/GVfs)".to_string()),
                        ("Status".to_string(), "Active / Ready".to_string()),
                    ]);
                }
                return Err(VfsError::Transport(format!("gio info failed for {uri_str}")));
            }

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
    let mock_names: &[&str] = if dir.scheme() == Scheme::Network {
        &["Windows Network", "Local Network"]
    } else {
        &["project_remote", "archive.tar.gz", "readme.txt", "video_stream.mp4"]
    };
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
        let network_provider = RemoteVfsBackend::network();

        let sftp_uri = VfsUri::parse("sftp://user@host.internal/home/user").unwrap_or_else(|e| panic!("{e}"));
        let smb_uri = VfsUri::parse("smb://nas.local/public/docs").unwrap_or_else(|e| panic!("{e}"));
        let network_uri = VfsUri::parse("network:///").unwrap_or_else(|e| panic!("{e}"));
        let file_uri = VfsUri::parse("/local/path").unwrap_or_else(|e| panic!("{e}"));

        assert!(sftp_provider.handles(&sftp_uri));
        assert!(!sftp_provider.handles(&smb_uri));
        assert!(!sftp_provider.handles(&file_uri));
        assert!(!sftp_provider.handles(&network_uri));

        assert!(smb_provider.handles(&smb_uri));
        assert!(!smb_provider.handles(&sftp_uri));
        assert!(!smb_provider.handles(&network_uri));

        assert!(network_provider.handles(&network_uri));
        assert!(!network_provider.handles(&sftp_uri));
        assert!(!network_provider.handles(&smb_uri));
        assert!(!network_provider.handles(&file_uri));

        // Test mock directory listing for SFTP
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let summary = sftp_provider
            .list_dir(&sftp_uri, ListOptions::default(), tx, CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(summary.entries, 4);
        let batch = rx.recv().await.unwrap_or_else(|| panic!("expected batch"));
        assert_eq!(batch.len(), 4);
        assert_eq!(batch[0].name, "project_remote");

        // Test mock directory listing for Network
        let (ntx, mut nrx) = tokio::sync::mpsc::channel(8);
        let n_summary = network_provider
            .list_dir(&network_uri, ListOptions::default(), ntx, CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(n_summary.entries, 2);
        let n_batch = nrx.recv().await.unwrap_or_else(|| panic!("expected network batch"));
        assert_eq!(n_batch.len(), 2);
        assert_eq!(n_batch[0].name, "Windows Network");
        assert_eq!(n_batch[0].kind, FileKind::Directory);

        // Test stat
        let stat_res = sftp_provider.stat(&sftp_uri.join("readme.txt").unwrap_or_else(|e| panic!("{e}"))).await;
        assert!(stat_res.is_ok());

        set_remote_test_mode(false);
    }
}

#[cfg(test)]
mod removal_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new(body: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.bug-check/remote-command-tests")
                .join(format!("{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&path).unwrap_or_else(|e| panic!("{e}"));
            let program = path.join("gio-test");
            std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap_or_else(|e| panic!("{e}"));
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
                .unwrap_or_else(|e| panic!("{e}"));
            Self(program)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Some(path) = self.0.parent() {
                let _ = std::fs::remove_dir_all(path);
            }
        }
    }
    fn remote_uri() -> VfsUri {
        VfsUri::parse("sftp://example.invalid/file").unwrap_or_else(|e| panic!("{e}"))
    }
    #[tokio::test]
    async fn failed_remote_remove_is_an_error() {
        let fixture = Fixture::new("exit 7");
        assert!(remove_with_program(fixture.0.as_os_str(), &[remote_uri()], CancellationToken::new()).await.is_err());
    }
    #[tokio::test]
    async fn missing_remote_remove_program_is_an_error() {
        let fixture = Fixture::new("exit 0");
        assert!(
            remove_with_program(
                fixture.0.with_file_name("missing").as_os_str(),
                &[remote_uri()],
                CancellationToken::new()
            )
            .await
            .is_err()
        );
    }
    #[tokio::test]
    async fn cancelled_remote_remove_does_not_start_a_process() {
        let fixture = Fixture::new("printf 'started' > \"$0.started\"\nexit 0");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = remove_with_program(fixture.0.as_os_str(), &[remote_uri()], cancel).await;
        assert_eq!(result, Err(VfsError::Cancelled));
        assert!(!fixture.0.with_file_name("gio-test.started").exists());
    }
    #[tokio::test]
    async fn remote_remove_stops_at_first_failed_item() {
        let fixture = Fixture::new("printf '%s\\n' \"$3\" >> \"$0.calls\"\ncase \"$3\" in *two) exit 9;; esac\nexit 0");
        let uris: Vec<_> = ["one", "two", "three"]
            .into_iter()
            .map(|name| VfsUri::parse(&format!("sftp://example.invalid/{name}")).unwrap_or_else(|e| panic!("{e}")))
            .collect();
        assert!(remove_with_program(fixture.0.as_os_str(), &uris, CancellationToken::new()).await.is_err());
        let calls =
            std::fs::read_to_string(fixture.0.with_file_name("gio-test.calls")).unwrap_or_else(|e| panic!("{e}"));
        assert!(calls.contains("/one"));
        assert!(calls.contains("/two"));
        assert!(!calls.contains("/three"));
    }
    #[tokio::test]
    async fn cancelling_active_remote_remove_kills_and_reaps_the_client() {
        use std::time::Duration;
        let fixture = Fixture::new("printf '%s' \"$$\" > \"$0.pid\"\nexec sleep 30");
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let program = fixture.0.clone();
        let task = tokio::spawn(async move { remove_with_program(program.as_os_str(), &[remote_uri()], token).await });
        let pid_path = fixture.0.with_file_name("gio-test.pid");
        tokio::time::timeout(Duration::from_secs(2), async {
            while !pid_path.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        let pid = std::fs::read_to_string(&pid_path).unwrap_or_else(|e| panic!("{e}"));
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(result, Err(VfsError::Cancelled));
        #[cfg(target_os = "linux")]
        assert!(!std::path::Path::new("/proc").join(pid.trim()).exists(), "child must be reaped");
    }
}
