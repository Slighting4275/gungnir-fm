//! Built-in remote filesystem backend supporting SFTP/SSH (`sftp://`, `ssh://`)
//! and SMB/CIFS (`smb://`, `cifs://`).
//!
//! Communicates with remote servers via GIO/GVFS (GNOME Virtual File System) subsystem,
//! streaming directory batches into [`core_vfs::EntrySink`].

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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
fn gio_cmd(program: &std::ffi::OsStr) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_remove("GIO_USE_VFS").env("LC_ALL", "C");
    cmd
}

/// How long one listing, info, mkdir or rename may take. GVfs waits on a hung SSH or SMB
/// server for minutes; the folder then fails with a timeout instead of loading forever.
const GIO_TIMEOUT: Duration = Duration::from_secs(20);

/// Runs `gio args` (blocking; call it on the blocking pool), killed and reaped once
/// `timeout` passes or `cancel` fires. Output is read on side threads so a full pipe
/// cannot stall it.
fn run_gio(
    gio: &std::ffi::OsStr,
    args: &[&str],
    timeout: Option<Duration>,
    cancel: Option<&CancellationToken>,
) -> VfsResult<Output> {
    run_gio_capped(gio, args, timeout, cancel, GIO_OUTPUT_MAX)
}

/// What one `gio` call may print before it is stopped: the whole output used to be read
/// into memory, and a hostile server streaming a listing that never ends filled it with
/// gigabytes before the time limit. Listings get [`GIO_LIST_OUTPUT_MAX`].
const GIO_OUTPUT_MAX: usize = 4 << 20;
/// A listing's bound (some 800,000 names), and the entries it may hold.
const GIO_LIST_OUTPUT_MAX: usize = 64 << 20;
const GIO_LIST_ENTRIES_MAX: usize = 500_000;
/// gio's error output, shown in messages.
const GIO_STDERR_MAX: usize = 64 << 10;

/// [`run_gio`] with `max_stdout` bytes of output at most: past it gio is killed and the
/// call fails ("too large").
fn run_gio_capped(
    gio: &std::ffi::OsStr,
    args: &[&str],
    timeout: Option<Duration>,
    cancel: Option<&CancellationToken>,
    max_stdout: usize,
) -> VfsResult<Output> {
    // Reads at most `max` bytes (then stops reading, which leaves gio blocked or failing
    // on a full pipe) and flags it in `over`.
    fn drain(
        pipe: Option<impl Read + Send + 'static>,
        max: usize,
        over: Arc<AtomicBool>,
    ) -> Option<std::thread::JoinHandle<Vec<u8>>> {
        pipe.map(|r| {
            std::thread::spawn(move || {
                let mut b = Vec::new();
                let read = r.take(max as u64 + 1).read_to_end(&mut b);
                // Out of memory before the bound counts as too large, not as gio failing.
                if b.len() > max || read.is_err_and(|e| e.kind() == std::io::ErrorKind::OutOfMemory) {
                    b.truncate(max);
                    over.store(true, Ordering::Release);
                }
                b
            })
        })
    }
    let what = args.first().copied().unwrap_or("");
    let mut child = gio_cmd(gio)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| VfsError::Transport(format!("gio {what} failed: {e}")))?;
    let over = Arc::new(AtomicBool::new(false));
    let (out, err) = (
        drain(child.stdout.take(), max_stdout, Arc::clone(&over)),
        drain(child.stderr.take(), GIO_STDERR_MAX, Arc::new(AtomicBool::new(false))),
    );
    let deadline = timeout.map(|t| Instant::now() + t);
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| VfsError::Transport(format!("gio {what}: {e}")))? {
            break status;
        }
        let stop = if over.load(Ordering::Acquire) {
            Some(VfsError::Transport(format!(
                "gio {what}: the server's answer is too large (over {} MiB)",
                max_stdout >> 20
            )))
        } else if cancel.is_some_and(CancellationToken::is_cancelled) {
            Some(VfsError::Cancelled)
        } else if deadline.is_some_and(|d| Instant::now() >= d) {
            Some(VfsError::Timeout)
        } else {
            None
        };
        if let Some(e) = stop {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let join = |t: Option<std::thread::JoinHandle<Vec<u8>>>| t.and_then(|t| t.join().ok()).unwrap_or_default();
    let (stdout, stderr) = (join(out), join(err));
    // gio may have finished on its own right after the bound was passed.
    if over.load(Ordering::Acquire) {
        return Err(VfsError::Transport(format!(
            "gio {what}: the server's answer is too large (over {} MiB)",
            max_stdout >> 20
        )));
    }
    Ok(Output { status, stdout, stderr })
}

