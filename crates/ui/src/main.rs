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

mod actions;
mod bridge;
mod clipboard;
mod controller;
#[cfg(test)]
mod controller_tests;
mod desktop;
mod drives;
mod hash;
mod init;
mod rename;
mod rows;
mod session;
mod templates;
mod thumbnails;
pub mod updater;

use std::sync::Arc;

use backends::gungnir::proto::socket_path_from_env;
use backends::{GungnirVfsBackend, LocalPosixBackend, MockGungnirBackend, RemoteVfsBackend};
use core_vfs::{VfsRegistry, VfsUri};
use init::InitConfig;
use slint::{ComponentHandle, Model};
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
    on!(on_tab_cycle, |p, d| Command::TabCycle { pane: pane(p), delta: d });
    on!(on_custom_action, |p, id| Command::CustomAction { pane: pane(p), id: id.to_string() });
    on!(on_pane_focused, |p| Command::PaneFocused(pane(p)));
    on!(on_command, |name| Command::Run(name.to_string()));
    on!(on_view_mode_changed, |m| Command::ViewMode(m));
    on!(on_zoom_changed, |z| Command::Zoom(z));
    on!(on_sidebar_open, |uri| Command::SidebarOpen(uri.to_string()));
    on!(on_sidebar_open_tab, |uri| Command::SidebarOpenTab(uri.to_string()));
    on!(on_sidebar_open_window, |uri| Command::SidebarOpenWindow(uri.to_string()));
    on!(on_tree_toggle, |i| Command::TreeToggle(index(i)));
    on!(on_sidebar_mode_changed, |m| Command::SidebarMode(m));
    on!(on_transfer_action, |id, a| Command::TransferAction { id: i64::from(id), action: a.to_string() });
    on!(on_dialog_accept, |t| Command::DialogAccept(t.to_string()));
    on!(on_dialog_cancel, || Command::DialogCancel);
    ui.on_dialog_custom_action({
        let tx = tx.clone();
        move |a, opt1, opt2, ex1, ex2| {
            send(
                &tx,
                Command::DialogCustomAction {
                    action: a.to_string(),
                    opt1,
                    opt2,
                    extra1: ex1.to_string(),
                    extra2: ex2.to_string(),
                },
            );
        }
    });
    on!(on_terminal_run, |cmd| Command::TerminalRun(cmd.to_string()));
    on!(on_terminal_clear, || Command::TerminalClear);
    on!(on_dark_mode_changed, |dark| Command::SetDarkMode(dark));
    on!(on_toggle_dark_mode, || Command::ToggleDarkMode);
    // Drag payload: construct full DataTransfer with local file paths and shell-safe plain text.
    // Enables native drag-and-drop to terminals (pasting paths), editors, web browsers, and file managers.
    let ui_weak = ui.as_weak();
    ui.on_drag_data(move |p, i| {
        let mut dt = slint::DataTransfer::default();
        let Some(ui) = ui_weak.upgrade() else { return dt };
        let files = if p == 0 { ui.get_left_files() } else { ui.get_right_files() };
        let Ok(idx) = usize::try_from(i) else { return dt };
        let Some(row) = files.row_data(idx) else { return dt };

        let mut local_paths = Vec::new();
        let mut uri_strings = Vec::new();

        if row.selected {
            for j in 0..files.row_count() {
                if let Some(r) = files.row_data(j)
                    && r.selected
                {
                    let u_str = r.uri.to_string();
                    if let Ok(uri) = core_vfs::VfsUri::parse(&u_str) {
                        if let Some(lp) = uri.to_local_path() {
                            local_paths.push(lp);
                        } else {
                            uri_strings.push(u_str);
                        }
                    } else {
                        uri_strings.push(u_str);
                    }
                }
            }
        }

        if local_paths.is_empty() && uri_strings.is_empty() {
            let u_str = row.uri.to_string();
            if let Ok(uri) = core_vfs::VfsUri::parse(&u_str) {
                if let Some(lp) = uri.to_local_path() {
                    local_paths.push(lp);
                } else {
                    uri_strings.push(u_str);
                }
            } else {
                uri_strings.push(u_str);
            }
        }

        if !local_paths.is_empty() {
            dt.set_file_paths(local_paths.iter().map(|p| p.as_path()));
            let formatted: Vec<String> = local_paths
                .iter()
                .map(|p| {
                    let s = p.to_string_lossy();
                    let needs_quotes = s.chars().any(|c| {
                        c.is_whitespace()
                            || matches!(
                                c,
                                '\'' | '\"'
                                    | '\\'
                                    | '$'
                                    | '`'
                                    | '&'
                                    | ';'
                                    | '|'
                                    | '<'
                                    | '>'
                                    | '('
                                    | ')'
                                    | '['
                                    | ']'
                                    | '{'
                                    | '}'
                                    | '*'
                                    | '?'
                                    | '~'
                                    | '!'
                            )
                    });
                    if needs_quotes { format!("'{}'", s.replace('\'', "'\\''")) } else { s.into_owned() }
                })
                .collect();
            dt.set_plain_text(slint::SharedString::from(formatted.join(" ")));
        } else if !uri_strings.is_empty() {
            dt.set_plain_text(slint::SharedString::from(uri_strings.join(" ")));
        }
        dt
    });
    on!(on_dialog_choose, |i| Command::DialogChoose(index(i)));
    on!(on_device_open, |o| Command::DeviceOpen(o.to_string()));
    on!(on_device_eject, |o| Command::DeviceEject(o.to_string()));
    on!(on_dropped, |sp, si, dp, di, copy| Command::Drop {
        src_pane: pane(sp),
        src_index: index(si),
        dst_pane: pane(dp),
        dst_index: usize::try_from(di).ok(),
        copy
    });
    on!(on_rename_commit, |p, i, n| Command::RenameCommit { pane: pane(p), index: index(i), name: n.to_string() });
    on!(on_rename_cancel, || Command::RenameCancel);
    on!(on_trigger_update, || Command::TriggerUpdate);
}

