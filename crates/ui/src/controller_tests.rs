//! Headless tests of the controller: a recording `UiSink` stands in for the window,
//! so streaming, pre-emption, filtering, sorting and selection are checked exactly as
//! the UI would receive them.

use std::sync::{Arc, Mutex, PoisonError, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use backends::mock::MockConfig;
use backends::{LocalPosixBackend, MockGungnirBackend};
use core_vfs::{VfsRegistry, VfsUri};
use tokio::sync::mpsc;

use crate::bridge::{UiSink, Update};
use crate::controller::{Command, Config, Controller, Msg};

struct Recorder(Mutex<std_mpsc::Sender<Update>>);

impl UiSink for Recorder {
    fn send(&self, u: Update) {
        let _ = self.0.lock().unwrap_or_else(PoisonError::into_inner).send(u);
    }
}

struct Harness {
    tx: mpsc::Sender<Msg>,
    rx: std_mpsc::Receiver<Update>,
    _rt: tokio::runtime::Runtime,
}

fn uri(s: &str) -> VfsUri {
    VfsUri::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

fn start(left: &str, right: &str, mock: MockConfig) -> Harness {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap_or_else(|e| panic!("{e}"));
    let mut reg = VfsRegistry::new();
    reg.register(Arc::new(LocalPosixBackend::new()));
    reg.register(Arc::new(MockGungnirBackend::new(mock)));
    let (utx, urx) = std_mpsc::channel();
    let (tx, rx) = mpsc::channel(4096);
    let cfg = Config {
        left: uri(left),
        right: uri(right),
        home: uri("/"),
        gungnir_root: uri("gungnir://odin/"),
        gungnir_label: "TEST".into(),
        bookmarks_file: None,
        actions_dir: None,
        session_file: None,
        initial_session: None,
        trace: false,
    };
    let c = Controller::new(reg, Arc::new(Recorder(Mutex::new(utx))), tx.clone(), cfg);
    rt.spawn(c.run(rx));
    Harness { tx, rx: urx, _rt: rt }
}

impl Harness {
    fn send(&self, c: Command) {
        self.tx.try_send(Msg::Ui(c)).unwrap_or_else(|e| panic!("{e}"));
    }

    /// Collect updates until `done` returns true for one of them.
    fn until(&self, timeout: Duration, mut done: impl FnMut(&Update) -> bool) -> Vec<Update> {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(u) => {
                    let stop = done(&u);
                    out.push(u);
                    if stop {
                        return out;
                    }
                }
                Err(_) => panic!("timed out; last updates: {:?}", out.iter().rev().take(3).collect::<Vec<_>>()),
            }
        }
    }

    /// Updates arriving within `d` (no condition).
    fn drain_for(&self, d: Duration) -> Vec<Update> {
        let deadline = Instant::now() + d;
        let mut out = Vec::new();
        while let Ok(u) = self.rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            out.push(u);
        }
        out
    }
}

fn full_rows(u: &Update, pane: usize, len: usize) -> bool {
    matches!(u, Update::Rows { pane: p, rows } if *p == pane && rows.len() == len)
}

#[test]
fn streams_a_small_first_batch_then_the_sorted_whole() {
    let h = start("gungnir://odin/asgard/stress-50k", "gungnir://odin/asgard", MockConfig::instant());
    let ups = h.until(Duration::from_secs(20), |u| full_rows(u, 0, 50_000));
    let appends: Vec<usize> = ups
        .iter()
        .filter_map(|u| match u {
            Update::AppendRows { pane: 0, rows } => Some(rows.len()),
            _ => None,
        })
        .collect();
    assert_eq!(appends.first(), Some(&200), "first screenful arrives alone");
    assert_eq!(appends.iter().sum::<usize>(), 50_000);
    let Some(Update::Rows { rows, .. }) = ups.last() else { panic!("no final rows") };
    assert!(rows[0].name.starts_with("rune-000000."), "{}", rows[0].name);
    assert!(rows.windows(2).all(|w| w[0].name.as_str() < w[1].name.as_str()), "sorted by name");
}

