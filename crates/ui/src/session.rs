//! Session persistence and crash recovery.
//!
//! Serializes window and navigation state to `~/.config/gungnir-fm/session.json`.
//! Uses atomic writes (`session.json.tmp` renamed to `session.json`) and a 500ms
//! debounced background saver.

use std::path::{Path, PathBuf};
use std::time::Duration;
use core_vfs::VfsUri;
use serde::{Deserialize, Serialize};

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
        Self {
            width: default_width(),
            height: default_height(),
            maximized: false,
        }
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
        Self::Structured {
            paths,
            active_index,
            tabs: Vec::new(),
        }
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
}

fn default_true() -> bool {
    true
}

/// Atomically write state to a JSON file via a temporary sibling file.
pub fn save_atomic(path: &Path, state: &SessionState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension(format!("tmp-{}", std::process::id()));
    let json_bytes = serde_json::to_vec_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(&json_bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Load and parse session state from file.
pub fn load(path: &Path) -> std::io::Result<SessionState> {
    let data = std::fs::read_to_string(path)?;
    serde_json::from_str(&data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
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

pub enum SessionSaveMsg {
    Update(SessionState),
    Flush(tokio::sync::oneshot::Sender<()>),
}

/// Background session saver that debounces writes by 500ms.
#[derive(Clone)]
pub struct SessionSaver {
    tx: tokio::sync::mpsc::Sender<SessionSaveMsg>,
}

impl SessionSaver {
    #[must_use]
    pub fn spawn(path: PathBuf) -> (Self, Option<tokio::task::JoinHandle<()>>) {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<SessionSaveMsg>(64);
        let task = async move {
            let mut pending_state: Option<SessionState> = None;
            let mut debounce_timer = Box::pin(tokio::time::sleep(Duration::MAX));
            let mut timer_active = false;

            loop {
                tokio::select! {
                    msg = rx.recv() => {
                        match msg {
                            Some(SessionSaveMsg::Update(state)) => {
                                pending_state = Some(state);
                                debounce_timer = Box::pin(tokio::time::sleep(Duration::from_millis(500)));
                                timer_active = true;
                            }
                            Some(SessionSaveMsg::Flush(ack)) => {
                                if let Some(state) = pending_state.take() {
                                    let _ = save_atomic(&path, &state);
                                }
                                timer_active = false;
                                let _ = ack.send(());
                            }
                            None => {
                                if let Some(state) = pending_state.take() {
                                    let _ = save_atomic(&path, &state);
                                }
                                break;
                            }
                        }
                    }
                    () = &mut debounce_timer, if timer_active => {
                        timer_active = false;
                        if let Some(state) = pending_state.take() {
                            let _ = save_atomic(&path, &state);
                        }
                    }
                }
            }
        };

        let handle = tokio::runtime::Handle::try_current().ok().map(|h| h.spawn(task));
        (Self { tx }, handle)
    }

    pub fn update(&self, state: SessionState) {
        let _ = self.tx.try_send(SessionSaveMsg::Update(state));
    }

    pub async fn flush(&self) {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        if self.tx.send(SessionSaveMsg::Flush(ack_tx)).await.is_ok() {
            let _ = ack_rx.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_serialization() {
        let state = SessionState {
            window: WindowState {
                width: 1400,
                height: 900,
                maximized: true,
            },
            split_view: true,
            active_pane: 1,
            primary_pane_tabs: PaneTabsState::new(
                vec!["/home/user/project_demo".into(), "/home/user/release_v1".into()],
                0,
            ),
            secondary_pane_tabs: Some(PaneTabsState::new(
                vec!["/home/user/backup_01".into()],
                0,
            )),
            show_hidden: true,
            sidebar_visible: false,
        };

        let json = serde_json::to_string(&state).unwrap_or_else(|e| panic!("{e}"));
        let deserialized: SessionState = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(state, deserialized);
        assert_eq!(deserialized.primary_pane_tabs.get_paths().len(), 2);
        assert_eq!(deserialized.secondary_pane_tabs.as_ref().map(|s| s.get_paths().len()), Some(1));
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
        };

        saver.update(state.clone());
        saver.flush().await;

        assert!(session_file.exists());
        let loaded = load(&session_file).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(state, loaded);
    }
}
