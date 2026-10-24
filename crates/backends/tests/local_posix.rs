//! LocalPosixBackend against the real filesystem.

mod common;

use std::time::Duration;

use backends::LocalPosixBackend;
use common::{TempDir, list};
use core_vfs::{
    CancellationToken, FileKind, ListOptions, TransferControl, TransferKind, TransferRequest, TransferState, VfsError,
    VfsProvider, VfsResult, WatchEvent,
};
use tokio::sync::{mpsc, watch};

fn write(p: &std::path::Path, n: usize) {
    std::fs::write(p, vec![b'x'; n]).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_10k_in_batches_with_hidden_files() -> VfsResult<()> {
    let t = TempDir::new("list");
    for i in 0..10_000 {
        write(&t.path().join(format!("f{i:05}.txt")), 1);
    }
    write(&t.path().join(".hidden"), 3);
    std::fs::create_dir(t.path().join("sub")).ok();
    std::os::unix::fs::symlink(t.path().join("sub"), t.path().join("link-to-sub")).ok();
    let b = LocalPosixBackend::new();
    let (sizes, all) = list(&b, &t.uri(""), ListOptions { first_batch: 200, batch: 1_000 }).await?;
    assert_eq!(all.len(), 10_003);
    assert_eq!(sizes[0], 200);
    assert!(sizes.iter().all(|&s| s <= 1_000));
    assert!(all.iter().any(|e| e.is_hidden() && e.size == 3));
    let link = all.iter().find(|e| e.name == "link-to-sub").unwrap_or_else(|| panic!("no link"));
    assert_eq!(link.kind, FileKind::Symlink);
    assert!(link.is_navigable());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_and_unreadable_dirs_error_cleanly() {
    let t = TempDir::new("err");
    let b = LocalPosixBackend::new();
    assert!(matches!(list(&b, &t.uri("nope"), ListOptions::default()).await, Err(VfsError::NotFound(_))));
    write(&t.path().join("file"), 1);
    assert!(matches!(list(&b, &t.uri("file"), ListOptions::default()).await, Err(VfsError::NotADirectory(_))));
}

#[tokio::test(flavor = "multi_thread")]
async fn mkdir_rename_delete() -> VfsResult<()> {
    let t = TempDir::new("ops");
    let b = LocalPosixBackend::new();
    b.create_dir(&t.uri("a")).await?;
    assert!(matches!(b.create_dir(&t.uri("a")).await, Err(VfsError::AlreadyExists(_))));
    write(&t.path().join("a/x"), 5);
    let renamed = b.rename(&t.uri("a"), "b").await?;
    assert_eq!(renamed, t.uri("b"));
    b.create_dir(&t.uri("c")).await?;
    assert!(matches!(b.rename(&t.uri("b"), "c").await, Err(VfsError::AlreadyExists(_))), "rename never clobbers");
    assert_eq!(b.stat(&t.uri("b/x")).await?.size, 5);
    b.delete(&[t.uri("b"), t.uri("c")], CancellationToken::new()).await?;
    assert!(!t.path().join("b").exists() && !t.path().join("c").exists());
    Ok(())
}

fn control() -> (TransferControl, mpsc::Receiver<core_vfs::TransferProgress>, watch::Sender<bool>, CancellationToken) {
    let (ptx, prx) = mpsc::channel(256);
    let (pause_tx, paused) = watch::channel(false);
    let cancel = CancellationToken::new();
    (TransferControl { progress: ptx, cancel: cancel.clone(), paused }, prx, pause_tx, cancel)
}

#[tokio::test(flavor = "multi_thread")]
async fn copy_tree_then_move_with_progress() -> VfsResult<()> {
    let t = TempDir::new("xfer");
    std::fs::create_dir_all(t.path().join("src/deep/er")).ok();
    write(&t.path().join("src/a.bin"), 3_000_000);
    write(&t.path().join("src/deep/er/b.bin"), 1_000);
    std::os::unix::fs::symlink("a.bin", t.path().join("src/ln")).ok();
    std::fs::create_dir(t.path().join("dst")).ok();
    let b = LocalPosixBackend::new();
    let (ctl, mut prx, _p, _c) = control();
    let req = TransferRequest {
        id: 7,
        kind: TransferKind::Copy,
        sources: vec![t.uri("src")],
        dest_dir: t.uri("dst"),
        overwrite: false,
    };
    b.transfer(req, ctl).await?;
    let mut last = None;
    while let Ok(p) = prx.try_recv() {
        last = Some(p);
    }
    let last = last.unwrap_or_else(|| panic!("no progress"));
    assert_eq!(last.state, TransferState::Done);
    assert_eq!(last.bytes_done, 3_001_000); // symlinks count as files, not bytes
    assert_eq!(std::fs::metadata(t.path().join("dst/src/deep/er/b.bin")).map(|m| m.len()).ok(), Some(1_000));
    assert_eq!(std::fs::read_link(t.path().join("dst/src/ln")).ok(), Some("a.bin".into()));

    // Copying into the same directory again picks a free name, never overwrites.
    let (ctl, _prx, _p, _c) = control();
    let req = TransferRequest {
        id: 8,
        kind: TransferKind::Copy,
        sources: vec![t.uri("src")],
        dest_dir: t.uri("dst"),
        overwrite: false,
    };
    b.transfer(req, ctl).await?;
    assert!(t.path().join("dst/src (copy)/a.bin").exists());

    let (ctl, _prx, _p, _c) = control();
    let req = TransferRequest {
        id: 9,
        kind: TransferKind::Move,
        sources: vec![t.uri("dst/src (copy)")],
        dest_dir: t.uri(""),
        overwrite: false,
    };
    b.transfer(req, ctl).await?;
    assert!(t.path().join("src (copy)/a.bin").exists() && !t.path().join("dst/src (copy)").exists());

    let (ctl, _prx, _p, _c) = control();
    let req = TransferRequest {
        id: 10,
        kind: TransferKind::Copy,
        sources: vec![t.uri("src")],
        dest_dir: t.uri("src/deep"),
        overwrite: false,
    };
    assert_eq!(b.transfer(req, ctl).await, Err(VfsError::WouldRecurse));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_then_cancel_leaves_no_partial_file() -> VfsResult<()> {
    let t = TempDir::new("cancel");
    write(&t.path().join("big.bin"), 64 << 20);
    std::fs::create_dir(t.path().join("dst")).ok();
    let b = LocalPosixBackend::new();
    let (ctl, mut prx, pause, cancel) = control();
    pause.send_replace(true);
    let req = TransferRequest {
        id: 1,
        kind: TransferKind::Copy,
        sources: vec![t.uri("big.bin")],
        dest_dir: t.uri("dst"),
        overwrite: false,
    };
    let task = tokio::spawn(async move { b.transfer(req, ctl).await });
    // Wait for the paused report, then cancel.
    let mut saw_paused = false;
    while let Ok(Some(p)) = tokio::time::timeout(Duration::from_secs(5), prx.recv()).await {
        if p.state == TransferState::Paused {
            saw_paused = true;
            break;
        }
    }
    assert!(saw_paused, "transfer reported Paused");
    cancel.cancel();
    let r = tokio::time::timeout(Duration::from_secs(5), task).await.unwrap_or_else(|_| panic!("cancel hung"));
    assert_eq!(r.unwrap_or_else(|e| panic!("{e}")), Err(VfsError::Cancelled));
    assert!(!t.path().join("dst/big.bin").exists(), "partial destination removed");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn trash_follows_the_freedesktop_spec() -> VfsResult<()> {
    let t = TempDir::new("trash");
    write(&t.path().join("doomed file.txt"), 4);
    // SAFETY: only this test in this binary reads XDG_DATA_HOME.
    unsafe { std::env::set_var("XDG_DATA_HOME", t.path().join("data")) };
    let b = LocalPosixBackend::new();
    b.trash(&[t.uri("doomed file.txt")]).await?;
    assert!(!t.path().join("doomed file.txt").exists());
    assert!(t.path().join("data/Trash/files/doomed file.txt").exists());
    let info = std::fs::read_to_string(t.path().join("data/Trash/info/doomed file.txt.trashinfo")).unwrap_or_default();
    assert!(info.starts_with("[Trash Info]\nPath="), "{info}");
    assert!(info.contains("doomed%20file.txt") && info.contains("DeletionDate="), "{info}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_reports_changes() -> VfsResult<()> {
    let t = TempDir::new("watch");
    let b = LocalPosixBackend::new();
    let mut w = b.watch(&t.uri(""))?.unwrap_or_else(|| panic!("local backend watches"));
    write(&t.path().join("new"), 1);
    let ev = tokio::time::timeout(Duration::from_secs(3), w.events.recv()).await.ok().flatten();
    assert_eq!(ev, Some(WatchEvent::Changed));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn volume_and_inspect() -> VfsResult<()> {
    let t = TempDir::new("vol");
    write(&t.path().join("f"), 9);
    let b = LocalPosixBackend::new();
    let v = b.volume(&t.uri("")).await?;
    assert!(v.total_bytes > 0 && v.free_bytes <= v.total_bytes);
    let info = b.inspect(&t.uri("f")).await?;
    assert!(info.iter().any(|(k, v)| k == "Size" && v == "9 bytes"));
    Ok(())
}
