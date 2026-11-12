//! Session persistence and crash recovery.
//!
//! Serializes window and navigation state to `~/.config/gungnir-fm/session.json`.
//! Uses exclusive temporary sibling files renamed to the session file and a 500ms
//! debounced background saver.

use core_vfs::VfsUri;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WindowState {
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
}

fn default_width() -> u32 {
    1320
}

fn default_height() -> u32 {
    820
}

impl Default for WindowState {
    fn default() -> Self {
        Self { width: default_width(), height: default_height(), maximized: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TabRecord {
    pub path: String,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum PaneTabsState {
    Structured {
        #[serde(default)]
        paths: Vec<String>,
        #[serde(default)]
        active_index: usize,
        #[serde(default)]
        tabs: Vec<TabRecord>,
    },
    Paths(Vec<String>),
    Records(Vec<TabRecord>),
}

impl PaneTabsState {
    #[must_use]
    pub fn new(paths: Vec<String>, active_index: usize) -> Self {
        Self::Structured { paths, active_index, tabs: Vec::new() }
    }

    #[must_use]
    pub fn get_paths(&self) -> Vec<String> {
        match self {
            Self::Structured { paths, tabs, .. } => {
                if !paths.is_empty() {
                    paths.clone()
                } else {
                    tabs.iter().map(|t| t.path.clone()).collect()
                }
            }
            Self::Paths(p) => p.clone(),
            Self::Records(r) => r.iter().map(|t| t.path.clone()).collect(),
        }
    }

    #[must_use]
    pub fn get_active_index(&self) -> usize {
        match self {
            Self::Structured { active_index, .. } => *active_index,
            Self::Paths(_) | Self::Records(_) => 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionState {
    #[serde(default)]
    pub window: WindowState,
    #[serde(default)]
    pub split_view: bool,
    #[serde(default)]
    pub active_pane: usize,
    pub primary_pane_tabs: PaneTabsState,
    #[serde(default)]
    pub secondary_pane_tabs: Option<PaneTabsState>,
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default = "default_true")]
    pub sidebar_visible: bool,
    #[serde(default)]
    pub sidebar_mode: i32,
    #[serde(default = "default_zoom")]
    pub zoom_level: f32,
    #[serde(default = "default_true")]
    pub dark_mode: bool,
    #[serde(default = "default_view_mode")]
    pub view_mode: i32,
}

/// Explicit startup paths replace navigation state while retaining view preferences.
pub fn startup_state(
    saved: Option<SessionState>,
    left: &VfsUri,
    right: &VfsUri,
    explicit_paths: bool,
    second_path: bool,
) -> SessionState {
    let mut state = saved.unwrap_or_else(|| SessionState {
        window: WindowState::default(),
        split_view: false,
        active_pane: 0,
        primary_pane_tabs: PaneTabsState::new(vec![left.to_uri_string()], 0),
        secondary_pane_tabs: None,
        show_hidden: false,
        sidebar_visible: true,
        sidebar_mode: 0,
        zoom_level: 1.0,
        dark_mode: true,
        view_mode: default_view_mode(),
    });
    if explicit_paths {
        state.primary_pane_tabs = PaneTabsState::new(vec![left.to_uri_string()], 0);
        state.secondary_pane_tabs = Some(PaneTabsState::new(vec![right.to_uri_string()], 0));
        state.active_pane = 0;
    }
    if second_path {
        state.split_view = true;
    }
    state
}

fn default_true() -> bool {
    true
}

fn default_view_mode() -> i32 {
    1
}

fn default_zoom() -> f32 {
    1.0
}

/// Atomically write state to a JSON file via a temporary sibling file.
pub fn save_atomic(path: &Path, state: &SessionState) -> std::io::Result<()> {
    let json_bytes =
        serde_json::to_vec_pretty(state).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_atomic(path, &json_bytes)
}

/// Replaces `path` with `bytes` all at once: an exclusive temporary sibling, written and
/// synced, renamed over it, the directory synced. A crash or a full disk leaves the old
/// file or the new one, never a truncated mix.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let name = path.file_name().ok_or_else(|| std::io::Error::other("path has no filename"))?;
    let (tmp_path, mut file) = loop {
        let mut tmp_name = name.to_os_string();
        tmp_name.push(format!(".tmp-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let candidate = parent.join(tmp_name);
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&candidate) {
            Ok(file) => break (candidate, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp_path, path)?;
        std::fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

/// Largest config text read (session, actions, desktop entries, MIME tables).
pub const CONFIG_TEXT_MAX: u64 = 16 << 20;

/// A text file the app reads its config from: opened non-blocking, used only if it is a
/// regular file (links followed), at most [`CONFIG_TEXT_MAX`] bytes, UTF-8. These were
/// `read_to_string` by name: a FIFO named like one waited for a writer forever (at start
/// for the session and action files, so the app never came up; on the controller for
/// Open With, so the window froze), and a huge file was read whole. `NotFound` comes back
/// as it is.
pub fn read_text_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY).open(path)?;
    let md = file.metadata()?;
    if !md.is_file() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"));
    }
    if md.len() > CONFIG_TEXT_MAX {
        return Err(std::io::Error::from_raw_os_error(libc::EFBIG));
    }
    let mut text = String::new();
    file.take(CONFIG_TEXT_MAX + 1).read_to_string(&mut text)?;
    if text.len() as u64 > CONFIG_TEXT_MAX {
        return Err(std::io::Error::from_raw_os_error(libc::EFBIG));
    }
    Ok(text)
}

/// Load and parse session state from file.
pub fn load(path: &Path) -> std::io::Result<SessionState> {
    let data = read_text_file(path)?;
    serde_json::from_str(&data).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Resolve a saved path or URI safely, falling back to `home` if nonexistent or inaccessible.
#[must_use]
pub fn resolve_path_with_fallback(path_str: &str, home: &VfsUri) -> VfsUri {
    if path_str.contains("://") {
        if let Ok(uri) = VfsUri::parse(path_str) {
            if uri.scheme() == core_vfs::Scheme::File {
                if let Some(p) = uri.to_local_path()
                    && p.is_dir()
                {
                    return uri;
                }
            } else {
                // Gungnir or other virtual backends handle remote states gracefully
                return uri;
            }
        }
    } else {
        let p = Path::new(path_str);
        if p.is_dir()
            && let Ok(uri) = VfsUri::from_local_path(p)
        {
            return uri;
        }
    }
    home.clone()
}

/// Background session saver that coalesces updates without dropping the latest state.
#[derive(Clone)]
pub struct SessionSaver {
    latest: tokio::sync::watch::Sender<Option<SessionState>>,
    flush: tokio::sync::mpsc::Sender<tokio::sync::oneshot::Sender<std::io::Result<()>>>,
}

async fn persist(path: &Path, pending: &mut Option<SessionState>) -> std::io::Result<()> {
    if let Some(state) = pending.clone() {
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || save_atomic(&path, &state)).await.map_err(std::io::Error::other)??;
        *pending = None;
    }
    Ok(())
}

impl SessionSaver {
    #[must_use]
    pub fn spawn(path: PathBuf) -> (Self, Option<tokio::task::JoinHandle<()>>) {
        let (latest, mut updates) = tokio::sync::watch::channel::<Option<SessionState>>(None);
        let (flush, mut flushes) = tokio::sync::mpsc::channel::<tokio::sync::oneshot::Sender<std::io::Result<()>>>(8);
        let task = async move {
            let mut pending = None;
            let timer = tokio::time::sleep(Duration::from_millis(500));
            tokio::pin!(timer);
            let mut timer_active = false;
            loop {
                tokio::select! {
                    changed = updates.changed() => {
                        if let Some(state) = updates.borrow_and_update().clone() {
                            pending = Some(state);
                        }
                        if changed.is_err() {
                            if let Err(e) = persist(&path, &mut pending).await {
                                eprintln!("gungnir-fm: session save failed: {e}");
                            }
                            break;
                        }
                        timer.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(500));
                        timer_active = true;
                    }
                    ack = flushes.recv() => {
                        let Some(ack) = ack else {
                            if let Some(state) = updates.borrow_and_update().clone() { pending = Some(state); }
                            if let Err(e) = persist(&path, &mut pending).await {
                                eprintln!("gungnir-fm: session save failed: {e}");
                            }
                            break;
                        };
                        if let Some(state) = updates.borrow_and_update().clone() {
                            pending = Some(state);
                        }
                        let result = persist(&path, &mut pending).await;
                        timer_active = false;
                        let _ = ack.send(result);
                    }
                    () = &mut timer, if timer_active => {
                        timer_active = false;
                        if let Err(e) = persist(&path, &mut pending).await {
                            eprintln!("gungnir-fm: session save failed: {e}");
                        }
                    }
                }
            }
        };
        let handle = tokio::runtime::Handle::try_current().ok().map(|h| h.spawn(task));
        (Self { latest, flush }, handle)
    }

    pub fn update(&self, state: SessionState) {
        self.latest.send_replace(Some(state));
    }

    pub async fn flush(&self) -> std::io::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.flush.send(tx).await.map_err(|_| std::io::Error::other("session saver stopped"))?;
        rx.await.map_err(|_| std::io::Error::other("session saver stopped"))?
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn round_trip_serialization() {
        let state = SessionState {
            window: WindowState { width: 1400, height: 900, maximized: true },
            split_view: true,
            active_pane: 1,
            primary_pane_tabs: PaneTabsState::new(
                vec!["/home/user/project_demo".into(), "/home/user/release_v1".into()],
                0,
            ),
            secondary_pane_tabs: Some(PaneTabsState::new(vec!["/home/user/backup_01".into()], 0)),
            show_hidden: true,
            sidebar_visible: false,
            sidebar_mode: 0,
            zoom_level: 1.5,
            dark_mode: false,
            view_mode: 2,
        };

        let json = serde_json::to_string(&state).unwrap_or_else(|e| panic!("{e}"));
        let deserialized: SessionState = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(state, deserialized);
        assert_eq!(deserialized.primary_pane_tabs.get_paths().len(), 2);
        assert_eq!(deserialized.secondary_pane_tabs.as_ref().map(|s| s.get_paths().len()), Some(1));
        assert_eq!(deserialized.zoom_level, 1.5);
    }

    struct TestSandbox(PathBuf);

    impl TestSandbox {
        fn new(name: &str) -> Self {
            let base = std::env::temp_dir().join("gungnir-test-sandbox");
            let _ = std::fs::create_dir_all(&base);
            let p = base.join(format!("{name}-{}-{}", std::process::id(), fastrand()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap_or_else(|e| panic!("{e}"));
            Self(p)
        }
    }

    impl Drop for TestSandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fastrand() -> u64 {
        use std::time::SystemTime;
        SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
    }

    #[test]
    fn atomic_save_and_load_in_isolated_temp() {
        let sandbox = TestSandbox::new("session-test");
        let session_file = sandbox.0.join("sub/session.json");

        let state = SessionState {
            window: WindowState::default(),
            split_view: false,
            active_pane: 0,
            primary_pane_tabs: PaneTabsState::new(vec!["/home/user".into()], 0),
            secondary_pane_tabs: None,
            show_hidden: false,
            sidebar_visible: true,
            sidebar_mode: 0,
            zoom_level: 1.0,
            dark_mode: true,
            view_mode: 1,
        };

        save_atomic(&session_file, &state).unwrap_or_else(|e| panic!("{e}"));
        assert!(session_file.exists());
        assert!(!session_file.with_extension("json.tmp").exists());

        let loaded = load(&session_file).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(state, loaded);
    }

    #[test]
    fn fallback_on_inaccessible_path() {
        let home = VfsUri::from_local_path(Path::new("/tmp")).unwrap_or_else(|_| panic!("home"));
        let nonexistent = "/path/that/definitely/does/not/exist_1234567";
        let resolved = resolve_path_with_fallback(nonexistent, &home);
        assert_eq!(resolved, home);
    }

    fn sample() -> SessionState {
        serde_json::from_str(r#"{"primary_pane_tabs":["/tmp"]}"#).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn explicit_locations_keep_preferences_and_replace_restored_tabs() {
        let mut old = sample();
        old.dark_mode = false;
        old.view_mode = 2;
        old.zoom_level = 1.5;
        old.show_hidden = true;
        old.sidebar_visible = false;
        old.active_pane = 1;
        old.window = WindowState { width: 1000, height: 700, maximized: true };
        let left = VfsUri::parse("/tmp/new-left").unwrap_or_else(|e| panic!("{e}"));
        let right = VfsUri::parse("/tmp/new-right").unwrap_or_else(|e| panic!("{e}"));
        let new = startup_state(Some(old.clone()), &left, &right, true, true);
        assert_eq!(new.primary_pane_tabs.get_paths(), [left.to_uri_string()]);
        assert_eq!(new.secondary_pane_tabs.as_ref().map(PaneTabsState::get_paths), Some(vec![right.to_uri_string()]));
        assert!(new.split_view);
        assert_eq!(new.active_pane, 0);
        assert_eq!(new.window, old.window);
        assert!(!new.dark_mode);
        assert_eq!(new.view_mode, 2);
        assert_eq!(new.zoom_level, 1.5);
        assert!(new.show_hidden);
        assert!(!new.sidebar_visible);
        assert_eq!(startup_state(Some(old.clone()), &left, &right, false, false), old);
    }

    #[test]
    fn two_initial_paths_show_the_right_pane_without_a_previous_session() {
        let left = VfsUri::parse("/tmp/left").unwrap_or_else(|e| panic!("{e}"));
        let right = VfsUri::parse("/tmp/right").unwrap_or_else(|e| panic!("{e}"));
        let new = startup_state(None, &left, &right, true, true);
        assert!(new.split_view);
        assert_eq!(new.primary_pane_tabs.get_paths(), [left.to_uri_string()]);
        assert_eq!(new.secondary_pane_tabs.map(|s| s.get_paths()), Some(vec![right.to_uri_string()]));
    }

    #[test]
    fn legacy_sessions_default_to_detailed_view() {
        assert_eq!(sample().view_mode, 1);
    }

    #[test]
    fn atomic_save_preserves_preexisting_temporary_links_and_cleans_failures() {
        use std::os::unix::fs::PermissionsExt;
        let sandbox = TestSandbox::new("save-links");
        let path = sandbox.0.join("session.json");
        let victim = sandbox.0.join("other-file");
        std::fs::write(&victim, b"must survive").unwrap_or_else(|e| panic!("{e}"));
        let old_temp = path.with_extension(format!("tmp-{}", std::process::id()));
        std::fs::hard_link(&victim, &old_temp).unwrap_or_else(|e| panic!("{e}"));
        save_atomic(&path, &sample()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(std::fs::read(&victim).unwrap_or_default(), b"must survive");
        assert_eq!(std::fs::metadata(&path).unwrap_or_else(|e| panic!("{e}")).permissions().mode() & 0o777, 0o600);
        let blocked = sandbox.0.join("a-directory");
        std::fs::create_dir(&blocked).unwrap_or_else(|e| panic!("{e}"));
        assert!(save_atomic(&blocked, &sample()).is_err());
        let entries: Vec<_> = std::fs::read_dir(&sandbox.0).unwrap_or_else(|e| panic!("{e}")).collect();
        assert_eq!(entries.len(), 4, "failed saves must remove their own temporary file");
    }

    #[test]
    fn concurrent_atomic_saves_leave_valid_complete_json() {
        let sandbox = TestSandbox::new("save-concurrent");
        let path = sandbox.0.join("session.json");
        std::thread::scope(|scope| {
            for i in 0..16 {
                let path = &path;
                scope.spawn(move || {
                    let mut state = sample();
                    state.window.width = 1000 + i;
                    save_atomic(path, &state).unwrap_or_else(|e| panic!("{e}"));
                });
            }
        });
        let loaded = load(&path).unwrap_or_else(|e| panic!("{e}"));
        assert!((1000..1016).contains(&loaded.window.width));
        assert_eq!(std::fs::read_dir(&sandbox.0).unwrap_or_else(|e| panic!("{e}")).count(), 1);
    }

    #[tokio::test]
    async fn rapid_session_updates_keep_the_latest_snapshot() {
        let sandbox = TestSandbox::new("session-flood");
        let path = sandbox.0.join("session.json");
        let (saver, _) = SessionSaver::spawn(path.clone());
        let mut state = sample();
        // No yielding: the old bounded queue dropped all updates after the 64th.
        for i in 0..1000 {
            state.window.width = 1000 + i;
            saver.update(state.clone());
        }
        saver.flush().await.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(load(&path).unwrap_or_else(|e| panic!("{e}")), state);
    }

    #[tokio::test]
    async fn failed_flush_is_reported_and_can_be_retried() {
        let sandbox = TestSandbox::new("session-retry");
        let parent = sandbox.0.join("blocked");
        std::fs::write(&parent, b"keep").unwrap_or_else(|e| panic!("{e}"));
        let path = parent.join("session.json");
        let (saver, _) = SessionSaver::spawn(path.clone());
        let state = sample();
        saver.update(state.clone());
        assert!(saver.flush().await.is_err());
        assert_eq!(std::fs::read(&parent).unwrap_or_default(), b"keep");
        std::fs::remove_file(&parent).unwrap_or_else(|e| panic!("{e}"));
        saver.flush().await.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(load(&path).unwrap_or_else(|e| panic!("{e}")), state);
    }

    #[tokio::test]
    async fn dropping_saver_persists_last_update() {
        let sandbox = TestSandbox::new("session-drop");
        let path = sandbox.0.join("session.json");
        let (saver, handle) = SessionSaver::spawn(path.clone());
        saver.update(sample());
        drop(saver);
        handle.unwrap_or_else(|| panic!("no saver task")).await.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(load(&path).unwrap_or_else(|e| panic!("{e}")), sample());
    }

    #[tokio::test]
    async fn test_session_saver_debounce_and_flush() {
        let sandbox = TestSandbox::new("saver-test");
        let session_file = sandbox.0.join("session.json");

        let (saver, _handle) = SessionSaver::spawn(session_file.clone());
        let state = SessionState {
            window: WindowState::default(),
            split_view: false,
            active_pane: 0,
            primary_pane_tabs: PaneTabsState::new(vec!["/home/user".into()], 0),
            secondary_pane_tabs: None,
            show_hidden: false,
            sidebar_visible: true,
            sidebar_mode: 0,
            zoom_level: 1.0,
            dark_mode: true,
            view_mode: 1,
        };

        saver.update(state.clone());
        saver.flush().await.unwrap_or_else(|e| panic!("{e}"));

        assert!(session_file.exists());
        let loaded = load(&session_file).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(state, loaded);
    }

    /// A FIFO at `p` (a name a loader would read).
    pub(crate) fn fifo(p: &Path) {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap_or_else(|e| panic!("{e}"));
        // SAFETY: a valid path in a test sandbox.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo {}", p.display());
    }

    /// `f` on its own thread; panics if it takes 5 s (it was waiting on a FIFO).
    pub(crate) fn bounded<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap_or_else(|_| panic!("{what} waited on a FIFO"))
    }

    #[test]
    fn a_fifo_for_a_session_file_is_an_error_not_a_hang() {
        // Read at start-up in main: a FIFO there kept Gungnir FM from ever starting.
        let sandbox = TestSandbox::new("session-fifo");
        let path = sandbox.0.join("session.json");
        fifo(&path);
        assert!(bounded("session::load", move || load(&path)).is_err());
    }
}
