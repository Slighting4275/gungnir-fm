//! Gungnir FM: a fast dual-pane Linux file manager (Rust + Slint), companion GUI of
//! Gungnir VFS.
//!
//! Threads: the Slint event loop owns the window and does nothing but apply
//! updates; a multi-threaded tokio runtime runs the controller actor, the backends and
//! all sorting/filtering/formatting (`spawn_blocking`). UI callbacks reach the
//! controller through a bounded channel (`try_send`, never blocking the UI thread);
//! updates come back only through `slint::invoke_from_event_loop`.
//!
//! Environment:
//! - `GUNGNIR_FM_BACKEND` picks the Gungnir backend:
//!   - `auto` (default): the live gungnird if it answers its handshake within 500 ms,
//!     else the mock backend (development);
//!   - `live-or-off`: the live gungnird if it answers, else no Gungnir at all (daily use);
//!   - `live`, `mock`, `off`: always that one (`live` keeps retrying a missing daemon).
//! - `GUNGNIR_SOCKET`: the gungnird socket (else gungnird's own default path).
//! - `GUNGNIR_FM_TRACE=1`: print listing timings to stderr.
//!
//! Arguments: `gungnir-fm [LEFT] [RIGHT]` (paths or URIs).

mod bridge;
mod controller;
#[cfg(test)]
mod controller_tests;
mod rows;

use std::sync::Arc;

use backends::gungnir::proto::socket_path_from_env;
use backends::{GungnirVfsBackend, LocalPosixBackend, MockGungnirBackend};
use core_vfs::{VfsRegistry, VfsUri};
use slint::ComponentHandle;
use tokio::sync::mpsc;

use bridge::SlintSink;
use controller::{Command, Config, Controller, Msg};

// Code generated from ui/*.slint. The workspace denies `unwrap`/`expect`; the generator
// uses them internally, so the allowance is scoped to the generated module only.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::all, clippy::pedantic)]
mod generated {
    slint::include_modules!();
}
pub use generated::*;

/// Commands queued from the UI; a full queue drops the command rather than stall.
const QUEUE: usize = 4_096;

fn pane(p: i32) -> usize {
    usize::from(p == 1)
}

fn index(i: i32) -> usize {
    usize::try_from(i).unwrap_or(0)
}

fn send(tx: &mpsc::Sender<Msg>, c: Command) {
    if let Err(e) = tx.try_send(Msg::Ui(c)) {
        eprintln!("gungnir-fm: command dropped ({e})");
    }
}