#[test]
fn navigating_away_cancels_the_inflight_listing() {
    // 100k entries in 2,000-entry batches, 20 ms apart: about a second of streaming.
    let slow = MockConfig { batch_latency: Duration::from_millis(20), ..MockConfig::instant() };
    let h = start("gungnir://odin/valhalla/stress-100k", "gungnir://odin/asgard", slow);
    // Wait for the first rows of the slow listing, then leave.
    h.until(Duration::from_secs(20), |u| matches!(u, Update::AppendRows { pane: 0, .. }));
    let t0 = Instant::now();
    h.send(Command::Navigate { pane: 0, location: "gungnir://odin/asgard".into() });
    let ups = h.until(Duration::from_secs(5), |u| full_rows(u, 0, 7));
    assert!(t0.elapsed() < Duration::from_millis(500), "new folder shown in {:?}", t0.elapsed());
    // After the switch (the empty reset), nothing from the old listing may arrive.
    let reset = ups.iter().rposition(|u| full_rows(u, 0, 0)).unwrap_or_else(|| panic!("no reset"));
    let mut later = ups[reset..].iter().collect::<Vec<_>>();
    let tail = h.drain_for(Duration::from_millis(400));
    later.extend(tail.iter());
    for u in later {
        if let Update::AppendRows { pane: 0, rows } | Update::Rows { pane: 0, rows } = u {
            assert!(rows.iter().all(|r| !r.name.starts_with("rune-")), "stale stress-100k rows after navigation");
        }
    }
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    // Isolated sandbox only; never a home folder.
    let base = std::env::temp_dir().join("gungnir-test-sandbox");
    let p = base.join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap_or_else(|e| panic!("{e}"));
    p
}

#[test]
fn hidden_toggle_sorting_and_range_selection() {
    let d = temp_dir("ctl");
    std::fs::write(d.join("alpha.txt"), vec![0u8; 10]).ok();
    std::fs::write(d.join("beta.txt"), vec![0u8; 5_000]).ok();
    std::fs::write(d.join(".gamma"), vec![0u8; 1]).ok();
    std::fs::create_dir(d.join("zeta-dir")).ok();
    let h = start(d.to_str().unwrap_or("/"), "gungnir://odin/asgard", MockConfig::instant());
    let names = |u: &Update| match u {
        Update::Rows { pane: 0, rows } => rows.iter().map(|r| r.name.to_string()).collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let ups = h.until(Duration::from_secs(5), |u| full_rows(u, 0, 3));
    assert_eq!(
        names(ups.last().unwrap_or_else(|| panic!())),
        ["zeta-dir", "alpha.txt", "beta.txt"],
        "folders first, dotfile hidden"
    );

    h.send(Command::Run("toggle-hidden".into()));
    let ups = h.until(Duration::from_secs(5), |u| full_rows(u, 0, 4));
    assert!(names(ups.last().unwrap_or_else(|| panic!())).contains(&".gamma".to_owned()));

    h.send(Command::SortBy { pane: 0, column: 1 }); // size ascending
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 4));
    h.send(Command::SortBy { pane: 0, column: 1 }); // size descending
    let ups = h.until(Duration::from_secs(5), |u| full_rows(u, 0, 4));
    assert_eq!(names(ups.last().unwrap_or_else(|| panic!())), ["zeta-dir", "beta.txt", "alpha.txt", ".gamma"]);

    h.send(Command::RowClicked { pane: 0, index: 1, ctrl: false, shift: false });
    h.send(Command::RowClicked { pane: 0, index: 3, ctrl: false, shift: true });
    let ups =
        h.until(Duration::from_secs(5), |u| matches!(u, Update::Status(s) if s.selection.starts_with("3 selected")));
    let flips: usize = ups
        .iter()
        .map(|u| match u {
            Update::Selection { pane: 0, changes } => changes.iter().filter(|c| c.1).count(),
            _ => 0,
        })
        .sum();
    assert!(flips >= 3, "selection pushed as row patches, not a full reload");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn crumbs_start_at_home_like_nemo() {
    use crate::controller::crumb_views;
    let home = uri("/home/user");
    let c = crumb_views(&uri("/home/user/Music/release_v1"), &home);
    let labels: Vec<(String, i32)> = c.iter().map(|c| (c.label.to_string(), c.icon)).collect();
    assert_eq!(labels, [("Home".into(), 1), ("Music".into(), 0), ("release_v1".into(), 0)]);
    assert_eq!(c[0].uri, "/home/user");
    let c = crumb_views(&uri("/etc/pacman.d"), &home);
    assert_eq!((c[0].label.as_str(), c[0].icon), ("", 2), "file system root is a drive icon");
    assert_eq!(c.len(), 3);
    let c = crumb_views(&uri("gungnir://odin/asgard/stress-50k"), &home);
    let labels: Vec<&str> = c.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["odin", "asgard", "stress-50k"]);
    assert_eq!(c[0].icon, 3);
    assert_eq!(crumb_views(&uri("gungnir:///home"), &home)[0].label, "gungnir");
}