/// One line of `gio list -l -u`: `<child URI>\t<size>\t(<type>)`. The URI is the
/// folder's child, escaped, so a name holding a newline or tab stays one entry (plain
/// `-l` printed names raw: `a\nb` became two entries, `c\td` an entry named `c`).
fn parse_list_line(line: &str) -> Option<(String, u64, bool)> {
    let mut parts = line.split('\t');
    let uri = parts.next()?;
    let name = core_vfs::uri::percent_decode(uri.rsplit('/').next()?).ok()?;
    // No NUL either: no file has one, and a server sending %00 made every local path
    // built from the name invalid.
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return None;
    }
    let size = parts.next().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
    let is_dir = parts.next().is_some_and(|t| t.contains("directory"));
    Some((name, size, is_dir))
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

#[derive(Debug, Clone)]
pub struct RemoteVfsBackend {
    kind: BackendKind,
    /// The `gio` program (tests put a fake one here).
    gio: std::ffi::OsString,
    /// Time limit of a listing, info, mkdir or rename.
    timeout: Duration,
}

impl RemoteVfsBackend {
    #[must_use]
    pub fn sftp() -> Self {
        Self { kind: BackendKind::Sftp, gio: "gio".into(), timeout: GIO_TIMEOUT }
    }

    #[must_use]
    pub fn smb() -> Self {
        Self { kind: BackendKind::Smb, gio: "gio".into(), timeout: GIO_TIMEOUT }
    }

    #[must_use]
    pub fn network() -> Self {
        Self { kind: BackendKind::Network, gio: "gio".into(), timeout: GIO_TIMEOUT }
    }

    /// This backend running another `gio` program; it never takes the hermetic test path.
    #[cfg(test)]
    fn with_gio(mut self, program: &std::path::Path, timeout: Duration) -> Self {
        self.gio = program.as_os_str().to_owned();
        self.timeout = timeout;
        self
    }