fn setup_display_environment() {
    // Provide standard ICCCM WM_CLASS ("gungnir-fm", "gungnir-fm") for X11 window managers and compositors
    // (dwm, i3, bspwm, xmonad, awesome, qtile, leftwm, herbstluftwm, spectrwm, ratpoison, stumpwm, cwm,
    // twm, mwm, openbox, fluxbox, blackbox, icewm, jwm, window maker, afterstep, fvwm, pekwm, kwin, mutter, compiz, etc.)
    if std::env::var_os("RESOURCE_NAME").is_none() {
        // SAFETY: Called synchronously at main entry before worker threads are spawned.
        unsafe {
            std::env::set_var("RESOURCE_NAME", "gungnir-fm");
        }
    }
    if std::env::var_os("XCLIENT_NAME").is_none() {
        // SAFETY: Called synchronously at main entry before worker threads are spawned.
        unsafe {
            std::env::set_var("XCLIENT_NAME", "gungnir-fm");
        }
    }

    // Clear DESKTOP_STARTUP_ID so child processes or terminal sessions don't inherit a stale startup ID
    if std::env::var_os("DESKTOP_STARTUP_ID").is_some() {
        // SAFETY: Called synchronously at main entry before worker threads are spawned.
        unsafe {
            std::env::remove_var("DESKTOP_STARTUP_ID");
        }
    }

    // DirectFB / Linux Framebuffer (fbdev / direct DRM/KMS) & kmscon support:
    // If neither X11 nor Wayland is running, attempt linuxkms backend if DRM/KMS or framebuffer is present.
    if std::env::var_os("DISPLAY").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
        && std::env::var_os("SLINT_BACKEND").is_none()
    {
        let has_drm = (0..8).any(|i| std::path::Path::new(&format!("/dev/dri/card{i}")).exists());
        let has_fb = (0..8).any(|i| std::path::Path::new(&format!("/dev/fb{i}")).exists());
        if has_drm || has_fb {
            // SAFETY: Called synchronously at main entry before worker threads are spawned.
            unsafe {
                std::env::set_var("SLINT_BACKEND", "linuxkms");
            }
        }
    }
}

