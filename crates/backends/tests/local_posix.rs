//! LocalPosixBackend against the real filesystem.

mod common;

use std::time::Duration;

use backends::LocalPosixBackend;
use common::{TempDir, list};
use core_vfs::{
    CancellationToken, FileKind, ListOptions, TransferControl, TransferKind, TransferRequest, TransferState, VfsError,
    VfsProvider, VfsResult, VfsUri, WatchEvent,
};
use tokio::sync::{mpsc, watch};

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    let _env_guard = ENV_LOCK.lock().await;
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

    // Test restore
    let trash_uri = VfsUri::trash("doomed file.txt")?;
    b.restore(&[trash_uri]).await?;
    assert!(t.path().join("doomed file.txt").exists());
    assert!(!t.path().join("data/Trash/files/doomed file.txt").exists());
    assert!(!t.path().join("data/Trash/info/doomed file.txt.trashinfo").exists());

    // Test empty trash
    b.trash(&[t.uri("doomed file.txt")]).await?;
    assert!(t.path().join("data/Trash/files/doomed file.txt").exists());
    b.empty_trash().await?;
    assert!(!t.path().join("data/Trash/files/doomed file.txt").exists());
    assert!(!t.path().join("data/Trash/info/doomed file.txt.trashinfo").exists());

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn recent_listing_stat_delete_and_clear() -> VfsResult<()> {
    let _env_guard = ENV_LOCK.lock().await;
    let t = TempDir::new("recent");
    let prev_xdg = std::env::var_os("XDG_DATA_HOME");
    let data_dir = t.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap_or_else(|e| panic!("{e}"));
    // SAFETY: test sets isolated env var for the test process
    unsafe {
        std::env::set_var("XDG_DATA_HOME", &data_dir);
    }

    let f = t.path().join("my-doc.txt");
    write(&f, 10);
    let xbel_content = format!(
        "<?xml version=\"1.0\"?>\n<xbel version=\"1.0\">\n\
         <bookmark href=\"file://{}\" added=\"x\"/>\n</xbel>\n",
        f.display()
    );
    std::fs::write(data_dir.join("recently-used.xbel"), xbel_content).unwrap_or_else(|e| panic!("{e}"));

    let b = LocalPosixBackend::new();
    let recent_root = VfsUri::recent("")?;
    let (_, entries) = list(&b, &recent_root, ListOptions::default()).await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "my-doc.txt");
    assert_eq!(entries[0].size, 10);

    let stat_entry = b.stat(&entries[0].uri).await?;
    assert_eq!(stat_entry.name, "my-doc.txt");

    // Remove entry
    b.delete(&[entries[0].uri.clone()], CancellationToken::new()).await?;
    let (_, left) = list(&b, &recent_root, ListOptions::default()).await?;
    assert!(left.is_empty());
    assert!(f.exists(), "original file must not be touched");

    // Re-add and clear
    let xbel_content2 = format!(
        "<?xml version=\"1.0\"?>\n<xbel version=\"1.0\">\n\
         <bookmark href=\"file://{}\" added=\"x\"/>\n</xbel>\n",
        f.display()
    );
    std::fs::write(data_dir.join("recently-used.xbel"), xbel_content2).unwrap_or_else(|e| panic!("{e}"));
    let (_, entries2) = list(&b, &recent_root, ListOptions::default()).await?;
    assert_eq!(entries2.len(), 1);

    b.clear_recent().await?;
    let (_, empty_entries) = list(&b, &recent_root, ListOptions::default()).await?;
    assert!(empty_entries.is_empty());
    assert!(f.exists());

    // Restore env
    unsafe {
        if let Some(prev) = prev_xdg {
            std::env::set_var("XDG_DATA_HOME", prev);
        } else {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }
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
async fn create_symlink_creates_valid_link() -> VfsResult<()> {
    let t = TempDir::new("symlink-test");
    let target = t.path().join("target.txt");
    write(&target, 12);
    let link_uri = t.uri("link-to-target.txt");
    let b = LocalPosixBackend::new();
    b.create_symlink(&link_uri, "target.txt").await?;
    let link_path = t.path().join("link-to-target.txt");
    assert!(link_path.is_symlink());
    let read_target = std::fs::read_link(&link_path)
        .map_err(|e| core_vfs::VfsError::from_io("readlink", &e, "link-to-target.txt"))?;
    assert_eq!(read_target, std::path::Path::new("target.txt"));
    let stat_entry = b.stat(&link_uri).await?;
    assert_eq!(stat_entry.name, "link-to-target.txt");
    assert_eq!(stat_entry.kind, core_vfs::FileKind::Symlink);
    assert_eq!(stat_entry.size, 10);
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

#[tokio::test]
async fn overwrite_failure_preserves_existing_destination() -> VfsResult<()> {
    let t = TempDir::new("overwrite-failure");
    std::fs::create_dir_all(t.path().join("src/item")).unwrap_or_else(|e| panic!("{e}"));
    std::fs::create_dir_all(t.path().join("dst/item")).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(t.path().join("dst/item/keep"), b"original").unwrap_or_else(|e| panic!("{e}"));
    let _socket =
        std::os::unix::net::UnixListener::bind(t.path().join("src/item/socket")).unwrap_or_else(|e| panic!("{e}"));
    let (ctl, _rx, _pause, _cancel) = control();
    let result = LocalPosixBackend::new()
        .transfer(
            TransferRequest {
                id: 21,
                kind: TransferKind::Copy,
                sources: vec![t.uri("src/item")],
                dest_dir: t.uri("dst"),
                overwrite: true,
            },
            ctl,
        )
        .await;
    assert!(result.is_err());
    assert_eq!(std::fs::read(t.path().join("dst/item/keep")).ok().as_deref(), Some(b"original".as_slice()));
    Ok(())
}

#[tokio::test]
async fn transfer_rejects_destination_alias_inside_source() -> VfsResult<()> {
    let t = TempDir::new("transfer-alias");
    std::fs::create_dir_all(t.path().join("src/inner")).unwrap_or_else(|e| panic!("{e}"));
    std::os::unix::fs::symlink(t.path().join("src/inner"), t.path().join("alias")).unwrap_or_else(|e| panic!("{e}"));
    let (ctl, _rx, _pause, _cancel) = control();
    // Cancel as soon as progress starts if a broken implementation attempts recursion.
    let cancel = ctl.cancel.clone();
    let (ptx, mut prx) = mpsc::channel(4);
    let ctl = TransferControl { progress: ptx, ..ctl };
    let task = tokio::spawn(async move {
        if prx.recv().await.is_some() {
            cancel.cancel();
        }
    });
    let result = LocalPosixBackend::new()
        .transfer(
            TransferRequest {
                id: 22,
                kind: TransferKind::Copy,
                sources: vec![t.uri("src")],
                dest_dir: t.uri("alias"),
                overwrite: false,
            },
            ctl,
        )
        .await;
    task.abort();
    assert_eq!(result, Err(VfsError::WouldRecurse));
    Ok(())
}

#[tokio::test]
async fn deleting_ordinary_files_directory_keeps_unrelated_info() -> VfsResult<()> {
    let t = TempDir::new("delete-info");
    std::fs::create_dir_all(t.path().join("files")).unwrap_or_else(|e| panic!("{e}"));
    std::fs::create_dir_all(t.path().join("info")).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(t.path().join("files/item"), b"delete").unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(t.path().join("info/item.trashinfo"), b"keep").unwrap_or_else(|e| panic!("{e}"));
    LocalPosixBackend::new().delete(&[t.uri("files/item")], CancellationToken::new()).await?;
    assert!(t.path().join("info/item.trashinfo").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_listing_with_full_receiver_stops_all_workers() -> VfsResult<()> {
    let t = TempDir::new("blocked-listing");
    for i in 0..100 {
        write(&t.path().join(format!("item{i}")), 1);
    }
    let (tx, _rx) = mpsc::channel(1);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let dir = t.uri("");
    let task = tokio::spawn(async move {
        LocalPosixBackend::new().list_dir(&dir, ListOptions { first_batch: 1, batch: 1 }, tx, token).await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap_or_else(|_| panic!("listing stayed blocked after cancellation"))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(result, Err(VfsError::Cancelled));
    Ok(())
}

#[tokio::test]
async fn overwrite_success_replaces_complete_tree_and_preserves_source() -> VfsResult<()> {
    let t = TempDir::new("overwrite-success");
    for dir in ["src/item", "dst/item"] {
        std::fs::create_dir_all(t.path().join(dir)).unwrap_or_else(|e| panic!("{e}"));
    }
    std::fs::write(t.path().join("src/item/new"), b"new").unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(t.path().join("dst/item/old"), b"old").unwrap_or_else(|e| panic!("{e}"));
    let (ctl, _rx, _pause, _cancel) = control();
    LocalPosixBackend::new()
        .transfer(
            TransferRequest {
                id: 23,
                kind: TransferKind::Copy,
                sources: vec![t.uri("src/item")],
                dest_dir: t.uri("dst"),
                overwrite: true,
            },
            ctl,
        )
        .await?;
    assert_eq!(std::fs::read(t.path().join("dst/item/new")).ok().as_deref(), Some(b"new".as_slice()));
    assert!(!t.path().join("dst/item/old").exists());
    assert!(t.path().join("src/item/new").exists());
    assert!(
        !std::fs::read_dir(t.path().join("dst"))
            .unwrap_or_else(|e| panic!("{e}"))
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with(".gungnir-transfer-"))
    );
    Ok(())
}

#[tokio::test]
async fn listing_never_aliases_non_utf8_names_to_other_files() -> VfsResult<()> {
    use std::os::unix::ffi::OsStrExt;
    let t = TempDir::new("invalid-utf8");
    std::fs::write(t.path().join(std::ffi::OsStr::from_bytes(b"raw-\xff")), b"invalid")
        .unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(t.path().join("raw-�"), b"valid").unwrap_or_else(|e| panic!("{e}"));
    let (_, entries) = list(&LocalPosixBackend::new(), &t.uri(""), ListOptions::default()).await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "raw-�");
    assert_eq!(entries[0].size, 5);
    Ok(())
}

#[tokio::test]
async fn empty_file_creation_never_overwrites_existing_entries() -> VfsResult<()> {
    let t = TempDir::new("create-file");
    let b = LocalPosixBackend::new();
    b.create_file(&t.uri("empty.txt")).await?;
    assert_eq!(std::fs::read(t.path().join("empty.txt")).ok(), Some(Vec::new()));
    std::fs::write(t.path().join("empty.txt"), b"keep").unwrap_or_else(|e| panic!("{e}"));
    assert!(matches!(b.create_file(&t.uri("empty.txt")).await, Err(VfsError::AlreadyExists(_))));
    assert_eq!(std::fs::read(t.path().join("empty.txt")).ok().as_deref(), Some(b"keep".as_slice()));
    std::os::unix::fs::symlink("empty.txt", t.path().join("link")).unwrap_or_else(|e| panic!("{e}"));
    assert!(matches!(b.create_file(&t.uri("link")).await, Err(VfsError::AlreadyExists(_))));
    Ok(())
}

#[tokio::test]
async fn template_copy_preserves_binary_bytes_and_publishes_only_the_document() -> VfsResult<()> {
    let t = TempDir::new("template-bytes");
    let bytes: Vec<u8> = (0..1_100_000).map(|i| (i % 251) as u8).collect();
    std::fs::write(t.path().join("template.odt"), &bytes).unwrap_or_else(|e| panic!("{e}"));
    std::fs::create_dir(t.path().join("out")).unwrap_or_else(|e| panic!("{e}"));
    LocalPosixBackend::new().create_from_template(&t.uri("template.odt"), &t.uri("out/Letter.odt")).await?;
    assert_eq!(std::fs::read(t.path().join("out/Letter.odt")).unwrap_or_default(), bytes);
    assert_eq!(std::fs::read(t.path().join("template.odt")).unwrap_or_default(), bytes);
    assert_eq!(std::fs::read_dir(t.path().join("out")).unwrap_or_else(|e| panic!("{e}")).count(), 1);
    Ok(())
}

#[tokio::test]
async fn template_copy_refuses_existing_files_and_symlinks() -> VfsResult<()> {
    let t = TempDir::new("template-no-clobber");
    std::fs::write(t.path().join("template"), b"template").unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(t.path().join("existing"), b"keep").unwrap_or_else(|e| panic!("{e}"));
    std::os::unix::fs::symlink("existing", t.path().join("link")).unwrap_or_else(|e| panic!("{e}"));
    let backend = LocalPosixBackend::new();
    for name in ["existing", "link", "template"] {
        assert!(matches!(
            backend.create_from_template(&t.uri("template"), &t.uri(name)).await,
            Err(VfsError::AlreadyExists(_))
        ));
    }
    assert_eq!(std::fs::read(t.path().join("existing")).unwrap_or_default(), b"keep");
    assert!(t.path().join("link").is_symlink());
    Ok(())
}

#[tokio::test]
async fn template_copy_rejects_symlinks_directories_and_fifos_without_waiting() -> VfsResult<()> {
    use std::os::unix::ffi::OsStrExt;
    let t = TempDir::new("template-special");
    std::fs::write(t.path().join("target"), b"keep").unwrap_or_else(|e| panic!("{e}"));
    std::os::unix::fs::symlink("target", t.path().join("link")).unwrap_or_else(|e| panic!("{e}"));
    std::fs::create_dir(t.path().join("folder")).unwrap_or_else(|e| panic!("{e}"));
    let fifo = std::ffi::CString::new(t.path().join("fifo").as_os_str().as_bytes()).unwrap_or_else(|e| panic!("{e}"));
    // SAFETY: fifo is a valid NUL-terminated path in this private test directory.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    for name in ["link", "folder", "fifo"] {
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            LocalPosixBackend::new().create_from_template(&t.uri(name), &t.uri("created")),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(result.is_err());
        assert!(!t.path().join("created").exists());
    }
    assert_eq!(std::fs::read_dir(t.path()).unwrap_or_else(|e| panic!("{e}")).count(), 4);
    Ok(())
}

#[tokio::test]
async fn private_template_documents_keep_private_permissions_without_executable_bits() -> VfsResult<()> {
    use std::os::unix::fs::PermissionsExt;
    let t = TempDir::new("template-permissions");
    let source = t.path().join("private-template");
    std::fs::write(&source, b"private fixture").unwrap_or_else(|e| panic!("{e}"));
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o4700)).unwrap_or_else(|e| panic!("{e}"));
    LocalPosixBackend::new().create_from_template(&t.uri("private-template"), &t.uri("document")).await?;
    let mode =
        std::fs::metadata(t.path().join("document")).unwrap_or_else(|e| panic!("{e}")).permissions().mode() & 0o7777;
    assert_eq!(mode, 0o600, "a private template must not become readable by other users or executable");
    assert_eq!(std::fs::read(t.path().join("document")).unwrap_or_default(), b"private fixture");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn replacing_a_folder_never_destroys_the_source_inside_it() -> VfsResult<()> {
    // Copying x/A/A into x with Replace swapped x/A out and deleted it, and the source
    // inside it went too: a copy that destroyed what it copied.
    let t = TempDir::new("replace-ancestor");
    std::fs::create_dir_all(t.path().join("A/A")).ok();
    write(&t.path().join("A/A/inner.txt"), 5);
    write(&t.path().join("A/sibling.txt"), 7);
    let b = LocalPosixBackend::new();
    for kind in [TransferKind::Copy, TransferKind::Move] {
        let (ctl, _prx, _p, _c) = control();
        let req = TransferRequest { id: 1, kind, sources: vec![t.uri("A/A")], dest_dir: t.uri(""), overwrite: true };
        assert_eq!(b.transfer(req, ctl).await, Err(VfsError::WouldRecurse), "{kind:?}");
        assert_eq!(std::fs::read(t.path().join("A/A/inner.txt")).ok(), Some(b"xxxxx".to_vec()), "{kind:?}");
        assert!(t.path().join("A/sibling.txt").exists(), "{kind:?}");
    }
    Ok(())
}

/// A chain of `depth` directories named `d` below `root`, made with directory fds (the
/// full path can be longer than one path call allows).
fn deep_chain(root: &std::path::Path, depth: usize) {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(root.as_os_str().as_bytes()).unwrap_or_else(|e| panic!("{e}"));
    // SAFETY: plain directory syscalls on fds this function owns and closes.
    unsafe {
        let mut fd = libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY);
        assert!(fd >= 0);
        for _ in 0..depth {
            assert_eq!(libc::mkdirat(fd, c"d".as_ptr(), 0o755), 0);
            let next = libc::openat(fd, c"d".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY);
            libc::close(fd);
            assert!(next >= 0);
            fd = next;
        }
        libc::close(fd);
    }
}

/// How many directories named `d` are chained below `root` (walked with fds).
fn chain_depth(root: &std::path::Path) -> usize {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(root.as_os_str().as_bytes()).unwrap_or_else(|e| panic!("{e}"));
    // SAFETY: plain directory syscalls on fds this function owns and closes.
    unsafe {
        let mut fd = libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY);
        let mut n = 0;
        while fd >= 0 {
            let next = libc::openat(fd, c"d".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY);
            libc::close(fd);
            if next >= 0 {
                n += 1;
            }
            fd = next;
        }
        n
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn deep_folders_are_copied_and_deleted_without_crashing() -> VfsResult<()> {
    // 2,020 levels exceed the 1,024 descriptors used by the low-limit test job.
    let t = TempDir::new("deep-copy");
    std::fs::create_dir_all(t.path().join("src")).ok();
    std::fs::create_dir_all(t.path().join("dst")).ok();
    deep_chain(&t.path().join("src"), 2020);
    let outside = t.path().join("outside");
    std::fs::create_dir(&outside).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(outside.join("keep"), b"keep").unwrap_or_else(|e| panic!("{e}"));
    std::os::unix::fs::symlink(&outside, t.path().join("src/d/outside")).unwrap_or_else(|e| panic!("{e}"));
    let b = LocalPosixBackend::new();
    let (ctl, _prx, _p, _c) = control();
    let req = TransferRequest {
        id: 2,
        kind: TransferKind::Copy,
        sources: vec![t.uri("src")],
        dest_dir: t.uri("dst"),
        overwrite: false,
    };
    b.transfer(req, ctl).await?;
    assert_eq!(chain_depth(&t.path().join("dst/src")), 2020, "the whole chain is copied");
    b.delete(&[t.uri("src"), t.uri("dst/src")], CancellationToken::new()).await?;
    assert!(!t.path().join("src").exists() && !t.path().join("dst/src").exists());
    assert_eq!(std::fs::read(outside.join("keep")).unwrap_or_else(|e| panic!("{e}")), b"keep");
    Ok(())
}

#[tokio::test]
async fn deleting_a_tree_requires_search_but_not_read_permission_on_its_parent() -> VfsResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let t = TempDir::new("delete-search-only-parent");
    let parent = t.path().join("parent");
    let tree = parent.join("tree");
    std::fs::create_dir_all(&tree).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(tree.join("file"), b"remove this").unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(parent.join("sibling"), b"keep this").unwrap_or_else(|e| panic!("{e}"));
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o300)).unwrap_or_else(|e| panic!("{e}"));
    let result = LocalPosixBackend::new().delete(&[t.uri("parent/tree")], CancellationToken::new()).await;
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap_or_else(|e| panic!("{e}"));
    result?;
    assert!(!tree.exists());
    assert_eq!(std::fs::read(parent.join("sibling")).unwrap_or_else(|e| panic!("{e}")), b"keep this");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn sparse_files_stay_sparse_and_identical() -> VfsResult<()> {
    // Every byte was copied: a sparse 2 GiB image with nothing allocated became 2 GiB of
    // zeros on disk (a 100 GB VM disk could fill the drive).
    use std::io::{Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt;
    let t = TempDir::new("sparse");
    let src = t.path().join("disk.img");
    let mut f = std::fs::File::create(&src).unwrap_or_else(|e| panic!("{e}"));
    f.write_all(&[b'a'; 4096]).ok();
    f.seek(SeekFrom::Start(32 << 20)).ok();
    f.write_all(&[b'b'; 4096]).ok();
    f.set_len(64 << 20).ok(); // a trailing hole too
    drop(f);
    std::fs::create_dir(t.path().join("dst")).ok();
    let b = LocalPosixBackend::new();
    let (ctl, mut prx, _p, _c) = control();
    let req = TransferRequest {
        id: 3,
        kind: TransferKind::Copy,
        sources: vec![t.uri("disk.img")],
        dest_dir: t.uri("dst"),
        overwrite: false,
    };
    b.transfer(req, ctl).await?;
    let copy = t.path().join("dst/disk.img");
    assert!(std::fs::read(&src).ok() == std::fs::read(&copy).ok(), "contents differ");
    let (s, d) = (std::fs::metadata(&src).map(|m| m.blocks()).ok(), std::fs::metadata(&copy).map(|m| m.blocks()).ok());
    assert_eq!(std::fs::metadata(&copy).map(|m| m.len()).ok(), Some(64 << 20));
    assert!(s.is_some_and(|s| s < 2048), "the source is sparse here: {s:?} blocks");
    assert!(d.is_some_and(|d| d < 2048), "the copy filled its holes: {d:?} blocks of 512 B");
    let mut last = None;
    while let Ok(p) = prx.try_recv() {
        last = Some(p);
    }
    assert_eq!(last.map(|p| (p.bytes_done, p.state)), Some((64 << 20, TransferState::Done)), "holes count as done");
    Ok(())
}
