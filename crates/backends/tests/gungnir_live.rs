//! GungnirVfsBackend against a real gungnird built from ../gungnir-vfs
//! (`scripts/build-gungnird.sh`, or `GUNGNIRD_BIN=<path>`). Without a binary the
//! tests print why and pass vacuously; CI should build it first.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use backends::GungnirVfsBackend;
use backends::gungnir::proto::{Health, Value};
use common::{TempDir, list};
use core_vfs::{
    CancellationToken, ListOptions, MountStatus, TransferControl, TransferKind, TransferRequest, TransferState,
    VfsError, VfsProvider, VfsResult, VfsStatus, VfsUri,
};
use tokio::sync::{mpsc, watch};

fn gungnird() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("GUNGNIRD_BIN") {
        return Some(PathBuf::from(p));
    }
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.build/gungnird/release/gungnird");
    p.exists().then_some(p)
}

struct Daemon {
    child: Child,
    sock: PathBuf,
}

impl Daemon {
    fn start(dir: &Path) -> Option<Self> {
        let Some(bin) = gungnird() else {
            eprintln!("SKIP: no gungnird (run scripts/build-gungnird.sh or set GUNGNIRD_BIN)");
            return None;
        };
        let sock = dir.join("g.sock");
        let log = std::fs::File::create(dir.join("gungnird.log")).ok()?;
        let child = Command::new(bin)
            .arg(dir.join("data/metadata.db"))
            .env("GUNGNIR_SOCKET", &sock)
            .env("HOME", dir)
            .env("XDG_DATA_HOME", dir.join("data"))
            .env("XDG_RUNTIME_DIR", dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start gungnird: {e}"));
        let d = Self { child, sock };
        let t0 = Instant::now();
        while std::os::unix::net::UnixStream::connect(&d.sock).is_err() {
            assert!(t0.elapsed() < Duration::from_secs(10), "gungnird did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
        Some(d)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn g(p: &Path) -> VfsUri {
    VfsUri::gungnir("", p.to_str().unwrap_or_else(|| panic!("utf-8"))).unwrap_or_else(|e| panic!("{e}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn handshake_metadata_listing_and_inspect() -> VfsResult<()> {
    let t = TempDir::new("live-md");
    let Some(d) = Daemon::start(t.path()) else { return Ok(()) };
    let b = GungnirVfsBackend::connect(d.sock.clone()).await?;
    assert!(b.ping().await? < Duration::from_millis(100));

    let files = t.path().join("files");
    std::fs::create_dir(&files).ok();
    std::fs::write(files.join("tagged.flac"), b"riff").ok();
    std::fs::write(files.join("plain.txt"), b"x").ok();
    let id = b.identify(&files.join("tagged.flac")).await?;
    assert!(!id.fs_uuid.is_empty() && id.inode_num > 0);
    b.set(&id, "emblems", &Value::Stringv(vec!["important".into(), "viking".into()])).await?;
    b.set(&id, "comment", &Value::String("sample comment".into())).await?;
    assert_eq!(b.list_keys(&id).await?, ["comment", "emblems"]);
    assert_eq!(b.get(&id, "comment").await?, Some(Value::String("sample comment".into())));
    assert_eq!(b.get(&id, "missing").await?, None);

    let (_, entries) = list(&b, &g(&files), ListOptions::default()).await?;
    let tagged = entries.iter().find(|e| e.name == "tagged.flac").unwrap_or_else(|| panic!("listed"));
    assert_eq!(tagged.vfs.status, VfsStatus::Tagged);
    assert!(tagged.vfs.attributes.iter().any(|(k, v)| k == "metadata::emblems" && v == "[important, viking]"));
    assert!(tagged.uri.to_uri_string().starts_with("gungnir:///"));
    let plain = entries.iter().find(|e| e.name == "plain.txt").unwrap_or_else(|| panic!("listed"));
    assert_eq!(plain.vfs.status, VfsStatus::Local);

    let info = b.inspect(&tagged.uri).await?;
    let get = |k: &str| info.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
    assert_eq!(get("metadata::comment").as_deref(), Some("sample comment"));
    assert_eq!(get("Inode"), Some(id.inode_num.to_string()));
    assert!(get("Mount health").is_some_and(|h| h.starts_with("Healthy")), "{info:?}");

    let h = b.health_of(&files).await?;
    assert_eq!(h.state, Health::Healthy);
    let mounts = b.mounts().await?;
    assert_eq!(mounts[0].status, MountStatus::Online);

    // Rename keeps metadata (keyed by identity) and tells gungnird.
    let renamed = b.rename(&tagged.uri, "renamed.flac").await?;
    let id2 = b.identify(Path::new(renamed.path())).await?;
    assert_eq!(id2.inode_num, id.inode_num);
    assert_eq!(b.list_keys(&id2).await?.len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_file_ops_with_progress_and_metadata() -> VfsResult<()> {
    let t = TempDir::new("live-ops");
    let Some(d) = Daemon::start(t.path()) else { return Ok(()) };
    let b = GungnirVfsBackend::connect(d.sock.clone()).await?;
    let src = t.path().join("album");
    std::fs::create_dir_all(src.join("assets")).ok();
    std::fs::write(src.join("01.flac"), vec![1u8; 8 << 20]).ok();
    std::fs::write(src.join("assets/sample.wav"), vec![2u8; 1 << 20]).ok();
    let id = b.identify(&src.join("01.flac")).await?;
    b.set(&id, "rating", &Value::String("5".into())).await?;
    let dst = t.path().join("backup");
    std::fs::create_dir(&dst).ok();

    let (ptx, mut prx) = mpsc::channel(512);
    let (_pause, paused) = watch::channel(false);
    let ctl = TransferControl { progress: ptx, cancel: CancellationToken::new(), paused };
    // file:// source into a gungnir:// destination: gungnird does the copy.
    let req = TransferRequest {
        id: 1,
        kind: TransferKind::Copy,
        sources: vec![VfsUri::from_local_path(&src)?],
        dest_dir: g(&dst),
        overwrite: false,
    };
    assert!(b.can_transfer(&req.sources[0], &req.dest_dir));
    b.transfer(req, ctl).await?;
    let mut last = None;
    while let Ok(p) = prx.try_recv() {
        last = Some(p);
    }
    let last = last.unwrap_or_else(|| panic!("no progress"));
    assert_eq!(last.state, TransferState::Done);
    assert_eq!((last.bytes_done, last.files_done), (9 << 20, 2));
    assert_eq!(std::fs::read(dst.join("album/01.flac")).map(|v| v.len()).ok(), Some(8 << 20));
    // ALL_METADATA: the copy carries metadata::rating.
    let cid = b.identify(&dst.join("album/01.flac")).await?;
    assert_eq!(b.get(&cid, "rating").await?, Some(Value::String("5".into())));

    // Delete through the daemon (delete-tree for directories).
    b.delete(&[g(&dst.join("album"))], CancellationToken::new()).await?;
    assert!(!dst.join("album").exists());
    // Errors come back typed.
    let e = b.delete(&[g(&t.path().join("nope"))], CancellationToken::new()).await;
    assert!(matches!(e, Err(VfsError::NotFound(_))), "{e:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dead_daemon_fails_fast_and_listing_still_works() -> VfsResult<()> {
    let t = TempDir::new("live-dead");
    let Some(mut d) = Daemon::start(t.path()) else { return Ok(()) };
    let b = GungnirVfsBackend::connect(d.sock.clone()).await?;
    std::fs::write(t.path().join("f"), b"x").ok();
    let _ = d.child.kill();
    let _ = d.child.wait();
    std::thread::sleep(Duration::from_millis(100));
    let t0 = Instant::now();
    let r = b.identify(&t.path().join("f")).await;
    assert!(matches!(r, Err(VfsError::DaemonUnavailable(_))), "{r:?}");
    assert!(t0.elapsed() < Duration::from_secs(1), "failed in {:?}", t0.elapsed());
    let (_, entries) = list(&b, &g(t.path()), ListOptions::default()).await?;
    assert!(entries.iter().any(|e| e.name == "f"));
    assert_eq!(b.mounts().await?[0].status, MountStatus::Disconnected);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn no_daemon_is_reported_not_hung() {
    let t = TempDir::new("live-none");
    let t0 = Instant::now();
    let r = GungnirVfsBackend::connect(t.path().join("absent.sock")).await;
    assert!(matches!(r, Err(VfsError::DaemonUnavailable(_))));
    assert!(t0.elapsed() < Duration::from_millis(600));
}

#[tokio::test(flavor = "multi_thread")]
async fn live_transfer_rejects_symlink_recursion_and_merges_directory_replacements() -> VfsResult<()> {
    let t = TempDir::new("live-overwrite-alias");
    let Some(d) = Daemon::start(t.path()) else { return Ok(()) };
    let b = GungnirVfsBackend::connect(d.sock.clone()).await?;
    for directory in ["source/item/inside", "dest/item"] {
        std::fs::create_dir_all(t.path().join(directory)).unwrap_or_else(|e| panic!("{e}"));
    }
    std::fs::write(t.path().join("source/item/new.txt"), b"new").unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(t.path().join("dest/item/old.txt"), b"old").unwrap_or_else(|e| panic!("{e}"));
    std::os::unix::fs::symlink(t.path().join("source/item/inside"), t.path().join("alias"))
        .unwrap_or_else(|e| panic!("{e}"));
    let make_control = || {
        let (progress, receiver) = mpsc::channel(128);
        let (pause, paused) = watch::channel(false);
        (TransferControl { progress, cancel: CancellationToken::new(), paused }, receiver, pause)
    };
    let (ctl, _rx, _pause) = make_control();
    assert_eq!(
        b.transfer(
            TransferRequest {
                id: 31,
                kind: TransferKind::Copy,
                sources: vec![g(&t.path().join("source/item"))],
                dest_dir: g(&t.path().join("alias")),
                overwrite: false
            },
            ctl
        )
        .await,
        Err(VfsError::WouldRecurse)
    );
    let (ctl, _rx, _pause) = make_control();
    b.transfer(
        TransferRequest {
            id: 32,
            kind: TransferKind::Copy,
            sources: vec![g(&t.path().join("source/item"))],
            dest_dir: g(&t.path().join("dest")),
            overwrite: true,
        },
        ctl,
    )
    .await?;
    assert_eq!(std::fs::read(t.path().join("dest/item/new.txt")).ok().as_deref(), Some(b"new".as_slice()));
    assert!(t.path().join("dest/item/old.txt").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn mixed_same_device_and_cross_device_moves_do_not_recopy_moved_sources() -> VfsResult<()> {
    use std::os::unix::fs::MetadataExt;
    let t = TempDir::new("live-mixed-move");
    let Some(d) = Daemon::start(t.path()) else { return Ok(()) };
    let external = PathBuf::from("/dev/shm").join(format!("gungnir-fm-mixed-move-{}", std::process::id()));
    if std::fs::metadata("/dev/shm").map(|m| m.dev()).ok() == std::fs::metadata(t.path()).map(|m| m.dev()).ok() {
        eprintln!("SKIP: no separate /dev/shm filesystem");
        return Ok(());
    }
    std::fs::create_dir(&external).unwrap_or_else(|e| panic!("{e}"));
    let external = TempDir(external);
    std::fs::create_dir(t.path().join("local-dir")).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(t.path().join("local-dir/local.txt"), b"local").unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(external.path().join("remote.txt"), b"cross-device").unwrap_or_else(|e| panic!("{e}"));
    std::fs::create_dir(t.path().join("dest")).unwrap_or_else(|e| panic!("{e}"));
    let b = GungnirVfsBackend::connect(d.sock.clone()).await?;
    let (progress, _rx) = mpsc::channel(128);
    let (_pause, paused) = watch::channel(false);
    b.transfer(
        TransferRequest {
            id: 33,
            kind: TransferKind::Move,
            sources: vec![g(&t.path().join("local-dir")), g(&external.path().join("remote.txt"))],
            dest_dir: g(&t.path().join("dest")),
            overwrite: false,
        },
        TransferControl { progress, cancel: CancellationToken::new(), paused },
    )
    .await?;
    assert_eq!(std::fs::read(t.path().join("dest/local-dir/local.txt")).ok().as_deref(), Some(b"local".as_slice()));
    assert_eq!(std::fs::read(t.path().join("dest/remote.txt")).ok().as_deref(), Some(b"cross-device".as_slice()));
    assert!(!t.path().join("local-dir").exists());
    assert!(!external.path().join("remote.txt").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn live_empty_file_creation_is_exclusive() -> VfsResult<()> {
    let t = TempDir::new("live-create-file");
    let Some(d) = Daemon::start(t.path()) else { return Ok(()) };
    let b = GungnirVfsBackend::connect(d.sock.clone()).await?;
    let target = t.path().join("new document.txt");
    b.create_file(&g(&target)).await?;
    assert_eq!(std::fs::metadata(&target).ok().map(|m| m.len()), Some(0));
    std::fs::write(&target, b"existing content").unwrap_or_else(|e| panic!("{e}"));
    assert!(matches!(b.create_file(&g(&target)).await, Err(VfsError::AlreadyExists(_))));
    assert_eq!(std::fs::read(&target).ok().as_deref(), Some(b"existing content".as_slice()));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_live_listing_with_full_receiver_stops_forwarder() -> VfsResult<()> {
    let t = TempDir::new("live-blocked-listing");
    let files = t.path().join("files");
    std::fs::create_dir(&files).unwrap_or_else(|e| panic!("{e}"));
    for i in 0..100 {
        std::fs::write(files.join(format!("item{i}")), b"x").unwrap_or_else(|e| panic!("{e}"));
    }
    let (tx, mut rx) = mpsc::channel(1);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let socket = t.path().join("missing.sock");
    let task = tokio::spawn(async move {
        GungnirVfsBackend::new(socket).list_dir(&g(&files), ListOptions { first_batch: 1, batch: 1 }, tx, token).await
    });
    // Wait until the output is full; keep its receiver alive without consuming it.
    tokio::time::timeout(Duration::from_secs(2), async {
        while rx.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("listing did not produce a batch"));
    tokio::time::sleep(Duration::from_millis(30)).await;
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap_or_else(|_| panic!("live forwarder stayed blocked after cancellation"))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(result, Err(VfsError::Cancelled));
    rx.close();
    Ok(())
}

#[tokio::test]
async fn template_copy_to_gungnir_preserves_contents_and_namespace() -> VfsResult<()> {
    let t = TempDir::new("template-gungnir");
    std::fs::write(t.path().join("template"), b"document bytes").unwrap_or_else(|e| panic!("{e}"));
    let backend = GungnirVfsBackend::new(t.path().join("missing.sock"));
    let target = g(&t.path().join("created.txt"));
    backend.create_from_template(&t.uri("template"), &target).await?;
    assert_eq!(std::fs::read(t.path().join("created.txt")).unwrap_or_default(), b"document bytes");
    assert_eq!(backend.stat(&target).await?.uri, target);
    Ok(())
}

/// The trash through gungnird: list, stat, browse, change notice, delete, restore, empty.
#[tokio::test(flavor = "multi_thread")]
async fn trash_goes_through_the_daemon() -> VfsResult<()> {
    let t = TempDir::new("live-trash");
    let dir = t.0.clone();
    let trash = dir.join("data/Trash");
    let (files, info) = (trash.join("files"), trash.join("info"));
    std::fs::create_dir_all(files.join("tree/sub")).unwrap_or_else(|e| panic!("{e}"));
    std::fs::create_dir_all(&info).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(files.join("tree/sub/leaf"), b"leaf").unwrap_or_else(|e| panic!("{e}"));
    let item = |n: &str, orig: &Path| {
        std::fs::write(files.join(n), b"data").unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(info.join(format!("{n}.trashinfo")), format!("[Trash Info]\nPath={}\n", orig.display()))
            .unwrap_or_else(|e| panic!("{e}"));
    };
    let back = dir.join("back.txt");
    item("back.txt", &back);
    item("gone.txt", &dir.join("gone.txt"));
    std::fs::write(info.join("tree.trashinfo"), format!("[Trash Info]\nPath={}\n", dir.join("real-name").display()))
        .unwrap_or_else(|e| panic!("{e}"));
    let Some(d) = Daemon::start(&dir) else { return Ok(()) };
    let b = GungnirVfsBackend::connect(d.sock.clone()).await?;
    assert!(b.handles(&VfsUri::trash("/")?));
    let opts = ListOptions::default();
    let names = |v: (Vec<usize>, Vec<core_vfs::FileEntry>)| {
        let mut n: Vec<_> = v.1.into_iter().map(|e| e.name).collect();
        n.sort();
        n
    };
    let root = VfsUri::trash("/")?;
    assert_eq!(names(list(&b, &root, opts).await?), ["back.txt", "gone.txt", "real-name"]);
    let leaf = b.stat(&VfsUri::trash("/tree/sub/leaf")?).await?;
    assert_eq!((leaf.name.as_str(), leaf.size), ("leaf", 4));
    assert_eq!(names(list(&b, &VfsUri::trash("/tree/sub")?, opts).await?), ["leaf"]);
    assert_eq!(
        b.delete(&[VfsUri::trash("/tree/sub/leaf")?], CancellationToken::new()).await,
        Err(VfsError::Unsupported("only a whole item can be changed; restore the folder first".into()))
    );
    let mut w = b.watch(&root)?.unwrap_or_else(|| panic!("no watch"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    b.delete(&[VfsUri::trash("/gone.txt")?], CancellationToken::new()).await?;
    assert!(!files.join("gone.txt").exists() && !info.join("gone.txt.trashinfo").exists());
    let ev = tokio::time::timeout(Duration::from_secs(6), w.events.recv()).await;
    assert!(matches!(ev, Ok(Some(core_vfs::WatchEvent::Changed))), "{ev:?}");
    b.restore(&[VfsUri::trash("/back.txt")?]).await?;
    assert_eq!(std::fs::read(&back).unwrap_or_else(|e| panic!("{e}")), b"data");
    assert_eq!(names(list(&b, &root, opts).await?), ["real-name"]);
    b.empty_trash().await?;
    assert!(list(&b, &root, opts).await?.1.is_empty());
    assert!(!files.join("tree").exists());
    Ok(())
}