fn create_app_window() -> Result<AppWindow, Box<dyn std::error::Error>> {
    match AppWindow::new() {
        Ok(ui) => Ok(ui),
        Err(err) => {
            // Virtual/headless X servers (Xvfb, Xdummy, TinyX/KDrive, nxagent/X2Go, software Xephyr)
            // often lack hardware GLX/OpenGL support. Fallback to Slint's software renderer.
            let is_software = std::env::var("SLINT_BACKEND").map(|v| v == "winit-software").unwrap_or(false);
            if !is_software {
                eprintln!(
                    "gungnir-fm: default display backend initialization failed ({err}), falling back to software renderer (SLINT_BACKEND=winit-software)"
                );
                // SAFETY: Called on main thread to switch rendering backend.
                unsafe {
                    std::env::set_var("SLINT_BACKEND", "winit-software");
                }
                match AppWindow::new() {
                    Ok(ui) => Ok(ui),
                    Err(fallback_err) => Err(format!(
                        "gungnir-fm: failed to initialize display window.\n\
                             Neither hardware nor software display rendering could be started.\n\
                             Primary error: {err}\n\
                             Fallback error: {fallback_err}\n\
                             Ensure a valid display server (Wayland, X11, or DRM/KMS framebuffer) is accessible."
                    )
                    .into()),
                }
            } else {
                Err(Box::new(err))
            }
        }
    }
}

