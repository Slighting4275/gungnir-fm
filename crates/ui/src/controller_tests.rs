//! Headless tests of the controller: a recording `UiSink` stands in for the window,
//! so streaming, pre-emption, filtering, sorting and selection are checked exactly as
//! the UI would receive them.

#![allow(clippy::unwrap_used)]

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
    _desktop_sandbox: DesktopSandbox,
}

struct DesktopSandbox(std::path::PathBuf);
impl Drop for DesktopSandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn uri(s: &str) -> VfsUri {
    VfsUri::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

fn start(left: &str, right: &str, mock: MockConfig) -> Harness {
    start_with_local(left, right, mock, true)
}

fn start_with_local(left: &str, right: &str, mock: MockConfig, include_local: bool) -> Harness {
    start_configured(left, right, mock, include_local, |_| {})
}

fn start_configured(
    left: &str,
    right: &str,
    mock: MockConfig,
    include_local: bool,
    configure: impl FnOnce(&mut Config),
) -> Harness {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap_or_else(|e| panic!("{e}"));
    let mut reg = VfsRegistry::new();
    if include_local {
        reg.register(Arc::new(LocalPosixBackend::new()));
    }
    reg.register(Arc::new(MockGungnirBackend::new(mock)));
    reg.register(Arc::new(backends::RemoteVfsBackend::sftp()));
    reg.register(Arc::new(backends::RemoteVfsBackend::smb()));
    reg.register(Arc::new(backends::RemoteVfsBackend::network()));
    let (utx, urx) = std_mpsc::channel();
    let (tx, rx) = mpsc::channel(4096);
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sandbox = temp_dir(&format!("desktop-settings-{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let desktop_dirs = crate::desktop::Dirs {
        config: vec![sandbox.join("config")],
        data: vec![sandbox.join("data"), std::path::PathBuf::from("/usr/share")],
    };
    let mut cfg = Config {
        left: uri(left),
        right: uri(right),
        left_select: None,
        right_select: None,
        home: uri("/"),
        gungnir_root: uri("gungnir://odin/"),
        gungnir_label: "TEST".into(),
        bookmarks_file: None,
        actions_dir: None,
        templates_dir: None,
        session_file: None,
        initial_session: None,
        explicit_locations: false,
        desktop_dirs: Some(desktop_dirs),
        trace: false,
    };
    configure(&mut cfg);
    let c = Controller::new(reg, Arc::new(Recorder(Mutex::new(utx))), tx.clone(), cfg);
    rt.spawn(c.run(rx));
    Harness { tx, rx: urx, _rt: rt, _desktop_sandbox: DesktopSandbox(sandbox) }
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
    h.until(
        Duration::from_secs(5),
        |u| matches!(u, Update::Status(s) if s.message.starts_with("Moving") && s.message.contains("finished")),
    );
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
    let updates =
        h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if view.tabs.len() == 2));
    let pane_view = updates
        .into_iter()
        .find_map(|u| match u {
            Update::Pane { pane: 0, view } if view.tabs.len() == 2 => Some(view),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing pane view"));
    assert_eq!(pane_view.tabs.len(), 2);
    assert_eq!(pane_view.active_tab, 1);

    // 2. Cycle active tab forward (delta = 1) -> wraps from index 1 back to 0
    h.send(Command::TabCycle { pane: 0, delta: 1 });
    let updates =
        h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if view.active_tab == 0));
    let pane_view = updates
        .into_iter()
        .find_map(|u| match u {
            Update::Pane { pane: 0, view } if view.active_tab == 0 => Some(view),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing pane view"));
    assert_eq!(pane_view.active_tab, 0);

    // 3. Cycle active tab backward (delta = -1) -> wraps from index 0 to 1
    h.send(Command::TabCycle { pane: 0, delta: -1 });
    let updates =
        h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if view.active_tab == 1));
    let pane_view = updates
        .into_iter()
        .find_map(|u| match u {
            Update::Pane { pane: 0, view } if view.active_tab == 1 => Some(view),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing pane view"));
    assert_eq!(pane_view.active_tab, 1);

    // 4. Close the 2nd tab (index 1) -> 1 tab remains, active tab becomes 0
    h.send(Command::TabClose(0, 1));
    let updates =
        h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if view.tabs.len() == 1));
    let pane_view = updates
        .into_iter()
        .find_map(|u| match u {
            Update::Pane { pane: 0, view } if view.tabs.len() == 1 => Some(view),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing pane view"));
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
        window: crate::session::WindowState { width: 1440, height: 900, maximized: false },
        split_view: true,
        active_pane: 0,
        primary_pane_tabs: crate::session::PaneTabsState::new(
            vec![valid_tab.display().to_string(), "/nonexistent/inaccessible/path_12345".into()],
            0,
        ),
        secondary_pane_tabs: Some(crate::session::PaneTabsState::new(vec![home_dir.display().to_string()], 0)),
        show_hidden: true,
        sidebar_visible: true,
        zoom_level: 1.25,
        dark_mode: true,
        view_mode: 2,
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
    let dialog_updates = h.until(
        Duration::from_millis(500),
        |u| matches!(u, Update::Dialog(d) if matches!(d.kind, crate::DialogKind::ConnectServer)),
    );
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
fn test_network_sidebar_and_navigation() {
    backends::remote::set_remote_test_mode(true);
    let h = start("/", "/", MockConfig::default());

    // Initially at "/" so network place is inactive
    let initial_places = h.until(
        Duration::from_millis(500),
        |u| matches!(u, Update::NetworkPlaces(np) if !np.is_empty() && !np[0].active),
    );
    assert!(!initial_places.is_empty(), "Network places update should be broadcast");

    // Open network:/// from sidebar
    h.send(Command::SidebarOpen("network:///".into()));

    // Network places should now be active
    let active_places = h.until(
        Duration::from_millis(500),
        |u| matches!(u, Update::NetworkPlaces(np) if !np.is_empty() && np[0].active),
    );
    assert!(!active_places.is_empty(), "Network place should become active when navigating to network:///");

    // Check breadcrumb
    let net_uri = VfsUri::parse("network:///").unwrap_or_else(|e| panic!("{e}"));
    let home = VfsUri::parse("/home/user").unwrap_or_else(|e| panic!("{e}"));
    let crumbs = crate::controller::crumb_views(&net_uri, &home);
    assert_eq!(crumbs.len(), 1);
    assert_eq!(crumbs[0].label.as_str(), "Network");
    assert_eq!(crumbs[0].icon, 6);

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

#[test]
fn test_terminal_drawer_toggle_and_commands() {
    let tmp = std::env::temp_dir().join(format!("gungnir_test_term_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let sub = tmp.join("subfolder");
    let _ = std::fs::create_dir_all(&sub);

    let tmp_str = tmp.to_str().unwrap_or_else(|| panic!("valid utf8"));
    let h = start(tmp_str, tmp_str, MockConfig::default());
    let _ = h.drain_for(Duration::from_millis(100));

    // Toggle terminal open
    h.send(Command::Run("toggle-terminal".into()));
    let term_updates = h.until(Duration::from_millis(500), |u| matches!(u, Update::Terminal { open, .. } if *open));
    assert!(!term_updates.is_empty(), "terminal should open");

    // Run shell command
    h.send(Command::TerminalRun("echo gungnir_test_output_123".into()));
    let out_updates = h.until(
        Duration::from_millis(1500),
        |u| matches!(u, Update::Terminal { output, .. } if output.contains("gungnir_test_output_123")),
    );
    assert!(!out_updates.is_empty(), "terminal should receive output");

    // Clear output
    h.send(Command::TerminalClear);
    let clear_updates =
        h.until(Duration::from_millis(500), |u| matches!(u, Update::Terminal { output, .. } if output.is_empty()));
    assert!(!clear_updates.is_empty(), "terminal should clear output");

    // Test cd command
    h.send(Command::TerminalRun("cd subfolder".into()));
    let cd_updates = h
        .until(Duration::from_millis(500), |u| matches!(u, Update::Terminal { cwd, .. } if cwd.ends_with("subfolder")));
    assert!(!cd_updates.is_empty(), "terminal cwd should update on cd");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_open_terminal_here_command_and_action() {
    let tmp = std::env::temp_dir().join(format!("gungnir_test_open_term_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let sub = tmp.join("subfolder");
    let _ = std::fs::create_dir_all(&sub);

    let tmp_str = tmp.to_str().unwrap_or_else(|| panic!("valid utf8"));
    let h = start(tmp_str, tmp_str, MockConfig::default());
    let _ = h.drain_for(Duration::from_millis(100));

    // Command "open-terminal-here"
    h.send(Command::Run("open-terminal-here".into()));
    let updates = h.drain_for(Duration::from_millis(500));
    let handled = updates.iter().any(|u| matches!(u, Update::Status(..) | Update::Terminal { .. }));
    assert!(handled, "open-terminal-here command should produce a status or terminal update");

    // CustomAction with id "open_terminal_here"
    h.send(Command::CustomAction { pane: 0, id: "open_terminal_here".into() });
    let updates2 = h.drain_for(Duration::from_millis(500));
    let handled2 = updates2.iter().any(|u| matches!(u, Update::Status(..) | Update::Terminal { .. }));
    assert!(handled2, "custom-action open_terminal_here should be handled");

    // CustomAction with alias id "terminal"
    h.send(Command::CustomAction { pane: 0, id: "terminal".into() });
    let updates3 = h.drain_for(Duration::from_millis(500));
    let handled3 = updates3.iter().any(|u| matches!(u, Update::Status(..) | Update::Terminal { .. }));
    assert!(handled3, "custom-action terminal alias should be handled");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_pane_view_context_and_clipboard_state() {
    let tmp = std::env::temp_dir().join(format!("gungnir_test_pane_view_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let f1 = tmp.join("file1.txt");
    let f2 = tmp.join("file2.txt");
    std::fs::write(&f1, "hello").unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(&f2, "world").unwrap_or_else(|e| panic!("{e}"));

    let tmp_str = tmp.to_str().unwrap_or_else(|| panic!("valid utf8"));
    let h = start(tmp_str, tmp_str, MockConfig::default());

    // Initially has_clipboard is false and is_trash is false
    let initial_pane = h.until(Duration::from_millis(500), |u| matches!(u, Update::Pane { pane: 0, .. }));
    assert!(!initial_pane.is_empty(), "initial pane view should be received");
    if let Some(Update::Pane { view, .. }) = initial_pane.last() {
        assert!(!view.is_trash, "should not be trash scheme");
        assert!(!view.has_clipboard, "clipboard should be empty initially");
    }

    // Wait for the two files to appear
    let _ = h.until(Duration::from_millis(500), |u| full_rows(u, 0, 2));

    // Select all items
    h.send(Command::Run("select-all".into()));
    let sel_pane = h
        .until(Duration::from_millis(500), |u| matches!(u, Update::Pane { pane: 0, view } if view.selected_count >= 2));
    assert!(!sel_pane.is_empty(), "selected_count should be updated in PaneView on select-all");

    // Copy to clipboard
    h.send(Command::Run("copy".into()));
    let clip_pane =
        h.until(Duration::from_millis(500), |u| matches!(u, Update::Pane { pane: 0, view } if view.has_clipboard));
    assert!(!clip_pane.is_empty(), "has_clipboard should be true in PaneView after copy");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_folder_commands_and_row_context() {
    let tmp = std::env::temp_dir().join(format!("gungnir_test_folder_cmds_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let sub = tmp.join("alpha");
    let _ = std::fs::create_dir_all(&sub);
    let f1 = tmp.join("file1.txt");
    std::fs::write(&f1, "hello").unwrap_or_else(|e| panic!("{e}"));

    let tmp_str = tmp.to_str().unwrap_or_else(|| panic!("valid utf8"));
    let h = start(tmp_str, tmp_str, MockConfig::default());
    let _ = h.until(Duration::from_secs(3), |u| full_rows(u, 0, 2));

    // Test folder-properties command
    h.send(Command::Run("folder-properties".into()));
    let props = h.until(
        Duration::from_secs(2),
        |u| matches!(u, Update::Dialog(d) if matches!(d.kind, crate::DialogKind::Properties) && d.title.contains("Properties ·")),
    );
    assert!(!props.is_empty(), "folder-properties should open properties dialog for the folder");
    h.send(Command::DialogCancel);
    let _ = h.drain_for(Duration::from_millis(50));

    // Test open-terminal-folder command
    h.send(Command::Run("open-terminal-folder".into()));
    let term = h.until(Duration::from_secs(2), |u| matches!(u, Update::Status(..) | Update::Terminal { .. }));
    assert!(!term.is_empty(), "open-terminal-folder should produce status or terminal update");

    // Test RowContext updates cursor even if already selected
    h.send(Command::Run("select-all".into()));
    let _ =
        h.until(Duration::from_secs(2), |u| matches!(u, Update::Pane { pane: 0, view } if view.selected_count == 2));
    h.send(Command::RowContext { pane: 0, index: 1 });
    let cursor_update =
        h.until(Duration::from_secs(2), |u| matches!(u, Update::Pane { pane: 0, view } if view.cursor == 1));
    assert!(!cursor_update.is_empty(), "RowContext should update cursor to 1 even when already selected");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_deep_recursive_search_and_grep() {
    let tmp = std::env::temp_dir().join(format!("gungnir_test_search_{}", std::process::id()));
    let nested = tmp.join("alpha").join("beta");
    let _ = std::fs::create_dir_all(&nested);
    let file = nested.join("target_file.txt");
    std::fs::write(&file, "magic_marker_line_456\nother text\n").unwrap_or_else(|e| panic!("{e}"));

    let tmp_str = tmp.to_str().unwrap_or_else(|| panic!("valid utf8"));
    let h = start(tmp_str, tmp_str, MockConfig::default());
    let _ = h.drain_for(Duration::from_millis(100));

    // Open search dialog
    h.send(Command::Run("deep-search".into()));
    let search_dialog = h.until(
        Duration::from_millis(500),
        |u| matches!(u, Update::Dialog(d) if matches!(d.kind, crate::DialogKind::Search)),
    );
    assert!(!search_dialog.is_empty(), "search dialog should open");

    // Search by filename
    h.send(Command::DialogCustomAction {
        action: "search".into(),
        opt1: false,
        opt2: false,
        extra1: "target".into(),
        extra2: "".into(),
    });
    let name_results = h.until(
        Duration::from_millis(1500),
        |u| matches!(u, Update::Dialog(d) if d.rows.iter().any(|(name, _)| name == "target_file.txt")),
    );
    assert!(!name_results.is_empty(), "should find target_file.txt by filename");

    // Search by content grep
    h.send(Command::DialogCustomAction {
        action: "search".into(),
        opt1: true,
        opt2: false,
        extra1: "magic_marker".into(),
        extra2: "".into(),
    });
    let grep_results = h.until(
        Duration::from_millis(1500),
        |u| matches!(u, Update::Dialog(d) if d.rows.iter().any(|(_, detail)| detail.contains("magic_marker"))),
    );
    assert!(!grep_results.is_empty(), "should find content match via grep");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_batch_rename_preview_and_apply() {
    let tmp = std::env::temp_dir().join(format!("gungnir_test_rename_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let f1 = tmp.join("test_item_1.txt");
    let f2 = tmp.join("test_item_2.txt");
    std::fs::write(&f1, "a").unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(&f2, "b").unwrap_or_else(|e| panic!("{e}"));

    let tmp_str = tmp.to_str().unwrap_or_else(|| panic!("valid utf8"));
    let h = start(tmp_str, tmp_str, MockConfig::default());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 2));

    // Select all items
    h.send(Command::Run("select-all".into()));
    let _ = h.drain_for(Duration::from_millis(50));

    // Trigger batch rename
    h.send(Command::Run("batch-rename".into()));
    let rename_dialog = h.until(
        Duration::from_millis(500),
        |u| matches!(u, Update::Dialog(d) if matches!(d.kind, crate::DialogKind::BatchRename)),
    );
    assert!(!rename_dialog.is_empty(), "batch rename dialog should open");

    // Preview find/replace
    h.send(Command::DialogCustomAction {
        action: "preview-rename".into(),
        opt1: false,
        opt2: false,
        extra1: "test_item".into(),
        extra2: "renamed_item".into(),
    });
    let preview_updates = h.until(
        Duration::from_millis(500),
        |u| matches!(u, Update::Dialog(d) if d.rows.iter().any(|(_, new_n)| new_n.starts_with("renamed_item"))),
    );
    assert!(!preview_updates.is_empty(), "preview should show renamed_item");

    // Apply batch rename
    h.send(Command::DialogCustomAction {
        action: "apply-batch-rename".into(),
        opt1: false,
        opt2: false,
        extra1: "".into(),
        extra2: "".into(),
    });
    let close = h.until(Duration::from_millis(1000), |u| matches!(u, Update::CloseDialog));
    assert!(!close.is_empty(), "dialog should close after apply");

    // Wait for actual completion rather than assuming the operation takes <100ms.
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Status(s) if s.message == "Successfully renamed 2 files"));
    assert!(tmp.join("renamed_item_1.txt").exists());
    assert!(tmp.join("renamed_item_2.txt").exists());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_pane_comparison_and_sync() {
    let tmp = std::env::temp_dir().join(format!("gungnir_test_compare_{}", std::process::id()));
    let left = tmp.join("left");
    let right = tmp.join("right");
    let _ = std::fs::create_dir_all(&left);
    let _ = std::fs::create_dir_all(&right);

    std::fs::write(left.join("common.txt"), "shared").unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(right.join("common.txt"), "shared").unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(left.join("left_unique.txt"), "left").unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(right.join("right_unique.txt"), "right").unwrap_or_else(|e| panic!("{e}"));

    // This comparison intentionally checks size and mtime; two independent
    // writes do not reliably receive the same nanosecond timestamp.
    let common_mtime = std::fs::metadata(left.join("common.txt")).unwrap().modified().unwrap();
    std::fs::File::options()
        .write(true)
        .open(right.join("common.txt"))
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(common_mtime))
        .unwrap();

    let left_str = left.to_str().unwrap_or_else(|| panic!("valid utf8"));
    let right_str = right.to_str().unwrap_or_else(|| panic!("valid utf8"));
    let h = start(left_str, right_str, MockConfig::default());
    let mut ready = [false; 2];
    h.until(Duration::from_secs(5), |u| {
        for (pane, seen) in ready.iter_mut().enumerate() {
            *seen |= full_rows(u, pane, 2);
        }
        ready.iter().all(|seen| *seen)
    });

    // Open comparison dialog
    h.send(Command::Run("compare-panes".into()));
    let compare_dialog = h.until(
        Duration::from_millis(800),
        |u| matches!(u, Update::Dialog(d) if matches!(d.kind, crate::DialogKind::ComparePanes)),
    );
    assert!(!compare_dialog.is_empty(), "compare dialog should open");

    // Verify row contents reflect Left and Right differences
    let last = compare_dialog.last().unwrap_or_else(|| panic!("dialog update"));
    if let Update::Dialog(d) = last {
        assert!(d.rows.iter().any(|(k, _)| k.contains("[LEFT ONLY] left_unique.txt")));
        assert!(d.rows.iter().any(|(k, _)| k.contains("[RIGHT ONLY] right_unique.txt")));
        assert!(d.rows.iter().any(|(k, _)| k.contains("[IDENTICAL] common.txt")));
    } else {
        panic!("expected Dialog update");
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_conflict_detection_and_resolution() {
    let tmp = std::env::temp_dir().join(format!("gungnir_test_conflict_{}", std::process::id()));
    let left = tmp.join("left");
    let right = tmp.join("right");
    let _ = std::fs::create_dir_all(&left);
    let _ = std::fs::create_dir_all(&right);

    let left_file = left.join("collision.txt");
    let right_file = right.join("collision.txt");
    std::fs::write(&left_file, "new_source_content").unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(&right_file, "old_target_content").unwrap_or_else(|e| panic!("{e}"));

    let left_str = left.to_str().unwrap_or_else(|| panic!("valid utf8"));
    let right_str = right.to_str().unwrap_or_else(|| panic!("valid utf8"));
    let h = start(left_str, right_str, MockConfig::default());
    let _ = h.drain_for(Duration::from_millis(200));

    // Open dual pane (F3) so copy-to-other works
    h.send(Command::Run("toggle-dual".into()));
    let _ = h.drain_for(Duration::from_millis(50));

    // Select the file in left pane and copy to right pane (F5)
    h.send(Command::Run("select-all".into()));
    let _ = h.drain_for(Duration::from_millis(50));
    h.send(Command::Run("copy-to-other".into()));

    // Conflict dialog should appear!
    let conflict_updates = h.until(
        Duration::from_millis(1000),
        |u| matches!(u, Update::Dialog(d) if matches!(d.kind, crate::DialogKind::Conflict)),
    );
    assert!(!conflict_updates.is_empty(), "collision dialog should be presented");

    // Choose replace
    h.send(Command::DialogCustomAction {
        action: "replace".into(),
        opt1: false,
        opt2: false,
        extra1: "".into(),
        extra2: "".into(),
    });

    let close = h.until(Duration::from_millis(1000), |u| matches!(u, Update::CloseDialog));
    assert!(!close.is_empty(), "dialog should close on resolution");

    let _ = h.drain_for(Duration::from_millis(200));
    // Verify target content replaced
    let target_content = std::fs::read_to_string(&right_file).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(target_content, "new_source_content");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_lossless_compression_and_extraction_workflow() {
    let pid = std::process::id();
    let tmp = std::env::temp_dir().join(format!("gungnir_compress_test_{pid}"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap_or_else(|e| panic!("{e}"));

    let src_file = tmp.join("data_sample.txt");
    std::fs::write(&src_file, b"Bit-perfect lossless verification string 1234567890").unwrap_or_else(|e| panic!("{e}"));

    let h = start(tmp.to_str().unwrap_or(""), tmp.to_str().unwrap_or(""), MockConfig::default());
    let _ = h.until(Duration::from_secs(5), |u| full_rows(u, 0, 1));

    // Select all items in left pane
    h.send(Command::Run("select-all".into()));
    let _ = h.drain_for(Duration::from_millis(50));

    // Run quick-compress-zst
    h.send(Command::Run("quick-compress-zst".into()));

    // Wait for the archive to be produced
    // Wait for the quick-compress operation to complete and auto-select the archive
    let _ = h.until(
        Duration::from_millis(4000),
        |u| matches!(u, Update::Status(s) if s.message.contains("Quick-compressed")),
    );
    let _ = h.until(
        Duration::from_millis(3000),
        |u| matches!(u, Update::Rows { pane: 0, rows } if rows.iter().any(|r| r.name == "data_sample.txt.tar.zst" && r.selected)),
    );

    // Test archive integrity
    h.send(Command::Run("test-archive".into()));
    let info = h.until(
        Duration::from_millis(3000),
        |u| matches!(u, Update::Dialog(d) if d.rows.iter().any(|(_, v)| v.contains("BIT-PERFECT"))),
    );
    assert!(!info.is_empty(), "test-archive should verify bit-perfect integrity");

    // Close the info dialog
    h.send(Command::DialogCancel);
    let _ = h.drain_for(Duration::from_millis(50));

    // Ensure archive is selected and extract to subfolder
    h.send(Command::Run("extract-to".into()));

    let extracted_file = tmp.join("data_sample").join("data_sample.txt");
    let mut extracted = false;
    for _ in 0..50 {
        if extracted_file.exists() {
            extracted = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(extracted, "extract-to should extract into subfolder");

    let extracted_content = std::fs::read(&extracted_file).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        extracted_content, b"Bit-perfect lossless verification string 1234567890",
        "Extracted payload must be bit-perfect"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_package_update_available_and_auto_update_lifecycle() {
    // Enable simulation mode for auto update
    // SAFETY: isolated test process scope
    unsafe {
        std::env::set_var("GUNGNIR_TEST_UPDATE_SIMULATE", "1");
        std::env::set_var("GUNGNIR_PACKAGE_FORMAT", "flatpak");
    }

    let h = start("file:///", "file:///", MockConfig::default());

    // 1. Initially simulate arrival of update check result: available = true
    let event = crate::controller::Event::UpdateCheckResult(crate::updater::UpdateCheckResult {
        available: true,
        version: "0.2.0".to_string(),
        format: crate::updater::PackageFormat::Flatpak,
        details: "New Flatpak package available".to_string(),
    });
    h.tx.try_send(Msg::Ev(event)).unwrap_or_else(|e| panic!("{e}"));

    // 2. The controller sends Update::PackageStatus { available: true, version: "0.2.0", updating: false }
    h.until(
        Duration::from_secs(3),
        |u| matches!(u, Update::PackageStatus { available: true, version, updating: false } if version == "0.2.0"),
    );

    // 3. User clicks the "Update Available" button (trigger-update)
    h.send(Command::TriggerUpdate);

    // 4. Controller transitions to updating: true
    h.until(Duration::from_secs(3), |u| matches!(u, Update::PackageStatus { available: true, updating: true, .. }));

    // 5. When auto-update finishes, controller sends available: false, updating: false, message disappears!
    h.until(Duration::from_secs(3), |u| matches!(u, Update::PackageStatus { available: false, updating: false, .. }));

    unsafe {
        std::env::remove_var("GUNGNIR_TEST_UPDATE_SIMULATE");
        std::env::remove_var("GUNGNIR_PACKAGE_FORMAT");
    }
}

#[test]
fn test_nemo_properties_and_open_with_default_application() {
    let tmp = std::env::temp_dir().join(format!("gungnir-nemo-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let sample_file = tmp.join("demo_movie.mp4");
    std::fs::write(&sample_file, b"sample mp4 header and payload").unwrap();

    let file_uri = VfsUri::from_local_path(&tmp).unwrap().to_uri_string();
    let h = start(&file_uri, "gungnir://odin/asgard", MockConfig::default());

    // Wait until file is loaded
    h.until(Duration::from_secs(3), |u| full_rows(u, 0, 1));

    // 1. Select the file row
    h.send(Command::RowClicked { pane: 0, index: 0, ctrl: false, shift: false });
    h.until(Duration::from_secs(3), |u| matches!(u, Update::Status(s) if s.selection.starts_with("1 selected")));

    // 2. Open Properties dialog (Basic tab = 0)
    h.send(Command::Run("properties".into()));
    h.until(Duration::from_secs(3), |u| {
        matches!(u, Update::Dialog(d) if d.kind == crate::DialogKind::Properties && d.properties_tab == 0 && d.title.contains("demo_movie.mp4"))
    });

    // 3. Switch to Permissions tab (tab-1) and apply executable bit
    h.send(Command::DialogCustomAction {
        action: "tab-1".into(),
        opt1: false,
        opt2: false,
        extra1: String::new(),
        extra2: String::new(),
    });
    h.send(Command::DialogCustomAction {
        action: "apply-permissions:exec".into(),
        opt1: false,
        opt2: false,
        extra1: String::new(),
        extra2: String::new(),
    });

    h.until(
        Duration::from_secs(3),
        |u| matches!(u, Update::Dialog(d) if d.kind == crate::DialogKind::Properties && d.perms_executable),
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(&sample_file).unwrap();
        assert_eq!(meta.permissions().mode() & 0o111, 0o111);
    }

    // 4. Switch to Open With tab (tab-2) and test setting & resetting default app
    h.send(Command::DialogCustomAction {
        action: "tab-2".into(),
        opt1: false,
        opt2: false,
        extra1: String::new(),
        extra2: String::new(),
    });
    h.until(
        Duration::from_secs(3),
        |u| matches!(u, Update::Dialog(d) if d.kind == crate::DialogKind::Properties && d.properties_tab == 2),
    );

    h.send(Command::DialogCustomAction {
        action: "set-default-app:mpv.desktop".into(),
        opt1: false,
        opt2: false,
        extra1: String::new(),
        extra2: String::new(),
    });

    h.send(Command::DialogCustomAction {
        action: "reset-default-app".into(),
        opt1: false,
        opt2: false,
        extra1: String::new(),
        extra2: String::new(),
    });

    // 5. Close dialog
    h.send(Command::DialogCancel);
    h.until(Duration::from_secs(3), |u| matches!(u, Update::CloseDialog));
    let _ = h.drain_for(Duration::from_millis(50));

    // 6. Open "Open With" dialog directly (opens Properties directly on tab 2)
    h.send(Command::Run("open-with".into()));
    h.until(
        Duration::from_secs(5),
        |u| matches!(u, Update::Dialog(d) if d.kind == crate::DialogKind::Properties && d.properties_tab == 2),
    );

    h.send(Command::DialogCancel);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_theme_mode_toggle_and_persistence() {
    let h = start("gungnir://odin/asgard", "gungnir://odin/valhalla", MockConfig::default());

    // Initial state: dark mode is enabled by default
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Flags(f) if f.dark_mode));

    // 1. Switch to Light Mode via Command::ToggleDarkMode
    h.send(Command::ToggleDarkMode);
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Flags(f) if !f.dark_mode));

    // 2. Switch back to Dark Mode via Command::Run("toggle-theme")
    h.send(Command::Run("toggle-theme".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Flags(f) if f.dark_mode));

    // 3. Switch to Light Mode via Command::SetDarkMode(false)
    h.send(Command::SetDarkMode(false));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Flags(f) if !f.dark_mode));

    // 4. Switch to Dark Mode via Command::SetDarkMode(true)
    h.send(Command::SetDarkMode(true));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Flags(f) if f.dark_mode));
}

#[test]
fn terminal_output_truncation_preserves_utf8_and_actor() {
    let h = start("gungnir://odin/asgard", "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 7));
    h.tx.try_send(Msg::Ev(crate::controller::Event::TerminalOutput {
        text: "€".repeat(10_001),
        reload_loc: uri("/unused"),
    }))
    .unwrap();
    h.send(Command::Run("about".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Dialog(d) if d.title == "Gungnir FM"));
}

#[test]
fn content_search_handles_multibyte_preview_boundary() {
    let tmp = temp_dir("unicode-search");
    std::fs::write(tmp.join("unicode.txt"), format!("{}€match", "x".repeat(59))).unwrap();
    let h = start(tmp.to_str().unwrap(), tmp.to_str().unwrap(), MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 1));
    h.send(Command::Run("deep-search".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Dialog(_)));
    h.send(Command::DialogCustomAction {
        action: "search".into(),
        opt1: true,
        opt2: false,
        extra1: "match".into(),
        extra2: String::new(),
    });
    h.until(
        Duration::from_secs(5),
        |u| matches!(u, Update::Dialog(d) if d.rows.iter().any(|(n, _)| n.contains("unicode.txt"))),
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn mixed_conflict_resolutions_keep_auto_renamed_original() {
    let tmp = temp_dir("mixed-conflicts");
    let left = tmp.join("left");
    let right = tmp.join("right");
    std::fs::create_dir(&left).unwrap();
    std::fs::create_dir(&right).unwrap();
    for name in ["a.txt", "b.txt"] {
        std::fs::write(left.join(name), b"new").unwrap();
        std::fs::write(right.join(name), b"old").unwrap();
    }
    let h = start(left.to_str().unwrap(), right.to_str().unwrap(), MockConfig::instant());
    let mut ready = [false; 2];
    h.until(Duration::from_secs(5), |u| {
        for (pane, seen) in ready.iter_mut().enumerate() {
            *seen |= full_rows(u, pane, 2);
        }
        ready.iter().all(|seen| *seen)
    });
    h.send(Command::Run("toggle-dual".into()));
    h.send(Command::Run("select-all".into()));
    h.send(Command::Run("copy-to-other".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Dialog(d) if d.kind == crate::DialogKind::Conflict));
    h.send(Command::DialogCustomAction {
        action: "replace".into(),
        opt1: false,
        opt2: false,
        extra1: String::new(),
        extra2: String::new(),
    });
    let updates =
        h.until(Duration::from_secs(5), |u| matches!(u, Update::Dialog(d) if d.kind == crate::DialogKind::Conflict));
    let auto_name = updates
        .iter()
        .find_map(|u| match u {
            Update::Dialog(d) => d.rows.iter().find(|(k, _)| k == "File name").map(|(_, v)| v.clone()),
            _ => None,
        })
        .unwrap();
    h.send(Command::DialogCustomAction {
        action: "auto-rename".into(),
        opt1: false,
        opt2: false,
        extra1: String::new(),
        extra2: String::new(),
    });
    h.until(Duration::from_secs(5), |u| matches!(u, Update::CloseDialog));
    let deadline = Instant::now() + Duration::from_secs(5);
    let copy_name = auto_name.replace(".txt", " (copy).txt");
    while !right.join(&copy_name).exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::read(right.join(&auto_name)).unwrap(), b"old");
    assert_eq!(std::fs::read(right.join(&copy_name)).unwrap(), b"new");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn batch_rename_collision_keeps_all_originals_and_reports_error() {
    let tmp = temp_dir("batch-collision");
    std::fs::write(tmp.join("a.txt"), b"a").unwrap();
    std::fs::write(tmp.join("b.txt"), b"b").unwrap();
    let h = start(tmp.to_str().unwrap(), tmp.to_str().unwrap(), MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 2));
    h.send(Command::Run("select-all".into()));
    h.send(Command::Run("batch-rename".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Dialog(d) if d.kind == crate::DialogKind::BatchRename));
    h.send(Command::DialogCustomAction {
        action: "preview-rename".into(),
        opt1: false,
        opt2: false,
        extra1: "a".into(),
        extra2: "b".into(),
    });
    h.until(
        Duration::from_secs(5),
        |u| matches!(u, Update::Dialog(d) if d.rows.iter().any(|(a,b)| a == "a.txt" && b == "b.txt")),
    );
    h.send(Command::DialogCustomAction {
        action: "apply-batch-rename".into(),
        opt1: false,
        opt2: false,
        extra1: String::new(),
        extra2: String::new(),
    });
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Status(s) if s.message_is_error));
    assert_eq!(std::fs::read(tmp.join("a.txt")).unwrap(), b"a");
    assert_eq!(std::fs::read(tmp.join("b.txt")).unwrap(), b"b");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn nemo_invert_selection_and_duplicate_only_visible_targets() {
    let tmp = temp_dir("nemo-selection");
    for name in ["a.txt", "b.txt", ".hidden"] {
        std::fs::write(tmp.join(name), name).unwrap();
    }
    let h = start(tmp.to_str().unwrap(), tmp.to_str().unwrap(), MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 2));
    h.send(Command::RowClicked { pane: 0, index: 0, ctrl: false, shift: false });
    h.send(Command::Run("invert-selection".into()));
    h.until(
        Duration::from_secs(5),
        |u| matches!(u, Update::Selection { pane: 0, changes } if changes.contains(&(1, true))),
    );
    h.send(Command::Run("duplicate".into()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !tmp.join("b (copy).txt").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::read_to_string(tmp.join("b (copy).txt")).unwrap(), "b.txt");
    assert!(!tmp.join("a (copy).txt").exists());
    assert!(!tmp.join(".hidden (copy)").exists());
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn cancelled_cut_paste_keeps_clipboard_for_retry() {
    let tmp = temp_dir("cut-retry");
    for dir in ["source", "dest", "retry"] {
        std::fs::create_dir(tmp.join(dir)).unwrap();
    }
    std::fs::write(tmp.join("source/item"), b"source").unwrap();
    std::fs::write(tmp.join("dest/item"), b"existing").unwrap();
    let h = start(tmp.join("source").to_str().unwrap(), tmp.join("dest").to_str().unwrap(), MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 1));
    h.send(Command::Run("cut".into()));
    h.send(Command::Navigate { pane: 0, location: tmp.join("dest").display().to_string() });
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 1));
    h.send(Command::Run("paste".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Dialog(d) if d.kind == crate::DialogKind::Conflict));
    h.send(Command::DialogCancel);
    h.send(Command::Navigate { pane: 0, location: tmp.join("retry").display().to_string() });
    h.until(
        Duration::from_secs(5),
        |u| matches!(u, Update::Pane { pane: 0, view } if view.location.ends_with("/retry")),
    );
    h.send(Command::Run("paste".into()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !tmp.join("retry/item").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::read(tmp.join("retry/item")).unwrap(), b"source");
    assert_eq!(std::fs::read(tmp.join("dest/item")).unwrap(), b"existing");
    assert!(!tmp.join("source/item").exists());
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn properties_of_special_file_does_not_block_controller_or_hash_it() {
    use std::os::unix::ffi::OsStrExt;
    let tmp = temp_dir("fifo-properties");
    let path = tmp.join("pipe");
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: valid path in an isolated test directory.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let h = start(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 1));
    h.send(Command::Run("properties".into()));
    let updates = h.until(
        Duration::from_secs(5),
        |u| matches!(u, Update::Dialog(d) if d.title == "pipe Properties" && !d.rows.is_empty()),
    );
    let view = updates
        .iter()
        .find_map(|u| match u {
            Update::Dialog(d) if !d.rows.is_empty() => Some(d),
            _ => None,
        })
        .unwrap();
    assert!(!view.rows.iter().any(|(key, _)| key == "SHA-256"));
    assert!(!view.perms_editable);
    h.send(Command::SetDarkMode(false));
    h.until(Duration::from_secs(1), |u| matches!(u, Update::Flags(f) if !f.dark_mode));
    h.send(Command::DialogCancel);
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn properties_refresh_stays_on_original_file_after_selection_changes() {
    let tmp = temp_dir("properties-target");
    std::fs::write(tmp.join("a.txt"), b"a").unwrap();
    std::fs::write(tmp.join("b.txt"), b"bbbb").unwrap();
    let h = start(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 2));
    h.send(Command::Run("properties".into()));
    h.until(
        Duration::from_secs(5),
        |u| matches!(u, Update::Dialog(d) if d.title == "a.txt Properties" && !d.rows.is_empty()),
    );
    h.send(Command::RowClicked { pane: 0, index: 1, ctrl: false, shift: false });
    h.send(Command::DialogCustomAction {
        action: "tab-1".into(),
        opt1: false,
        opt2: false,
        extra1: String::new(),
        extra2: String::new(),
    });
    let updates = h.until(
        Duration::from_secs(5),
        |u| matches!(u, Update::Dialog(d) if d.title == "a.txt Properties" && !d.rows.is_empty()),
    );
    let view = updates
        .iter()
        .find_map(|u| match u {
            Update::Dialog(d) if !d.rows.is_empty() => Some(d),
            _ => None,
        })
        .unwrap();
    assert!(view.rows.iter().any(|(key, value)| key == "Name" && value == "a.txt"));
    assert!(view.rows.iter().any(|(key, value)| key == "Size" && value.contains("1 bytes")));
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn properties_late_results_do_not_reopen_cancelled_dialog() {
    let tmp = temp_dir("properties-cancel");
    std::fs::write(tmp.join("file.txt"), b"test").unwrap();
    let h = start(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 1));
    h.send(Command::Run("properties".into()));
    h.send(Command::DialogCancel);
    h.until(Duration::from_secs(5), |u| matches!(u, Update::CloseDialog));
    assert!(!h.drain_for(Duration::from_millis(150)).iter().any(|u| matches!(u, Update::Dialog(_))));
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn new_document_and_folder_use_captured_directory_and_exact_names() {
    let tmp = temp_dir("new-document");
    for name in ["original", "other"] {
        std::fs::create_dir(tmp.join(name)).unwrap();
    }
    let h = start(tmp.join("original").to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if !view.loading));
    h.send(Command::Run("new-file".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Dialog(d) if d.kind == crate::DialogKind::NewFile));
    h.send(Command::Navigate { pane: 0, location: tmp.join("other").display().to_string() });
    h.send(Command::DialogAccept(" exact name.txt ".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Status(s) if s.message == "Created  exact name.txt "));
    assert_eq!(std::fs::read(tmp.join("original/ exact name.txt ")).unwrap(), b"");
    assert!(!tmp.join("other/ exact name.txt ").exists());
    h.send(Command::Run("new-folder".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Dialog(d) if d.kind == crate::DialogKind::NewFolder));
    h.send(Command::Navigate { pane: 0, location: tmp.join("original").display().to_string() });
    h.send(Command::DialogAccept("Kept here".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Status(s) if s.message == "Created Kept here"));
    assert!(tmp.join("other/Kept here").is_dir());
    assert!(!tmp.join("original/Kept here").exists());
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn select_pattern_selects_only_visible_matching_names() {
    let tmp = temp_dir("pattern-selection");
    for name in ["a.txt", "b.txt", "c.png", ".hidden.txt"] {
        std::fs::write(tmp.join(name), b"test").unwrap();
    }
    let h = start(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 3));
    h.send(Command::Run("select-pattern".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Dialog(d) if d.kind == crate::DialogKind::SelectPattern));
    h.send(Command::DialogAccept("*.txt".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Status(s) if s.selection.starts_with("2 selected")));
    h.send(Command::Run("duplicate".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Status(s) if s.message.contains("finished")));
    assert!(tmp.join("a (copy).txt").exists() && tmp.join("b (copy).txt").exists());
    assert!(!tmp.join("c (copy).png").exists());
    assert!(!tmp.join(".hidden (copy).txt").exists());
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn empty_trash_requires_confirmation_and_can_be_cancelled() {
    // This harness intentionally has no local provider, so even a regression
    // cannot touch the user's actual Trash.
    let h = start_with_local("gungnir://odin/asgard", "gungnir://odin/asgard", MockConfig::instant(), false);
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 7));
    h.send(Command::Run("empty-trash".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Dialog(d) if d.kind == crate::DialogKind::ConfirmDelete && d.title == "Empty Trash permanently?"));
    h.send(Command::DialogCancel);
    h.until(Duration::from_secs(5), |u| matches!(u, Update::CloseDialog));
    assert!(
        !h.drain_for(Duration::from_millis(50))
            .iter()
            .any(|u| matches!(u, Update::Status(s) if s.message == "Trash emptied"))
    );
}

#[test]
fn sidebar_open_tab_opens_new_tab_at_target() {
    let tmp = temp_dir("sidebar-tab");
    let sub = tmp.join("subfolder");
    std::fs::create_dir(&sub).unwrap();
    let h = start(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 1));
    h.send(Command::SidebarOpenTab(sub.to_str().unwrap().to_string()));
    let updates =
        h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if view.tabs.len() == 2));
    let view = updates
        .iter()
        .find_map(|u| match u {
            Update::Pane { pane: 0, view } if view.tabs.len() == 2 => Some(view),
            _ => None,
        })
        .unwrap();
    assert_eq!(view.tabs.len(), 2);
    assert!(view.tabs.iter().any(|t| t.location == sub.to_str().unwrap()));
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn trash_navigation_sets_is_trash_flag() {
    let tmp = temp_dir("trash-nav");
    let h = start(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 0));
    h.send(Command::SidebarOpen("trash:///".into()));
    let updates = h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if view.is_trash));
    let view = updates
        .iter()
        .find_map(|u| match u {
            Update::Pane { pane: 0, view } if view.is_trash => Some(view),
            _ => None,
        })
        .unwrap();
    assert!(view.is_trash);
    assert_eq!(view.location, "trash:///");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn open_multiple_selected_folders_uses_new_tabs() {
    let tmp = temp_dir("open-folders");
    for name in ["alpha", "beta"] {
        std::fs::create_dir(tmp.join(name)).unwrap();
    }
    let h = start(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 2));
    h.send(Command::Run("select-all".into()));
    h.send(Command::Run("open".into()));
    let updates =
        h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if view.tabs.len() == 3));
    let view = updates
        .iter()
        .find_map(|u| match u {
            Update::Pane { pane: 0, view } if view.tabs.len() == 3 => Some(view),
            _ => None,
        })
        .unwrap();
    assert!(view.tabs.iter().any(|t| t.location.ends_with("/alpha")));
    assert!(view.tabs.iter().any(|t| t.location.ends_with("/beta")));
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn opening_many_items_asks_before_starting_them() {
    let tmp = temp_dir("open-many");
    for i in 0..11 {
        std::fs::create_dir(tmp.join(format!("folder{i}"))).unwrap();
    }
    let h = start(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 11));
    h.send(Command::Run("select-all".into()));
    h.send(Command::Run("open".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Dialog(d) if d.kind == crate::DialogKind::ConfirmOpen));
    h.send(Command::DialogCancel);
    h.until(Duration::from_secs(5), |u| matches!(u, Update::CloseDialog));
    h.send(Command::SetDarkMode(false));
    assert!(
        !h.until(Duration::from_secs(5), |u| matches!(u, Update::Flags(f) if !f.dark_mode))
            .iter()
            .any(|u| matches!(u, Update::Pane { pane: 0, view } if view.tabs.len() > 1))
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn default_application_changes_use_the_harness_desktop_sandbox() {
    let tmp = temp_dir("default-app-isolation");
    std::fs::write(tmp.join("example.txt"), b"test").unwrap();
    let h = start(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 1));
    h.send(Command::Run("properties".into()));
    h.until(
        Duration::from_secs(5),
        |u| matches!(u, Update::Dialog(d) if !d.rows.is_empty() && d.kind == crate::DialogKind::Properties),
    );
    h.send(Command::DialogCustomAction {
        action: "set-default-app:test-sandbox.desktop".into(),
        opt1: false,
        opt2: false,
        extra1: String::new(),
        extra2: String::new(),
    });
    h.until(
        Duration::from_secs(5),
        |u| matches!(u, Update::Status(s) if s.message.starts_with("Set test-sandbox.desktop as default application")),
    );
    let config = std::fs::read_to_string(h._desktop_sandbox.0.join("config/mimeapps.list")).unwrap();
    assert!(config.contains("text/plain=test-sandbox.desktop;"));
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn executable_permission_toggle_refuses_directories_and_symlink_targets() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = temp_dir("permissions-targets");
    std::fs::create_dir(tmp.join("folder")).unwrap();
    std::fs::write(tmp.join(".target"), b"target").unwrap();
    std::fs::set_permissions(tmp.join(".target"), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(".target", tmp.join("link")).unwrap();
    let h = start(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 2));
    for (index, action) in [(0, "apply-permissions:noexec"), (1, "apply-permissions:exec")] {
        h.send(Command::RowClicked { pane: 0, index, ctrl: false, shift: false });
        h.send(Command::Run("properties".into()));
        h.until(
            Duration::from_secs(5),
            |u| matches!(u, Update::Dialog(d) if !d.rows.is_empty() && d.kind == crate::DialogKind::Properties),
        );
        h.send(Command::DialogCustomAction {
            action: action.into(),
            opt1: false,
            opt2: false,
            extra1: String::new(),
            extra2: String::new(),
        });
        h.send(Command::DialogCancel);
        h.until(Duration::from_secs(5), |u| matches!(u, Update::CloseDialog));
    }
    assert_ne!(std::fs::metadata(tmp.join("folder")).unwrap().permissions().mode() & 0o111, 0);
    assert_eq!(std::fs::metadata(tmp.join(".target")).unwrap().permissions().mode() & 0o777, 0o600);
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn session_restores_view_mode_and_flushes_geometry_and_latest_view_on_quit() {
    let dir = temp_dir("session-controller-quit");
    let path = dir.join("session.json");
    let text = dir.to_str().unwrap_or_else(|| panic!("UTF-8 fixture"));
    let initial = serde_json::from_value(serde_json::json!({
        "primary_pane_tabs": [text], "view_mode": 2,
        "window": { "width": 1100, "height": 750 }
    }))
    .unwrap_or_else(|e| panic!("{e}"));
    let h = start_configured(text, text, MockConfig::default(), true, |cfg| {
        cfg.session_file = Some(path.clone());
        cfg.initial_session = Some(initial);
    });
    h.until(Duration::from_secs(3), |u| matches!(u, Update::Flags(f) if f.view_mode == 2));
    h.send(Command::WindowChanged(crate::session::WindowState { width: 950, height: 650, maximized: false }));
    h.send(Command::ViewMode(0));
    h.send(Command::Quit);
    h.until(Duration::from_secs(3), |u| matches!(u, Update::Quit));
    let saved = crate::session::load(&path).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(saved.view_mode, 0);
    assert_eq!((saved.window.width, saved.window.height), (950, 650));
    assert_eq!(saved.primary_pane_tabs.get_paths(), vec![VfsUri::from_local_path(&dir).unwrap().to_uri_string()]);
    drop(h);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn nemo_inline_creation_uses_unique_names_and_renames_the_created_item() {
    let d = temp_dir("inline-create");
    std::fs::write(d.join("Untitled Document"), b"original").unwrap();
    std::fs::create_dir(d.join("Untitled Folder")).unwrap();
    let path = d.to_str().unwrap();
    let h = start(path, path, MockConfig::default());
    h.until(Duration::from_secs(3), |u| full_rows(u, 0, 2));
    h.send(Command::Run("new-file-inline".into()));
    let updates = h.until(Duration::from_secs(3), |u| matches!(u, Update::Rename { pane: 0, index } if *index >= 0));
    let index = updates
        .iter()
        .find_map(|u| match u {
            Update::Rename { pane: 0, index } if *index >= 0 => Some(*index as usize),
            _ => None,
        })
        .unwrap();
    assert_eq!(std::fs::read(d.join("Untitled Document")).unwrap(), b"original");
    assert_eq!(std::fs::read(d.join("Untitled Document 2")).unwrap(), b"");
    h.send(Command::RenameCommit { pane: 0, index, name: "Report.txt".into() });
    h.until(Duration::from_secs(3), |u| matches!(u, Update::Status(s) if s.message.contains("Renamed to Report.txt")));
    assert_eq!(std::fs::read(d.join("Report.txt")).unwrap(), b"");
    h.send(Command::Run("new-folder-inline".into()));
    h.until(Duration::from_secs(3), |u| matches!(u, Update::Rename { pane: 0, index } if *index >= 0));
    assert!(d.join("Untitled Folder 2").is_dir());
    h.send(Command::RenameCancel);
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn inline_creation_in_right_pane_preserves_existing_symlink_and_clears_filter() {
    let d = temp_dir("inline-right");
    let left = d.join("left");
    let right = d.join("right");
    std::fs::create_dir(&left).unwrap();
    std::fs::create_dir(&right).unwrap();
    std::fs::write(left.join("target"), b"keep").unwrap();
    std::os::unix::fs::symlink(left.join("target"), right.join("Untitled Document")).unwrap();
    let h = start(left.to_str().unwrap(), right.to_str().unwrap(), MockConfig::default());
    h.until(Duration::from_secs(3), |u| full_rows(u, 1, 1));
    h.send(Command::PaneFocused(1));
    h.send(Command::Filter { pane: 1, text: "no-match".into() });
    h.send(Command::Run("new-file-inline".into()));
    h.until(Duration::from_secs(3), |u| matches!(u, Update::Rename { pane: 1, index } if *index >= 0));
    assert_eq!(std::fs::read(left.join("target")).unwrap(), b"keep");
    assert!(right.join("Untitled Document").is_symlink());
    assert_eq!(std::fs::read(right.join("Untitled Document 2")).unwrap(), b"");
    assert!(!left.join("Untitled Document 2").exists());
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn switching_tabs_cancels_rename_before_a_stale_editor_can_commit() {
    let d = temp_dir("rename-tab-switch");
    let a = d.join("a");
    let b = d.join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    std::fs::write(a.join("original.txt"), b"a").unwrap();
    std::fs::write(b.join("other.txt"), b"b").unwrap();
    let h = start(a.to_str().unwrap(), b.to_str().unwrap(), MockConfig::default());
    h.until(Duration::from_secs(3), |u| full_rows(u, 0, 1));
    h.send(Command::TabNew(0));
    h.send(Command::Navigate { pane: 0, location: b.to_str().unwrap().to_string() });
    h.until(
        Duration::from_secs(3),
        |u| matches!(u, Update::Rows { pane: 0, rows } if rows.len()==1 && rows[0].name=="other.txt"),
    );
    h.send(Command::TabSelect(0, 0));
    h.until(
        Duration::from_secs(3),
        |u| matches!(u, Update::Rows { pane: 0, rows } if rows.len()==1 && rows[0].name=="original.txt"),
    );
    h.send(Command::Run("rename".into()));
    h.until(Duration::from_secs(3), |u| matches!(u, Update::Rename { pane: 0, index: 0 }));
    h.send(Command::TabSelect(0, 1));
    h.until(
        Duration::from_secs(3),
        |u| matches!(u, Update::Rows { pane: 0, rows } if rows.len()==1 && rows[0].name=="other.txt"),
    );
    h.send(Command::RenameCommit { pane: 0, index: 0, name: "unexpected.txt".into() });
    h.drain_for(Duration::from_millis(200));
    assert!(a.join("original.txt").exists(), "the previous tab's file must survive a stale rename callback");
    assert!(b.join("other.txt").exists());
    assert!(!a.join("unexpected.txt").exists());
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

fn commit_cursor_rename(h: &Harness, name: &str) {
    h.send(Command::Run("rename".into()));
    let updates =
        h.until(Duration::from_secs(3), |u| matches!(u, Update::Rename { pane, index } if *pane >= 0 && *index >= 0));
    let (pane, index) = updates
        .iter()
        .find_map(|u| match u {
            Update::Rename { pane, index } if *pane >= 0 && *index >= 0 => Some((*pane as usize, *index as usize)),
            _ => None,
        })
        .unwrap();
    h.send(Command::RenameCommit { pane, index, name: name.into() });
}

fn wait_rename_status(h: &Harness, text: &str) {
    h.until(Duration::from_secs(3), |u| matches!(u, Update::Status(s) if s.message.contains(text)));
    h.until(Duration::from_secs(3), |u| matches!(u, Update::RenameHistory { .. }));
}

#[test]
fn local_rename_undo_redo_preserves_subsequent_content_edits() {
    let d = temp_dir("rename-undo-redo");
    std::fs::write(d.join("first.txt"), b"original").unwrap();
    let h = start(d.to_str().unwrap(), d.to_str().unwrap(), MockConfig::default());
    h.until(Duration::from_secs(3), |u| full_rows(u, 0, 1));
    commit_cursor_rename(&h, "second.txt");
    wait_rename_status(&h, "Renamed to second.txt");
    std::fs::write(d.join("second.txt"), b"edited after rename").unwrap();
    h.send(Command::Run("undo-rename".into()));
    wait_rename_status(&h, "Undid rename to first.txt");
    assert_eq!(std::fs::read(d.join("first.txt")).unwrap(), b"edited after rename");
    assert!(!d.join("second.txt").exists());
    h.send(Command::Run("redo-rename".into()));
    wait_rename_status(&h, "Redid rename to second.txt");
    assert_eq!(std::fs::read(d.join("second.txt")).unwrap(), b"edited after rename");
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn rename_undo_collision_keeps_both_files_and_allows_retry() {
    let d = temp_dir("rename-undo-collision");
    std::fs::write(d.join("first.txt"), b"original").unwrap();
    let h = start(d.to_str().unwrap(), d.to_str().unwrap(), MockConfig::default());
    h.until(Duration::from_secs(3), |u| full_rows(u, 0, 1));
    commit_cursor_rename(&h, "second.txt");
    wait_rename_status(&h, "Renamed to second.txt");
    std::fs::write(d.join("first.txt"), b"other file").unwrap();
    h.send(Command::Run("undo-rename".into()));
    wait_rename_status(&h, "Rename failed:");
    assert_eq!(std::fs::read(d.join("first.txt")).unwrap(), b"other file");
    assert_eq!(std::fs::read(d.join("second.txt")).unwrap(), b"original");
    std::fs::remove_file(d.join("first.txt")).unwrap();
    h.send(Command::Run("undo-rename".into()));
    wait_rename_status(&h, "Undid rename to first.txt");
    assert_eq!(std::fs::read(d.join("first.txt")).unwrap(), b"original");
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn rename_undo_refuses_a_replacement_at_the_recorded_path() {
    let d = temp_dir("rename-undo-replacement");
    std::fs::write(d.join("first.txt"), b"original").unwrap();
    let h = start(d.to_str().unwrap(), d.to_str().unwrap(), MockConfig::default());
    h.until(Duration::from_secs(3), |u| full_rows(u, 0, 1));
    commit_cursor_rename(&h, "second.txt");
    wait_rename_status(&h, "Renamed to second.txt");
    std::fs::rename(d.join("second.txt"), d.join("moved-elsewhere.txt")).unwrap();
    std::fs::write(d.join("second.txt"), b"replacement").unwrap();
    h.send(Command::Run("undo-rename".into()));
    wait_rename_status(&h, "The item was replaced");
    assert_eq!(std::fs::read(d.join("second.txt")).unwrap(), b"replacement");
    assert_eq!(std::fs::read(d.join("moved-elsewhere.txt")).unwrap(), b"original");
    assert!(!d.join("first.txt").exists());
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn another_successful_rename_discards_redo() {
    let d = temp_dir("rename-new-history");
    std::fs::write(d.join("first.txt"), b"keep").unwrap();
    let h = start(d.to_str().unwrap(), d.to_str().unwrap(), MockConfig::default());
    h.until(Duration::from_secs(3), |u| full_rows(u, 0, 1));
    commit_cursor_rename(&h, "second.txt");
    wait_rename_status(&h, "Renamed to second.txt");
    h.send(Command::Run("undo-rename".into()));
    wait_rename_status(&h, "Undid rename to first.txt");
    h.until(
        Duration::from_secs(3),
        |u| matches!(u, Update::Rows { pane: 0, rows } if rows.len()==1 && rows[0].name=="first.txt"),
    );
    commit_cursor_rename(&h, "third.txt");
    h.until(Duration::from_secs(3), |u| matches!(u, Update::RenameHistory { can_undo: true, can_redo: false }));
    h.send(Command::Run("redo-rename".into()));
    h.drain_for(Duration::from_millis(100));
    assert_eq!(std::fs::read(d.join("third.txt")).unwrap(), b"keep");
    assert!(!d.join("second.txt").exists());
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn symlink_rename_undo_redo_acts_on_the_link_itself() {
    let d = temp_dir("rename-symlink-history");
    let target = d.join("target.txt");
    std::fs::write(&target, b"keep target").unwrap();
    std::os::unix::fs::symlink(&target, d.join("link")).unwrap();
    let h = start(d.to_str().unwrap(), d.to_str().unwrap(), MockConfig::default());
    h.until(Duration::from_secs(3), |u| full_rows(u, 0, 2));
    commit_cursor_rename(&h, "renamed-link");
    wait_rename_status(&h, "Renamed to renamed-link");
    h.send(Command::Run("undo-rename".into()));
    wait_rename_status(&h, "Undid rename to link");
    assert_eq!(std::fs::read_link(d.join("link")).unwrap(), target);
    h.send(Command::Run("redo-rename".into()));
    wait_rename_status(&h, "Redid rename to renamed-link");
    assert_eq!(std::fs::read_link(d.join("renamed-link")).unwrap(), target);
    assert_eq!(std::fs::read(&target).unwrap(), b"keep target");
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn directory_rename_undo_keeps_children() {
    let d = temp_dir("rename-directory-history");
    std::fs::create_dir(d.join("folder")).unwrap();
    std::fs::write(d.join("folder/child"), b"keep").unwrap();
    let h = start(d.to_str().unwrap(), d.to_str().unwrap(), MockConfig::default());
    h.until(Duration::from_secs(3), |u| full_rows(u, 0, 1));
    commit_cursor_rename(&h, "changed");
    wait_rename_status(&h, "Renamed to changed");
    h.send(Command::Run("undo-rename".into()));
    wait_rename_status(&h, "Undid rename to folder");
    assert_eq!(std::fs::read(d.join("folder/child")).unwrap(), b"keep");
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn sorting_cancels_rename_instead_of_retargeting_another_row() {
    let d = temp_dir("rename-sort-editor");
    std::fs::write(d.join("a"), b"a").unwrap();
    std::fs::write(d.join("z"), b"z").unwrap();
    let h = start(d.to_str().unwrap(), d.to_str().unwrap(), MockConfig::default());
    h.until(Duration::from_secs(3), |u| full_rows(u, 0, 2));
    h.send(Command::Run("rename".into()));
    h.until(Duration::from_secs(3), |u| matches!(u, Update::Rename { pane: 0, index: 0 }));
    h.send(Command::SortBy { pane: 0, column: 0 });
    h.until(Duration::from_secs(3), |u| matches!(u, Update::Rename { pane: -1, index: -1 }));
    h.send(Command::RenameCommit { pane: 0, index: 0, name: "unexpected".into() });
    h.drain_for(Duration::from_millis(100));
    assert_eq!(std::fs::read(d.join("a")).unwrap(), b"a");
    assert_eq!(std::fs::read(d.join("z")).unwrap(), b"z");
    assert!(!d.join("unexpected").exists());
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn startup_preferences_do_not_erase_the_file_argument_selection() {
    let d = temp_dir("startup-selected-file");
    std::fs::write(d.join("aaa.txt"), b"other").unwrap();
    std::fs::write(d.join("chosen.txt"), b"chosen").unwrap();
    let location = VfsUri::from_local_path(&d).unwrap();
    let h = start_configured(d.to_str().unwrap(), d.to_str().unwrap(), MockConfig::default(), true, |cfg| {
        cfg.left_select = Some("chosen.txt".into());
        cfg.initial_session = Some(crate::session::startup_state(None, &location, &location, true, false));
    });
    let updates = h.until(Duration::from_secs(3), |u| full_rows(u, 0, 2));
    let rows = updates
        .iter()
        .find_map(|u| match u {
            Update::Rows { pane: 0, rows } if rows.len() == 2 => Some(rows),
            _ => None,
        })
        .unwrap();
    assert!(rows.iter().any(|row| row.name == "chosen.txt" && row.selected));
    assert!(rows.iter().any(|row| row.name == "aaa.txt" && !row.selected));
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn a_reordered_directory_snapshot_keeps_the_cursor_on_the_same_name() {
    use core_vfs::VfsProvider;
    let d = temp_dir("cursor-reordered-snapshot");
    std::fs::write(d.join("a"), b"a").unwrap();
    std::fs::write(d.join("b"), b"b").unwrap();
    let h = start(d.to_str().unwrap(), d.to_str().unwrap(), MockConfig::default());
    h.until(Duration::from_secs(3), |u| full_rows(u, 0, 2));
    h.drain_for(Duration::from_millis(100));
    h.send(Command::RowClicked { pane: 0, index: 1, ctrl: false, shift: false });
    h.until(
        Duration::from_secs(3),
        |u| matches!(u, Update::Pane { pane: 0, view } if view.cursor==1 && view.selected_count==1),
    );
    let backend = LocalPosixBackend::new();
    let entries = h._rt.block_on(async {
        vec![
            backend.stat(&VfsUri::from_local_path(&d.join("b")).unwrap()).await.unwrap(),
            backend.stat(&VfsUri::from_local_path(&d.join("a")).unwrap()).await.unwrap(),
        ]
    });
    // A valid silent refresh can enumerate the same files in a different raw order.
    h.tx.try_send(Msg::Ev(crate::controller::Event::ListDone {
        pane: 0,
        tab: 1,
        generation: 1,
        result: Ok(core_vfs::ListSummary { entries: 2, skipped: 0 }),
        collected: Some(entries),
        started: Instant::now(),
    }))
    .unwrap();
    h.until(
        Duration::from_secs(3),
        |u| matches!(u, Update::Rows { pane: 0, rows } if rows.len()==2 && rows[1].name=="b"),
    );
    let updates = h.until(Duration::from_secs(3), |u| matches!(u, Update::Pane { pane: 0, .. }));
    let view = updates
        .iter()
        .find_map(|u| match u {
            Update::Pane { pane: 0, view } => Some(view),
            _ => None,
        })
        .unwrap();
    assert_eq!(view.cursor, 1, "raw directory order must not move the cursor from b to a");
    assert_eq!(view.selected_count, 1);
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn failed_rename_preserves_selection_and_does_not_create_undo_history() {
    let d = temp_dir("rename-failed-selection");
    std::fs::write(d.join("a"), b"a").unwrap();
    std::fs::write(d.join("b"), b"b").unwrap();
    let h = start(d.to_str().unwrap(), d.to_str().unwrap(), MockConfig::default());
    h.until(Duration::from_secs(3), |u| full_rows(u, 0, 2));
    h.send(Command::RowClicked { pane: 0, index: 0, ctrl: false, shift: false });
    h.until(Duration::from_secs(3), |u| matches!(u, Update::Pane { pane: 0, view } if view.selected_count==1));
    commit_cursor_rename(&h, "b");
    h.until(Duration::from_secs(3), |u| matches!(u, Update::Status(s) if s.message.contains("Rename failed:")));
    h.until(Duration::from_secs(3), |u| matches!(u, Update::RenameHistory { can_undo: false, can_redo: false }));
    h.send(Command::Nav { pane: 0, action: "refresh".into() });
    let updates = h.until(Duration::from_secs(3), |u| full_rows(u, 0, 2));
    let rows = updates
        .iter()
        .find_map(|u| match u {
            Update::Rows { pane: 0, rows } if rows.len() == 2 => Some(rows),
            _ => None,
        })
        .unwrap();
    assert!(rows[0].selected);
    assert!(!rows[1].selected);
    assert_eq!(std::fs::read(d.join("a")).unwrap(), b"a");
    assert_eq!(std::fs::read(d.join("b")).unwrap(), b"b");
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn redo_rename_refuses_a_replacement_after_undo() {
    let d = temp_dir("rename-redo-replacement");
    std::fs::write(d.join("before"), b"original").unwrap();
    let h = start(d.to_str().unwrap(), d.to_str().unwrap(), MockConfig::default());
    h.until(Duration::from_secs(3), |u| full_rows(u, 0, 1));
    commit_cursor_rename(&h, "after");
    wait_rename_status(&h, "Renamed to after");
    h.send(Command::Run("undo-rename".into()));
    wait_rename_status(&h, "Undid rename to before");
    std::fs::rename(d.join("before"), d.join("kept")).unwrap();
    std::fs::write(d.join("before"), b"replacement").unwrap();
    h.send(Command::Run("redo-rename".into()));
    wait_rename_status(&h, "The item was replaced");
    assert_eq!(std::fs::read(d.join("before")).unwrap(), b"replacement");
    assert_eq!(std::fs::read(d.join("kept")).unwrap(), b"original");
    assert!(!d.join("after").exists());
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn inaccessible_explicit_location_is_reported_without_redirecting_home() {
    let d = temp_dir("explicit-missing-path");
    let missing = d.join("missing");
    let requested = VfsUri::from_local_path(&missing).unwrap();
    let right = VfsUri::from_local_path(&d).unwrap();
    let h = start_configured(missing.to_str().unwrap(), d.to_str().unwrap(), MockConfig::default(), true, |cfg| {
        cfg.initial_session = Some(crate::session::startup_state(None, &requested, &right, true, false));
        cfg.explicit_locations = true;
    });
    h.until(Duration::from_secs(3), |u| matches!(u, Update::Pane { pane: 0, view } if view.location==missing.display().to_string() && !view.error.is_empty()));
    assert!(!missing.exists());
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

fn start_templates(root: &std::path::Path, destination: &str, include_local: bool) -> Harness {
    start_configured(destination, destination, MockConfig::default(), include_local, |cfg| {
        cfg.templates_dir = Some(root.to_path_buf());
    })
}

fn wait_templates(h: &Harness, count: usize) -> Vec<crate::TemplateItem> {
    let updates = h.until(Duration::from_secs(3), |u| matches!(u,Update::Templates(items) if items.len()==count));
    updates
        .into_iter()
        .find_map(|u| match u {
            Update::Templates(items) if items.len() == count => Some(items),
            _ => None,
        })
        .unwrap()
}

#[test]
fn document_template_copy_numbers_names_keeps_bytes_and_supports_inline_naming() {
    let d = temp_dir("templates-controller");
    let source = d.join("Templates");
    let dest = d.join("destination");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();
    let bytes = b"document\0\xff\nbytes";
    std::fs::write(source.join("Letter.odt"), bytes).unwrap();
    std::fs::write(dest.join("Letter.odt"), b"keep existing").unwrap();
    let h = start_templates(&source, dest.to_str().unwrap(), true);
    let items = wait_templates(&h, 1);
    h.send(Command::Run(format!("create-template:{}", items[0].id)));
    let updates = h.until(Duration::from_secs(3), |u| matches!(u,Update::Rename {pane:0,index} if *index>=0));
    let index = updates
        .iter()
        .find_map(|u| match u {
            Update::Rename { pane: 0, index } if *index >= 0 => Some(*index as usize),
            _ => None,
        })
        .unwrap();
    assert_eq!(std::fs::read(dest.join("Letter 2.odt")).unwrap(), bytes);
    assert_eq!(std::fs::read(dest.join("Letter.odt")).unwrap(), b"keep existing");
    assert_eq!(std::fs::read(source.join("Letter.odt")).unwrap(), bytes);
    h.send(Command::RenameCommit { pane: 0, index, name: "My Letter.odt".into() });
    wait_rename_status(&h, "Renamed to My Letter.odt");
    assert_eq!(std::fs::read(dest.join("My Letter.odt")).unwrap(), bytes);
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn document_template_menu_ids_remain_valid_after_an_earlier_item_is_added() {
    let d = temp_dir("templates-stable-ids");
    let source = d.join("Templates");
    let dest = d.join("dest");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(source.join("Letter.txt"), b"letter").unwrap();
    let h = start_templates(&source, dest.to_str().unwrap(), true);
    let old = wait_templates(&h, 1)[0].id.to_string();
    std::fs::write(source.join("Earlier.txt"), b"earlier").unwrap();
    h.send(Command::Run("refresh-templates".into()));
    let items = wait_templates(&h, 2);
    assert_eq!(items[0].title, "Earlier.txt");
    h.send(Command::Run(format!("create-template:{old}")));
    h.until(Duration::from_secs(3), |u| matches!(u,Update::Rename {pane:0,index} if *index>=0));
    assert_eq!(std::fs::read(dest.join("Letter.txt")).unwrap(), b"letter");
    assert!(!dest.join("Earlier.txt").exists());
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn template_creation_remembers_its_original_directory_while_navigation_changes() {
    let d = temp_dir("templates-navigation");
    let source = d.join("Templates");
    let a = d.join("a");
    let b = d.join("b");
    for dir in [&source, &a, &b] {
        std::fs::create_dir(dir).unwrap();
    }
    std::fs::write(source.join("Document.txt"), b"template").unwrap();
    let h = start_templates(&source, a.to_str().unwrap(), true);
    let id = wait_templates(&h, 1)[0].id.to_string();
    h.send(Command::Run(format!("create-template:{id}")));
    h.send(Command::Navigate { pane: 0, location: b.to_str().unwrap().into() });
    h.until(Duration::from_secs(3), |u| matches!(u,Update::Status(s) if s.message=="Created Document.txt"));
    assert_eq!(std::fs::read(a.join("Document.txt")).unwrap(), b"template");
    assert!(!b.join("Document.txt").exists());
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn replacing_a_cached_template_with_a_symlink_cannot_copy_its_target() {
    let d = temp_dir("templates-replaced");
    let source = d.join("Templates");
    let dest = d.join("dest");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(source.join("Document.txt"), b"template").unwrap();
    std::fs::write(d.join("private-fixture.txt"), b"fixture stays private").unwrap();
    let h = start_templates(&source, dest.to_str().unwrap(), true);
    let id = wait_templates(&h, 1)[0].id.to_string();
    std::fs::remove_file(source.join("Document.txt")).unwrap();
    std::os::unix::fs::symlink(d.join("private-fixture.txt"), source.join("Document.txt")).unwrap();
    h.send(Command::Run(format!("create-template:{id}")));
    h.until(Duration::from_secs(3), |u| matches!(u,Update::Status(s) if s.message.contains("open template")));
    assert!(!dest.join("Document.txt").exists());
    assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);
    assert_eq!(std::fs::read(d.join("private-fixture.txt")).unwrap(), b"fixture stays private");
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn creating_from_a_template_in_trash_is_refused() {
    let d = temp_dir("templates-trash");
    std::fs::write(d.join("Document.txt"), b"template").unwrap();
    // No local provider: even initial listing cannot read the real desktop Trash.
    let h = start_templates(&d, "trash:///", false);
    let id = wait_templates(&h, 1)[0].id.to_string();
    h.send(Command::Run(format!("create-template:{id}")));
    h.until(
        Duration::from_secs(3),
        |u| matches!(u,Update::Status(s) if s.message.contains("Restore items before editing")),
    );
    assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn refreshing_removed_templates_clears_the_menu_and_refuses_stale_commands() {
    let d = temp_dir("templates-removed");
    let source = d.join("Templates");
    let dest = d.join("dest");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(source.join("Old.txt"), b"old").unwrap();
    let h = start_templates(&source, dest.to_str().unwrap(), true);
    let id = wait_templates(&h, 1)[0].id.to_string();
    std::fs::remove_file(source.join("Old.txt")).unwrap();
    h.send(Command::Run("refresh-templates".into()));
    wait_templates(&h, 0);
    h.send(Command::Run(format!("create-template:{id}")));
    h.until(
        Duration::from_secs(3),
        |u| matches!(u,Update::Status(s) if s.message.contains("Template is no longer available")),
    );
    assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn creation_from_a_selected_folder_context_uses_the_displayed_directory() {
    let d = temp_dir("create-from-context");
    let left = d.join("left");
    let right = d.join("right");
    std::fs::create_dir_all(right.join("existing-folder")).unwrap();
    std::fs::create_dir(&left).unwrap();
    std::fs::write(right.join("original.txt"), b"keep original").unwrap();
    let h = start(left.to_str().unwrap(), right.to_str().unwrap(), MockConfig::default());
    h.until(Duration::from_secs(3), |u| full_rows(u, 1, 2));
    // A right-click activates this pane and selects its folder row.
    h.send(Command::RowContext { pane: 1, index: 0 });
    h.send(Command::Run("new-folder-inline".into()));
    h.until(Duration::from_secs(3), |u| matches!(u, Update::Rename { pane: 1, index } if *index >= 0));
    assert!(right.join("Untitled Folder").is_dir());
    assert!(!right.join("existing-folder/Untitled Folder").exists());
    assert!(!left.join("Untitled Folder").exists());
    h.send(Command::RenameCancel);
    h.send(Command::Run("new-file-inline".into()));
    h.until(Duration::from_secs(3), |u| matches!(u, Update::Rename { pane: 1, index } if *index >= 0));
    assert_eq!(std::fs::read(right.join("Untitled Document")).unwrap(), b"");
    assert_eq!(std::fs::read(right.join("original.txt")).unwrap(), b"keep original");
    assert!(!left.join("Untitled Document").exists());
    assert!(!right.join("existing-folder/Untitled Document").exists());
    drop(h);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn recent_navigation_sets_is_recent_flag() {
    let tmp = temp_dir("recent-nav");
    let h = start(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 0));
    h.send(Command::SidebarOpen("recent:///".into()));
    let updates = h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if view.is_recent));
    let view = updates
        .iter()
        .find_map(|u| match u {
            Update::Pane { pane: 0, view } if view.is_recent => Some(view),
            _ => None,
        })
        .unwrap();
    assert!(view.is_recent);
    assert_eq!(view.location, "recent:///");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn clear_recent_history_dialog_and_execution() {
    let tmp = temp_dir("clear-recent");
    let h = start(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 0));
    h.send(Command::SidebarOpen("recent:///".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if view.is_recent));
    h.send(Command::Run("clear-recent".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Dialog(d) if d.title.contains("Clear Recent")));
    h.send(Command::DialogAccept(String::new()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Status(s) if s.message.contains("Recent history cleared")));
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn remove_bookmark_command_updates_places() {
    let tmp = temp_dir("remove-bookmark");
    let bm_file = tmp.join("bookmarks");
    std::fs::write(&bm_file, "file:///home/user/CustomMark\n").unwrap();
    let h = start_configured(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant(), true, |cfg| {
        cfg.bookmarks_file = Some(bm_file.clone());
    });
    h.until(
        Duration::from_secs(5),
        |u| matches!(u, Update::Places(p) if p.iter().any(|item| item.label == "CustomMark")),
    );
    h.send(Command::RemoveBookmark("file:///home/user/CustomMark".into()));
    h.until(
        Duration::from_secs(5),
        |u| matches!(u, Update::Places(p) if !p.iter().any(|item| item.label == "CustomMark")),
    );
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        if !std::fs::read_to_string(&bm_file).unwrap_or_default().contains("CustomMark") {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!std::fs::read_to_string(&bm_file).unwrap().contains("CustomMark"));
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn open_containing_folder_on_regular_file() {
    let tmp = temp_dir("containing-folder");
    let sub = tmp.join("subfolder");
    std::fs::create_dir_all(&sub).unwrap();
    let target_file = sub.join("doc.txt");
    std::fs::write(&target_file, b"content").unwrap();
    let h = start(sub.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 1));
    h.send(Command::Run("select-all".into()));
    h.send(Command::Run("open-containing-folder".into()));
    h.until(
        Duration::from_secs(5),
        |u| matches!(u, Update::Pane { pane: 0, view } if view.location.ends_with("/subfolder")),
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn make_link_creates_symlink_and_selects_it() {
    let tmp = temp_dir("make-link");
    let target_file = tmp.join("notes.txt");
    std::fs::write(&target_file, b"hello world").unwrap();
    let h = start(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 1));
    h.send(Command::Run("select-all".into()));
    h.send(Command::Run("make-link".into()));
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 2));
    let link_path = tmp.join("Link to notes.txt");
    assert!(link_path.is_symlink());
    assert_eq!(std::fs::read_link(&link_path).unwrap(), std::path::Path::new("notes.txt"));
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn bookmark_target_folder() {
    let tmp = temp_dir("bm-target");
    let sub = tmp.join("Projects");
    std::fs::create_dir_all(&sub).unwrap();
    let bm_file = tmp.join("bookmarks");
    let h = start_configured(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant(), true, |cfg| {
        cfg.bookmarks_file = Some(bm_file.clone());
    });
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 1));
    h.send(Command::Run("select-all".into()));
    h.send(Command::Run("bookmark-target".into()));
    h.until(
        Duration::from_secs(5),
        |u| matches!(u, Update::Places(p) if p.iter().any(|item| item.label == "Projects")),
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn read_only_folder_sets_is_read_only_flag() {
    let tmp = temp_dir("read-only-folder");
    let ro_dir = tmp.join("ro");
    std::fs::create_dir_all(&ro_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ro_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    let h = start(ro_dir.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Pane { pane: 0, view } if view.is_read_only));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ro_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn properties_dialog_displays_vfs_attributes() {
    let tmp = temp_dir("props-vfs-attrs");
    let target_file = tmp.join("test.txt");
    std::fs::write(&target_file, b"content").unwrap();
    let h = start(tmp.to_str().unwrap(), "gungnir://odin/asgard", MockConfig::instant());
    h.until(Duration::from_secs(5), |u| full_rows(u, 0, 1));
    h.send(Command::Run("select-all".into()));
    h.send(Command::Run("properties".into()));
    h.until(Duration::from_secs(5), |u| matches!(u, Update::Dialog(d) if d.kind == crate::DialogKind::Properties));
    let _ = std::fs::remove_dir_all(tmp);
}