    /// The hermetic test path (canned answers, no process): test mode with the real gio.
    fn mock(&self) -> bool {
        self.gio == "gio" && TEST_MODE.load(Ordering::Relaxed)
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

        if self.mock() {
            return list_mock_remote(&target_dir, opts, sink, cancel).await;
        }

        let (gio, timeout) = (self.gio.clone(), self.timeout);
        tokio::task::spawn_blocking(move || {
            let args = ["list", "-l", "-u", "--", &uri_str];
            let output = match run_gio_capped(&gio, &args, Some(timeout), Some(&cancel), GIO_LIST_OUTPUT_MAX) {
                // Network discovery that never answers is shown like one that failed.
                Err(VfsError::Timeout) if target_dir.scheme() == Scheme::Network => {
                    return Ok(ListSummary { entries: 0, skipped: 0 });
                }
                r => r?,
            };

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
                let Some((name, size, is_dir)) = parse_list_line(trimmed) else {
                    continue;
                };
                if entries.len() >= GIO_LIST_ENTRIES_MAX {
                    return Err(VfsError::Transport(format!(
                        "{uri_str}: the server listed too large a folder (over {GIO_LIST_ENTRIES_MAX} entries)"
                    )));
                }
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

        if self.mock() {
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

        let (gio, timeout) = (self.gio.clone(), self.timeout);
        tokio::task::spawn_blocking(move || {
            let output = run_gio(&gio, &["info", "--", &uri_str], Some(timeout), None)?;

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
        if self.mock() {
            return Ok(());
        }
        let (gio, timeout) = (self.gio.clone(), self.timeout);
        tokio::task::spawn_blocking(move || {
            let status = run_gio(&gio, &["mkdir", "--", &uri_str], Some(timeout), None)?.status;
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

        if self.mock() {
            return Ok(new_uri);
        }

        let (gio, timeout) = (self.gio.clone(), self.timeout);
        tokio::task::spawn_blocking(move || {
            let status = run_gio(&gio, &["rename", "--", &uri_str, &new_name_owned], Some(timeout), None)?.status;
            if status.success() { Ok(new_uri) } else { Err(VfsError::Transport(format!("failed to rename {uri_str}"))) }
        })
        .await
        .map_err(|e| VfsError::Transport(e.to_string()))?
    }

    async fn delete(&self, uris: &[VfsUri], cancel: CancellationToken) -> VfsResult<()> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        if self.mock() {
            return Ok(());
        }
        remove_with_program(&self.gio, uris, cancel).await
    }

    async fn transfer(&self, req: TransferRequest, ctl: TransferControl) -> VfsResult<()> {
        let dest_str = req.dest_dir.to_uri_string();
        let src_strings: Vec<String> = req.sources.iter().map(VfsUri::to_uri_string).collect();
        let is_move = matches!(req.kind, core_vfs::model::TransferKind::Move);

        if self.mock() {
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

        let gio = self.gio.clone();
        tokio::task::spawn_blocking(move || {
            let cmd_name = if is_move { "move" } else { "copy" };
            for s in &src_strings {
                if ctl.cancel.is_cancelled() {
                    return Err(VfsError::Cancelled);
                }
                // No time limit (a big copy takes as long as it takes), but a cancel kills it.
                let status = run_gio(&gio, &[cmd_name, "--", s, &dest_str], None, Some(&ctl.cancel))?.status;
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
        if self.mock() || uri.scheme() == Scheme::Network {
            return Ok(VolumeInfo { total_bytes: 0, free_bytes: 0 });
        }

        let (gio, timeout) = (self.gio.clone(), self.timeout);
        tokio::task::spawn_blocking(move || {
            let output = run_gio(&gio, &["info", "-f", "--", &uri_str], Some(timeout), None).ok();
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
        if self.mock() {
            return Ok(vec![
                ("URI".to_string(), uri_str),
                ("Backend".to_string(), self.kind.badge().to_string()),
                ("Protocol".to_string(), uri.scheme().as_str().to_string()),
            ]);
        }

        let (gio, timeout) = (self.gio.clone(), self.timeout);
        tokio::task::spawn_blocking(move || {
            let output = run_gio(&gio, &["info", "--", &uri_str], Some(timeout), None)?;

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
            // The executable is written by a child `install`, never by this process: a
            // script this process wrote and then ran failed now and then with "Text file
            // busy", when another test thread forked while the write handle was open.
            let source = path.join("gio-test.src");
            std::fs::write(&source, format!("#!/bin/sh\n{body}\n")).unwrap_or_else(|e| panic!("{e}"));
            let st = std::process::Command::new("install")
                .args(["-m", "700"])
                .arg(&source)
                .arg(&program)
                .status()
                .unwrap_or_else(|e| panic!("install: {e}"));
            assert!(st.success(), "install: {st}");
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
        // The whole pid, not just the file: the shell creates it before writing to it,
        // and an empty pid made the reaped check look at /proc itself.
        let pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(p) = std::fs::read_to_string(&pid_path)
                    && p.trim().parse::<u32>().is_ok()
                {
                    return p;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(result, Err(VfsError::Cancelled));
        #[cfg(target_os = "linux")]
        assert!(!std::path::Path::new("/proc").join(pid.trim()).exists(), "child must be reaped");
    }

    /// A fake `gio` that records its pid next to itself, then never answers.
    const HANGS: &str = "echo $$ > \"$(dirname \"$0\")/pid\"; exec sleep 600";

    fn gone(fixture: &Fixture) -> bool {
        let pid = std::fs::read_to_string(fixture.0.with_file_name("pid")).unwrap_or_else(|e| panic!("{e}"));
        let pid: u32 = pid.trim().parse().unwrap_or_else(|e| panic!("no whole pid ({pid:?}): {e}"));
        !std::path::Path::new("/proc").join(pid.to_string()).exists()
    }

    fn list(backend: &RemoteVfsBackend, uri: &VfsUri, cancel: CancellationToken) -> VfsResult<Vec<FileEntry>> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap_or_else(|e| panic!("{e}"));
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        rt.block_on(async {
            backend.list_dir(uri, ListOptions::default(), tx, cancel).await?;
            let mut all = Vec::new();
            while let Ok(batch) = rx.try_recv() {
                all.extend(batch);
            }
            Ok(all)
        })
    }

    #[test]
    fn a_remote_listing_that_never_answers_times_out_and_kills_gio() {
        // gio list waited on a hung server for as long as GVfs did: the folder never loaded.
        let fixture = Fixture::new(HANGS);
        let backend = RemoteVfsBackend::sftp().with_gio(&fixture.0, Duration::from_millis(300));
        let t0 = Instant::now();
        let r =
            list(&backend, &VfsUri::parse("sftp://h/d").unwrap_or_else(|e| panic!("{e}")), CancellationToken::new());
        assert!(matches!(r, Err(VfsError::Timeout)), "{r:?}");
        assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
        assert!(gone(&fixture), "gio must be killed and reaped");
    }

    /// Names a hostile server may send: none with a NUL (no file can have one, and every
    /// local path built from it breaks), "/", "." or ".." gets in; ordinary odd names do.
    #[test]
    fn hostile_remote_names_never_become_entries() {
        let long = "x".repeat(100_000);
        let cases = [
            ("sftp://h/d/a%00b\t1\t(regular)", None),
            ("sftp://h/d/%00\t1\t(regular)", None),
            ("sftp://h/d/..%2F..%2Fetc\t1\t(regular)", None),
            ("sftp://h/d/%2E%2E\t1\t(directory)", None),
            ("sftp://h/d/.\t1\t(directory)", None),
            ("sftp://h/d/%2Fabs\t1\t(regular)", None),
            ("sftp://h/d/ok%20name\t3\t(regular)", Some("ok name")),
            ("sftp://h/d/new%0Aline\t3\t(regular)", Some("new\nline")),
        ];
        for (line, want) in cases {
            let got = parse_list_line(&line.replace("\\t", "\t")).map(|(n, _, _)| n);
            assert_eq!(got.as_deref(), want, "{line}");
        }
        let got = parse_list_line(&format!("sftp://h/d/{long}\t1\t(regular)")).map(|(n, _, _)| n.len());
        assert_eq!(got, Some(100_000), "a long name is a name (the folder decides what it holds)");
    }

    /// A hostile server streams a listing that never ends (gio prints it as fast as it
    /// arrives): the listing used to be read whole into memory first, gigabytes before
    /// the time limit. It now stops at a bound, kills gio and says why.
    #[test]
    fn an_endless_remote_listing_is_cut_off_not_buffered() {
        let fixture = Fixture::new(
            "echo $$ > \"$(dirname \"$0\")/pid\"; exec yes \"$(printf 'sftp://h/d/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\t1\\t(regular)')\"",
        );
        let backend = RemoteVfsBackend::sftp().with_gio(&fixture.0, Duration::from_secs(60));
        let t0 = Instant::now();
        let r =
            list(&backend, &VfsUri::parse("sftp://h/d").unwrap_or_else(|e| panic!("{e}")), CancellationToken::new());
        assert!(matches!(&r, Err(VfsError::Transport(m)) if m.contains("too large")), "{:?}", r.as_ref().map(Vec::len));
        assert!(t0.elapsed() < Duration::from_secs(20), "{:?}", t0.elapsed());
        assert!(gone(&fixture), "gio must be killed and reaped");
    }

    #[test]
    fn cancelling_a_remote_listing_kills_gio() {
        let fixture = Fixture::new(HANGS);
        let backend = RemoteVfsBackend::sftp().with_gio(&fixture.0, Duration::from_secs(60));
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            c.cancel();
        });
        let t0 = Instant::now();
        let r = list(&backend, &VfsUri::parse("sftp://h/d").unwrap_or_else(|e| panic!("{e}")), cancel);
        assert!(matches!(r, Err(VfsError::Cancelled)), "{r:?}");
        assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
        assert!(gone(&fixture), "gio must be killed and reaped");
    }

    #[test]
    fn remote_info_mkdir_and_rename_time_out() {
        let fixture = Fixture::new(HANGS);
        let backend = RemoteVfsBackend::sftp().with_gio(&fixture.0, Duration::from_millis(300));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap_or_else(|e| panic!("{e}"));
        let u = VfsUri::parse("sftp://h/d/x").unwrap_or_else(|e| panic!("{e}"));
        let r = rt.block_on(backend.stat(&u));
        assert!(matches!(r, Err(VfsError::Timeout)), "{r:?}");
        assert!(matches!(rt.block_on(backend.create_dir(&u)), Err(VfsError::Timeout)));
        assert!(matches!(rt.block_on(backend.rename(&u, "y")), Err(VfsError::Timeout)));
        assert!(gone(&fixture));
    }

    #[test]
    fn cancelling_a_remote_copy_kills_gio() {
        // Cancel was only checked between items: one big file ran to the end regardless.
        let fixture = Fixture::new(HANGS);
        let backend = RemoteVfsBackend::sftp().with_gio(&fixture.0, Duration::from_millis(100));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap_or_else(|e| panic!("{e}"));
        let (progress, _rx) = tokio::sync::mpsc::channel(8);
        let (_ptx, paused) = tokio::sync::watch::channel(false);
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500)); // well past the 100 ms listing limit
            c.cancel();
        });
        let req = TransferRequest {
            id: 1,
            kind: core_vfs::model::TransferKind::Copy,
            sources: vec![VfsUri::parse("sftp://h/d/big").unwrap_or_else(|e| panic!("{e}"))],
            dest_dir: VfsUri::parse("sftp://h/e").unwrap_or_else(|e| panic!("{e}")),
            overwrite: false,
        };
        let t0 = Instant::now();
        let r = rt.block_on(backend.transfer(req, TransferControl { progress, cancel, paused }));
        assert!(matches!(r, Err(VfsError::Cancelled)), "{r:?}");
        assert!(t0.elapsed() >= Duration::from_millis(400), "a copy has no time limit: {:?}", t0.elapsed());
        assert!(gone(&fixture), "gio must be killed and reaped");
    }

    #[test]
    fn remote_names_with_a_newline_or_tab_stay_one_entry() {
        // `gio list -l` printed names raw: "a\nb" listed as two entries, "c\td" as "c".
        let fixture = Fixture::new(
            "case \"$*\" in *' -u '*) ;; *) exit 9 ;; esac\n\
             printf 'sftp://h/d/a%%0Ab\\t3\\t(regular)\\nsftp://h/d/c%%09d\\t5\\t(regular)\\n'\n\
             printf 'sftp://h/d/sub\\t0\\t(directory)\\nsftp://h/d/50%%25\\t1\\t(regular)\\n'",
        );
        let backend = RemoteVfsBackend::sftp().with_gio(&fixture.0, Duration::from_secs(10));
        let got =
            list(&backend, &VfsUri::parse("sftp://h/d").unwrap_or_else(|e| panic!("{e}")), CancellationToken::new())
                .unwrap_or_else(|e| panic!("{e}"));
        let names: Vec<(&str, u64, FileKind)> = got.iter().map(|e| (e.name.as_str(), e.size, e.kind)).collect();
        assert_eq!(
            names,
            [
                ("a\nb", 3, FileKind::File),
                ("c\td", 5, FileKind::File),
                ("sub", 0, FileKind::Directory),
                ("50%", 1, FileKind::File)
            ]
        );
    }
}
