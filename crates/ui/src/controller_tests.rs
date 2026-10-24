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