fn wire(ui: &AppWindow, tx: &mpsc::Sender<Msg>) {
    macro_rules! on {
        ($method:ident, || $cmd:expr) => {{
            let tx = tx.clone();
            ui.$method(move || send(&tx, $cmd));
        }};
        ($method:ident, |$($arg:ident),*| $cmd:expr) => {{
            let tx = tx.clone();
            ui.$method(move |$($arg),*| send(&tx, $cmd));
        }};
    }
    on!(on_navigate, |p, loc| Command::Navigate { pane: pane(p), location: loc.to_string() });
    on!(on_nav, |p, action| Command::Nav { pane: pane(p), action: action.to_string() });
    on!(on_row_clicked, |p, i, ctrl, shift| Command::RowClicked { pane: pane(p), index: index(i), ctrl, shift });
    on!(on_row_activated, |p, i| Command::RowActivated { pane: pane(p), index: index(i) });
    on!(on_row_context, |p, i| Command::RowContext { pane: pane(p), index: index(i) });
    on!(on_move_cursor, |p, d, extend| Command::MoveCursor { pane: pane(p), delta: i64::from(d), extend });
    on!(on_sort_by, |p, c| Command::SortBy { pane: pane(p), column: c });
    on!(on_filter_changed, |p, t| Command::Filter { pane: pane(p), text: t.to_string() });
    on!(on_tab_new, |p| Command::TabNew(pane(p)));
    on!(on_tab_close, |p, i| Command::TabClose(pane(p), index(i)));
    on!(on_tab_select, |p, i| Command::TabSelect(pane(p), index(i)));
    on!(on_pane_focused, |p| Command::PaneFocused(pane(p)));
    on!(on_command, |name| Command::Run(name.to_string()));
    on!(on_view_mode_changed, |m| Command::ViewMode(m));
    on!(on_sidebar_open, |uri| Command::SidebarOpen(uri.to_string()));
    on!(on_tree_toggle, |i| Command::TreeToggle(index(i)));
    on!(on_sidebar_mode_changed, |m| Command::SidebarMode(m));
    on!(on_transfer_action, |id, a| Command::TransferAction { id: i64::from(id), action: a.to_string() });
    on!(on_dialog_accept, |t| Command::DialogAccept(t.to_string()));
    on!(on_dialog_cancel, || Command::DialogCancel);
    on!(on_rename_commit, |p, i, n| Command::RenameCommit { pane: pane(p), index: index(i), name: n.to_string() });
    on!(on_rename_cancel, || Command::RenameCancel);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("gungnir-fm-worker")
        .enable_all()
        .build()?;

    let home_path = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let home = VfsUri::parse(&home_path)
        .unwrap_or_else(|_| VfsUri::from_local_path(std::path::Path::new("/")).unwrap_or_else(|_| unreachable_root()));
    let mode = std::env::var("GUNGNIR_FM_BACKEND").unwrap_or_else(|_| "auto".into());

    // Pick the Gungnir backend before the window exists (bounded: 500 ms handshake).
    let mut registry = VfsRegistry::new();
    registry.register(Arc::new(LocalPosixBackend::new()));
    let mode = match mode.as_str() {
        "auto" | "live" | "mock" | "off" | "live-or-off" => mode,
        other => {
            eprintln!("gungnir-fm: unknown GUNGNIR_FM_BACKEND={other:?}, using auto");
            "auto".to_owned()
        }
    };
    let live = if matches!(mode.as_str(), "mock" | "off") {
        None
    } else {
        match socket_path_from_env() {
            Ok(sock) => match rt.block_on(GungnirVfsBackend::connect(sock.clone())) {
                Ok(b) => Some(b),
                Err(e) if mode == "live" => {
                    eprintln!("gungnir-fm: live gungnird unavailable ({e}); it will be retried on use");
                    Some(GungnirVfsBackend::new(sock))
                }
                Err(_) => None,
            },
            Err(e) => {
                eprintln!("gungnir-fm: {e}");
                None
            }
        }
    };
    let (gungnir_root, gungnir_label, right_default) = match live {
        Some(b) => {
            let label = "GUNGNIR VFS · LIVE".to_owned();
            registry.register(Arc::new(b));
            let root = VfsUri::gungnir("", "/")?;
            (root, label, VfsUri::gungnir("", home.path())?)
        }
        // No Gungnir at all: local files only, and the sidebar section is hidden.
        None if matches!(mode.as_str(), "off" | "live-or-off") => (home.clone(), String::new(), home.clone()),
        None => {
            registry.register(Arc::new(MockGungnirBackend::default()));
            (VfsUri::gungnir("odin", "/")?, "GUNGNIR VFS · MOCK".to_owned(), VfsUri::gungnir("odin", "/asgard")?)
        }
    };

    let args: Vec<String> = std::env::args().skip(1).collect();
    // Arguments may be URIs, absolute paths, or paths relative to the working directory.
    let arg_uri = |i: usize| {
        let a = args.get(i)?;
        let parsed = if a.starts_with('/') || a.contains("://") {
            VfsUri::parse(a)
        } else {
            let abs = std::env::current_dir().ok()?.join(a);
            VfsUri::from_local_path(&std::fs::canonicalize(&abs).unwrap_or(abs))
        };
        parsed.map_err(|e| eprintln!("gungnir-fm: {a}: {e}")).ok()
    };
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| std::path::Path::new(&home_path).join(".config"));
    let cfg = Config {
        left: arg_uri(0).unwrap_or_else(|| home.clone()),
        right: arg_uri(1).unwrap_or(right_default),
        home,
        gungnir_root,
        gungnir_label,
        bookmarks_file: Some(config_dir.join("gungnir-fm/bookmarks")),
        trace: std::env::var_os("GUNGNIR_FM_TRACE").is_some(),
    };

    let ui = AppWindow::new()?;
    let sink = Arc::new(SlintSink::attach(&ui));
    let (tx, rx) = mpsc::channel(QUEUE);
    wire(&ui, &tx);
    let controller = Controller::new(registry, sink, tx, cfg);
    rt.spawn(controller.run(rx));

    ui.run()?;
    drop(ui);
    rt.shutdown_timeout(std::time::Duration::from_millis(500));
    Ok(())
}

fn unreachable_root() -> VfsUri {
    // "/" always parses; this only satisfies the type checker without unwrap.
    VfsUri::gungnir("", "/").unwrap_or_else(|_| std::process::exit(2))
}
