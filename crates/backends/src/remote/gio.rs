//! Owns a GIO child from spawn through exit, cancellation, or timeout.

use std::ffi::OsStr;
use std::future::pending;
use std::io;
use std::process::{Output, Stdio};
use std::time::Duration;

use core_vfs::{VfsError, VfsResult};
use tokio::io::{AsyncReadExt, sink};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

const STDERR_LIMIT: usize = 64 << 10;

pub(super) enum Capture {
    Standard,
    Listing,
    Discard,
}

pub(super) async fn run(
    program: &OsStr,
    args: &[&str],
    timeout: Option<Duration>,
    cancel: Option<&CancellationToken>,
    capture: Capture,
) -> VfsResult<Output> {
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err(VfsError::Cancelled);
    }

    let stdout_limit = match capture {
        Capture::Standard => 4 << 20,
        Capture::Listing => 64 << 20,
        Capture::Discard => 0,
    };
    let stdio = || if stdout_limit == 0 { Stdio::null() } else { Stdio::piped() };
    let operation = args.first().copied().unwrap_or_default();
    let transport = |e: io::Error| VfsError::Transport(format!("gio {operation}: {e}"));
    // Always use the system GVfs and its stable output format.
    let mut child = Command::new(program)
        .env_remove("GIO_USE_VFS")
        .env("LC_ALL", "C")
        .args(args)
        .stdin(Stdio::null())
        .stdout(stdio())
        .stderr(stdio())
        .kill_on_drop(true)
        .spawn()
        .map_err(transport)?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let collect = async {
        let stdout = async {
            let mut bytes = Vec::new();
            if let Some(pipe) = stdout {
                let read = pipe.take(stdout_limit as u64 + 1).read_to_end(&mut bytes).await;
                if bytes.len() > stdout_limit || read.as_ref().is_err_and(|e| e.kind() == io::ErrorKind::OutOfMemory) {
                    return Err(VfsError::Transport(format!(
                        "gio {operation}: the server's answer is too large (over {} MiB)",
                        stdout_limit >> 20
                    )));
                }
                read.map_err(transport)?;
            }
            Ok(bytes)
        };
        let stderr = async {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = stderr {
                (&mut pipe).take(STDERR_LIMIT as u64).read_to_end(&mut bytes).await.map_err(transport)?;
                // Keep draining after the captured prefix so diagnostics cannot stall or
                // change the command's exit status.
                tokio::io::copy(&mut pipe, &mut sink()).await.map_err(transport)?;
            }
            Ok(bytes)
        };
        let wait = async { child.wait().await.map_err(transport) };
        let (status, stdout, stderr) = tokio::try_join!(wait, stdout, stderr)?;
        Ok(Output { status, stdout, stderr })
    };
    let cancelled = async {
        match cancel {
            Some(token) => token.cancelled().await,
            None => pending().await,
        }
    };
    let deadline = async {
        match timeout {
            Some(duration) => tokio::time::sleep(duration).await,
            None => pending().await,
        }
    };
    let result = tokio::select! {
        biased;
        // Once the outcome is available, report it even if cancellation arrived
        // at the same time. A finished move may already have removed its source.
        result = collect => result,
        () = cancelled => Err(VfsError::Cancelled),
        () = deadline => Err(VfsError::Timeout),
    };
    if result.is_err() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    result
}
