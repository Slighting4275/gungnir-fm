//! Interoperate with Nemo and other desktop file managers through the standard
//! x-special/gnome-copied-files clipboard target. Keep all subprocess I/O off the actor.

use core_vfs::VfsUri;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const MIME: &str = "x-special/gnome-copied-files";
pub type Files = (Vec<VfsUri>, bool);

pub fn available() -> bool {
    !cfg!(test) && (std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some())
}

fn encode(uris: &[VfsUri], cut: bool) -> String {
    let mut text = if cut { "cut\n" } else { "copy\n" }.to_owned();
    for uri in uris {
        text.push_str(&uri.to_uri_string());
        text.push('\n');
    }
    text
}

fn decode(text: &str) -> Option<Files> {
    let mut lines = text.lines();
    let cut = match lines.next()? {
        "cut" => true,
        "copy" => false,
        _ => return None,
    };
    let uris: Vec<_> = lines.filter(|line| !line.is_empty()).map(VfsUri::parse).collect::<Result<_, _>>().ok()?;
    if uris.is_empty() { None } else { Some((uris, cut)) }
}

pub enum Request {
    Write(Files),
    Read(VfsUri),
    Moved(Vec<VfsUri>),
}

pub fn spawn(tx: crate::controller::MsgTx) -> tokio::sync::mpsc::Sender<Request> {
    let (requests, mut rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        while let Some(request) = rx.recv().await {
            match request {
                Request::Write(files) => write(&files).await,
                Request::Moved(moved) => {
                    if let Ok(Some((mut pending, true))) = read().await {
                        pending.retain(|uri| !moved.contains(uri));
                        write(&(pending, true)).await;
                    }
                }
                Request::Read(dest) => {
                    let files = read().await;
                    if tx
                        .send(crate::controller::Msg::Ev(crate::controller::Event::ClipboardPaste { dest, files }))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    requests
}

pub async fn write(files: &Files) {
    if !available() {
        return;
    }
    let payload = encode(&files.0, files.1);
    for (tool, args) in [("wl-copy", vec!["--type", MIME]), ("xclip", vec!["-selection", "clipboard", "-t", MIME])] {
        if tool == "wl-copy" && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            continue;
        }
        let mut command = Command::new(tool);
        command.args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
        let Ok(mut child) = command.spawn() else { continue };
        let operation = async {
            let mut stdin = child.stdin.take()?;
            stdin.write_all(payload.as_bytes()).await.ok()?;
            drop(stdin);
            child.wait().await.ok().filter(|s| s.success())
        };
        if matches!(tokio::time::timeout(Duration::from_secs(2), operation).await, Ok(Some(_))) {
            return;
        }
    }
}

pub async fn read() -> Result<Option<Files>, ()> {
    if !available() {
        return Err(());
    }
    for (tool, args) in [
        ("wl-paste", vec!["--no-newline", "--type", MIME]),
        ("xclip", vec!["-selection", "clipboard", "-t", MIME, "-o"]),
    ] {
        if tool == "wl-paste" && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            continue;
        }
        let mut command = Command::new(tool);
        command.args(args).stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
        if let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(2), command.output()).await
            && output.status.success()
            && let Ok(text) = std::str::from_utf8(&output.stdout)
            && let Some(files) = decode(text)
        {
            return Ok(Some(files));
        }
    }
    // A valid clipboard with a different MIME type must suppress old internal
    // file selections, rather than pasting stale files after the user copied text.
    for (tool, args) in
        [("wl-paste", vec!["--list-types"]), ("xclip", vec!["-selection", "clipboard", "-t", "TARGETS", "-o"])]
    {
        if tool == "wl-paste" && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            continue;
        }
        let mut command = Command::new(tool);
        command.args(args).stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
        if let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(2), command.output()).await
            && output.status.success()
        {
            return Ok(None);
        }
    }
    Err(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn desktop_clipboard_round_trip_keeps_special_filenames() {
        let uris = vec![VfsUri::parse("/").unwrap().join("a b\n€%.txt").unwrap()];
        for cut in [false, true] {
            assert_eq!(decode(&encode(&uris, cut)), Some((uris.clone(), cut)));
        }
    }
    #[test]
    fn reject_text_or_partially_invalid_file_clipboards() {
        for text in ["", "hello", "copy\n", "cut\nfile:///ok\ninvalid", "delete\nfile:///ok"] {
            assert_eq!(decode(text), None);
        }
    }
}