fn autostart_gungnird() {
    // 1. Try systemd user socket if running under systemd on Linux
    #[cfg(target_os = "linux")]
    {
        if std::path::Path::new("/run/systemd/system").exists() {
            let systemd_ok = std::process::Command::new("systemctl")
                .args(["--user", "start", "gungnird.socket"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);

            if systemd_ok {
                return;
            }
        }
    }

    // 2. Direct spawn fallback across all Linux and BSD distributions
    // (OpenRC, runit, s6, dinit, SysVinit, FreeBSD, OpenBSD, NetBSD, DragonFly BSD)
    let mut candidates = vec![
        dirs::home_dir().map(|h| h.join(".local/bin/gungnird")),
        Some(std::path::PathBuf::from("/usr/local/bin/gungnird")),
        Some(std::path::PathBuf::from("/usr/pkg/bin/gungnird")),
        Some(std::path::PathBuf::from("/opt/local/bin/gungnird")),
        Some(std::path::PathBuf::from("/usr/bin/gungnird")),
    ];

    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            candidates.push(Some(dir.join("gungnird")));
        }
    }

    for candidate in candidates.into_iter().flatten() {
        if candidate.is_file() {
            let mut cmd = std::process::Command::new(candidate);
            cmd.args(["--idle-timeout", "0"]);
            #[cfg(target_os = "linux")]
            cmd.arg("--seccomp");

            let _ = cmd
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            break;
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let mut init_cfg = InitConfig::default();
    let mut positional_args: Vec<String> = Vec::new();

    let mut i = 0;
    while i < raw_args.len() {
        let a = &raw_args[i];
        match a.as_str() {
            "--help" | "-h" => {
                println!(
                    "Gungnir FM {} - Fast dual-pane Linux file manager\n\n\
                     USAGE:\n    \
                         gungnir-fm [OPTIONS] [LEFT_PATH] [RIGHT_PATH]\n\n\
                     ARGUMENTS:\n    \
                         [LEFT_PATH]     Initial path or URI for the primary (left) pane\n    \
                         [RIGHT_PATH]    Initial path or URI for the secondary (right) pane\n\n\
                     OPTIONS:\n    \
                         -h, --help       Print this help message\n    \
                         -V, --version    Print version information\n    \
                         -f, --foreground Run in foreground (default for process supervisors)\n    \
                         -d, --daemon     Fork into background (daemonize for classic SysV/BSD)\n    \
                         --pidfile PATH   Write process PID to file\n    \
                         --ready-fd FD    Write ready notification byte to inherited file descriptor\n\n\
                     DISPLAY TARGETS:\n    \
                         Natively supports Wayland, X11 (X.Org, XLibre, XFree86, Phoenix X,\n    \
                         TinyX/KDrive, Xephyr, Xvfb, Xvnc, nxagent, Xdummy), direct Linux\n    \
                         Framebuffer / DRM/KMS, Arcan, Mir, and all standard Linux window\n    \
                         managers and compositors (i3, dwm, bspwm, awesome, xmonad, openbox,\n    \
                         kwin, mutter, etc.).\n\n\
                     INIT SYSTEMS & SUPERVISORS:\n    \
                         Natively compatible with systemd, OpenRC, runit, s6, dinit, SysVinit,\n    \
                         BSD rc.subr, GNU Shepherd, BusyBox/Toybox init, Finit, procd, sinit,\n    \
                         nosh, daemontools, perp, anopa, cinit, initng, Upstart, OpenLaunchD,\n    \
                         Epoch, minit, simpleinit-msb.\n\n\
                     ENVIRONMENT:\n    \
                         GUNGNIR_FM_BACKEND  Backend selection: auto, live, mock, off, live-or-off\n    \
                         GUNGNIR_SOCKET      Custom gungnird UNIX socket path\n    \
                         SLINT_BACKEND       Slint backend: winit, winit-software, linuxkms\n    \
                         GUNGNIR_FM_TRACE=1  Log listing timings to stderr",
                    env!("CARGO_PKG_VERSION")
                );
                return Ok(());
            }
            "--version" | "-V" => {
                println!("gungnir-fm {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "-f" | "--foreground" => {
                init_cfg.daemon = false;
            }
            "-d" | "--daemon" => {
                init_cfg.daemon = true;
            }
            "--pidfile" => {
                i += 1;
                if i < raw_args.len() {
                    init_cfg.pidfile = Some(std::path::PathBuf::from(&raw_args[i]));
                }
            }
            "--ready-fd" => {
                i += 1;
                if i < raw_args.len() {
                    init_cfg.ready_fd = raw_args[i].parse::<i32>().ok();
                }
            }
            other => {
                if !other.starts_with('-') {
                    positional_args.push(other.to_string());
                }
            }
        }
        i += 1;
    }

    init::daemonize_if_requested(&init_cfg)?;
    let _pidfile_guard = init::create_pidfile(init_cfg.pidfile.as_deref())?;

    setup_display_environment();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("gungnir-fm-worker")
        .enable_all()
        .build()?;
    let _guard = rt.enter();

    let home_path = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let home = VfsUri::parse(&home_path)
        .unwrap_or_else(|_| VfsUri::from_local_path(std::path::Path::new("/")).unwrap_or_else(|_| unreachable_root()));
    let mode = std::env::var("GUNGNIR_FM_BACKEND").unwrap_or_else(|_| "auto".into());

    // Pick the Gungnir backend before the window exists (bounded: 500 ms handshake).
    let mut registry = VfsRegistry::new();
    registry.register(Arc::new(LocalPosixBackend::new()));
    registry.register(Arc::new(RemoteVfsBackend::sftp()));
    registry.register(Arc::new(RemoteVfsBackend::smb()));
    registry.register(Arc::new(RemoteVfsBackend::network()));
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
            Ok(sock) => {
                let first_try = rt.block_on(GungnirVfsBackend::connect(sock.clone()));
                match first_try {
                    Ok(b) => Some(b),
                    Err(_) => {
                        // Daemon was not running: attempt to auto-start it
                        autostart_gungnird();
                        // Brief retry loop for the socket to initialize
                        let mut connected = None;
                        for _ in 0..10 {
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            if sock.exists()
                                && let Ok(b) = rt.block_on(GungnirVfsBackend::connect(sock.clone()))
                            {
                                connected = Some(b);
                                break;
                            }
                        }
                        if connected.is_none() && mode == "live" {
                            Some(GungnirVfsBackend::new(sock))
                        } else {
                            connected
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("gungnir-fm: {e}");
                None
            }
        }
    };
    let (gungnir_root, gungnir_label, right_default) = match live {
        Some(b) => {
            let label = "GUNGNIR VFS · LIVE".to_owned();
            // Ahead of the local backend: the daemon serves `trash:` for every volume, and
            // falls back to the same local code when it is not there.
            registry.register_front(Arc::new(b));
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

    let args = positional_args;
    // Arguments may be URIs, absolute paths, or paths relative to the working directory.
    let resolve_arg = |i: usize| -> (Option<VfsUri>, Option<String>) {
        let Some(a) = args.get(i) else {
            return (None, None);
        };
        let parsed = if a.starts_with('/') || a.contains("://") {
            VfsUri::parse(a)
        } else {
            let abs = match std::env::current_dir() {
                Ok(cwd) => cwd.join(a),
                Err(_) => std::path::PathBuf::from(a),
            };
            VfsUri::from_local_path(&std::fs::canonicalize(&abs).unwrap_or(abs))
        };
        let uri = match parsed {
            Ok(u) => u,
            Err(e) => {
                eprintln!("gungnir-fm: {a}: {e}");
                return (None, None);
            }
        };

        // If the argument points to a file, open its containing folder and select the file,
        // matching standard desktop file manager behavior.
        if uri.scheme() == core_vfs::Scheme::File
            && let Some(local_path) = uri.to_local_path()
        {
            if local_path.is_file() {
                if let Some(parent) = local_path.parent()
                    && let Ok(parent_uri) = VfsUri::from_local_path(parent)
                {
                    let filename = local_path.file_name().and_then(|n| n.to_str()).map(str::to_owned);
                    return (Some(parent_uri), filename);
                }
            } else if !local_path.exists()
                && let Some(parent) = local_path.parent()
                && parent.is_dir()
                && let Some(requested_name) = local_path.file_name().and_then(|n| n.to_str())
            {
                let req_lower = requested_name.to_lowercase();
                if let Ok(entries) = std::fs::read_dir(parent) {
                    for entry in entries.flatten() {
                        let actual_name = entry.file_name();
                        if actual_name.to_string_lossy().to_lowercase() == req_lower
                            && let Ok(parent_uri) = VfsUri::from_local_path(parent)
                        {
                            return (Some(parent_uri), Some(actual_name.to_string_lossy().into_owned()));
                        }
                    }
                }
                if local_path.extension().is_some()
                    && let Ok(parent_uri) = VfsUri::from_local_path(parent)
                {
                    return (Some(parent_uri), Some(requested_name.to_owned()));
                }
            }
        }

        (Some(uri), None)
    };

    let (left_arg, left_select) = resolve_arg(0);
    let (right_arg, right_select) = resolve_arg(1);

    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| std::path::Path::new(&home_path).join(".config"));
    let actions_dir = config_dir.join("gungnir-fm/actions");
    let session_file = config_dir.join("gungnir-fm/session.json");
    let saved_session = crate::session::load(&session_file).ok();

    let left = left_arg.unwrap_or_else(|| {
        saved_session
            .as_ref()
            .and_then(|s| s.primary_pane_tabs.get_paths().first().cloned())
            .map(|p| crate::session::resolve_path_with_fallback(&p, &home))
            .unwrap_or_else(|| home.clone())
    });
    let right = right_arg.unwrap_or_else(|| {
        saved_session
            .as_ref()
            .and_then(|s| s.secondary_pane_tabs.as_ref())
            .and_then(|s| s.get_paths().first().cloned())
            .map(|p| crate::session::resolve_path_with_fallback(&p, &home))
            .unwrap_or(right_default)
    });
    let initial_session =
        crate::session::startup_state(saved_session.clone(), &left, &right, !args.is_empty(), args.len() > 1);
    let cfg = Config {
        left,
        right,
        left_select,
        right_select,
        home,
        gungnir_root,
        gungnir_label,
        bookmarks_file: Some(config_dir.join("gungnir-fm/bookmarks")),
        actions_dir: Some(actions_dir),
        templates_dir: dirs::template_dir().filter(|path| path != std::path::Path::new(&home_path)),
        session_file: Some(session_file),
        initial_session: Some(initial_session),
        explicit_locations: !args.is_empty(),
        desktop_dirs: None,
        trace: std::env::var_os("GUNGNIR_FM_TRACE").is_some(),
    };

    let ui = create_app_window()?;
    if let Some(ref s) = saved_session {
        if s.window.width >= 200 && s.window.height >= 200 {
            ui.window().set_size(slint::LogicalSize::new(s.window.width as f32, s.window.height as f32));
        }
        if s.window.maximized {
            ui.window().set_maximized(true);
        }
    }
    let initial_dark = saved_session.as_ref().map(|s| s.dark_mode).unwrap_or(true);
    ui.set_dark_mode(initial_dark);
    let sink = Arc::new(SlintSink::attach(&ui));
    let (tx, rx) = mpsc::channel(QUEUE);
    wire(&ui, &tx);
    let controller = Controller::new(registry, sink, tx.clone(), cfg);
    let controller_task = rt.spawn(controller.run(rx));
    // Capture geometry on the UI thread; persistence itself runs on a worker.
    let window_timer = slint::Timer::default();
    let weak = ui.as_weak();
    let window_tx = tx.clone();
    let mut normal_size = saved_session.as_ref().map(|s| s.window.clone()).unwrap_or_default();
    let mut previous: Option<session::WindowState> = None;
    window_timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(250), move || {
        let Some(ui) = weak.upgrade() else { return };
        let window = ui.window();
        let size = window.size().to_logical(window.scale_factor());
        let mut state = session::WindowState {
            width: size.width as u32,
            height: size.height as u32,
            maximized: window.is_maximized(),
        };
        // Keep the normal size when maximized or minimized so unmaximizing restores it.
        if state.maximized || state.width < 200 || state.height < 200 {
            state.width = normal_size.width;
            state.height = normal_size.height;
        } else {
            normal_size = state.clone();
        }
        if previous.as_ref() != Some(&state)
            && window_tx.try_send(Msg::Ui(Command::WindowChanged(state.clone()))).is_ok()
        {
            previous = Some(state);
        }
    });
    let close_tx = tx.clone();
    ui.window().on_close_requested(move || {
        send(&close_tx, Command::Quit);
        slint::CloseRequestResponse::KeepWindowShown
    });

    init::notify_ready(&init_cfg);

    // Graceful exit hooks (SIGINT, SIGTERM)
    let sig_tx = tx.clone();
    rt.spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => return,
        };
        tokio::select! {
            _ = sigint.recv() => {
                let _ = sig_tx.send(Msg::Ui(Command::Quit)).await;
            }
            _ = sigterm.recv() => {
                let _ = sig_tx.send(Msg::Ui(Command::Quit)).await;
            }
        }
    });

    ui.run()?;
    rt.block_on(async {
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let _ = tx.send(Msg::Ui(Command::Quit)).await;
            let _ = controller_task.await;
        })
        .await;
        if result.is_err() {
            eprintln!("gungnir-fm: timed out waiting for session save on exit");
        }
    });
    drop(ui);
    rt.shutdown_timeout(std::time::Duration::from_millis(500));
    Ok(())
}

fn unreachable_root() -> VfsUri {
    // "/" always parses; this only satisfies the type checker without unwrap.
    VfsUri::gungnir("", "/").unwrap_or_else(|_| std::process::exit(2))
}