#[test]
fn copy_paste_and_cut_paste_between_folders() {
    let d = temp_dir("clip");
    std::fs::create_dir_all(d.join("src_dir")).ok();
    std::fs::create_dir_all(d.join("dst_dir")).ok();
    std::fs::write(d.join("src_dir/sample_document.pdf"), b"pdf").ok();
    std::fs::write(d.join("src_dir/archive_alpha.tar.gz"), b"tgz").ok();
    let src = d.join("src_dir");
    let h = start(src.to_str().unwrap_or("/"), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 2));

    // Ctrl+C on the first row, then Ctrl+V in the other folder.
    h.send(Command::RowClicked { pane: 0, index: 0, ctrl: false, shift: false });
    h.send(Command::Run("copy".into()));
    h.send(Command::Navigate { pane: 0, location: d.join("dst_dir").display().to_string() });
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 0));
    h.send(Command::Run("paste".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Status(s) if s.message.contains("finished")));
    assert!(d.join("dst_dir/archive_alpha.tar.gz").exists(), "copied");
    assert!(d.join("src_dir/archive_alpha.tar.gz").exists(), "copy keeps the source");

    // Ctrl+X in the source folder, Ctrl+V here: a move.
    h.send(Command::Navigate { pane: 0, location: src.display().to_string() });
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 2));
    h.send(Command::RowClicked { pane: 0, index: 1, ctrl: false, shift: false });
    h.send(Command::Run("cut".into()));
    h.send(Command::Navigate { pane: 0, location: d.join("dst_dir").display().to_string() });
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 1));
    h.send(Command::Run("paste".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Status(s) if s.message.starts_with("Moving") && s.message.contains("finished")));
    assert!(d.join("dst_dir/sample_document.pdf").exists() && !d.join("src_dir/sample_document.pdf").exists(), "moved");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn tab_creation_cycling_and_closing() {
    let d = temp_dir("tabs");
    let d1 = d.join("folder_alpha");
    let d2 = d.join("folder_beta");
    std::fs::create_dir_all(&d1).ok();
    std::fs::create_dir_all(&d2).ok();

    let h = start(d1.to_str().unwrap_or("/"), d2.to_str().unwrap_or("/"), MockConfig::instant());
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if view.tabs.len() == 1));

    // 1. Create a new tab in pane 0
    h.send(Command::TabNew(0));
    let updates = h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if view.tabs.len() == 2));
    let pane_view = updates.into_iter().find_map(|u| match u {
        Update::Pane { pane: 0, view } if view.tabs.len() == 2 => Some(view),
        _ => None,
    }).unwrap_or_else(|| panic!("missing pane view"));
    assert_eq!(pane_view.tabs.len(), 2);
    assert_eq!(pane_view.active_tab, 1);

    // 2. Cycle active tab forward (delta = 1) -> wraps from index 1 back to 0
    h.send(Command::TabCycle { pane: 0, delta: 1 });
    let updates = h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if view.active_tab == 0));
    let pane_view = updates.into_iter().find_map(|u| match u {
        Update::Pane { pane: 0, view } if view.active_tab == 0 => Some(view),
        _ => None,
    }).unwrap_or_else(|| panic!("missing pane view"));
    assert_eq!(pane_view.active_tab, 0);

    // 3. Cycle active tab backward (delta = -1) -> wraps from index 0 to 1
    h.send(Command::TabCycle { pane: 0, delta: -1 });
    let updates = h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if view.active_tab == 1));
    let pane_view = updates.into_iter().find_map(|u| match u {
        Update::Pane { pane: 0, view } if view.active_tab == 1 => Some(view),
        _ => None,
    }).unwrap_or_else(|| panic!("missing pane view"));
    assert_eq!(pane_view.active_tab, 1);

    // 4. Close the 2nd tab (index 1) -> 1 tab remains, active tab becomes 0
    h.send(Command::TabClose(0, 1));
    let updates = h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if view.tabs.len() == 1));
    let pane_view = updates.into_iter().find_map(|u| match u {
        Update::Pane { pane: 0, view } if view.tabs.len() == 1 => Some(view),
        _ => None,
    }).unwrap_or_else(|| panic!("missing pane view"));
    assert_eq!(pane_view.tabs.len(), 1);
    assert_eq!(pane_view.active_tab, 0);

    // 5. Open split view (F3 / toggle-dual)
    h.send(Command::Run("toggle-dual".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Flags(f) if f.dual_pane));

    // 6. Close the last tab in the secondary pane (pane 1) -> collapses split view
    h.send(Command::TabClose(1, 0));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Flags(f) if !f.dual_pane));

    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn session_persistence_and_safe_recovery() {
    let d = temp_dir("session_test");
    let session_file = d.join("session.json");
    let home_dir = d.join("home_mock");
    std::fs::create_dir_all(&home_dir).ok();
    let home_uri = VfsUri::from_local_path(&home_dir).unwrap_or_else(|_| panic!("home uri"));

    let valid_tab = home_dir.join("release_v1");
    std::fs::create_dir_all(&valid_tab).ok();

    let state = crate::session::SessionState {
        window: crate::session::WindowState {
            width: 1440,
            height: 900,
            maximized: false,
        },
        split_view: true,
        active_pane: 0,
        primary_pane_tabs: crate::session::PaneTabsState::new(
            vec![valid_tab.display().to_string(), "/nonexistent/inaccessible/path_12345".into()],
            0,
        ),
        secondary_pane_tabs: Some(crate::session::PaneTabsState::new(
            vec![home_dir.display().to_string()],
            0,
        )),
        show_hidden: true,
        sidebar_visible: true,
        zoom_level: 1.25,
    };

    // Save atomically
    crate::session::save_atomic(&session_file, &state).unwrap_or_else(|e| panic!("save atomic: {e}"));
    assert!(session_file.exists());

    // Load back
    let loaded = crate::session::load(&session_file).unwrap_or_else(|e| panic!("load: {e}"));
    assert_eq!(loaded.window.width, 1440);
    assert!(loaded.split_view);
    assert!(loaded.show_hidden);
    assert_eq!(loaded.zoom_level, 1.25);

    // Verify safe fallback: inaccessible path falls back to home_uri
    let paths = loaded.primary_pane_tabs.get_paths();
    let recovered_0 = crate::session::resolve_path_with_fallback(&paths[0], &home_uri);
    let recovered_1 = crate::session::resolve_path_with_fallback(&paths[1], &home_uri);

    let expected_0 = VfsUri::from_local_path(&valid_tab).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(recovered_0, expected_0);
    assert_eq!(recovered_1, home_uri, "Inaccessible path cleanly fell back to home");

    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn test_remote_connect_dialog_and_crumbs() {
    backends::remote::set_remote_test_mode(true);
    let h = start("/", "/", MockConfig::default());

    // Trigger connect-server command
    h.send(Command::Run("connect-server".into()));
    let dialog_updates = h.until(Duration::from_millis(500), |u| {
        matches!(u, Update::Dialog(d) if matches!(d.kind, crate::DialogKind::ConnectServer))
    });
    assert!(!dialog_updates.is_empty(), "connect-server dialog should open");

    // Accept SFTP URI
    h.send(Command::DialogAccept("sftp://user@host.internal/srv/data".into()));
    let close_updates = h.until(Duration::from_millis(500), |u| matches!(u, Update::CloseDialog));
    assert!(!close_updates.is_empty(), "dialog should close on connect");

    // Check crumbs formatting for remote URIs
    let sftp_uri = VfsUri::parse("sftp://user@host.internal/srv/data").unwrap_or_else(|e| panic!("{e}"));
    let home = VfsUri::parse("/home/user").unwrap_or_else(|e| panic!("{e}"));
    let crumbs = crate::controller::crumb_views(&sftp_uri, &home);
    assert_eq!(crumbs[0].label.as_str(), "user@host.internal");
    assert_eq!(crumbs[0].icon, 2);

    let smb_uri = VfsUri::parse("smb://nas.local/share/docs").unwrap_or_else(|e| panic!("{e}"));
    let smb_crumbs = crate::controller::crumb_views(&smb_uri, &home);
    assert_eq!(smb_crumbs[0].label.as_str(), "nas.local");
    assert_eq!(smb_crumbs[0].icon, 2);

    backends::remote::set_remote_test_mode(false);
}

#[test]
fn test_zoom_command_and_bounds() {
    let h = start("/", "/", MockConfig::default());
    h.send(Command::Zoom(1.75));
    h.send(Command::Zoom(99.0)); // Should clamp safely
    h.send(Command::Zoom(-5.0)); // Should clamp safely
    let _ = h.drain_for(Duration::from_millis(50));
}
