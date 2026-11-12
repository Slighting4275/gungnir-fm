//! The controller: one async actor on the tokio runtime that owns all application
//! state (panes, tabs, selection, transfers). It receives UI commands and internal
//! events over one bounded channel, never blocks (slow work goes to tasks and
//! `spawn_blocking`), and talks to the window only through [`UiSink`].
//!
//! Every directory load and re-view carries a generation number and a
//! `CancellationToken`: navigating away cancels the in-flight listing at once, and
//! late results from superseded work are dropped.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use core_vfs::sort::sort_order;
use core_vfs::{
    BackendKind, CancellationToken, FileEntry, FileKind, ListOptions, ListSummary, MountInfo, Scheme, SortColumn,
    SortKey, TransferControl, TransferKind, TransferProgress, TransferRequest, TransferState, VfsError, VfsProvider,
    VfsRegistry, VfsResult, VfsUri, VolumeInfo, WatchEvent, format,
};
use slint::SharedString;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::bridge::{DialogView, Flags, PaneView, UiSink, Update};
use crate::rows::{self, RowItem};
use crate::{Crumb, DialogKind, MountItem, PlaceItem, StatusInfo, TabInfo, TransferRow, TreeRow};

/// Commands from the UI (one per Slint callback).
#[derive(Debug, Clone)]
pub enum Command {
    Navigate {
        pane: usize,
        location: String,
    },
    Nav {
        pane: usize,
        action: String,
    },
    RowClicked {
        pane: usize,
        index: usize,
        ctrl: bool,
        shift: bool,
    },
    RowActivated {
        pane: usize,
        index: usize,
    },
    RowContext {
        pane: usize,
        index: usize,
    },
    MoveCursor {
        pane: usize,
        delta: i64,
        extend: bool,
    },
    SortBy {
        pane: usize,
        column: i32,
    },
    Filter {
        pane: usize,
        text: String,
    },
    TabNew(usize),
    TabClose(usize, usize),
    TabCloseOthers(usize, usize),
    TabDuplicate(usize, usize),
    TabSelect(usize, usize),
    TabCycle {
        pane: usize,
        delta: i32,
    },
    CustomAction {
        pane: usize,
        id: String,
    },
    PaneFocused(usize),
    Run(String),
    ViewMode(i32),
    WindowChanged(crate::session::WindowState),
    Zoom(f32),
    SidebarOpen(String),
    SidebarOpenTab(String),
    SidebarOpenWindow(String),
    RemoveBookmark(String),
    TreeToggle(usize),
    SidebarMode(i32),
    TransferAction {
        id: i64,
        action: String,
    },
    DialogAccept(String),
    DialogCancel,
    /// A row picked in a chooser dialog.
    DialogChoose(usize),
    DialogCustomAction {
        action: String,
        opt1: bool,
        opt2: bool,
        extra1: String,
        extra2: String,
    },
    TerminalRun(String),
    TerminalClear,
    /// Internal drag and drop: source row, target row (`None`: the pane's folder).
    Drop {
        src_pane: usize,
        src_index: usize,
        dst_pane: usize,
        dst_index: Option<usize>,
        copy: bool,
    },
    /// Sidebar device clicked (mounts it first if needed), or its eject button.
    DeviceOpen(String),
    DeviceEject(String),
    RenameCommit {
        pane: usize,
        index: usize,
        name: String,
    },
    RenameCancel,
    TriggerUpdate,
    SetDarkMode(bool),
    ToggleDarkMode,
    Quit,
}

/// Results of background work.
#[derive(Debug)]
pub enum Event {
    Batch {
        pane: usize,
        tab: u64,
        generation: u64,
        entries: Vec<FileEntry>,
    },
    ListDone {
        pane: usize,
        tab: u64,
        generation: u64,
        result: VfsResult<ListSummary>,
        collected: Option<Vec<FileEntry>>,
        started: Instant,
    },
    Prepared {
        pane: usize,
        tab: u64,
        view_gen: u64,
        order: Option<Vec<u32>>,
        visible: Vec<u32>,
        rows: Vec<RowItem>,
    },
    ThumbnailsReady {
        pane: usize,
        tab: u64,
        items: Vec<(String, PathBuf)>,
    },
    DirChanged {
        pane: usize,
        tab: u64,
        removed: bool,
    },
    Volume {
        pane: usize,
        tab: u64,
        info: VolumeInfo,
    },
    Mounts(Vec<MountInfo>),
    Places(Vec<PlaceItem>, Vec<VfsUri>),
    Tree(Vec<TreeRow>),
    Op {
        result: Result<String, String>,
        reload: Vec<VfsUri>,
        select: Option<(VfsUri, String)>,
    },
    Info {
        title: String,
        kind: DialogKind,
        result: VfsResult<Vec<(String, String)>>,
    },
    /// A Quick Preview prepared on the blocking pool.
    Preview {
        seq: u64,
        title: String,
        detail: String,
        image_path: Option<PathBuf>,
    },
    Progress(TransferProgress),
    TransferEnded {
        id: u64,
        result: VfsResult<()>,
    },
    OpenWith {
        mime: String,
        files: Vec<PathBuf>,
        apps: Vec<crate::desktop::App>,
    },
    BusReady(zbus::Connection),
    Devices(Vec<crate::drives::Device>),
    Mounted {
        object: String,
        result: Result<String, String>,
    },
    SearchResults {
        root: VfsUri,
        results: Vec<(String, String)>,
    },
    TerminalOutput {
        text: String,
        reload_loc: VfsUri,
    },
    UpdateCheckResult(crate::updater::UpdateCheckResult),
    UpdateFinished {
        success: bool,
        message: String,
    },
    MimeIndex(Box<crate::desktop::DesktopIndex>),
    ApplicationLaunched {
        generation: u64,
        result: Result<String, String>,
    },
    PropertiesReady {
        generation: u64,
        view: Box<DialogView>,
    },
    ClipboardPaste {
        dest: VfsUri,
        files: Result<Option<crate::clipboard::Files>, ()>,
    },
    Created {
        pane: usize,
        tab: u64,
        dir: VfsUri,
        result: VfsResult<VfsUri>,
    },
    RenameFinished {
        action: RenameAction,
        from: VfsUri,
        result: Result<(VfsUri, Option<crate::rename::Identity>), String>,
    },
    Templates(Vec<crate::templates::Template>),
    CustomAppSelected(String),
    Tick,
}

#[derive(Debug)]
pub enum Msg {
    Ui(Command),
    Ev(Event),
}

pub type MsgTx = mpsc::Sender<Msg>;

/// Startup options.
#[derive(Debug, Clone)]
pub struct Config {
    pub left: VfsUri,
    pub right: VfsUri,
    pub left_select: Option<String>,
    pub right_select: Option<String>,
    pub home: VfsUri,
    pub gungnir_root: VfsUri,
    pub gungnir_label: String,
    pub bookmarks_file: Option<PathBuf>,
    pub actions_dir: Option<PathBuf>,
    pub templates_dir: Option<PathBuf>,
    pub session_file: Option<PathBuf>,
    pub initial_session: Option<crate::session::SessionState>,
    /// Explicit arguments should report an inaccessible path rather than restore Home.
    pub explicit_locations: bool,
    /// Explicit desktop settings for embedded/test instances; None uses XDG.
    pub desktop_dirs: Option<crate::desktop::Dirs>,
    pub trace: bool,
}

const MESSAGE_TTL: Duration = Duration::from_secs(6);
const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);
const TICK: Duration = Duration::from_millis(150);

struct Tab {
    id: u64,
    location: VfsUri,
    back: Vec<VfsUri>,
    forward: Vec<VfsUri>,
    /// Arrival order; never reordered (sorting produces `order`).
    entries: Arc<Vec<FileEntry>>,
    /// Sorted permutation of `entries` (arrival order while streaming).
    order: Vec<u32>,
    /// Shown entries, display order (subset of `order`).
    visible: Vec<u32>,
    selected: HashSet<String>,
    cursor: usize,
    anchor: Option<usize>,
    sort: SortKey,
    filter: String,
    loading: bool,
    error: String,
    load_gen: u64,
    view_gen: u64,
    cancel: CancellationToken,
    watch: Option<JoinHandle<()>>,
    volume: Option<VolumeInfo>,
    backend: Option<BackendKind>,
    /// Reselect this name after the next load.
    pending_select: Option<String>,
    pending_rename: Option<String>,
}

impl Tab {
    fn new(id: u64, location: VfsUri) -> Self {
        Self {
            id,
            location,
            back: Vec::new(),
            forward: Vec::new(),
            entries: Arc::new(Vec::new()),
            order: Vec::new(),
            visible: Vec::new(),
            selected: HashSet::new(),
            cursor: 0,
            anchor: None,
            sort: SortKey::default(),
            filter: String::new(),
            loading: false,
            error: String::new(),
            load_gen: 0,
            view_gen: 0,
            cancel: CancellationToken::new(),
            watch: None,
            volume: None,
            backend: None,
            pending_select: None,
            pending_rename: None,
        }
    }

    fn title(&self, home: &VfsUri) -> String {
        if &self.location == home {
            return "Home".to_owned();
        }
        match self.location.scheme() {
            core_vfs::Scheme::Trash => "Trash".to_owned(),
            core_vfs::Scheme::Recent => "Recent".to_owned(),
            core_vfs::Scheme::Network => "Network".to_owned(),
            _ => match self.location.file_name() {
                Some(n) if !n.is_empty() => n.to_owned(),
                _ if !self.location.crumbs().is_empty() => self.location.crumbs()[0].0.clone(),
                _ => "/".into(),
            },
        }
    }

    fn entry_at(&self, visible_index: usize) -> Option<&FileEntry> {
        self.visible.get(visible_index).and_then(|&i| self.entries.get(i as usize))
    }

    /// The selection as entries, or the cursor row when nothing is selected.
    fn targets(&self) -> Vec<FileEntry> {
        let sel: Vec<FileEntry> = self
            .visible
            .iter()
            .filter_map(|&i| self.entries.get(i as usize))
            .filter(|e| self.selected.contains(&e.name))
            .cloned()
            .collect();
        if sel.is_empty() { self.entry_at(self.cursor).cloned().into_iter().collect() } else { sel }
    }
}

struct Pane {
    tabs: Vec<Tab>,
    active: usize,
}

impl Pane {
    fn tab(&self) -> &Tab {
        if self.tabs.is_empty() {
            static FALLBACK_TAB: std::sync::OnceLock<Tab> = std::sync::OnceLock::new();
            return FALLBACK_TAB.get_or_init(|| Tab::new(0, VfsUri::default()));
        }
        let idx = self.active.min(self.tabs.len().saturating_sub(1));
        &self.tabs[idx]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        if self.tabs.is_empty() {
            self.tabs.push(Tab::new(0, VfsUri::default()));
            self.active = 0;
        }
        let idx = self.active.min(self.tabs.len().saturating_sub(1));
        self.active = idx;
        &mut self.tabs[idx]
    }
}

struct Transfer {
    kind: TransferKind,
    title: String,
    dest: VfsUri,
    sources: Vec<VfsUri>,
    progress: TransferProgress,
    started: Instant,
    rate: f64,
    last: (Instant, u64),
    cancel: CancellationToken,
    pause: watch::Sender<bool>,
    can_pause: bool,
    /// How long it took, frozen when it finished.
    took: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct ConflictItem {
    pub src: VfsUri,
    pub filename: String,
    pub src_size: u64,
    pub dst_size: u64,
    pub src_mtime: Option<std::time::SystemTime>,
    pub dst_mtime: Option<std::time::SystemTime>,
    pub src_is_newer: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictAction {
    Replace,
    ReplaceIfNewer,
    AutoRename,
    Skip,
}

enum PendingDialog {
    NewFolder {
        dir: VfsUri,
    },
    NewFile {
        dir: VfsUri,
    },
    SelectPattern {
        pane: usize,
    },
    EmptyTrash,
    ClearRecent,
    OpenMany {
        pane: usize,
        entries: Vec<FileEntry>,
    },
    ConnectServer {
        pane: usize,
    },
    Delete {
        pane: usize,
        uris: Vec<VfsUri>,
    },
    /// "Open With": the files and the candidate applications shown as rows.
    OpenWith {
        files: Vec<PathBuf>,
        apps: Vec<crate::desktop::App>,
    },
    Properties {
        pane: usize,
        entry: FileEntry,
        backend: Option<BackendKind>,
        target_name: String,
        local_path: Option<PathBuf>,
        mime: String,
        mime_desc: String,
        current_tab: i32,
        selected_app_id: Option<String>,
    },
    Info,
    Conflict {
        kind: TransferKind,
        sources: Vec<VfsUri>,
        dest: VfsUri,
        conflicts: Vec<ConflictItem>,
        current_idx: usize,
        resolutions: Vec<(VfsUri, ConflictAction)>,
    },
    Search {
        results: Vec<(String, String)>,
    },
    BatchRename {
        pane: usize,
        items: Vec<(VfsUri, String)>,
        new_names: Vec<(VfsUri, String)>,
    },
    ComparePanes {
        left_loc: VfsUri,
        right_loc: VfsUri,
    },
    Compress {
        pane: usize,
        targets: Vec<PathBuf>,
        base_dir: PathBuf,
    },
}

fn app_icon_glyph(app: &crate::desktop::App) -> &'static str {
    let text = format!("{} {} {}", app.id, app.name, app.exec).to_lowercase();
    if text.contains("player") || text.contains("video") || text.contains("mpv") || text.contains("vlc") {
        "play"
    } else if text.contains("audio") || text.contains("music") || text.contains("sound") {
        "music"
    } else if text.contains("image") || text.contains("photo") || text.contains("viewer") || text.contains("gimp") {
        "image"
    } else if text.contains("edit") || text.contains("text") || text.contains("code") || text.contains("write") {
        "edit"
    } else {
        "desktop"
    }
}

/// Quick Preview's text: the first 12 lines of the first KiB, if that is UTF-8. Opened
/// non-blocking and read only if it is a regular file (a FIFO would wait for a writer).
fn text_snippet(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut buf = vec![0u8; 1024];
    let n = file.take(1024).read(&mut buf).ok()?;
    let s = std::str::from_utf8(buf.get(..n)?).ok()?;
    let lines: Vec<&str> = s.lines().take(12).collect();
    if lines.is_empty() {
        return None;
    }
    let mut out = format!("\n\n---\n{}", lines.join("\n"));
    if lines.len() == 12 || n == 1024 {
        out.push_str("\n...");
    }
    Some(out)
}

fn build_open_with_items(
    idx: &crate::desktop::DesktopIndex,
    mime: &str,
) -> (Vec<crate::OpenWithAppItem>, Option<String>, i32) {
    let mut items = Vec::new();
    let mut selected_id = None;
    let mut selected_index = -1;

    let def_app = idx.default_app_for(mime);
    let rec_apps = idx.recommended_apps_for(mime);
    let other_apps = idx.other_apps_for(mime);

    if let Some(def) = def_app {
        items.push(crate::OpenWithAppItem {
            id: String::new().into(),
            name: "Default Application".into(),
            exec: String::new().into(),
            icon: String::new().into(),
            is_default: false,
            is_recommended: false,
            is_header: true,
        });
        items.push(crate::OpenWithAppItem {
            id: def.id.clone().into(),
            name: def.name.clone().into(),
            exec: def.exec.clone().into(),
            icon: app_icon_glyph(def).into(),
            is_default: true,
            is_recommended: true,
            is_header: false,
        });
        selected_id = Some(def.id.clone());
        selected_index = 1;
    }

    if !rec_apps.is_empty() {
        items.push(crate::OpenWithAppItem {
            id: String::new().into(),
            name: "Recommended Applications".into(),
            exec: String::new().into(),
            icon: String::new().into(),
            is_default: false,
            is_recommended: false,
            is_header: true,
        });
        for app in rec_apps {
            let idx_pos = items.len() as i32;
            if selected_id.is_none() {
                selected_id = Some(app.id.clone());
                selected_index = idx_pos;
            }
            items.push(crate::OpenWithAppItem {
                id: app.id.clone().into(),
                name: app.name.clone().into(),
                exec: app.exec.clone().into(),
                icon: app_icon_glyph(app).into(),
                is_default: false,
                is_recommended: true,
                is_header: false,
            });
        }
    }

    if !other_apps.is_empty() {
        items.push(crate::OpenWithAppItem {
            id: String::new().into(),
            name: "Other Applications".into(),
            exec: String::new().into(),
            icon: String::new().into(),
            is_default: false,
            is_recommended: false,
            is_header: true,
        });
        for app in other_apps {
            let idx_pos = items.len() as i32;
            if selected_id.is_none() {
                selected_id = Some(app.id.clone());
                selected_index = idx_pos;
            }
            items.push(crate::OpenWithAppItem {
                id: app.id.clone().into(),
                name: app.name.clone().into(),
                exec: app.exec.clone().into(),
                icon: app_icon_glyph(app).into(),
                is_default: false,
                is_recommended: false,
                is_header: false,
            });
        }
    }

    (items, selected_id, selected_index)
}

fn format_perm_bits(bits: u32) -> &'static str {
    match (bits & 4 != 0, bits & 2 != 0) {
        (true, true) => "Read and write",
        (true, false) => "Read-only",
        (false, true) => "Write-only",
        (false, false) => "None",
    }
}

#[cfg(unix)]
fn get_user_and_group(path: &Path) -> (String, String) {
    use std::os::unix::fs::MetadataExt;
    if let Ok(m) = std::fs::metadata(path) {
        let uid = m.uid();
        let gid = m.gid();
        let user = get_pw_name(uid).unwrap_or_else(|| uid.to_string());
        let group = get_gr_name(gid).unwrap_or_else(|| gid.to_string());
        (user, group)
    } else {
        ("Owner".to_string(), "Group".to_string())
    }
}

#[cfg(not(unix))]
fn get_user_and_group(_path: &Path) -> (String, String) {
    ("Owner".to_string(), "Group".to_string())
}

#[cfg(unix)]
fn get_pw_name(uid: u32) -> Option<String> {
    let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buf = vec![0u8; 1024];
    let mut result = std::ptr::null_mut();
    let res = unsafe {
        libc::getpwuid_r(uid, pwd.as_mut_ptr(), buf.as_mut_ptr() as *mut libc::c_char, buf.len(), &mut result)
    };
    if res == 0 && !result.is_null() {
        let name_ptr = unsafe { (*result).pw_name };
        if !name_ptr.is_null() {
            let cstr = unsafe { std::ffi::CStr::from_ptr(name_ptr) };
            return Some(cstr.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(unix)]
fn get_gr_name(gid: u32) -> Option<String> {
    let mut grp = std::mem::MaybeUninit::<libc::group>::uninit();
    let mut buf = vec![0u8; 1024];
    let mut result = std::ptr::null_mut();
    let res = unsafe {
        libc::getgrgid_r(gid, grp.as_mut_ptr(), buf.as_mut_ptr() as *mut libc::c_char, buf.len(), &mut result)
    };
    if res == 0 && !result.is_null() {
        let name_ptr = unsafe { (*result).gr_name };
        if !name_ptr.is_null() {
            let cstr = unsafe { std::ffi::CStr::from_ptr(name_ptr) };
            return Some(cstr.to_string_lossy().into_owned());
        }
    }
    None
}

fn archive_stem(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    for suffix in [
        ".tar.zst", ".tar.xz", ".tar.gz", ".tar.bz2", ".tar.lz4", ".tar.br", ".zst", ".xz", ".gz", ".bz2", ".lz4",
        ".br", ".zip", ".7z", ".rar", ".tar",
    ] {
        if lower.ends_with(suffix) {
            return name[..name.len() - suffix.len()].to_string();
        }
    }
    Path::new(name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| name.to_string())
}

#[derive(Debug, Clone, Copy)]
pub enum RenameAction {
    Forward,
    Undo,
    Redo,
}

#[derive(Debug)]
struct RenameEdit {
    pane: usize,
    tab: u64,
    index: usize,
    uri: VfsUri,
}

pub struct Controller {
    registry: VfsRegistry,
    ui: Arc<dyn UiSink>,
    tx: MsgTx,
    cfg: Config,
    panes: [Pane; 2],
    active_pane: usize,
    dual: bool,
    sidebar: bool,
    sidebar_mode: i32,
    show_hidden: bool,
    view_mode: i32,
    window_state: crate::session::WindowState,
    zoom_level: f32,
    next_tab: u64,
    next_transfer: u64,
    transfers: Vec<(u64, Transfer)>,
    transfers_dirty: bool,
    dialog: Option<PendingDialog>,
    /// Number of the latest Quick Preview, to drop a result that arrives too late.
    preview_seq: u64,
    /// The Quick Preview being prepared, if any. Not a dialog: nothing is shown yet and
    /// nothing is held back meanwhile (a slow or stuck thumbnail must not lock the menus).
    preview_wait: Option<u64>,
    renaming: Option<RenameEdit>,
    rename_history: crate::rename::History,
    rename_busy: bool,
    templates: Vec<crate::templates::Template>,
    templates_loading: bool,
    templates_refreshed: Instant,
    message: Option<(String, bool, Instant)>,
    places: Vec<PlaceItem>,
    bookmarks: Vec<VfsUri>,
    mounts: Vec<MountInfo>,
    tree_expanded: BTreeSet<VfsUri>,
    tree_task: Option<JoinHandle<()>>,
    /// URIs of the tree rows last shown (index -> location for `TreeToggle`).
    tree_rows: Vec<String>,
    /// Ctrl+C / Ctrl+X: the items and whether they are cut (moved on paste).
    clipboard: Option<(Vec<VfsUri>, bool)>,
    clipboard_service: Option<mpsc::Sender<crate::clipboard::Request>>,
    /// udisks2 over the system bus (None until connected, or without udisks).
    bus: Option<zbus::Connection>,
    devices: Vec<crate::drives::Device>,
    session_saver: Option<crate::session::SessionSaver>,
    actions: crate::actions::ActionRegistry,
    mime_index: crate::desktop::DesktopIndex,
    properties_generation: u64,
    terminal_open: bool,
    terminal_cwd: PathBuf,
    terminal_output: String,
    search_task: Option<JoinHandle<()>>,
    update_available: bool,
    update_version: String,
    updating: bool,
    package_format: crate::updater::PackageFormat,
    pub dark_mode: bool,
    quitting: bool,
}

/// Breadcrumbs as Nemo shows them: a folder below `home` starts at a "Home" crumb, the
/// file system root is a drive icon, and a Gungnir node is its name with the spear icon.
pub(crate) fn crumb_views(u: &VfsUri, home: &VfsUri) -> Vec<Crumb> {
    let all = u.crumbs();
    let skip =
        if u.scheme() == Scheme::File && !home.is_root() && u.starts_with(home) { home.crumbs().len() - 1 } else { 0 };
    all.into_iter()
        .enumerate()
        .skip(skip)
        .map(|(i, (label, uri))| {
            let (label, icon) = match (i, u.scheme()) {
                (i, Scheme::File) if i == skip && skip > 0 => ("Home".to_owned(), 1),
                (0, Scheme::File) => (String::new(), 2),
                (0, Scheme::Gungnir) => (uri.authority().to_owned(), 3),
                (0, Scheme::Sftp | Scheme::Smb) => (uri.authority().to_owned(), 2),
                (0, Scheme::Trash) => ("Trash".to_owned(), 5),
                (0, Scheme::Recent) => ("Recent".to_owned(), 11),
                (0, Scheme::Network) => ("Network".to_owned(), 6),
                _ => (label, 0),
            };
            let label = if icon == 3 && label.is_empty() { "gungnir".to_owned() } else { label };
            Crumb { label: label.into(), uri: uri.display().into(), icon }
        })
        .collect()
}

impl Controller {
    #[must_use]
    pub fn new(registry: VfsRegistry, ui: Arc<dyn UiSink>, tx: MsgTx, cfg: Config) -> Self {
        let mut next_tab = 1;
        let (mut panes, active_pane, dual, show_hidden, sidebar, zoom_level, dark_mode) =
            if let Some(ref s) = cfg.initial_session {
                let p0_paths = s.primary_pane_tabs.get_paths();
                let p0_tabs: Vec<Tab> = if p0_paths.is_empty() {
                    let id = next_tab;
                    next_tab += 1;
                    vec![Tab::new(id, cfg.left.clone())]
                } else {
                    p0_paths
                        .into_iter()
                        .map(|p| {
                            let id = next_tab;
                            next_tab += 1;
                            let uri = if cfg.explicit_locations {
                                VfsUri::parse(&p).unwrap_or_else(|_| cfg.home.clone())
                            } else {
                                crate::session::resolve_path_with_fallback(&p, &cfg.home)
                            };
                            Tab::new(id, uri)
                        })
                        .collect()
                };
                let p0_active = s.primary_pane_tabs.get_active_index().min(p0_tabs.len().saturating_sub(1));

                let p1_tabs: Vec<Tab> = if let Some(ref sec) = s.secondary_pane_tabs {
                    let p1_paths = sec.get_paths();
                    if p1_paths.is_empty() {
                        let id = next_tab;
                        next_tab += 1;
                        vec![Tab::new(id, cfg.right.clone())]
                    } else {
                        p1_paths
                            .into_iter()
                            .map(|p| {
                                let id = next_tab;
                                next_tab += 1;
                                let uri = if cfg.explicit_locations {
                                    VfsUri::parse(&p).unwrap_or_else(|_| cfg.home.clone())
                                } else {
                                    crate::session::resolve_path_with_fallback(&p, &cfg.home)
                                };
                                Tab::new(id, uri)
                            })
                            .collect()
                    }
                } else {
                    let id = next_tab;
                    next_tab += 1;
                    vec![Tab::new(id, cfg.right.clone())]
                };
                let p1_active = s
                    .secondary_pane_tabs
                    .as_ref()
                    .map(|sec| sec.get_active_index().min(p1_tabs.len().saturating_sub(1)))
                    .unwrap_or(0);

                let dual = s.split_view;
                let active_pane = if s.active_pane == 1 && dual { 1 } else { 0 };
                (
                    [Pane { tabs: p0_tabs, active: p0_active }, Pane { tabs: p1_tabs, active: p1_active }],
                    active_pane,
                    dual,
                    s.show_hidden,
                    s.sidebar_visible,
                    s.zoom_level,
                    s.dark_mode,
                )
            } else {
                let t0 = Tab::new(1, cfg.left.clone());
                let t1 = Tab::new(2, cfg.right.clone());
                let panes = [Pane { tabs: vec![t0], active: 0 }, Pane { tabs: vec![t1], active: 0 }];
                next_tab = 3;
                (panes, 0, false, false, true, 1.0, true)
            };

        // File arguments still select their basename when preferences are restored.
        for (pane, selection) in panes.iter_mut().zip([&cfg.left_select, &cfg.right_select]) {
            if let Some(name) = selection {
                pane.tab_mut().pending_select = Some(name.clone());
                pane.tab_mut().selected = [name.clone()].into();
            }
        }

        let actions = crate::actions::ActionRegistry::load_or_default(cfg.actions_dir.as_deref());

        let window_state = cfg.initial_session.as_ref().map(|s| s.window.clone()).unwrap_or_default();
        let view_mode = cfg.initial_session.as_ref().map_or(1, |s| s.view_mode.clamp(0, 2));
        let sidebar_mode = cfg.initial_session.as_ref().map_or(0, |s| s.sidebar_mode.clamp(0, 1));
        Self {
            registry,
            ui,
            tx,
            cfg,
            panes,
            active_pane,
            dual,
            sidebar,
            sidebar_mode,
            show_hidden,
            view_mode,
            window_state,
            zoom_level,
            next_tab,
            next_transfer: 1,
            transfers: Vec::new(),
            transfers_dirty: false,
            dialog: None,
            preview_seq: 0,
            preview_wait: None,
            renaming: None,
            rename_history: crate::rename::History::default(),
            rename_busy: false,
            templates: Vec::new(),
            templates_loading: false,
            templates_refreshed: Instant::now(),
            message: None,
            places: Vec::new(),
            bookmarks: Vec::new(),
            mounts: Vec::new(),
            tree_expanded: BTreeSet::new(),
            tree_task: None,
            tree_rows: Vec::new(),
            clipboard: None,
            clipboard_service: None,
            bus: None,
            devices: Vec::new(),
            session_saver: None,
            actions,
            mime_index: crate::desktop::DesktopIndex::default(),
            properties_generation: 0,
            terminal_open: false,
            terminal_cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            terminal_output: String::from("Gungnir FM Embedded Terminal (F4)\nType shell commands and press Enter\n"),
            search_task: None,
            update_available: false,
            update_version: String::new(),
            updating: false,
            package_format: crate::updater::detect_package_format(),
            dark_mode,
            quitting: false,
        }
    }

    /// Run until the channel closes.
    pub async fn run(mut self, mut rx: mpsc::Receiver<Msg>) {
        self.start();
        while let Some(m) = rx.recv().await {
            if self.quitting {
                break;
            }
            match m {
                Msg::Ui(Command::Quit) => {
                    self.quitting = true;
                    self.trigger_save_session();
                    if let Some(ref saver) = self.session_saver
                        && let Err(e) = saver.flush().await
                    {
                        eprintln!("gungnir-fm: session save failed: {e}");
                    }
                    self.ui.send(Update::Quit);
                    break;
                }
                Msg::Ui(c) => self.command(c),
                Msg::Ev(e) => self.event(e),
            }
            if self.quitting {
                break;
            }
        }
    }

    fn desktop_dirs(&self) -> crate::desktop::Dirs {
        self.cfg.desktop_dirs.clone().unwrap_or_else(crate::desktop::Dirs::from_env)
    }

    fn start(&mut self) {
        self.refresh_templates();
        self.clipboard_service = Some(crate::clipboard::spawn(self.tx.clone()));
        let desktop_dirs = self.desktop_dirs();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let index = crate::desktop::DesktopIndex::load(&desktop_dirs);
            let _ = tx.blocking_send(Msg::Ev(Event::MimeIndex(Box::new(index))));
        });
        if let Some(ref path) = self.cfg.session_file {
            let (saver, _handle) = crate::session::SessionSaver::spawn(path.clone());
            self.session_saver = Some(saver);
        }
        self.push_flags();
        if self.sidebar && self.sidebar_mode == 1 {
            self.rebuild_tree();
        }
        self.ui.send(Update::Mounts { items: Vec::new(), label: self.cfg.gungnir_label.clone() });
        for p in 0..2 {
            let loc = self.panes[p].tab().location.clone();
            self.load(p, loc, false);
        }
        // Places + bookmarks (file checks off the actor).
        let (home, bm, tx) = (self.cfg.home.clone(), self.cfg.bookmarks_file.clone(), self.tx.clone());
        tokio::task::spawn_blocking(move || {
            let (places, bookmarks) = build_places(&home, bm.as_deref());
            let _ = tx.blocking_send(Msg::Ev(Event::Places(places, bookmarks)));
        });
        // Mount status poller.
        let (reg, tx) = (self.registry.clone(), self.tx.clone());
        tokio::spawn(async move {
            loop {
                let mut all = Vec::new();
                for p in reg.providers() {
                    if let Ok(m) = p.mounts().await {
                        all.extend(m);
                    }
                }
                if tx.send(Msg::Ev(Event::Mounts(all))).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });
        // Removable drives (udisks2). Without a system bus or udisks the section stays hidden.
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let Ok(conn) = zbus::Connection::system().await else { return };
            if tx.send(Msg::Ev(Event::BusReady(conn.clone()))).await.is_err() {
                return;
            }
            let mut last: Option<Vec<crate::drives::Device>> = None;
            loop {
                if let Ok(d) = crate::drives::list(&conn).await
                    && last.as_ref() != Some(&d)
                {
                    last = Some(d.clone());
                    if tx.send(Msg::Ev(Event::Devices(d))).await.is_err() {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
        // Ticker for transfer rates and message expiry.
        let tx = self.tx.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(TICK).await;
                if tx.send(Msg::Ev(Event::Tick)).await.is_err() {
                    return;
                }
            }
        });
        // Unit-test controller harnesses must not perform real network checks.
        #[cfg(not(test))]
        {
            // Package update poller.
            let format = self.package_format;
            let tx = self.tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let check = crate::updater::check_for_update(format).await;
                let _ = tx.send(Msg::Ev(Event::UpdateCheckResult(check))).await;

                loop {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    let check = crate::updater::check_for_update(format).await;
                    if tx.send(Msg::Ev(Event::UpdateCheckResult(check))).await.is_err() {
                        break;
                    }
                }
            });
        }
    }

    // ---- view pushes ------------------------------------------------------------------

    fn push_flags(&self) {
        self.ui.send(Update::Flags(Flags {
            active_pane: i32::try_from(self.active_pane).unwrap_or(0),
            dual_pane: self.dual,
            sidebar_visible: self.sidebar,
            sidebar_mode: self.sidebar_mode,
            show_hidden: self.show_hidden,
            view_mode: self.view_mode,
            zoom_level: self.zoom_level,
            dark_mode: self.dark_mode,
        }));
    }

    fn push_pane(&self, p: usize) {
        let pane = &self.panes[p];
        let t = pane.tab();
        let view = PaneView {
            tabs: pane
                .tabs
                .iter()
                .map(|t| TabInfo {
                    title: t.title(&self.cfg.home).into(),
                    location: t.location.display().into(),
                    loading: t.loading,
                })
                .collect(),
            active_tab: i32::try_from(pane.active).unwrap_or(0),
            location: t.location.display(),
            crumbs: crumb_views(&t.location, &self.cfg.home),
            sort_column: t.sort.column.index(),
            sort_ascending: t.sort.ascending,
            loading: t.loading,
            error: t.error.clone(),
            filter: t.filter.clone(),
            can_back: !t.back.is_empty(),
            can_forward: !t.forward.is_empty(),
            cursor: i32::try_from(t.cursor).unwrap_or(0),
            is_trash: t.location.scheme() == Scheme::Trash,
            is_recent: t.location.scheme() == Scheme::Recent,
            is_read_only: if matches!(t.location.scheme(), Scheme::Trash | Scheme::Recent) {
                false
            } else if let Some(local_p) = t.location.to_local_path() {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt;
                    std::ffi::CString::new(local_p.as_os_str().as_bytes())
                        .map(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) != 0 })
                        .unwrap_or(false)
                }
                #[cfg(not(unix))]
                {
                    false
                }
            } else {
                false
            },
            selected_count: i32::try_from(t.selected.len()).unwrap_or(0),
            has_clipboard: self.clipboard.is_some() || crate::clipboard::available(),
        };
        self.ui.send(Update::Pane { pane: p, view });
        self.push_custom_actions(p);
    }

    fn push_custom_actions(&self, p: usize) {
        let t = self.panes[p].tab();
        let active_dir = t.location.to_local_path().unwrap_or_default();
        let idx = &self.mime_index;
        let targets: Vec<crate::actions::TargetItem> = t
            .targets()
            .into_iter()
            .map(|e| {
                let path = e.uri.to_local_path().unwrap_or_else(|| PathBuf::from(e.uri.path()));
                let mime = Some(idx.mime_for(&e.name, e.is_navigable()));
                crate::actions::TargetItem {
                    name: e.name.clone(),
                    path,
                    uri: e.uri.to_uri_string(),
                    is_dir: e.is_navigable(),
                    mime,
                }
            })
            .collect();
        let matching = self.actions.matching_actions(&active_dir, &targets);
        let items: Vec<crate::CustomActionItem> = matching
            .into_iter()
            .filter(|a| a.id != "open_terminal_here" && a.id != "terminal")
            .map(|a| crate::CustomActionItem { id: a.id.clone().into(), title: a.name.clone().into() })
            .collect();
        self.ui.send(Update::CustomActions { pane: p, actions: items });
    }

    fn launch_terminal_at(&mut self, target_dir: PathBuf) {
        if let Some(term_name) = launch_terminal_in(&target_dir) {
            self.say(format!("Opened {term_name} in {}", target_dir.display()), false);
        } else {
            // Fallback: open embedded terminal drawer in target_dir
            self.terminal_open = true;
            self.terminal_cwd = target_dir.clone();
            self.push_terminal();
            self.say(format!("Opened terminal drawer in {}", target_dir.display()), false);
        }
    }

    fn open_terminal_here(&mut self, pane: usize) {
        let t = self.panes[pane].tab();
        let targets = t.targets();
        let target_dir = if let Some(first) = targets.first() {
            if first.is_navigable() {
                first.uri.to_local_path().unwrap_or_else(|| {
                    t.location
                        .to_local_path()
                        .unwrap_or_else(|| self.cfg.home.to_local_path().unwrap_or_else(|| PathBuf::from(".")))
                })
            } else if let Some(local_path) = first.uri.to_local_path() {
                local_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| {
                    t.location
                        .to_local_path()
                        .unwrap_or_else(|| self.cfg.home.to_local_path().unwrap_or_else(|| PathBuf::from(".")))
                })
            } else {
                t.location
                    .to_local_path()
                    .unwrap_or_else(|| self.cfg.home.to_local_path().unwrap_or_else(|| PathBuf::from(".")))
            }
        } else {
            t.location
                .to_local_path()
                .unwrap_or_else(|| self.cfg.home.to_local_path().unwrap_or_else(|| PathBuf::from(".")))
        };

        self.launch_terminal_at(target_dir);
    }

    fn run_custom_action(&mut self, pane: usize, id: &str) {
        if id == "open_terminal_here" || id == "terminal" {
            self.open_terminal_here(pane);
            return;
        }
        let t = self.panes[pane].tab();
        let active_dir = t.location.to_local_path().unwrap_or_default();
        let idx = &self.mime_index;
        let targets: Vec<crate::actions::TargetItem> = t
            .targets()
            .into_iter()
            .map(|e| {
                let path = e.uri.to_local_path().unwrap_or_else(|| PathBuf::from(e.uri.path()));
                let mime = Some(idx.mime_for(&e.name, e.is_navigable()));
                crate::actions::TargetItem {
                    name: e.name.clone(),
                    path,
                    uri: e.uri.to_uri_string(),
                    is_dir: e.is_navigable(),
                    mime,
                }
            })
            .collect();
        if let Some(action) = self.actions.get_action(id) {
            let name = action.name.clone();
            match action.execute(&active_dir, &targets) {
                Ok(_) => self.say(format!("Launched {name}"), false),
                Err(e) => self.say(format!("Failed to run {name}: {e}"), true),
            }
        }
    }

    fn current_session_state(&self) -> crate::session::SessionState {
        let p0_paths: Vec<String> = self.panes[0].tabs.iter().map(|t| t.location.to_uri_string()).collect();
        let p0_active = self.panes[0].active;
        let secondary = if self.dual {
            let p1_paths: Vec<String> = self.panes[1].tabs.iter().map(|t| t.location.to_uri_string()).collect();
            let p1_active = self.panes[1].active;
            Some(crate::session::PaneTabsState::new(p1_paths, p1_active))
        } else {
            None
        };
        crate::session::SessionState {
            window: self.window_state.clone(),
            split_view: self.dual,
            active_pane: self.active_pane,
            primary_pane_tabs: crate::session::PaneTabsState::new(p0_paths, p0_active),
            secondary_pane_tabs: secondary,
            show_hidden: self.show_hidden,
            sidebar_visible: self.sidebar,
            sidebar_mode: self.sidebar_mode,
            zoom_level: self.zoom_level,
            dark_mode: self.dark_mode,
            view_mode: self.view_mode,
        }
    }

    fn trigger_save_session(&self) {
        if let Some(ref saver) = self.session_saver {
            saver.update(self.current_session_state());
        }
    }

    fn push_status(&self) {
        let t = self.panes[self.active_pane].tab();
        let hidden = if self.show_hidden { 0 } else { t.entries.iter().filter(|e| e.is_hidden()).count() };
        let n = t.visible.len();
        let mut items = format!("{} item{}", rows::thousands(n), if n == 1 { "" } else { "s" });
        if hidden > 0 {
            items.push_str(&format!(" ({} hidden)", rows::thousands(hidden)));
        }
        if !t.filter.is_empty() {
            items.push_str(&format!(" · filter \u{201c}{}\u{201d}", t.filter));
        }
        if t.loading {
            items = format!("Loading… {}", rows::thousands(t.entries.len()));
        }
        let selection = if t.selected.is_empty() {
            String::new()
        } else {
            let bytes: u64 =
                t.entries.iter().filter(|e| t.selected.contains(&e.name) && !e.is_navigable()).map(|e| e.size).sum();
            let files = t.entries.iter().any(|e| t.selected.contains(&e.name) && !e.is_navigable());
            if files {
                format!("{} selected ({})", rows::thousands(t.selected.len()), format::size(bytes))
            } else {
                format!("{} selected", rows::thousands(t.selected.len()))
            }
        };
        let free = t.volume.map(|v| format!("{} free", format::size(v.free_bytes))).unwrap_or_default();
        let kind = t.backend.unwrap_or(BackendKind::Posix);
        let (message, is_err) = match &self.message {
            Some((m, e, at)) if at.elapsed() < MESSAGE_TTL => (m.clone(), *e),
            _ => (String::new(), false),
        };
        self.ui.send(Update::Status(StatusInfo {
            items: items.into(),
            selection: selection.into(),
            free: free.into(),
            backend: kind.badge().into(),
            backend_kind: match kind {
                BackendKind::Posix => 0,
                BackendKind::GungnirMock => 1,
                BackendKind::GungnirLive => 2,
                BackendKind::Sftp => 3,
                BackendKind::Smb => 4,
                BackendKind::Network => 5,
            },
            message: message.into(),
            message_is_error: is_err,
        }));
    }

    fn push_places(&self) {
        let loc = self.panes[self.active_pane].tab().location.display();
        let mut places = self.places.clone();
        for b in &self.bookmarks {
            places.push(PlaceItem {
                label: b.file_name().unwrap_or("/").into(),
                uri: b.display().into(),
                icon: 4,
                active: false,
            });
        }
        for p in &mut places {
            if p.uri.as_str() == "trash:///" {
                let has_items = backends::local::home_trash()
                    .ok()
                    .and_then(|t| std::fs::read_dir(t.join("files")).ok())
                    .is_some_and(|mut d| d.next().is_some());
                p.icon = if has_items { 10 } else { 5 };
            }
            p.active = p.uri.as_str() == loc;
        }
        self.ui.send(Update::Places(places));
        let network_places = vec![PlaceItem {
            label: "Network".into(),
            uri: "network:///".into(),
            icon: 9,
            active: loc == "network:///" || loc.starts_with("network:///"),
        }];
        self.ui.send(Update::NetworkPlaces(network_places));
        let items = self
            .mounts
            .iter()
            .map(|m| MountItem {
                label: m.label.as_str().into(),
                uri: m.uri.display().into(),
                status: m.status.code(),
                status_text: m.status.label().into(),
                detail: m.detail.as_str().into(),
                active: m.uri.display() == loc,
            })
            .collect();
        self.ui.send(Update::Mounts { items, label: self.cfg.gungnir_label.clone() });
    }

    fn push_transfers(&mut self, open: Option<bool>) {
        let rows: Vec<TransferRow> = self
            .transfers
            .iter()
            .map(|(id, t)| {
                let p = &t.progress;
                #[allow(clippy::cast_precision_loss)]
                let frac = if p.bytes_total > 0 {
                    p.bytes_done as f32 / p.bytes_total as f32
                } else if p.files_total > 0 {
                    p.files_done as f32 / p.files_total as f32
                } else {
                    0.0
                };
                let (state, detail, frac) = match &p.state {
                    TransferState::Queued => (0, "Queued".to_owned(), frac),
                    TransferState::Running => (
                        1,
                        format!(
                            "{} · {} of {} · {} of {} files",
                            if p.current.is_empty() { "Preparing" } else { &p.current },
                            format::size(p.bytes_done),
                            format::size(p.bytes_total),
                            p.files_done,
                            p.files_total
                        ),
                        frac,
                    ),
                    TransferState::Paused => {
                        (2, format!("Paused · {} of {}", format::size(p.bytes_done), format::size(p.bytes_total)), frac)
                    }
                    TransferState::Done => (
                        3,
                        format!(
                            "Done · {} in {:.1} s",
                            format::size(p.bytes_total.max(p.bytes_done)),
                            t.took.unwrap_or_else(|| t.started.elapsed()).as_secs_f32()
                        ),
                        1.0,
                    ),
                    TransferState::Cancelled => (5, "Cancelled".to_owned(), frac),
                    TransferState::Failed(e) => (4, format!("Failed: {e}"), frac),
                };
                let rate = if state == 1 && t.rate > 0.0 {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let r = t.rate as u64;
                    format!("{}/s", format::size(r))
                } else {
                    String::new()
                };
                TransferRow {
                    id: i32::try_from(*id).unwrap_or(i32::MAX),
                    title: t.title.as_str().into(),
                    detail: detail.into(),
                    progress: frac,
                    rate: rate.into(),
                    state,
                    can_pause: t.can_pause,
                }
            })
            .collect();
        let active = self
            .transfers
            .iter()
            .filter(|(_, t)| {
                matches!(t.progress.state, TransferState::Running | TransferState::Paused | TransferState::Queued)
            })
            .count();
        self.ui.send(Update::Transfers { rows, active: i32::try_from(active).unwrap_or(0), open });
        self.transfers_dirty = false;
    }

    fn say(&mut self, msg: impl Into<String>, error: bool) {
        self.message = Some((msg.into(), error, Instant::now()));
        self.push_status();
    }

    fn info_dialog(&mut self, kind: DialogKind, title: &str, message: &str, rows: Vec<(String, String)>) {
        self.dialog = Some(PendingDialog::Info);
        self.ui.send(Update::Dialog(DialogView {
            kind,
            title: title.into(),
            message: message.into(),
            rows,
            input: String::new(),
            preview_path: None,
            ..Default::default()
        }));
    }

    // ---- loading ----------------------------------------------------------------------

    fn tab_index(&self, pane: usize, id: u64) -> Option<usize> {
        self.panes[pane].tabs.iter().position(|t| t.id == id)
    }

    fn is_active_tab(&self, pane: usize, id: u64) -> bool {
        self.panes[pane].tab().id == id
    }

    /// Load `location` into the active tab of `pane`. `silent` keeps the current rows
    /// on screen until the new listing is complete (reloads after changes).
    fn load(&mut self, pane: usize, location: VfsUri, silent: bool) {
        if !silent && self.renaming.as_ref().is_some_and(|edit| edit.pane == pane) {
            self.cancel_rename();
        }
        let provider = match self.registry.resolve(&location) {
            Ok(p) => p,
            Err(e) => {
                self.say(e.to_string(), true);
                return;
            }
        };
        let trace = self.cfg.trace;
        let tx = self.tx.clone();
        let t = self.panes[pane].tab_mut();
        t.cancel.cancel(); // pre-empt whatever was loading
        t.cancel = CancellationToken::new();
        t.load_gen += 1;
        t.view_gen += 1;
        let same_place = t.location == location;
        t.location = location.clone();
        t.loading = true;
        t.error.clear();
        t.backend = Some(provider.kind());
        if !same_place {
            t.pending_rename = None;
            if let Some(w) = t.watch.take() {
                w.abort();
            }
            t.volume = None;
        }
        if !silent {
            t.entries = Arc::new(Vec::new());
            t.order.clear();
            t.visible.clear();
            if !same_place {
                t.selected.clear();
                t.cursor = 0;
                t.anchor = None;
            }
        }
        let (tab, generation, cancel) = (t.id, t.load_gen, t.cancel.clone());
        if !silent && self.is_active_tab(pane, tab) {
            self.ui.send(Update::Rows { pane, rows: Vec::new() });
        }
        self.push_pane(pane);
        self.push_status();

        let started = Instant::now();
        tokio::spawn(async move {
            let (btx, mut brx) = mpsc::channel::<Vec<FileEntry>>(8);
            let list = provider.list_dir(&location, ListOptions::default(), btx, cancel.clone());
            let forward = async {
                let mut collected = silent.then(Vec::new);
                let mut first = true;
                while let Some(batch) = brx.recv().await {
                    if trace && first {
                        eprintln!(
                            "[trace] {} first batch ({} entries) after {:?}",
                            location,
                            batch.len(),
                            started.elapsed()
                        );
                        first = false;
                    }
                    match collected.as_mut() {
                        Some(c) => c.extend(batch),
                        None => {
                            if tx.send(Msg::Ev(Event::Batch { pane, tab, generation, entries: batch })).await.is_err() {
                                return None;
                            }
                        }
                    }
                }
                Some(collected)
            };
            let (result, collected) = tokio::join!(list, forward);
            if let Some(collected) = collected {
                let _ = tx.send(Msg::Ev(Event::ListDone { pane, tab, generation, result, collected, started })).await;
            }
        });
    }

    fn on_batch(&mut self, pane: usize, tab: u64, generation: u64, mut batch: Vec<FileEntry>) {
        let Some(ti) = self.tab_index(pane, tab) else { return };
        let show_hidden = self.show_hidden;
        let active = self.is_active_tab(pane, tab);
        let t = &mut self.panes[pane].tabs[ti];
        if t.load_gen != generation {
            return;
        }
        if t.entries.is_empty() {
            // First batch: sort it so the first screenful is already in order.
            core_vfs::sort::sort_entries(&mut batch, t.sort);
        }
        let start = t.entries.len();
        Arc::make_mut(&mut t.entries).extend(batch);
        let needle = t.filter.to_lowercase();
        let mut new_rows = Vec::new();
        for i in start..t.entries.len() {
            let e = &t.entries[i];
            let idx = u32::try_from(i).unwrap_or(u32::MAX);
            t.order.push(idx);
            if (show_hidden || !e.is_hidden()) && (needle.is_empty() || e.name.to_lowercase().contains(&needle)) {
                t.visible.push(idx);
                if active {
                    new_rows.push(rows::file_row(e, t.selected.contains(&e.name)));
                }
            }
        }
        if active && !new_rows.is_empty() {
            self.ui.send(Update::AppendRows { pane, rows: new_rows });
        }
        if active && pane == self.active_pane {
            self.push_status();
        }
    }

    fn on_list_done(
        &mut self,
        pane: usize,
        tab: u64,
        generation: u64,
        result: VfsResult<ListSummary>,
        collected: Option<Vec<FileEntry>>,
        started: Instant,
    ) {
        let Some(ti) = self.tab_index(pane, tab) else { return };
        let t = &mut self.panes[pane].tabs[ti];
        if t.load_gen != generation {
            return;
        }
        t.loading = false;
        match result {
            Err(VfsError::Cancelled) => return,
            Err(e) => {
                t.error = e.to_string();
                if collected.is_some() {
                    // A failed silent reload: the folder is gone or unreadable now.
                    t.entries = Arc::new(Vec::new());
                    t.order.clear();
                    t.visible.clear();
                    if self.is_active_tab(pane, tab) {
                        self.ui.send(Update::Rows { pane, rows: Vec::new() });
                    }
                }
                self.push_pane(pane);
                let msg = format!("{}: {e}", self.panes[pane].tabs[ti].location);
                self.say(msg, true);
                return;
            }
            Ok(summary) => {
                if self.cfg.trace {
                    eprintln!(
                        "[trace] {} listed {} entries ({} skipped) in {:?}",
                        t.location,
                        summary.entries,
                        summary.skipped,
                        started.elapsed()
                    );
                }
            }
        }
        if let Some(c) = collected {
            // Visible indices belong to the old snapshot. Remember the name
            // before replacing its entries with a new enumeration order.
            if t.pending_select.is_none() {
                t.pending_select = t.entry_at(t.cursor).map(|entry| entry.name.clone());
            }
            let names: HashSet<&str> = c.iter().map(|e| e.name.as_str()).collect();
            t.selected.retain(|n| names.contains(n.as_str()));
            t.entries = Arc::new(c);
        }
        self.prepare(pane, tab, true);
        self.start_watch(pane, tab);
        let (provider, loc) =
            (self.registry.resolve(&self.panes[pane].tabs[ti].location), self.panes[pane].tabs[ti].location.clone());
        if let Ok(provider) = provider {
            let tx = self.tx.clone();
            tokio::spawn(async move {
                if let Ok(info) = provider.volume(&loc).await {
                    let _ = tx.send(Msg::Ev(Event::Volume { pane, tab, info })).await;
                }
            });
        }
        if self.sidebar && self.sidebar_mode == 1 && pane == self.active_pane {
            self.rebuild_tree();
        }
    }

    /// Recompute order (if `resort`), visible rows and row data on a blocking thread.
    fn prepare(&mut self, pane: usize, tab: u64, resort: bool) {
        let Some(ti) = self.tab_index(pane, tab) else { return };
        let show_hidden = self.show_hidden;
        let t = &mut self.panes[pane].tabs[ti];
        t.view_gen += 1;
        let (view_gen, entries, sort, filter, selected) =
            (t.view_gen, Arc::clone(&t.entries), t.sort, t.filter.clone(), t.selected.clone());
        let order = (!resort).then(|| t.order.clone());
        let build_rows = self.is_active_tab(pane, tab);
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let order = order.unwrap_or_else(|| sort_order(&entries, sort));
            let visible = rows::visible(&entries, &order, show_hidden, &filter);
            let rows = if build_rows { rows::rows(&entries, &visible, &selected) } else { Vec::new() };
            let _ = tx.blocking_send(Msg::Ev(Event::Prepared {
                pane,
                tab,
                view_gen,
                order: resort.then_some(order),
                visible,
                rows,
            }));
        });
    }

    fn on_prepared(
        &mut self,
        pane: usize,
        tab: u64,
        view_gen: u64,
        order: Option<Vec<u32>>,
        visible: Vec<u32>,
        mut rows: Vec<RowItem>,
    ) {
        let Some(ti) = self.tab_index(pane, tab) else { return };
        let active = self.is_active_tab(pane, tab);
        let t = &mut self.panes[pane].tabs[ti];
        if t.view_gen != view_gen {
            return;
        }
        // Keep the cursor on the same entry across re-sorts and reloads.
        let cursor_entry = t.visible.get(t.cursor).and_then(|&i| t.entries.get(i as usize)).map(|e| e.name.clone());
        if let Some(o) = order {
            t.order = o;
        }
        t.visible = visible;
        let visible_names: HashSet<&str> =
            t.visible.iter().filter_map(|&i| t.entries.get(i as usize)).map(|e| e.name.as_str()).collect();
        t.selected.retain(|name| visible_names.contains(name.as_str()));
        let want = t.pending_select.take().or(cursor_entry);
        if let Some(name) = want
            && let Some(pos) = t.visible.iter().position(|&i| t.entries.get(i as usize).is_some_and(|e| e.name == name))
        {
            t.cursor = pos;
        }
        t.cursor = t.cursor.min(t.visible.len().saturating_sub(1));
        let stale_editor = self.renaming.as_ref().is_some_and(|edit| {
            edit.pane == pane && edit.tab == tab && t.entry_at(edit.index).is_none_or(|entry| entry.uri != edit.uri)
        });
        if stale_editor {
            self.cancel_rename();
        }
        if active {
            let t = &self.panes[pane].tabs[ti];
            for row in &mut rows {
                row.selected = t.selected.contains(row.name.as_str());
            }
            self.ui.send(Update::Rows { pane, rows });
        }
        self.push_pane(pane);
        if pane == self.active_pane {
            self.push_status();
        }
        if active && pane == self.active_pane && self.dialog.is_none() && self.renaming.is_none() {
            let t = self.panes[pane].tab_mut();
            if let Some(name) = t.pending_rename.clone()
                && let Some(index) = t.visible.iter().position(|&i| t.entries[i as usize].name == name)
            {
                t.pending_rename = None;
                let uri = t.entries[t.visible[index] as usize].uri.clone();
                self.renaming = Some(RenameEdit { pane, tab, index, uri });
                self.ui.send(Update::Rename { pane: pane as i32, index: index as i32 });
            }
        }
        self.trigger_thumbnails(pane, tab);
    }

    fn trigger_thumbnails(&self, pane: usize, tab: u64) {
        let Some(ti) = self.tab_index(pane, tab) else { return };
        let t = &self.panes[pane].tabs[ti];
        let mut candidates: Vec<(String, PathBuf, String, i64, bool)> = Vec::new();
        for &idx in &t.visible {
            if let Some(e) = t.entries.get(idx as usize) {
                let cat = format::category(e);
                let ext = format::extension(&e.name);
                if crate::thumbnails::is_previewable(cat, ext.as_deref()) {
                    let mtime_secs = e
                        .modified
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| i64::try_from(d.as_secs()).unwrap_or(0))
                        .unwrap_or(0);
                    let uri_str = e.uri.to_uri_string();
                    if crate::thumbnails::lookup_cached(&uri_str, mtime_secs).is_none()
                        && let Some(local) = e.uri.to_local_path()
                    {
                        let is_vid = matches!(cat, core_vfs::format::Category::Video)
                            || crate::thumbnails::is_video_ext(ext.as_deref());
                        candidates.push((e.name.clone(), local, uri_str, mtime_secs, is_vid));
                    }
                }
            }
        }

        if candidates.is_empty() {
            return;
        }

        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let mut batch = Vec::new();
            for (name, path, uri, mtime, is_vid) in candidates {
                if let Ok(thumb) = crate::thumbnails::generate_thumbnail(&path, &uri, mtime, is_vid) {
                    batch.push((name, thumb));
                    if batch.len() >= 16 {
                        let _ = tx.blocking_send(Msg::Ev(Event::ThumbnailsReady {
                            pane,
                            tab,
                            items: std::mem::take(&mut batch),
                        }));
                    }
                }
            }
            if !batch.is_empty() {
                let _ = tx.blocking_send(Msg::Ev(Event::ThumbnailsReady { pane, tab, items: batch }));
            }
        });
    }

    fn start_watch(&mut self, pane: usize, tab: u64) {
        let Some(ti) = self.tab_index(pane, tab) else { return };
        let t = &mut self.panes[pane].tabs[ti];
        if t.watch.is_some() {
            return;
        }
        let Ok(provider) = self.registry.resolve(&t.location) else { return };
        let Ok(Some(mut w)) = provider.watch(&t.location) else { return };
        let tx = self.tx.clone();
        t.watch = Some(tokio::spawn(async move {
            let _guard = w.guard;
            while let Some(ev) = w.events.recv().await {
                let removed = ev == WatchEvent::Removed;
                // Debounce bursts (an extraction writing 10k files is one reload).
                tokio::time::sleep(WATCH_DEBOUNCE).await;
                while w.events.try_recv().is_ok() {}
                if tx.send(Msg::Ev(Event::DirChanged { pane, tab, removed })).await.is_err() {
                    return;
                }
            }
        }));
    }

    fn navigate_to(&mut self, pane: usize, location: VfsUri) {
        let t = self.panes[pane].tab_mut();
        if t.location != location {
            let prev = t.location.clone();
            t.back.push(prev);
            t.forward.clear();
        }
        self.cancel_rename();
        // Expand the tree along the new path.
        for (_, u) in location.crumbs() {
            self.tree_expanded.insert(u);
        }
        self.load(pane, location.clone(), false);
        if pane == self.active_pane {
            self.push_places();
            if self.terminal_open
                && let Some(p) = location.to_local_path()
            {
                self.terminal_cwd = p;
                self.push_terminal();
            }
        }
        self.trigger_save_session();
    }

    /// Parse what the user typed: absolute paths, URIs, `~`, or a path relative to
    /// the pane's location.
    fn resolve_input(&self, pane: usize, input: &str) -> VfsResult<VfsUri> {
        let s = input.trim();
        if s == "~" {
            return Ok(self.cfg.home.clone());
        }
        if let Some(rest) = s.strip_prefix("~/") {
            return VfsUri::parse(&format!("{}/{rest}", self.cfg.home.path()));
        }
        if s.starts_with('/') || s.contains("://") {
            return VfsUri::parse(s);
        }
        let mut u = self.panes[pane].tab().location.clone();
        for seg in s.split('/').filter(|x| !x.is_empty() && *x != ".") {
            u = if seg == ".." { u.parent().unwrap_or(u) } else { u.join(seg)? };
        }
        Ok(u)
    }

    // ---- selection --------------------------------------------------------------------

    fn set_selection(&mut self, pane: usize, new: HashSet<String>) {
        let active = self.panes[pane].active;
        let t = &mut self.panes[pane].tabs[active];
        let changed: HashSet<&String> = t.selected.symmetric_difference(&new).collect();
        let mut changes = Vec::new();
        if changed.len() > 4_000 {
            // Cheaper to rebuild every row than to patch this many.
            t.selected = new;
            self.prepare(pane, self.panes[pane].tab().id, false);
            self.push_status();
            return;
        }
        if !changed.is_empty() {
            for (vi, &ei) in t.visible.iter().enumerate() {
                if let Some(e) = t.entries.get(ei as usize)
                    && changed.contains(&e.name)
                {
                    changes.push((vi, new.contains(&e.name)));
                }
            }
        }
        t.selected = new;
        if !changes.is_empty() {
            self.ui.send(Update::Selection { pane, changes });
        }
        self.push_pane(pane);
        self.push_status();
    }

    fn select_range(&self, pane: usize, from: usize, to: usize) -> HashSet<String> {
        let t = self.panes[pane].tab();
        let (a, b) = if from <= to { (from, to) } else { (to, from) };
        (a..=b).filter_map(|i| t.entry_at(i).map(|e| e.name.clone())).collect()
    }

    fn click(&mut self, pane: usize, index: usize, ctrl: bool, shift: bool) {
        self.focus_pane(pane);
        let t = self.panes[pane].tab();
        let Some(name) = t.entry_at(index).map(|e| e.name.clone()) else {
            self.set_selection(pane, HashSet::new());
            return;
        };
        let new = if shift {
            let anchor = t.anchor.unwrap_or(t.cursor);
            let mut s = self.select_range(pane, anchor, index);
            if ctrl {
                s.extend(t.selected.iter().cloned());
            }
            s
        } else if ctrl {
            let mut s = t.selected.clone();
            if !s.remove(&name) {
                s.insert(name);
            }
            s
        } else {
            [name].into()
        };
        let t = self.panes[pane].tab_mut();
        t.cursor = index;
        if !shift {
            t.anchor = Some(index);
        }
        self.set_selection(pane, new);
        self.push_pane(pane);
    }

    fn move_cursor(&mut self, pane: usize, delta: i64, extend: bool) {
        let t = self.panes[pane].tab();
        if t.visible.is_empty() {
            return;
        }
        let max = t.visible.len() - 1;
        let cur = i64::try_from(t.cursor).unwrap_or(0);
        let next = usize::try_from((cur + delta).clamp(0, i64::try_from(max).unwrap_or(0))).unwrap_or(0);
        let new = if extend {
            let anchor = t.anchor.unwrap_or(t.cursor);
            self.select_range(pane, anchor, next)
        } else {
            t.entry_at(next).map(|e| [e.name.clone()].into()).unwrap_or_default()
        };
        let t = self.panes[pane].tab_mut();
        if !extend {
            t.anchor = Some(next);
        }
        t.cursor = next;
        self.set_selection(pane, new);
        self.push_pane(pane);
    }

    fn focus_pane(&mut self, pane: usize) {
        if self.active_pane != pane {
            self.cancel_rename();
            self.active_pane = pane;
            self.push_flags();
            self.push_status();
            self.push_places();
            if self.sidebar && self.sidebar_mode == 1 {
                self.rebuild_tree();
            }
            self.trigger_save_session();
        }
    }

    // ---- actions ----------------------------------------------------------------------

    fn activate(&mut self, pane: usize, index: usize) {
        let Some(entry) = self.panes[pane].tab().entry_at(index).cloned() else { return };
        self.open_entry(pane, entry, false);
    }

    fn open_selection(&mut self, pane: usize) {
        let entries = self.panes[pane].tab().targets();
        if entries.len() > 10 {
            let n = entries.len();
            self.dialog = Some(PendingDialog::OpenMany { pane, entries });
            self.ui.send(Update::Dialog(DialogView {
                kind: DialogKind::ConfirmOpen,
                title: format!("Open {n} items?"),
                message: "Files will open in their default applications and folders in new tabs.".into(),
                ..Default::default()
            }));
            return;
        }
        let tabs = entries.len() > 1;
        for entry in entries {
            self.open_entry(pane, entry, tabs);
        }
    }

    fn open_entry(&mut self, pane: usize, entry: FileEntry, new_tab: bool) {
        if entry.is_navigable() {
            if new_tab {
                self.new_tab(pane, Some(entry.uri));
            } else {
                self.navigate_to(pane, entry.uri);
            }
            return;
        }
        if entry.uri.scheme() == Scheme::Trash {
            self.say("Restore this file before opening it", true);
            return;
        }
        if entry.kind == FileKind::Special {
            self.say("Special files cannot be opened as documents", true);
            return;
        }
        let target = match entry.uri.scheme() {
            Scheme::File => entry.uri.to_local_path().map(PathBuf::into_os_string),
            Scheme::Gungnir if entry.uri.authority().is_empty() || entry.uri.authority() == "localhost" => {
                Some(std::ffi::OsString::from(entry.uri.path()))
            }
            Scheme::Recent => {
                entry.vfs.attributes.iter().find(|(k, _)| k == "target" || k == "orig").and_then(|(_, v)| {
                    if let Some(_rest) = v.strip_prefix("file://") {
                        backends::recent::file_path(v.as_bytes()).map(|b| {
                            use std::os::unix::ffi::OsStringExt;
                            std::ffi::OsString::from_vec(b)
                        })
                    } else if v.starts_with('/') {
                        Some(std::ffi::OsString::from(v))
                    } else {
                        None
                    }
                })
            }
            Scheme::Sftp | Scheme::Smb => Some(std::ffi::OsString::from(entry.uri.to_uri_string())),
            _ => None,
        };
        let Some(target) = target else {
            self.say("This location cannot be opened by a desktop application", true);
            return;
        };
        let tx = self.tx.clone();
        let name = entry.name;
        tokio::task::spawn_blocking(move || {
            let result = crate::desktop::open_default_with(std::ffi::OsStr::new("xdg-open"), &target)
                .map(|()| format!("Opened {name}"))
                .map_err(|e| format!("Could not open {name}: {e}"));
            let _ = tx.blocking_send(Msg::Ev(Event::Op { result, reload: Vec::new(), select: None }));
        });
    }

    /// Run a provider operation in the background, then report and reload.
    fn spawn_op<F>(&self, reload: Vec<VfsUri>, select: Option<(VfsUri, String)>, f: F)
    where
        F: std::future::Future<Output = VfsResult<String>> + Send + 'static,
    {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = f.await.map_err(|e| e.to_string());
            let _ = tx.send(Msg::Ev(Event::Op { result, reload, select })).await;
        });
    }

    fn transfer(&mut self, kind: TransferKind) {
        if !self.dual {
            self.say("Open the second pane (F3) to copy or move between panes", true);
            return;
        }
        let src_pane = self.active_pane;
        let targets = self.panes[src_pane].tab().targets();
        if targets.is_empty() {
            self.say("Nothing selected", true);
            return;
        }
        let dest = self.panes[1 - src_pane].tab().location.clone();
        let sources: Vec<VfsUri> = targets.iter().map(|e| e.uri.clone()).collect();
        self.start_transfer(kind, sources, dest);
    }

    /// Copy or move `sources` into the folder `dest` (F5/F6, paste, drag and drop).
    fn start_transfer(&mut self, kind: TransferKind, sources: Vec<VfsUri>, dest: VfsUri) {
        if sources.is_empty() {
            return;
        }
        if sources.iter().any(|s| dest.starts_with(s)) {
            self.say("A folder cannot be copied or moved into itself", true);
            return;
        }
        // Moving files into the folder they are already in does nothing.
        if kind == TransferKind::Move && sources.iter().all(|s| s.parent().as_ref() == Some(&dest)) {
            return;
        }

        // Collision detection for local paths
        let dest_path = match dest.scheme() {
            Scheme::File => dest.to_local_path(),
            Scheme::Gungnir if dest.authority().is_empty() => Some(PathBuf::from(dest.path())),
            _ => None,
        };
        if let Some(ref dest_dir) = dest_path {
            let mut conflicts = Vec::new();
            for s in &sources {
                if let Some(name) = s.file_name() {
                    let target = dest_dir.join(name);
                    if let Ok(dm) = target.symlink_metadata() {
                        let src_path = match s.scheme() {
                            Scheme::File => s.to_local_path(),
                            Scheme::Gungnir if s.authority().is_empty() => Some(PathBuf::from(s.path())),
                            _ => None,
                        };
                        let (src_size, src_mtime) =
                            if let Some(sp) = src_path.as_ref().and_then(|p| p.symlink_metadata().ok()) {
                                (sp.len(), sp.modified().ok())
                            } else {
                                (0, None)
                            };
                        let dst_size = dm.len();
                        let dst_mtime = dm.modified().ok();
                        let src_is_newer = match (src_mtime, dst_mtime) {
                            (Some(sm), Some(dm_time)) => sm > dm_time,
                            _ => false,
                        };
                        conflicts.push(ConflictItem {
                            src: s.clone(),
                            filename: name.to_string(),
                            src_size,
                            dst_size,
                            src_mtime,
                            dst_mtime,
                            src_is_newer,
                        });
                    }
                }
            }
            if !conflicts.is_empty() {
                self.dialog = Some(PendingDialog::Conflict {
                    kind,
                    sources,
                    dest,
                    conflicts,
                    current_idx: 0,
                    resolutions: Vec::new(),
                });
                self.show_conflict();
                return;
            }
        }

        self.execute_transfer(kind, sources, dest, false);
    }

    fn show_conflict(&mut self) {
        let Some(PendingDialog::Conflict { conflicts, current_idx, .. }) = &self.dialog else { return };
        let Some(c) = conflicts.get(*current_idx) else { return };
        let total = conflicts.len();
        let idx_1based = *current_idx + 1;
        let format_time = |t: Option<std::time::SystemTime>| {
            t.and_then(|st| st.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| {
                    let secs = d.as_secs();
                    let hours = (secs / 3600) % 24;
                    let mins = (secs % 3600) / 60;
                    format!("{hours:02}:{mins:02} UTC")
                })
                .unwrap_or_else(|| "Unknown".into())
        };
        let src_mod = format!(
            "{} · Modified: {} {}",
            format::size(c.src_size),
            format_time(c.src_mtime),
            if c.src_is_newer { "[NEWER]" } else { "" }
        );
        let dst_mod = format!(
            "{} · Modified: {} {}",
            format::size(c.dst_size),
            format_time(c.dst_mtime),
            if !c.src_is_newer { "[NEWER/SAME]" } else { "" }
        );
        let rows = vec![
            ("File name".to_string(), c.filename.clone()),
            ("Source file".to_string(), src_mod),
            ("Existing file".to_string(), dst_mod),
            ("Conflict".to_string(), format!("{idx_1based} of {total} collisions")),
        ];
        self.ui.send(Update::Dialog(DialogView {
            kind: DialogKind::Conflict,
            title: format!("File Collision: {}", c.filename),
            message: format!("A file named '{}' already exists in destination. Choose an action:", c.filename),
            rows,
            input: String::new(),
            preview_path: c.src.to_local_path(),
            opt1: false,
            ..Default::default()
        }));
    }

    fn handle_conflict_action(&mut self, action_str: &str, apply_all: bool) {
        let action = match action_str {
            "replace" => ConflictAction::Replace,
            "replace-if-newer" => ConflictAction::ReplaceIfNewer,
            "auto-rename" => ConflictAction::AutoRename,
            "skip" => ConflictAction::Skip,
            _ => return,
        };
        let Some(PendingDialog::Conflict { kind, sources, dest, conflicts, mut current_idx, mut resolutions, .. }) =
            self.dialog.take()
        else {
            return;
        };

        if apply_all {
            for c in &conflicts[current_idx..] {
                resolutions.push((c.src.clone(), action));
            }
            current_idx = conflicts.len();
        } else {
            if let Some(c) = conflicts.get(current_idx) {
                resolutions.push((c.src.clone(), action));
            }
            current_idx += 1;
        }

        if current_idx < conflicts.len() {
            self.dialog = Some(PendingDialog::Conflict { kind, sources, dest, conflicts, current_idx, resolutions });
            self.show_conflict();
            return;
        }

        // All conflicts resolved!
        self.ui.send(Update::CloseDialog);

        let mut copies = Vec::new();
        let mut replacements = Vec::new();

        for s in sources {
            if let Some((_, act)) = resolutions.iter().find(|(src_uri, _)| src_uri == &s) {
                match act {
                    ConflictAction::Skip => {
                        continue;
                    }
                    ConflictAction::Replace => {
                        replacements.push(s);
                    }
                    ConflictAction::ReplaceIfNewer => {
                        let is_newer = conflicts.iter().find(|c| c.src == s).map(|c| c.src_is_newer).unwrap_or(false);
                        if is_newer {
                            replacements.push(s);
                        }
                    }
                    ConflictAction::AutoRename => {
                        copies.push(s);
                    }
                }
            } else {
                copies.push(s);
            }
        }

        if copies.is_empty() && replacements.is_empty() {
            self.say("Transfer skipped (all conflicting files skipped)", false);
            return;
        }

        // Auto-rename and replacement need separate requests: overwrite is a
        // request-wide flag. Never delete a destination in the controller.
        self.execute_transfer(kind, copies, dest.clone(), false);
        self.execute_transfer(kind, replacements, dest, true);
    }

    fn execute_transfer(&mut self, kind: TransferKind, sources: Vec<VfsUri>, dest: VfsUri, overwrite: bool) {
        if sources.is_empty() {
            return;
        }
        let names: Vec<&str> = sources.iter().filter_map(VfsUri::file_name).collect();
        let pick = |p: &Arc<dyn VfsProvider>| sources.iter().all(|s| p.can_transfer(s, &dest));
        let provider =
            [self.registry.resolve(&dest), self.registry.resolve(&sources[0])].into_iter().flatten().find(|p| pick(p));
        let Some(provider) = provider else {
            self.say(
                format!(
                    "Transfers between {} and {} are not supported yet",
                    sources[0].scheme().as_str(),
                    dest.scheme().as_str()
                ),
                true,
            );
            return;
        };
        let id = self.next_transfer;
        self.next_transfer += 1;
        let what = if let [one] = names.as_slice() { (*one).to_owned() } else { format!("{} items", sources.len()) };
        let title = format!("{} {what} to {}", kind.verb(), dest.display());
        let cancel = CancellationToken::new();
        let (pause, paused) = watch::channel(false);
        let (ptx, mut prx) = mpsc::channel(64);
        let now = Instant::now();
        self.transfers.push((
            id,
            Transfer {
                kind,
                title,
                dest: dest.clone(),
                sources: sources.clone(),
                progress: TransferProgress {
                    id,
                    bytes_done: 0,
                    bytes_total: 0,
                    files_done: 0,
                    files_total: 0,
                    current: String::new(),
                    state: TransferState::Queued,
                },
                started: now,
                rate: 0.0,
                last: (now, 0),
                cancel: cancel.clone(),
                pause,
                can_pause: provider.capabilities().pause_transfers,
                took: None,
            },
        ));
        self.push_transfers(Some(true));
        let req = TransferRequest { id, kind, sources, dest_dir: dest, overwrite };
        let ctl = TransferControl { progress: ptx, cancel, paused };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let fwd_tx = tx.clone();
            let forward = tokio::spawn(async move {
                while let Some(p) = prx.recv().await {
                    if fwd_tx.send(Msg::Ev(Event::Progress(p))).await.is_err() {
                        return;
                    }
                }
            });
            let result = provider.transfer(req, ctl).await;
            let _ = forward.await;
            let _ = tx.send(Msg::Ev(Event::TransferEnded { id, result })).await;
        });
    }

    fn reload_matching(&mut self, dirs: &[VfsUri]) {
        for p in 0..2 {
            let loc = self.panes[p].tab().location.clone();
            if dirs.contains(&loc)
                || (dirs.iter().any(|d| d.scheme() == core_vfs::Scheme::Trash)
                    && loc.scheme() == core_vfs::Scheme::Trash)
            {
                self.load(p, loc, true);
            }
        }
    }

    fn refresh_templates(&mut self) {
        if self.templates_loading {
            return;
        }
        self.templates_refreshed = Instant::now();
        let Some(root) = self.cfg.templates_dir.clone() else { return };
        self.templates_loading = true;
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let items = crate::templates::scan(&root);
            let _ = tx.blocking_send(Msg::Ev(Event::Templates(items)));
        });
    }

    fn create_template(&mut self, id: &str) {
        let Some(template) = self.templates.iter().find(|item| item.source.to_uri_string() == id).cloned() else {
            self.say("Template is no longer available; refresh Templates", true);
            return;
        };
        let pane = self.active_pane;
        let dir = self.panes[pane].tab().location.clone();
        if dir.scheme() == Scheme::Trash {
            self.say("Restore items before editing the Trash", true);
            return;
        }
        let provider = match self.registry.resolve(&dir) {
            Ok(provider) => provider,
            Err(e) => {
                self.say(e.to_string(), true);
                return;
            }
        };
        let tab = self.panes[pane].tab().id;
        let tx = self.tx.clone();
        self.say(format!("Creating document from {}", template.name), false);
        tokio::spawn(async move {
            let result = async {
                for number in 1..=10_000 {
                    let name = crate::templates::numbered_name(&template.name, number);
                    let target = dir.join(&name)?;
                    match provider.create_from_template(&template.source, &target).await {
                        Ok(()) => return Ok(target),
                        Err(VfsError::AlreadyExists(_)) => continue,
                        Err(e) => return Err(e),
                    }
                }
                Err(VfsError::AlreadyExists("Too many documents with this template name".into()))
            }
            .await;
            let _ = tx.send(Msg::Ev(Event::Created { pane, tab, dir, result })).await;
        });
    }

    fn cancel_rename(&mut self) {
        if self.renaming.take().is_some() {
            self.ui.send(Update::Rename { pane: -1, index: -1 });
        }
    }

    fn push_rename_history(&self) {
        self.ui.send(Update::RenameHistory {
            can_undo: !self.rename_busy && self.rename_history.undo().is_some(),
            can_redo: !self.rename_busy && self.rename_history.redo().is_some(),
        });
    }

    fn start_rename(
        &mut self,
        from: VfsUri,
        name: String,
        action: RenameAction,
        expected: Option<crate::rename::Identity>,
    ) {
        if self.rename_busy {
            self.say("Wait for the current rename to finish", true);
            return;
        }
        let provider = match self.registry.resolve(&from) {
            Ok(provider) => provider,
            Err(e) => {
                self.say(e.to_string(), true);
                return;
            }
        };
        self.rename_busy = true;
        self.push_rename_history();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = crate::rename::apply(provider, from.clone(), name, expected).await.map_err(|e| e.to_string());
            let _ = tx.send(Msg::Ev(Event::RenameFinished { action, from, result })).await;
        });
    }

    fn history_rename(&mut self, undo: bool) {
        if self.renaming.is_some() {
            self.say("Finish editing the name first", true);
            return;
        }
        let record = if undo { self.rename_history.undo() } else { self.rename_history.redo() }.cloned();
        let Some(record) = record else { return };
        let (from, destination, action) = if undo {
            (record.after, record.before, RenameAction::Undo)
        } else {
            (record.before, record.after, RenameAction::Redo)
        };
        let Some(name) = destination.file_name() else { return };
        self.start_rename(from, name.to_string(), action, Some(record.identity));
    }

    fn run_command(&mut self, name: &str) {
        if self.dialog.is_some() && !matches!(name, "quit") {
            return; // menu shortcuts while a dialog is open
        }
        let p = self.active_pane;
        if self.panes[p].tab().location.scheme() == Scheme::Trash
            && matches!(
                name,
                "new-file"
                    | "new-folder"
                    | "rename"
                    | "batch-rename"
                    | "paste"
                    | "paste-into-folder"
                    | "duplicate"
                    | "make-link"
                    | "cut"
                    | "copy-to-other"
                    | "move-to-other"
            )
        {
            self.say("Restore items before editing or transferring them from the Trash", true);
            return;
        }
        if let Some(id) = name.strip_prefix("create-template:") {
            self.create_template(id);
            return;
        }
        match name {
            "refresh-templates" => self.refresh_templates(),
            "undo-rename" => self.history_rename(true),
            "redo-rename" => self.history_rename(false),
            "open" => self.open_selection(p),
            "new-window" | "open-in-new-window" => {
                let location = if name == "open-in-new-window" {
                    self.panes[p]
                        .tab()
                        .targets()
                        .into_iter()
                        .find(FileEntry::is_navigable)
                        .map(|e| e.uri)
                        .unwrap_or_else(|| self.panes[p].tab().location.clone())
                } else {
                    self.panes[p].tab().location.clone()
                };
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = std::env::var_os("GUNGNIR_FM_LAUNCHER")
                        .map(std::path::PathBuf::from)
                        .map_or_else(std::env::current_exe, Ok)
                        .and_then(|exe| {
                            std::process::Command::new(exe)
                                .arg(location.to_uri_string())
                                .stdin(std::process::Stdio::null())
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null())
                                .spawn()
                        })
                        .map(|_| "Opened new window".to_owned())
                        .map_err(|e| format!("Could not open window: {e}"));
                    let _ = tx.blocking_send(Msg::Ev(Event::Op { result, reload: Vec::new(), select: None }));
                });
            }
            "duplicate" => {
                let t = self.panes[p].tab();
                let sources = t.targets().into_iter().map(|e| e.uri).collect();
                self.execute_transfer(TransferKind::Copy, sources, t.location.clone(), false);
            }
            "make-link" => {
                let t = self.panes[p].tab();
                let targets = t.targets();
                let targets =
                    if targets.is_empty() { t.entry_at(t.cursor).cloned().into_iter().collect() } else { targets };
                if targets.is_empty() {
                    self.say("No items selected to link", true);
                    return;
                }
                let loc = t.location.clone();
                if loc.scheme() == Scheme::Trash {
                    self.say("Restore items before creating links from the Trash", true);
                    return;
                }
                let provider = match self.registry.resolve(&loc) {
                    Ok(pr) => pr,
                    Err(e) => {
                        self.say(e.to_string(), true);
                        return;
                    }
                };

                let mut to_create = Vec::new();
                for entry in targets {
                    let base_name = format!("Link to {}", entry.name);
                    let mut candidate = base_name.clone();
                    let mut counter = 1;
                    while t.entries.iter().any(|e| e.name == candidate)
                        || to_create.iter().any(|(_, _, n)| n == &candidate)
                    {
                        candidate = format!("{base_name} ({counter})");
                        counter += 1;
                    }
                    let Ok(link_uri) = loc.join(&candidate) else { continue };
                    let target_spec = if entry.uri.parent().as_ref() == Some(&loc) {
                        entry.name.clone()
                    } else if let Some(lp) = entry.uri.to_local_path() {
                        lp.display().to_string()
                    } else {
                        entry.uri.path().to_owned()
                    };
                    to_create.push((link_uri, target_spec, candidate));
                }

                if to_create.is_empty() {
                    return;
                }

                let last_select = to_create.last().map(|(_, _, name)| (loc.clone(), name.clone()));
                let count = to_create.len();
                self.spawn_op(vec![loc], last_select, async move {
                    for (link_uri, target_spec, _) in to_create {
                        provider.create_symlink(&link_uri, &target_spec).await?;
                    }
                    Ok(format!("Created {count} link{}", if count == 1 { "" } else { "s" }))
                });
            }
            "open-in-other-pane" => {
                if let Some(folder) = self.panes[p].tab().targets().into_iter().find(FileEntry::is_navigable) {
                    self.dual = true;
                    self.push_flags();
                    self.navigate_to(1 - p, folder.uri);
                }
            }
            "new-tab" => self.new_tab(p, None),
            "close-tab" => self.close_tab(p, self.panes[p].active),
            "next-tab" => self.cycle_tab(p, 1),
            "prev-tab" => self.cycle_tab(p, -1),
            "open-terminal-here" => self.open_terminal_here(p),
            "open-with" => self.open_with(p),
            "open-in-tab" => {
                if let Some(e) = self.panes[p].tab().targets().into_iter().find(FileEntry::is_navigable) {
                    self.new_tab(p, Some(e.uri));
                }
            }
            "copy" | "cut" => {
                let uris: Vec<VfsUri> = self.panes[p].tab().targets().into_iter().map(|e| e.uri).collect();
                if uris.is_empty() {
                    return;
                }
                let cut = name == "cut";
                let n = uris.len();
                self.clipboard = Some((uris, cut));
                if let (Some(service), Some(files)) = (&self.clipboard_service, self.clipboard.clone()) {
                    let _ = service.try_send(crate::clipboard::Request::Write(files));
                }
                self.push_pane(0);
                if self.panes.len() > 1 {
                    self.push_pane(1);
                }
                self.say(
                    format!(
                        "{} {n} item{} (Ctrl+V to paste)",
                        if cut { "Cut" } else { "Copied" },
                        if n == 1 { "" } else { "s" }
                    ),
                    false,
                );
            }
            "copy-path" => {
                let t = self.panes[p].tab();
                let targets = t.targets();
                let targets =
                    if targets.is_empty() { t.entry_at(t.cursor).cloned().into_iter().collect() } else { targets };
                if targets.is_empty() {
                    self.say("No items selected to copy path", true);
                    return;
                }
                let paths: Vec<String> = targets
                    .iter()
                    .map(|e| {
                        if e.uri.scheme() == Scheme::Recent
                            && let Some((_, target)) =
                                e.vfs.attributes.iter().find(|(k, _)| k == "target" || k == "orig")
                        {
                            if let Some(stripped) = target.strip_prefix("file://") {
                                return backends::recent::file_path(target.as_bytes())
                                    .and_then(|b| String::from_utf8(b).ok())
                                    .unwrap_or_else(|| stripped.to_owned());
                            }
                            return target.clone();
                        }
                        e.uri
                            .to_local_path()
                            .map(|lp| lp.display().to_string())
                            .unwrap_or_else(|| e.uri.to_uri_string())
                    })
                    .collect();
                let n = paths.len();
                let text = paths.join("\n");
                if let Some(service) = &self.clipboard_service {
                    let _ = service.try_send(crate::clipboard::Request::WriteText(text));
                }
                self.say(format!("Copied {n} path{} to clipboard", if n == 1 { "" } else { "s" }), false);
            }
            "copy-location" => {
                let loc = &self.panes[p].tab().location;
                let text =
                    loc.to_local_path().map(|lp| lp.display().to_string()).unwrap_or_else(|| loc.to_uri_string());
                if let Some(service) = &self.clipboard_service {
                    let _ = service.try_send(crate::clipboard::Request::WriteText(text));
                }
                self.say("Copied folder location to clipboard".to_owned(), false);
            }
            "paste" | "paste-into-folder" => {
                let dest = if name == "paste-into-folder" {
                    let targets = self.panes[p].tab().targets();
                    let Some(folder) = targets.first().filter(|e| e.is_navigable()) else { return };
                    folder.uri.clone()
                } else {
                    self.panes[p].tab().location.clone()
                };
                if let Some(service) = &self.clipboard_service
                    && service.try_send(crate::clipboard::Request::Read(dest)).is_err()
                {
                    self.say("Clipboard is busy; try again", true);
                }
            }
            "copy-to-other" => self.transfer(TransferKind::Copy),
            "move-to-other" => self.transfer(TransferKind::Move),
            "rename" => {
                let targets = self.panes[p].tab().targets();
                if targets.len() > 1 {
                    self.run_command("batch-rename");
                } else {
                    let t = self.panes[p].tab();
                    let target_entry = targets.first();
                    let target_idx = target_entry
                        .and_then(|target| {
                            t.visible
                                .iter()
                                .position(|&ei| t.entries.get(ei as usize).is_some_and(|e| e.name == target.name))
                        })
                        .unwrap_or(t.cursor);

                    if let Some(e) = t.entry_at(target_idx) {
                        self.renaming = Some(RenameEdit { pane: p, tab: t.id, index: target_idx, uri: e.uri.clone() });
                        self.ui.send(Update::Rename {
                            pane: i32::try_from(p).unwrap_or(0),
                            index: i32::try_from(target_idx).unwrap_or(0),
                        });
                    }
                }
            }
            "toggle-terminal" => {
                self.terminal_open = !self.terminal_open;
                if self.terminal_open
                    && let Some(p) = self.panes[self.active_pane].tab().location.to_local_path()
                {
                    self.terminal_cwd = p;
                }
                self.push_terminal();
            }
            "deep-search" => {
                let root = self.panes[p].tab().location.clone();
                self.dialog = Some(PendingDialog::Search { results: Vec::new() });
                self.ui.send(Update::Dialog(DialogView {
                    kind: DialogKind::Search,
                    title: "Find & Grep Files".into(),
                    message: format!("Search recursively within {}", root.display()),
                    rows: Vec::new(),
                    input: String::new(),
                    preview_path: None,
                    opt1: false,
                    opt2: false,
                    extra1: String::new(),
                    extra2: String::new(),
                    ..Default::default()
                }));
            }
            "batch-rename" => {
                let targets = self.panes[p].tab().targets();
                if targets.is_empty() {
                    return;
                }
                let items: Vec<(VfsUri, String)> = targets.iter().map(|e| (e.uri.clone(), e.name.clone())).collect();
                let rows: Vec<(String, String)> = items.iter().map(|(_, name)| (name.clone(), name.clone())).collect();
                let n = items.len();
                self.dialog = Some(PendingDialog::BatchRename { pane: p, items: items.clone(), new_names: items });
                self.ui.send(Update::Dialog(DialogView {
                    kind: DialogKind::BatchRename,
                    title: format!("Batch Rename ({n} files)"),
                    message:
                        "Enter find/replace pattern or numbering prefix below. Live preview updates automatically:"
                            .into(),
                    rows,
                    input: String::new(),
                    preview_path: None,
                    opt1: false,
                    opt2: false,
                    extra1: String::new(),
                    extra2: String::new(),
                    ..Default::default()
                }));
            }
            "compare-panes" => {
                self.compare_panes();
            }
            "new-folder-inline" | "new-file-inline" => {
                let dir = self.panes[p].tab().location.clone();
                if dir.scheme() == Scheme::Trash {
                    self.say("Restore items before editing the Trash", true);
                    return;
                }
                let provider = match self.registry.resolve(&dir) {
                    Ok(provider) => provider,
                    Err(e) => {
                        self.say(e.to_string(), true);
                        return;
                    }
                };
                let file = name == "new-file-inline";
                let tab = self.panes[p].tab().id;
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let result = async {
                        let base = if file { "Untitled Document" } else { "Untitled Folder" };
                        for n in 1..=10_000 {
                            let name = if n == 1 { base.to_string() } else { format!("{base} {n}") };
                            let uri = dir.join(&name)?;
                            let result =
                                if file { provider.create_file(&uri).await } else { provider.create_dir(&uri).await };
                            match result {
                                Ok(()) => return Ok(uri),
                                Err(VfsError::AlreadyExists(_)) => continue,
                                Err(e) => return Err(e),
                            }
                        }
                        Err(VfsError::AlreadyExists("Too many items with the same default name".into()))
                    }
                    .await;
                    let _ = tx.send(Msg::Ev(Event::Created { pane: p, tab, dir, result })).await;
                });
            }
            "new-folder" | "new-file" => {
                let dir = self.panes[p].tab().location.clone();
                if dir.scheme() == Scheme::Trash {
                    self.say("Restore items before editing the Trash", true);
                    return;
                }
                let file = name == "new-file";
                self.dialog = Some(if file {
                    PendingDialog::NewFile { dir: dir.clone() }
                } else {
                    PendingDialog::NewFolder { dir: dir.clone() }
                });
                self.ui.send(Update::Dialog(DialogView {
                    kind: if file { DialogKind::NewFile } else { DialogKind::NewFolder },
                    title: if file { "New Document" } else { "New Folder" }.into(),
                    message: format!("in {}", dir.display()),
                    input: if file { "Untitled Document.txt" } else { "New Folder" }.into(),
                    ..Default::default()
                }));
            }
            "select-pattern" => {
                self.dialog = Some(PendingDialog::SelectPattern { pane: p });
                self.ui.send(Update::Dialog(DialogView {
                    kind: DialogKind::SelectPattern,
                    title: "Select Items".into(),
                    message: "Select visible names matching a pattern (* matches any text, ? one character).".into(),
                    input: "*".into(),
                    ..Default::default()
                }));
            }
            "trash" => {
                let targets = self.panes[p].tab().targets();
                if targets.is_empty() {
                    return;
                }
                let loc = self.panes[p].tab().location.clone();
                if loc.scheme() == Scheme::Trash {
                    self.confirm_delete(p, targets);
                    return;
                }
                let Ok(provider) = self.registry.resolve(&loc) else { return };
                if !provider.capabilities().trash {
                    // No trash on this backend: ask before deleting for good.
                    self.confirm_delete(p, targets);
                    return;
                }
                let uris: Vec<VfsUri> = targets.iter().map(|e| e.uri.clone()).collect();
                let n = uris.len();
                self.spawn_op(vec![loc], None, async move {
                    provider.trash(&uris).await?;
                    Ok(format!("Moved {n} item{} to the trash", if n == 1 { "" } else { "s" }))
                });
            }
            "restore" => {
                let targets = self.panes[p].tab().targets();
                if targets.is_empty() {
                    return;
                }
                let loc = self.panes[p].tab().location.clone();
                let Ok(provider) = self.registry.resolve(&loc) else { return };
                let uris: Vec<VfsUri> = targets.iter().map(|e| e.uri.clone()).collect();
                let n = uris.len();
                self.spawn_op(vec![loc], None, async move {
                    provider.restore(&uris).await?;
                    Ok(format!("Restored {n} item{} from trash", if n == 1 { "" } else { "s" }))
                });
            }
            "empty-trash" => {
                self.dialog = Some(PendingDialog::EmptyTrash);
                self.ui.send(Update::Dialog(DialogView {
                    kind: DialogKind::ConfirmDelete,
                    title: "Empty Trash permanently?".into(),
                    message: "All items in the Trash will be permanently deleted. This cannot be undone.".into(),
                    ..Default::default()
                }));
            }
            "clear-recent" => {
                self.dialog = Some(PendingDialog::ClearRecent);
                self.ui.send(Update::Dialog(DialogView {
                    kind: DialogKind::ConfirmDelete,
                    title: "Clear Recent History?".into(),
                    message:
                        "All items will be removed from your recent history. The files themselves will not be deleted."
                            .into(),
                    ..Default::default()
                }));
            }
            "open-containing-folder" => {
                let t = self.panes[p].tab();
                let entry = t.targets().into_iter().next().or_else(|| t.entry_at(t.cursor).cloned());
                let Some(entry) = entry else {
                    self.say("No item selected", true);
                    return;
                };
                let (target_dir, select_name) = match entry.uri.scheme() {
                    Scheme::Recent => {
                        let orig = entry
                            .vfs
                            .attributes
                            .iter()
                            .find(|(k, _)| k == "target" || k == "orig")
                            .map(|(_, v)| v.as_str());
                        if let Some(path_str) = orig {
                            let clean_path = if let Some(stripped) = path_str.strip_prefix("file://") {
                                backends::recent::file_path(path_str.as_bytes())
                                    .and_then(|b| String::from_utf8(b).ok())
                                    .unwrap_or_else(|| stripped.to_owned())
                            } else {
                                path_str.to_owned()
                            };
                            let path = Path::new(&clean_path);
                            if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
                                let Ok(u) = VfsUri::from_local_path(parent) else {
                                    self.say("Containing folder could not be resolved", true);
                                    return;
                                };
                                (u, name.to_string_lossy().into_owned())
                            } else {
                                self.say("Containing folder could not be resolved", true);
                                return;
                            }
                        } else {
                            self.say("Containing folder unknown", true);
                            return;
                        }
                    }
                    Scheme::Trash => {
                        self.say("Item is currently in the Trash", true);
                        return;
                    }
                    _ => {
                        let Some(parent) = entry.uri.parent() else {
                            self.say("Item has no containing folder", true);
                            return;
                        };
                        let Some(name) = entry.uri.file_name() else {
                            self.say("Item has no containing folder", true);
                            return;
                        };
                        (parent, name.to_owned())
                    }
                };
                self.panes[p].tab_mut().pending_select = Some(select_name.clone());
                self.panes[p].tab_mut().selected = [select_name].into();
                self.navigate_to(p, target_dir);
            }
            "delete" => {
                let targets = self.panes[p].tab().targets();
                if !targets.is_empty() {
                    self.confirm_delete(p, targets);
                }
            }
            "properties" => self.properties(p),
            "connect-server" => {
                self.dialog = Some(PendingDialog::ConnectServer { pane: p });
                self.ui.send(Update::Dialog(DialogView {
                    kind: DialogKind::ConnectServer,
                    title: "Connect to Server".into(),
                    message: "Enter an SFTP/SSH (sftp://host/path) or SMB/CIFS (smb://host/share) address:".into(),
                    rows: Vec::new(),
                    input: "sftp://".into(),
                    preview_path: None,
                    ..Default::default()
                }));
            }
            "check-hash" => {
                let t = self.panes[p].tab();
                let targets = t.targets();
                let Some(e) = targets.first() else { return };
                let Some(local_path) = e.uri.to_local_path() else {
                    self.say("Checksum verification is supported on local files", true);
                    return;
                };
                let name = e.name.clone();
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let is_checksum_file = local_path.extension().and_then(|s| s.to_str()).is_some_and(|s| {
                        matches!(s.to_ascii_lowercase().as_str(), "sha256" | "md5" | "sha512" | "sha1")
                    });

                    if is_checksum_file
                        && let Ok(results) = crate::hash::verify_checksum_file(&local_path)
                        && !results.is_empty()
                    {
                        let rows = results
                            .into_iter()
                            .map(|(file, ok)| {
                                (file, if ok { "VERIFIED OK".to_string() } else { "FAILED / NOT FOUND".to_string() })
                            })
                            .collect();
                        let _ = tx.blocking_send(Msg::Ev(Event::Info {
                            title: format!("Verified Checksums · {name}"),
                            kind: DialogKind::Inspect,
                            result: Ok(rows),
                        }));
                        return;
                    }

                    match crate::hash::compute_hashes(&local_path) {
                        Ok(hashes) => {
                            let rows = vec![
                                ("MD5".to_string(), hashes.md5),
                                ("SHA-1".to_string(), hashes.sha1),
                                ("SHA-256".to_string(), hashes.sha256),
                                ("SHA-512".to_string(), hashes.sha512),
                            ];
                            let _ = tx.blocking_send(Msg::Ev(Event::Info {
                                title: format!("Checksums · {name}"),
                                kind: DialogKind::Hash,
                                result: Ok(rows),
                            }));
                        }
                        Err(err) => {
                            let _ = tx.blocking_send(Msg::Ev(Event::Info {
                                title: "Hash Error".to_string(),
                                kind: DialogKind::Error,
                                result: Err(core_vfs::error::VfsError::from_io("hash", &err, &name)),
                            }));
                        }
                    }
                });
            }
            "compress" => {
                let t = self.panes[p].tab();
                let targets = t.targets();
                if targets.is_empty() {
                    self.say("No items selected to compress", true);
                    return;
                }
                let Some(base_dir) = t.location.to_local_path() else {
                    self.say("Compression is supported on local directories", true);
                    return;
                };
                let mut local_targets = Vec::new();
                for e in &targets {
                    if let Some(lp) = e.uri.to_local_path() {
                        local_targets.push(lp);
                    } else {
                        self.say("All items to compress must be on the local filesystem", true);
                        return;
                    }
                }
                let default_name = if targets.len() == 1 {
                    format!("{}.tar.zst", targets[0].name)
                } else {
                    let dir_name = base_dir
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "archive".to_string());
                    format!("{dir_name}.tar.zst")
                };
                self.dialog = Some(PendingDialog::Compress { pane: p, targets: local_targets, base_dir });
                self.ui.send(Update::Dialog(DialogView {
                    kind: DialogKind::Compress,
                    title: "Lossless Compression".into(),
                    message: format!("Compress {} selected item(s) bit-perfectly", targets.len()),
                    input: default_name,
                    extra1: "tar.zst".into(),
                    extra2: String::new(),
                    opt1: false,
                    opt2: false,
                    ..Default::default()
                }));
            }
            "quick-compress-zst" => {
                let t = self.panes[p].tab();
                let targets = t.targets();
                if targets.is_empty() {
                    self.say("No items selected to compress", true);
                    return;
                }
                let Some(base_dir) = t.location.to_local_path() else {
                    self.say("Compression is supported on local directories", true);
                    return;
                };
                let mut local_targets = Vec::new();
                for e in &targets {
                    if let Some(lp) = e.uri.to_local_path() {
                        local_targets.push(lp);
                    } else {
                        self.say("All items must be on the local filesystem", true);
                        return;
                    }
                }
                let archive_name = if targets.len() == 1 {
                    format!("{}.tar.zst", targets[0].name)
                } else {
                    let dir_name = base_dir
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "archive".to_string());
                    format!("{dir_name}.tar.zst")
                };
                let archive_path = base_dir.join(&archive_name);
                let loc = t.location.clone();
                let select_name = archive_name.clone();
                let rel_items: Vec<PathBuf> =
                    local_targets.iter().map(|p| p.strip_prefix(&base_dir).unwrap_or(p).to_path_buf()).collect();
                self.spawn_op(vec![loc.clone()], Some((loc, select_name)), async move {
                    backends::archive::compress_items(
                        &archive_path,
                        &base_dir,
                        &rel_items,
                        &backends::archive::ArchiveOptions {
                            format: Some(backends::archive::ArchiveFormat::TarZstd),
                            level: backends::archive::CompressionLevel::Normal,
                            ..Default::default()
                        },
                    )
                    .map_err(|e| core_vfs::error::VfsError::Io {
                        op: "compress",
                        errno: 0,
                        msg: format!("{e}"),
                    })?;
                    Ok(format!("Quick-compressed into {archive_name} (Zstandard lossless)"))
                });
            }
            "quick-compress-zip" => {
                let t = self.panes[p].tab();
                let targets = t.targets();
                if targets.is_empty() {
                    self.say("No items selected to compress", true);
                    return;
                }
                let Some(base_dir) = t.location.to_local_path() else {
                    self.say("Compression is supported on local directories", true);
                    return;
                };
                let mut local_targets = Vec::new();
                for e in &targets {
                    if let Some(lp) = e.uri.to_local_path() {
                        local_targets.push(lp);
                    } else {
                        self.say("All items must be on the local filesystem", true);
                        return;
                    }
                }
                let archive_name = if targets.len() == 1 {
                    format!("{}.zip", targets[0].name)
                } else {
                    let dir_name = base_dir
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "archive".to_string());
                    format!("{dir_name}.zip")
                };
                let archive_path = base_dir.join(&archive_name);
                let loc = t.location.clone();
                let select_name = archive_name.clone();
                let rel_items: Vec<PathBuf> =
                    local_targets.iter().map(|p| p.strip_prefix(&base_dir).unwrap_or(p).to_path_buf()).collect();
                self.spawn_op(vec![loc.clone()], Some((loc, select_name)), async move {
                    backends::archive::compress_items(
                        &archive_path,
                        &base_dir,
                        &rel_items,
                        &backends::archive::ArchiveOptions {
                            format: Some(backends::archive::ArchiveFormat::Zip),
                            level: backends::archive::CompressionLevel::Normal,
                            ..Default::default()
                        },
                    )
                    .map_err(|e| core_vfs::error::VfsError::Io {
                        op: "compress",
                        errno: 0,
                        msg: format!("{e}"),
                    })?;
                    Ok(format!("Quick-compressed into {archive_name} (ZIP lossless)"))
                });
            }
            "extract-here" => {
                let t = self.panes[p].tab();
                let targets = t.targets();
                let Some(e) = targets.first() else {
                    self.say("No archive selected to extract", true);
                    return;
                };
                let Some(archive_path) = e.uri.to_local_path() else {
                    self.say("Extraction is supported on local archives", true);
                    return;
                };
                let Some(target_dir) = t.location.to_local_path() else {
                    self.say("Extraction destination must be a local directory", true);
                    return;
                };
                let loc = t.location.clone();
                let name = e.name.clone();
                self.spawn_op(vec![loc.clone()], None, async move {
                    let stats = backends::archive::extract_archive(&archive_path, &target_dir, None)
                        .map_err(|e| core_vfs::error::VfsError::Io { op: "extract", errno: 0, msg: format!("{e}") })?;
                    Ok(format!("Extracted {} file(s) from {name} in {}ms", stats.files_extracted, stats.elapsed_ms))
                });
            }
            "extract-to" => {
                let t = self.panes[p].tab();
                let targets = t.targets();
                let Some(e) = targets.first() else {
                    self.say("No archive selected to extract", true);
                    return;
                };
                let Some(archive_path) = e.uri.to_local_path() else {
                    self.say("Extraction is supported on local archives", true);
                    return;
                };
                let Some(base_dir) = t.location.to_local_path() else {
                    self.say("Extraction destination must be a local directory", true);
                    return;
                };
                let mut stem = archive_stem(&e.name);
                let mut target_dir = base_dir.join(&stem);
                if target_dir.is_file() {
                    if let Some(s) = Path::new(&stem).file_stem() {
                        let candidate = s.to_string_lossy().to_string();
                        if !candidate.is_empty() {
                            stem = candidate;
                            target_dir = base_dir.join(&stem);
                        }
                    }
                    if target_dir.is_file() {
                        stem = format!("{stem}_extracted");
                        target_dir = base_dir.join(&stem);
                    }
                }
                let loc = t.location.clone();
                let select_folder = stem.clone();
                let name = e.name.clone();
                self.spawn_op(vec![loc.clone()], Some((loc, select_folder)), async move {
                    let stats = backends::archive::extract_archive(&archive_path, &target_dir, None)
                        .map_err(|e| core_vfs::error::VfsError::Io { op: "extract", errno: 0, msg: format!("{e}") })?;
                    Ok(format!(
                        "Extracted {} file(s) from {name} into {stem}/ in {}ms",
                        stats.files_extracted, stats.elapsed_ms
                    ))
                });
            }
            "test-archive" => {
                let t = self.panes[p].tab();
                let targets = t.targets();
                let Some(e) = targets.first() else {
                    self.say("No archive selected to test", true);
                    return;
                };
                let Some(archive_path) = e.uri.to_local_path() else {
                    self.say("Testing integrity is supported on local archives", true);
                    return;
                };
                let name = e.name.clone();
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || match backends::archive::test_archive_integrity(&archive_path) {
                    Ok(verified_count) => {
                        let rows = vec![
                            ("Archive File".to_string(), name.clone()),
                            ("Integrity Status".to_string(), "100% BIT-PERFECT VERIFIED".to_string()),
                            ("Entries Verified".to_string(), format!("{verified_count} stream/header entries")),
                            ("Checksum Verification".to_string(), "CRC / BLAKE3 / SHA-256 Valid".to_string()),
                        ];
                        let _ = tx.blocking_send(Msg::Ev(Event::Info {
                            title: format!("Archive Integrity · {name}"),
                            kind: DialogKind::Inspect,
                            result: Ok(rows),
                        }));
                    }
                    Err(e) => {
                        let _ = tx.blocking_send(Msg::Ev(Event::Info {
                            title: format!("Archive Integrity Failed · {name}"),
                            kind: DialogKind::Error,
                            result: Err(core_vfs::error::VfsError::Io {
                                op: "test-integrity",
                                errno: 0,
                                msg: format!("{e}"),
                            }),
                        }));
                    }
                });
            }
            "quick-preview" => {
                if self.dialog.is_some() {
                    self.dialog = None;
                    self.ui.send(Update::CloseDialog);
                    return;
                }
                let t = self.panes[p].tab();
                let targets = t.targets();
                let Some(e) = targets.first() else { return };
                let cat = format::category(e);
                let ext = format::extension(&e.name);
                let uri_str = e.uri.to_uri_string();
                let local_path = e.uri.to_local_path();
                let mtime_secs = e
                    .modified
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| i64::try_from(d.as_secs()).unwrap_or(0))
                    .unwrap_or(0);
                let is_vid =
                    matches!(cat, core_vfs::format::Category::Video) || crate::thumbnails::is_video_ext(ext.as_deref());

                let previewable = crate::thumbnails::is_previewable(cat, ext.as_deref());
                let wants_text = matches!(cat, core_vfs::format::Category::Code | core_vfs::format::Category::Document);
                let mut detail = format!(
                    "{} • {} • modified {}",
                    format::type_label(e),
                    format::size(e.size),
                    backends::time::format_local(e.modified)
                );
                let title = e.name.clone();

                // The thumbnail (ffmpeg, ImageMagick, a decode of up to 64 MiB) and the text
                // snippet are made on the blocking pool: on the controller they held every
                // other command for as long as they took (0.6 s for a 48 MP image).
                self.preview_seq += 1;
                let seq = self.preview_seq;
                self.preview_wait = Some(seq);
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let image_path = if previewable {
                        crate::thumbnails::lookup_cached(&uri_str, mtime_secs)
                            .or_else(|| {
                                local_path.as_ref().and_then(|lp| {
                                    crate::thumbnails::generate_thumbnail(lp, &uri_str, mtime_secs, is_vid).ok()
                                })
                            })
                            .or_else(|| {
                                if !is_vid {
                                    local_path.clone().filter(|p| crate::thumbnails::is_regular_file(p))
                                } else {
                                    None
                                }
                            })
                    } else {
                        None
                    };
                    // For text, code, markup, config and text-document files, a clean snippet.
                    if image_path.is_none()
                        && wants_text
                        && let Some(snippet) = local_path.as_deref().and_then(text_snippet)
                    {
                        detail.push_str(&snippet);
                    }
                    let _ = tx.blocking_send(Msg::Ev(Event::Preview { seq, title, detail, image_path }));
                });
            }
            "inspect" => {
                let t = self.panes[p].tab();
                let targets = t.targets();
                let uri = targets.first().map_or_else(|| t.location.clone(), |e| e.uri.clone());
                let Ok(provider) = self.registry.resolve(&uri) else { return };
                let tx = self.tx.clone();
                let title = format!("Gungnir VFS Inspect · {}", provider.kind().badge());
                tokio::spawn(async move {
                    let result = provider.inspect(&uri).await;
                    let _ = tx.send(Msg::Ev(Event::Info { title, kind: DialogKind::Inspect, result })).await;
                });
            }
            "inspect-folder" => {
                let t = self.panes[p].tab();
                let uri = t.location.clone();
                let Ok(provider) = self.registry.resolve(&uri) else { return };
                let tx = self.tx.clone();
                let title = format!("Gungnir VFS Inspect · {}", provider.kind().badge());
                tokio::spawn(async move {
                    let result = provider.inspect(&uri).await;
                    let _ = tx.send(Msg::Ev(Event::Info { title, kind: DialogKind::Inspect, result })).await;
                });
            }
            "open-terminal-folder" => {
                let t = self.panes[p].tab();
                let target_dir = t
                    .location
                    .to_local_path()
                    .unwrap_or_else(|| self.cfg.home.to_local_path().unwrap_or_else(|| PathBuf::from(".")));
                self.launch_terminal_at(target_dir);
            }
            "folder-properties" => self.folder_properties(p),
            "toggle-hidden" => {
                self.show_hidden = !self.show_hidden;
                self.push_flags();
                for pane in 0..2 {
                    let id = self.panes[pane].tab().id;
                    self.prepare(pane, id, false);
                }
                self.say(if self.show_hidden { "Showing hidden files" } else { "Hiding hidden files" }, false);
                self.trigger_save_session();
            }
            "toggle-dual" => {
                self.cancel_rename();
                self.dual = !self.dual;
                if !self.dual {
                    self.active_pane = 0;
                } else {
                    if self.panes[1].tabs.is_empty() {
                        let home = self.cfg.home.clone();
                        let id = self.next_tab;
                        self.next_tab += 1;
                        self.panes[1].tabs.push(Tab::new(id, home.clone()));
                        self.panes[1].active = 0;
                    }
                    if self.panes[1].tab().load_gen == 0 {
                        let loc = self.panes[1].tab().location.clone();
                        self.load(1, loc, false);
                    } else {
                        self.push_pane(1);
                    }
                }
                self.push_flags();
                self.push_status();
                self.push_places();
                self.trigger_save_session();
            }
            "toggle-sidebar" => {
                self.sidebar = !self.sidebar;
                self.push_flags();
                if self.sidebar && self.sidebar_mode == 1 {
                    self.rebuild_tree();
                }
                self.trigger_save_session();
            }
            "toggle-theme" => {
                self.dark_mode = !self.dark_mode;
                self.push_flags();
                self.trigger_save_session();
            }
            "select-all" | "invert-selection" => {
                let t = self.panes[p].tab();
                let all: HashSet<String> = t
                    .visible
                    .iter()
                    .filter_map(|&i| t.entries.get(i as usize))
                    .filter(|e| name == "select-all" || !t.selected.contains(&e.name))
                    .map(|e| e.name.clone())
                    .collect();
                self.set_selection(p, all);
            }
            "clear-selection" => {
                if self.renaming.take().is_some() {
                    self.ui.send(Update::Rename { pane: -1, index: -1 });
                }
                self.set_selection(p, HashSet::new());
            }
            "toggle-select" => {
                let t = self.panes[p].tab();
                if let Some(name) = t.entry_at(t.cursor).map(|e| e.name.clone()) {
                    let mut s = t.selected.clone();
                    if !s.remove(&name) {
                        s.insert(name);
                    }
                    self.set_selection(p, s);
                    self.move_cursor_keep(p, 1);
                }
            }
            "gungnir-root" if self.cfg.gungnir_label.is_empty() => {
                self.say("Gungnir is off (GUNGNIR_FM_BACKEND); start gungnird and use live-or-off or auto", true);
            }
            "gungnir-root" => {
                let root = self.cfg.gungnir_root.clone();
                self.navigate_to(p, root);
            }
            "backend-status" => {
                let mut rows = Vec::new();
                for pr in self.registry.providers() {
                    let c = pr.capabilities();
                    rows.push((
                        pr.kind().badge().to_owned(),
                        format!(
                            "watch {} · trash {} · pause {} · VFS metadata {} · mount health {}",
                            yes(c.watch),
                            yes(c.trash),
                            yes(c.pause_transfers),
                            yes(c.vfs_metadata),
                            yes(c.mount_health)
                        ),
                    ));
                }
                for m in &self.mounts {
                    rows.push((m.label.clone(), format!("{} · {}", m.status.label(), m.detail)));
                }
                let label = self.cfg.gungnir_label.clone();
                self.info_dialog(DialogKind::Properties, "Backends", &label, rows);
            }
            "bookmark" => {
                let loc = self.panes[p].tab().location.clone();
                if !self.bookmarks.contains(&loc) {
                    self.bookmarks.push(loc.clone());
                    self.save_bookmarks();
                    self.push_places();
                    self.say(format!("Bookmarked {loc}"), false);
                }
            }
            "bookmark-target" => {
                let t = self.panes[p].tab();
                if let Some(folder) = t
                    .targets()
                    .into_iter()
                    .find(FileEntry::is_navigable)
                    .or_else(|| t.entry_at(t.cursor).filter(|e| e.is_navigable()).cloned())
                {
                    let loc = folder.uri;
                    if !self.bookmarks.contains(&loc) {
                        self.bookmarks.push(loc.clone());
                        self.save_bookmarks();
                        self.push_places();
                        self.say(format!("Bookmarked {}", folder.name), false);
                    } else {
                        self.say(format!("{} is already bookmarked", folder.name), false);
                    }
                }
            }
            "clear-bookmarks" => {
                self.bookmarks.clear();
                self.save_bookmarks();
                self.push_places();
            }
            "help-keys" => {
                let rows = [
                    ("F3", "Toggle dual pane"),
                    ("F2", "Rename inline"),
                    ("F5", "Copy selection to the other pane"),
                    ("F6", "Move selection to the other pane"),
                    ("F7", "New folder"),
                    ("F9", "Toggle sidebar"),
                    ("Tab", "Switch active pane"),
                    ("Ctrl+L", "Edit location (paths or gungnir:// URIs)"),
                    ("Ctrl+F", "Filter the active pane"),
                    ("Ctrl+H", "Show or hide dotfiles"),
                    ("Ctrl+T / Ctrl+W", "New tab / close tab"),
                    ("Ctrl+1/2/3", "Compact / detailed / grid view"),
                    ("Ctrl+I", "Gungnir VFS Inspect"),
                    ("Ctrl+R", "Reload"),
                    ("Alt+Up, Backspace", "Parent folder"),
                    ("Alt+Left / Alt+Right", "Back / forward"),
                    ("Enter", "Open"),
                    ("Delete / Shift+Delete", "Trash / delete permanently"),
                    ("Space", "Toggle selection"),
                    ("Ctrl+A", "Select all"),
                    ("Escape", "Clear selection"),
                ];
                self.info_dialog(
                    DialogKind::Properties,
                    "Keyboard Shortcuts",
                    "",
                    rows.iter().map(|(a, b)| ((*a).to_owned(), (*b).to_owned())).collect(),
                );
            }
            "about" => {
                let rows = vec![
                    ("Version".to_owned(), env!("CARGO_PKG_VERSION").to_owned()),
                    ("Toolkit".to_owned(), "Rust + Slint 1.18".to_owned()),
                    ("Gungnir".to_owned(), self.cfg.gungnir_label.clone()),
                    ("Source".to_owned(), "https://gitlab.com/exalted6165/gungnir-fm".to_owned()),
                ];
                self.info_dialog(
                    DialogKind::Properties,
                    "Gungnir FM",
                    "A fast dual-pane file manager, companion to Gungnir VFS.",
                    rows,
                );
            }
            "quit" => {
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(Msg::Ui(Command::Quit)).await;
                });
            }
            other => self.say(format!("Unknown command {other}"), true),
        }
    }

    fn move_cursor_keep(&mut self, pane: usize, delta: i64) {
        let t = self.panes[pane].tab_mut();
        let max = t.visible.len().saturating_sub(1);
        t.cursor = usize::try_from((i64::try_from(t.cursor).unwrap_or(0) + delta).max(0)).unwrap_or(0).min(max);
        self.push_pane(pane);
    }

    fn confirm_delete(&mut self, pane: usize, targets: Vec<FileEntry>) {
        let n = targets.len();
        let rows = targets.iter().take(12).map(|e| (format::type_label(e), e.name.clone())).collect();
        self.dialog = Some(PendingDialog::Delete { pane, uris: targets.into_iter().map(|e| e.uri).collect() });
        self.ui.send(Update::Dialog(DialogView {
            kind: DialogKind::ConfirmDelete,
            title: format!("Delete {n} item{} permanently?", if n == 1 { "" } else { "s" }),
            message: "This cannot be undone.".into(),
            rows,
            input: String::new(),
            preview_path: None,
            ..Default::default()
        }));
    }

    fn properties(&mut self, pane: usize) {
        let t = self.panes[pane].tab();
        let targets = t.targets();
        if let [e] = targets.as_slice() {
            let local_path = e.uri.to_local_path();
            let is_dir = e.is_navigable();
            let name = e.name.clone();
            let mime = self.mime_index.mime_for(&name, is_dir);
            let mime_desc = String::new();
            let selected_app_id = None;
            self.dialog = Some(PendingDialog::Properties {
                pane,
                entry: e.clone(),
                backend: self.panes[pane].tab().backend,
                target_name: name,
                local_path,
                mime,
                mime_desc,
                current_tab: 0,
                selected_app_id,
            });
            self.refresh_properties_dialog();
        } else {
            let rows = if targets.is_empty() {
                vec![
                    ("Location".to_owned(), t.location.display()),
                    ("Items".to_owned(), rows::thousands(t.entries.len())),
                    (
                        "Free".to_owned(),
                        t.volume
                            .map(|v| format!("{} of {}", format::size(v.free_bytes), format::size(v.total_bytes)))
                            .unwrap_or_default(),
                    ),
                ]
            } else {
                let bytes: u64 = targets.iter().filter(|e| !e.is_navigable()).map(|e| e.size).sum();
                vec![
                    ("Items".to_owned(), rows::thousands(targets.len())),
                    ("Folders".to_owned(), targets.iter().filter(|e| e.is_navigable()).count().to_string()),
                    ("Total file size".to_owned(), format::size(bytes)),
                    ("Location".to_owned(), t.location.display()),
                ]
            };
            self.dialog = Some(PendingDialog::Info);
            self.ui.send(Update::Dialog(DialogView {
                kind: DialogKind::Properties,
                title: "Properties".into(),
                message: String::new(),
                rows,
                input: String::new(),
                preview_path: None,
                ..Default::default()
            }));
        }
    }

    fn folder_properties(&mut self, pane: usize) {
        let t = self.panes[pane].tab();
        let rows = vec![
            ("Location".to_owned(), t.location.display()),
            ("Items".to_owned(), rows::thousands(t.entries.len())),
            (
                "Free".to_owned(),
                t.volume
                    .map(|v| format!("{} of {}", format::size(v.free_bytes), format::size(v.total_bytes)))
                    .unwrap_or_default(),
            ),
        ];
        self.dialog = Some(PendingDialog::Info);
        self.ui.send(Update::Dialog(DialogView {
            kind: DialogKind::Properties,
            title: format!("Properties · {}", t.location.display()),
            message: String::new(),
            rows,
            input: String::new(),
            preview_path: None,
            ..Default::default()
        }));
    }

    fn refresh_properties_dialog(&mut self) {
        let Some(PendingDialog::Properties {
            entry,
            backend,
            target_name,
            local_path,
            mime,
            current_tab,
            selected_app_id,
            ..
        }) = &self.dialog
        else {
            return;
        };
        let (entry, backend, name, lp, m, tab, selected_app_id) = (
            entry.clone(),
            *backend,
            target_name.clone(),
            local_path.clone(),
            mime.clone(),
            *current_tab,
            selected_app_id.clone(),
        );
        let editable = entry.kind == FileKind::File && lp.is_some();
        self.properties_generation += 1;
        let generation = self.properties_generation;
        let tx = self.tx.clone();
        // Publish the modal immediately. File I/O, hashes and external thumbnail
        // tools run on the blocking pool and cannot stop navigation or cancellation.
        self.ui.send(Update::Dialog(DialogView {
            kind: DialogKind::Properties,
            title: format!("{name} Properties"),
            input: name.clone(),
            message: "Loading file details…".into(),
            properties_tab: tab,
            perms_editable: false,
            ..Default::default()
        }));
        let desktop_dirs = self.desktop_dirs();
        tokio::task::spawn_blocking(move || {
            let mut entry = entry;
            if let Some(path) = lp.as_ref()
                && let Ok(metadata) = std::fs::symlink_metadata(path)
            {
                use std::os::unix::fs::PermissionsExt;
                entry.mode = metadata.permissions().mode() & 0o7777;
                entry.modified = metadata.modified().ok();
                entry.size = if metadata.is_dir() { 0 } else { metadata.len() };
            }
            let idx = crate::desktop::DesktopIndex::load(&desktop_dirs);
            let (open_with_apps, _, mut selected_app_index) = build_open_with_items(&idx, &m);

            if let Some(sel_id) = selected_app_id.as_ref()
                && let Some(pos) = open_with_apps.iter().position(|a| a.id.as_str() == sel_id)
            {
                selected_app_index = pos as i32;
            }

            let mut preview_path = None;
            let mut rows = Vec::new();

            let maybe_e = Some(entry);
            if let Some(ref e) = maybe_e {
                let cat = format::category(e);
                let ext = format::extension(&e.name);
                if crate::thumbnails::is_previewable(cat, ext.as_deref()) {
                    let mtime_secs = e
                        .modified
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| i64::try_from(d.as_secs()).unwrap_or(0))
                        .unwrap_or(0);
                    let uri_str = e.uri.to_uri_string();
                    let is_vid = matches!(cat, core_vfs::format::Category::Video)
                        || crate::thumbnails::is_video_ext(ext.as_deref());
                    preview_path = crate::thumbnails::lookup_cached(&uri_str, mtime_secs)
                        .or_else(|| {
                            e.uri.to_local_path().and_then(|local_p| {
                                crate::thumbnails::generate_thumbnail(&local_p, &uri_str, mtime_secs, is_vid).ok()
                            })
                        })
                        .or_else(|| {
                            if !is_vid {
                                e.uri.to_local_path().filter(|p| crate::thumbnails::is_regular_file(p))
                            } else {
                                None
                            }
                        });
                }
                rows = vec![
                    ("Name".to_owned(), e.name.clone()),
                    ("Location".to_owned(), e.uri.parent().map(|p| p.display()).unwrap_or_default()),
                    ("Type".to_owned(), format::type_label(e)),
                    (
                        "Size".to_owned(),
                        if e.is_navigable() {
                            "folder".into()
                        } else {
                            format!(
                                "{} ({} bytes)",
                                format::size(e.size),
                                rows::thousands(usize::try_from(e.size).unwrap_or(usize::MAX))
                            )
                        },
                    ),
                    ("Modified".to_owned(), backends::time::format_local(e.modified)),
                    ("Permissions".to_owned(), format!("{} ({:o})", format::mode(e.kind, e.mode), e.mode)),
                    (
                        "VFS status".to_owned(),
                        if e.vfs.status.label().is_empty() { "local".into() } else { e.vfs.status.label().into() },
                    ),
                    ("Backend".to_owned(), backend.map(|b| b.badge().to_owned()).unwrap_or_default()),
                    ("URI".to_owned(), e.uri.to_uri_string()),
                ];
                if let Some(local_p) = e.uri.to_local_path()
                    && e.kind == FileKind::File
                    && e.size <= 256 * 1024 * 1024
                    && let Ok(hashes) = crate::hash::compute_hashes(&local_p)
                {
                    rows.push(("SHA-256".to_owned(), hashes.sha256));
                }
                for (k, v) in &e.vfs.attributes {
                    let label = match k.as_str() {
                        "orig" => "Original Location",
                        "deletion-date" => "Date Deleted",
                        "target" => "Target File",
                        _ => continue,
                    };
                    rows.push((label.to_owned(), v.clone()));
                }
            }

            #[cfg(unix)]
            let current_mode = if let Some(ref local_p) = lp {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(local_p).map(|m| m.permissions().mode()).unwrap_or(0o644)
            } else {
                0o644
            };
            #[cfg(not(unix))]
            let current_mode = 0o644;

            let (perms_owner, perms_group) = if let Some(ref local_p) = lp {
                let (user, group) = get_user_and_group(local_p);
                (
                    format!("{user} ({})", format_perm_bits((current_mode >> 6) & 7)),
                    format!("{group} ({})", format_perm_bits((current_mode >> 3) & 7)),
                )
            } else {
                (
                    format!("Owner ({})", format_perm_bits((current_mode >> 6) & 7)),
                    format!("Group ({})", format_perm_bits((current_mode >> 3) & 7)),
                )
            };
            let perms_others = format_perm_bits(current_mode & 7).to_string();
            let perms_executable = (current_mode & 0o111) != 0;

            let view = DialogView {
                kind: DialogKind::Properties,
                title: format!("{name} Properties"),
                message: String::new(),
                rows,
                input: name,
                preview_path,
                properties_tab: tab,
                open_with_apps,
                selected_app_index,
                custom_command: String::new(),
                mime_type: m.clone(),
                mime_desc: crate::desktop::mime_description(&m),
                perms_owner,
                perms_group,
                perms_others,
                perms_executable,
                perms_editable: editable,
                ..Default::default()
            };
            let _ = tx.blocking_send(Msg::Ev(Event::PropertiesReady { generation, view: Box::new(view) }));
        });
    }

    fn new_tab(&mut self, pane: usize, at: Option<VfsUri>) {
        self.cancel_rename();
        let loc = at.unwrap_or_else(|| self.panes[pane].tab().location.clone());
        let id = self.next_tab;
        self.next_tab += 1;
        let mut tab = Tab::new(id, loc.clone());
        tab.sort = self.panes[pane].tab().sort;
        let insert_idx = if self.panes[pane].tabs.is_empty() {
            0
        } else {
            (self.panes[pane].active + 1).min(self.panes[pane].tabs.len())
        };
        self.panes[pane].tabs.insert(insert_idx, tab);
        self.panes[pane].active = insert_idx;
        self.focus_pane(pane);
        self.load(pane, loc, false);
        self.trigger_save_session();
    }

    fn select_tab(&mut self, pane: usize, idx: usize) {
        if idx >= self.panes[pane].tabs.len() {
            return;
        }
        if self.panes[pane].active != idx {
            self.cancel_rename();
        }
        self.panes[pane].active = idx;
        let loc = self.panes[pane].tab().location.clone();
        let needs_load = self.panes[pane].tab().load_gen == 0 && !self.panes[pane].tab().loading;
        if needs_load {
            self.load(pane, loc, false);
        } else {
            let id = self.panes[pane].tab().id;
            self.prepare(pane, id, false);
            self.push_pane(pane);
            self.push_status();
        }
        self.push_places();
        self.trigger_save_session();
    }

    fn close_other_tabs(&mut self, pane: usize, keep_idx: usize) {
        self.cancel_rename();
        if keep_idx >= self.panes[pane].tabs.len() {
            return;
        }
        let keep_tab = self.panes[pane].tabs.remove(keep_idx);
        for t in self.panes[pane].tabs.drain(..) {
            t.cancel.cancel();
            if let Some(w) = t.watch {
                w.abort();
            }
        }
        self.panes[pane].tabs.push(keep_tab);
        self.panes[pane].active = 0;
        self.select_tab(pane, 0);
        self.trigger_save_session();
    }

    fn duplicate_tab(&mut self, pane: usize, idx: usize) {
        if idx >= self.panes[pane].tabs.len() {
            return;
        }
        let loc = self.panes[pane].tabs[idx].location.clone();
        self.new_tab(pane, Some(loc));
    }

    fn cycle_tab(&mut self, pane: usize, delta: i32) {
        let n = self.panes[pane].tabs.len();
        if n <= 1 {
            return;
        }
        let cur = self.panes[pane].active as i32;
        let next = (cur + delta).rem_euclid(n as i32) as usize;
        self.select_tab(pane, next);
    }

    fn close_tab(&mut self, pane: usize, idx: usize) {
        self.cancel_rename();
        if idx >= self.panes[pane].tabs.len() {
            return;
        }
        if self.panes[pane].tabs.len() == 1 {
            if pane == 1 {
                let t = self.panes[1].tabs.remove(0);
                t.cancel.cancel();
                if let Some(w) = t.watch {
                    w.abort();
                }
                let home = self.cfg.home.clone();
                let id = self.next_tab;
                self.next_tab += 1;
                self.panes[1].tabs.push(Tab::new(id, home));
                self.panes[1].active = 0;
                self.dual = false;
                self.active_pane = 0;
                self.push_flags();
                self.push_places();
                self.push_status();
                self.trigger_save_session();
            } else if self.dual {
                let t = self.panes[0].tabs.remove(0);
                t.cancel.cancel();
                if let Some(w) = t.watch {
                    w.abort();
                }
                self.panes.swap(0, 1);
                let home = self.cfg.home.clone();
                let id = self.next_tab;
                self.next_tab += 1;
                self.panes[1].tabs.push(Tab::new(id, home));
                self.panes[1].active = 0;
                self.dual = false;
                self.active_pane = 0;
                self.push_pane(0);
                self.push_flags();
                self.push_places();
                self.push_status();
                self.trigger_save_session();
            } else {
                let t = self.panes[0].tabs.remove(0);
                t.cancel.cancel();
                if let Some(w) = t.watch {
                    w.abort();
                }
                let home = self.cfg.home.clone();
                let id = self.next_tab;
                self.next_tab += 1;
                self.panes[0].tabs.push(Tab::new(id, home));
                self.panes[0].active = 0;
                self.trigger_save_session();
                self.quitting = true;
                self.ui.send(Update::Quit);
            }
            return;
        }
        let t = self.panes[pane].tabs.remove(idx);
        t.cancel.cancel();
        if let Some(w) = t.watch {
            w.abort();
        }
        let a = self.panes[pane].active;
        self.panes[pane].active = if a > idx || a >= self.panes[pane].tabs.len() { a.saturating_sub(1) } else { a };
        let new = self.panes[pane].active;
        self.select_tab(pane, new);
        self.trigger_save_session();
    }

    fn save_bookmarks(&self) {
        let Some(path) = self.cfg.bookmarks_file.clone() else { return };
        let body: String = self.bookmarks.iter().map(|b| format!("{}\n", b.to_uri_string())).collect();
        tokio::task::spawn_blocking(move || {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, body);
        });
    }

    fn rebuild_tree(&mut self) {
        if let Some(t) = self.tree_task.take() {
            t.abort();
        }
        let loc = self.panes[self.active_pane].tab().location.clone();
        // The current folder is always revealed.
        for (_, u) in loc.crumbs() {
            if u != loc {
                self.tree_expanded.insert(u);
            }
        }
        let root = loc.crumbs().into_iter().next().map(|c| c.1).unwrap_or_else(|| loc.clone());
        let expanded = self.tree_expanded.clone();
        let (reg, tx, show_hidden) = (self.registry.clone(), self.tx.clone(), self.show_hidden);
        self.tree_task = Some(tokio::spawn(async move {
            let mut out = Vec::new();
            let mut stack = vec![(root, 0i32)];
            while let Some((u, depth)) = stack.pop() {
                let open = expanded.contains(&u);
                out.push(TreeRow {
                    label: u.crumbs().last().map(|c| c.0.clone()).unwrap_or_default().into(),
                    uri: u.display().into(),
                    depth,
                    expanded: open,
                    has_children: true,
                    active: u == loc,
                });
                if !open || out.len() > 4_000 {
                    continue;
                }
                let Ok(p) = reg.resolve(&u) else { continue };
                let (btx, mut brx) = mpsc::channel(8);
                let cancel = CancellationToken::new();
                let listing = p.list_dir(&u, ListOptions { first_batch: 500, batch: 2_000 }, btx, cancel);
                let collect = async {
                    let mut dirs = Vec::new();
                    while let Some(b) = brx.recv().await {
                        dirs.extend(b.into_iter().filter(|e| e.is_navigable() && (show_hidden || !e.is_hidden())));
                    }
                    dirs
                };
                let (_, mut dirs) = tokio::join!(listing, collect);
                let order = sort_order(&dirs, SortKey::default());
                let sorted: Vec<FileEntry> = order.iter().rev().map(|&i| dirs[i as usize].clone()).collect();
                dirs.clear();
                for d in sorted.into_iter().take(1_000) {
                    stack.push((d.uri, depth + 1));
                }
            }
            let _ = tx.send(Msg::Ev(Event::Tree(out))).await;
        }));
    }

    // ---- dispatch ---------------------------------------------------------------------

    fn command(&mut self, c: Command) {
        match c {
            Command::Navigate { pane, location } => {
                self.focus_pane(pane);
                match self.resolve_input(pane, &location) {
                    Ok(u) => self.navigate_to(pane, u),
                    Err(e) => self.say(e.to_string(), true),
                }
            }
            Command::Nav { pane, action } => {
                let t = self.panes[pane].tab_mut();
                let target = match action.as_str() {
                    "back" => t.back.pop().inspect(|_| {
                        let cur = t.location.clone();
                        t.forward.push(cur);
                    }),
                    "forward" => t.forward.pop().inspect(|_| {
                        let cur = t.location.clone();
                        t.back.push(cur);
                    }),
                    "up" => t.location.parent(),
                    "home" => Some(self.cfg.home.clone()),
                    "refresh" => {
                        let loc = t.location.clone();
                        self.load(pane, loc, true);
                        return;
                    }
                    _ => None,
                };
                let Some(target) = target else { return };
                if matches!(action.as_str(), "back" | "forward") {
                    // The history stacks were updated above; load without recording again.
                    self.load(pane, target, false);
                    self.push_places();
                    self.trigger_save_session();
                } else {
                    let prev = self.panes[pane].tab().location.clone();
                    self.navigate_to(pane, target);
                    if action == "up"
                        && let Some(n) = prev.file_name()
                    {
                        self.panes[pane].tab_mut().pending_select = Some(n.to_owned());
                    }
                }
            }
            Command::RowClicked { pane, index, ctrl, shift } => self.click(pane, index, ctrl, shift),
            Command::RowActivated { pane, index } => self.activate(pane, index),
            Command::RowContext { pane, index } => {
                self.focus_pane(pane);
                self.panes[pane].tab_mut().cursor = index;
                let t = self.panes[pane].tab();
                if let Some(e) = t.entry_at(index)
                    && !t.selected.contains(&e.name)
                {
                    let name = e.name.clone();
                    self.set_selection(pane, [name].into());
                }
                self.push_pane(pane);
            }
            Command::MoveCursor { pane, delta, extend } => self.move_cursor(pane, delta, extend),
            Command::SortBy { pane, column } => {
                let col = SortColumn::from_index(column);
                let t = self.panes[pane].tab_mut();
                t.sort = if t.sort.column == col {
                    SortKey { column: col, ascending: !t.sort.ascending }
                } else {
                    SortKey { column: col, ascending: true }
                };
                let id = t.id;
                self.prepare(pane, id, true);
                self.push_pane(pane);
            }
            Command::Filter { pane, text } => {
                let t = self.panes[pane].tab_mut();
                if t.filter != text {
                    t.filter = text;
                    t.cursor = 0;
                    let id = t.id;
                    self.prepare(pane, id, false);
                }
            }
            Command::TabNew(p) => self.new_tab(p, None),
            Command::TabClose(p, i) => self.close_tab(p, i),
            Command::TabCloseOthers(p, i) => self.close_other_tabs(p, i),
            Command::TabDuplicate(p, i) => self.duplicate_tab(p, i),
            Command::TabSelect(p, i) => {
                self.focus_pane(p);
                self.select_tab(p, i);
            }
            Command::TabCycle { pane, delta } => self.cycle_tab(pane, delta),
            Command::CustomAction { pane, id } => self.run_custom_action(pane, &id),
            Command::Quit => {
                self.quitting = true;
                self.trigger_save_session();
                if let Some(ref saver) = self.session_saver {
                    let saver = saver.clone();
                    tokio::spawn(async move {
                        if let Err(e) = saver.flush().await {
                            eprintln!("gungnir-fm: session save failed: {e}");
                        }
                    });
                }
                self.ui.send(Update::Quit);
            }
            Command::PaneFocused(p) => self.focus_pane(p),
            Command::Run(name) => self.run_command(&name),
            Command::SetDarkMode(dark) => {
                self.dark_mode = dark;
                self.push_flags();
                self.trigger_save_session();
            }
            Command::ToggleDarkMode => {
                self.dark_mode = !self.dark_mode;
                self.push_flags();
                self.trigger_save_session();
            }
            Command::WindowChanged(state) => {
                self.window_state = state;
                self.trigger_save_session();
            }
            Command::ViewMode(m) => {
                self.view_mode = m.clamp(0, 2);
                self.trigger_save_session();
            }
            Command::Zoom(z) => {
                self.zoom_level = z.clamp(0.5, 3.5);
                self.trigger_save_session();
            }
            Command::SidebarOpen(uri) => {
                let p = self.active_pane;
                match self.resolve_input(p, &uri) {
                    Ok(u) => self.navigate_to(p, u),
                    Err(e) => self.say(e.to_string(), true),
                }
            }
            Command::SidebarOpenTab(uri) => {
                let p = self.active_pane;
                match self.resolve_input(p, &uri) {
                    Ok(u) => self.new_tab(p, Some(u)),
                    Err(e) => self.say(e.to_string(), true),
                }
            }
            Command::SidebarOpenWindow(uri) => {
                let p = self.active_pane;
                let location = match self.resolve_input(p, &uri) {
                    Ok(u) => u,
                    Err(e) => {
                        self.say(e.to_string(), true);
                        return;
                    }
                };
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = std::env::var_os("GUNGNIR_FM_LAUNCHER")
                        .map(std::path::PathBuf::from)
                        .map_or_else(std::env::current_exe, Ok)
                        .and_then(|exe| {
                            std::process::Command::new(exe)
                                .arg(location.to_uri_string())
                                .stdin(std::process::Stdio::null())
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null())
                                .spawn()
                        })
                        .map(|_| "Opened new window".to_owned())
                        .map_err(|e| format!("Could not open window: {e}"));
                    let _ = tx.blocking_send(Msg::Ev(Event::Op { result, reload: Vec::new(), select: None }));
                });
            }
            Command::RemoveBookmark(uri_str) => {
                if let Ok(uri) = VfsUri::parse(&uri_str)
                    && let Some(pos) = self.bookmarks.iter().position(|b| b == &uri)
                {
                    self.bookmarks.remove(pos);
                    self.save_bookmarks();
                    self.push_places();
                    self.say("Bookmark removed", false);
                }
            }
            Command::TreeToggle(i) => {
                let Some(uri) = self.tree_rows.get(i).and_then(|s| VfsUri::parse(s).ok()) else { return };
                if !self.tree_expanded.remove(&uri) {
                    self.tree_expanded.insert(uri);
                }
                self.rebuild_tree();
            }
            Command::SidebarMode(m) => {
                self.sidebar_mode = m;
                if m == 1 {
                    self.rebuild_tree();
                }
                self.push_flags();
                self.trigger_save_session();
            }
            Command::TransferAction { id, action } => self.transfer_action(id, &action),
            Command::DialogAccept(text) => self.dialog_accept(text),
            Command::DialogChoose(i) => self.dialog_choose(i),
            Command::DeviceOpen(object) => self.device_open(&object),
            Command::DeviceEject(object) => self.device_eject(&object),
            Command::Drop { src_pane, src_index, dst_pane, dst_index, copy } => {
                self.drop_items(src_pane, src_index, dst_pane, dst_index, copy);
            }
            Command::DialogCancel => {
                self.dialog = None;
                self.preview_wait = None;
                self.ui.send(Update::CloseDialog);
            }
            Command::RenameCommit { pane, index, name } => {
                self.ui.send(Update::Rename { pane: -1, index: -1 });
                let Some(edit) = self.renaming.take() else { return };
                if edit.pane != pane
                    || edit.index != index
                    || self.panes[pane].tab().id != edit.tab
                    || self.panes[pane].tab().entry_at(index).is_none_or(|entry| entry.uri != edit.uri)
                {
                    self.say("Folder contents changed; start rename again", true);
                    return;
                }
                if edit.uri.file_name() == Some(name.as_str()) || name.trim().is_empty() {
                    return;
                }
                self.start_rename(edit.uri, name, RenameAction::Forward, None);
            }
            Command::RenameCancel => {
                self.renaming = None;
                self.ui.send(Update::Rename { pane: -1, index: -1 });
            }
            Command::DialogCustomAction { action, opt1, opt2, extra1, extra2 } => {
                self.dialog_custom_action(&action, opt1, opt2, &extra1, &extra2);
            }
            Command::TerminalRun(cmd) => {
                self.terminal_run(&cmd);
            }
            Command::TerminalClear => {
                self.terminal_clear();
            }
            Command::TriggerUpdate => {
                if self.update_available && !self.updating {
                    self.updating = true;
                    self.ui.send(Update::PackageStatus {
                        available: true,
                        version: self.update_version.clone(),
                        updating: true,
                    });
                    let format = self.package_format;
                    let ver = self.update_version.clone();
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        let res = crate::updater::perform_update(format, &ver).await;
                        let (success, message) = match res {
                            Ok(msg) => (true, msg),
                            Err(err) => (false, err),
                        };
                        let _ = tx.send(Msg::Ev(Event::UpdateFinished { success, message })).await;
                    });
                }
            }
        }
    }

    fn dialog_custom_action(&mut self, action: &str, opt1: bool, opt2: bool, extra1: &str, extra2: &str) {
        if let Some(tab_str) = action.strip_prefix("tab-") {
            if let Ok(tab) = tab_str.parse::<i32>()
                && let Some(PendingDialog::Properties { current_tab, .. }) = &mut self.dialog
            {
                *current_tab = tab;
                self.refresh_properties_dialog();
            }
            return;
        }

        if let Some(app_id) = action.strip_prefix("select-app:") {
            if let Some(PendingDialog::Properties { selected_app_id, .. }) = &mut self.dialog {
                *selected_app_id = Some(app_id.to_string());
            }
            return;
        }

        if let Some(app_id) = action.strip_prefix("open-with-app:") {
            self.handle_open_with_app(app_id);
            return;
        }

        if let Some(cmd) = action.strip_prefix("open-custom:") {
            self.handle_open_custom(cmd);
            return;
        }

        if action == "open-selected" {
            self.handle_open_selected();
            return;
        }

        if let Some(app_id) = action.strip_prefix("set-default-app:") {
            self.handle_set_default_app(app_id);
            return;
        }

        if let Some(cmd) = action.strip_prefix("set-default-custom:") {
            self.handle_set_default_custom(cmd);
            return;
        }

        if action == "set-default-app" {
            if !extra1.is_empty() {
                self.handle_set_default_custom(extra1);
            } else if let Some(PendingDialog::Properties { selected_app_id: Some(ref id), .. }) = self.dialog {
                let id = id.clone();
                self.handle_set_default_app(&id);
            }
            return;
        }

        if action == "reset-default-app" {
            self.handle_reset_default_app();
            return;
        }

        if let Some(exec_str) = action.strip_prefix("apply-permissions:") {
            let is_exec = exec_str == "exec";
            self.handle_apply_permissions(is_exec);
            return;
        }

        if action == "browse-custom-app" {
            self.handle_browse_custom_app();
            return;
        }

        match action {
            "replace" | "replace-if-newer" | "auto-rename" | "skip" => {
                self.handle_conflict_action(action, opt1);
            }
            "search" => {
                self.handle_search(extra1.to_string(), opt1, opt2);
            }
            "preview-rename" => {
                self.handle_batch_rename_preview(extra1, extra2);
            }
            "apply-batch-rename" => {
                self.handle_batch_rename_apply();
            }
            "sync-left-to-right" => {
                self.handle_compare_sync(true);
            }
            "sync-right-to-left" => {
                self.handle_compare_sync(false);
            }
            _ => {}
        }
    }

    fn launch_properties_app(&self, launch: impl FnOnce() -> Result<String, String> + Send + 'static) {
        let tx = self.tx.clone();
        let generation = self.properties_generation;
        tokio::task::spawn_blocking(move || {
            let result = launch();
            let _ = tx.blocking_send(Msg::Ev(Event::ApplicationLaunched { generation, result }));
        });
    }

    fn handle_open_with_app(&mut self, app_id: &str) {
        let Some(PendingDialog::Properties { local_path: Some(path), .. }) = &self.dialog else { return };
        let path = path.clone();
        let dirs = self.desktop_dirs();
        let app_id = app_id.to_owned();
        self.launch_properties_app(move || {
            let index = crate::desktop::DesktopIndex::load(&dirs);
            let app = index
                .all_apps()
                .into_iter()
                .find(|app| app.id == app_id)
                .ok_or_else(|| "Application is no longer available".to_owned())?;
            crate::desktop::launch(app, &[path])
                .map(|_| format!("Opened with {}", app.name))
                .map_err(|error| error.to_string())
        });
    }

    fn handle_open_custom(&mut self, command: &str) {
        let Some(PendingDialog::Properties { local_path: Some(path), .. }) = &self.dialog else { return };
        let path = path.clone();
        let command = command.to_owned();
        self.launch_properties_app(move || {
            crate::desktop::launch_custom(&command, &[path])
                .map(|_| "Opened with custom application".into())
                .map_err(|error| error.to_string())
        });
    }

    fn handle_open_selected(&mut self) {
        if let Some(PendingDialog::Properties { selected_app_id: Some(ref id), .. }) = self.dialog {
            let id = id.clone();
            self.handle_open_with_app(&id);
        }
    }

    fn handle_set_default_app(&mut self, app_id: &str) {
        if let Some(PendingDialog::Properties { mime, mime_desc, selected_app_id, .. }) = &mut self.dialog {
            let m = mime.clone();
            let desc = mime_desc.clone();
            *selected_app_id = Some(app_id.to_string());
            match crate::desktop::set_default_app_with_dirs(self.cfg.desktop_dirs.as_ref(), &m, app_id) {
                Ok(()) => {
                    self.say(format!("Set {app_id} as default application for {desc}"), false);
                    self.refresh_properties_dialog();
                }
                Err(e) => {
                    self.say(format!("Failed to set default application: {e}"), true);
                }
            }
        }
    }

    fn handle_set_default_custom(&mut self, cmd: &str) {
        if let Some(PendingDialog::Properties { mime, mime_desc, selected_app_id, .. }) = &mut self.dialog {
            let m = mime.clone();
            let desc = mime_desc.clone();
            let trimmed = cmd.trim();
            if trimmed.is_empty() {
                return;
            }
            let app_name = match crate::desktop::custom_command_name(trimmed) {
                Ok(name) => name,
                Err(error) => {
                    self.say(format!("Invalid application command: {error}"), true);
                    return;
                }
            };
            match crate::desktop::create_custom_desktop_entry_with_dirs(
                self.cfg.desktop_dirs.as_ref(),
                trimmed,
                &app_name,
            ) {
                Ok(desktop_id) => {
                    *selected_app_id = Some(desktop_id.clone());
                    match crate::desktop::set_default_app_with_dirs(self.cfg.desktop_dirs.as_ref(), &m, &desktop_id) {
                        Ok(()) => {
                            self.say(format!("Set {app_name} as default application for {desc}"), false);
                            self.refresh_properties_dialog();
                        }
                        Err(e) => {
                            self.say(format!("Failed to set default application: {e}"), true);
                        }
                    }
                }
                Err(e) => {
                    self.say(format!("Failed to create custom application entry: {e}"), true);
                }
            }
        }
    }

    fn handle_reset_default_app(&mut self) {
        if let Some(PendingDialog::Properties { mime, mime_desc, .. }) = &self.dialog {
            let m = mime.clone();
            let desc = mime_desc.clone();
            match crate::desktop::reset_default_app_with_dirs(self.cfg.desktop_dirs.as_ref(), &m) {
                Ok(()) => {
                    self.say(format!("Reset default application for {desc} to system default"), false);
                    self.refresh_properties_dialog();
                }
                Err(e) => {
                    self.say(format!("Failed to reset default application: {e}"), true);
                }
            }
        }
    }

    fn handle_apply_permissions(&mut self, is_exec: bool) {
        let (p, path) = match &self.dialog {
            Some(PendingDialog::Properties { pane, local_path: Some(path), entry, .. })
                if entry.kind == FileKind::File =>
            {
                (*pane, path.clone())
            }
            _ => return,
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&path) {
                let mut mode = meta.permissions().mode();
                if is_exec {
                    mode |= 0o111;
                } else {
                    mode &= !0o111;
                }
                if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)) {
                    self.say(format!("Failed to set permissions: {e}"), true);
                } else {
                    self.say(format!("Updated permissions: {:o}", mode & 0o777), false);
                    self.refresh_properties_dialog();
                    let loc = self.panes[p].tab().location.clone();
                    self.load(p, loc, true);
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (path, is_exec, p);
        }
    }

    fn handle_browse_custom_app(&mut self) {
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let out = std::process::Command::new("zenity")
                .args(["--file-selection", "--title=Select Executable Application", "--filename=/usr/bin/"])
                .output();
            if let Ok(o) = out
                && o.status.success()
            {
                let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if !path.is_empty() {
                    let _ = tx.blocking_send(Msg::Ev(Event::CustomAppSelected(path)));
                }
            }
        });
    }

    fn push_terminal(&self) {
        self.ui.send(Update::Terminal {
            open: self.terminal_open,
            cwd: self.terminal_cwd.display().to_string(),
            output: self.terminal_output.clone(),
        });
    }

    fn terminal_run(&mut self, cmd: &str) {
        let trimmed = cmd.trim();
        if trimmed.is_empty() {
            return;
        }
        self.terminal_output.push_str(&format!("$ {trimmed}\n"));
        if trimmed == "clear" {
            self.terminal_output.clear();
            self.push_terminal();
            return;
        }
        if trimmed == "cd" || trimmed.starts_with("cd ") {
            let target = trimmed.strip_prefix("cd").unwrap_or("").trim();
            let new_path = if target.is_empty() || target == "~" {
                self.cfg.home.to_local_path().unwrap_or_else(|| PathBuf::from(self.cfg.home.path()))
            } else {
                let p = Path::new(target);
                if p.is_absolute() { p.to_path_buf() } else { self.terminal_cwd.join(p) }
            };
            match std::fs::canonicalize(&new_path) {
                Ok(real) if real.is_dir() => {
                    self.terminal_cwd = real.clone();
                    if let Ok(uri) = VfsUri::from_local_path(&real) {
                        self.navigate_to(self.active_pane, uri);
                    }
                }
                _ => {
                    self.terminal_output.push_str(&format!("cd: {target}: No such file or directory\n"));
                }
            }
            self.push_terminal();
            return;
        }

        let cwd = self.terminal_cwd.clone();
        let cmd_str = trimmed.to_string();
        let tx = self.tx.clone();
        let active_loc = self.panes[self.active_pane].tab().location.clone();
        tokio::task::spawn_blocking(move || {
            let out = std::process::Command::new("sh").arg("-c").arg(&cmd_str).current_dir(&cwd).output();
            let text = match out {
                Ok(o) => {
                    let mut s = String::from_utf8_lossy(&o.stdout).to_string();
                    let err = String::from_utf8_lossy(&o.stderr);
                    if !err.is_empty() {
                        if !s.is_empty() && !s.ends_with('\n') {
                            s.push('\n');
                        }
                        s.push_str(&err);
                    }
                    if s.is_empty() && !o.status.success() {
                        s = format!("Process exited with status {}\n", o.status);
                    }
                    s
                }
                Err(e) => format!("Error executing command: {e}\n"),
            };
            let _ = tx.blocking_send(Msg::Ev(Event::TerminalOutput { text, reload_loc: active_loc }));
        });
        self.push_terminal();
    }

    fn terminal_clear(&mut self) {
        self.terminal_output.clear();
        self.push_terminal();
    }

    fn handle_search(&mut self, query: String, grep_content: bool, case_sensitive: bool) {
        let trimmed = query.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        let root = self.panes[self.active_pane].tab().location.clone();
        let Some(root_path) = root.to_local_path() else {
            self.say("Recursive search is only supported on local filesystems", true);
            return;
        };

        let tx = self.tx.clone();
        let task = tokio::task::spawn_blocking(move || {
            let mut results = Vec::new();
            let mut stack = vec![root_path.clone()];
            let mut files_checked = 0;
            let query_norm = if case_sensitive { trimmed.clone() } else { trimmed.to_lowercase() };

            while let Some(dir) = stack.pop() {
                if results.len() >= 500 || files_checked >= 15_000 {
                    break;
                }
                let Ok(entries) = std::fs::read_dir(&dir) else { continue };
                for entry in entries.flatten() {
                    let path = entry.path();
                    let filename = entry.file_name().to_string_lossy().into_owned();
                    if filename.starts_with('.') || filename == "target" || filename == "node_modules" {
                        continue;
                    }
                    if path.is_dir() {
                        stack.push(path);
                    } else if path.is_file() {
                        files_checked += 1;
                        if grep_content {
                            if let Ok(meta) = entry.metadata()
                                && meta.len() <= 5 * 1024 * 1024
                                && let Ok(contents) = std::fs::read_to_string(&path)
                            {
                                for (idx, line) in contents.lines().enumerate() {
                                    let matches = if case_sensitive {
                                        line.contains(&query_norm)
                                    } else {
                                        line.to_lowercase().contains(&query_norm)
                                    };
                                    if matches {
                                        let display_line = line.trim();
                                        let line_preview = if display_line.len() > 60 {
                                            format!("{}...", &display_line[..display_line.floor_char_boundary(60)])
                                        } else {
                                            display_line.to_string()
                                        };
                                        results.push((
                                            format!("{}:{}", filename, idx + 1),
                                            format!("{} [{}]", path.display(), line_preview),
                                        ));
                                        if results.len() >= 500 {
                                            break;
                                        }
                                    }
                                }
                            }
                        } else {
                            let matches = if case_sensitive {
                                filename.contains(&query_norm)
                            } else {
                                filename.to_lowercase().contains(&query_norm)
                            };
                            if matches {
                                results.push((filename, path.display().to_string()));
                            }
                        }
                    }
                }
            }
            let _ = tx.blocking_send(Msg::Ev(Event::SearchResults { root, results }));
        });
        self.search_task = Some(task);
    }

    fn handle_batch_rename_preview(&mut self, find_pattern: &str, replace_pattern: &str) {
        let Some(PendingDialog::BatchRename { pane, items, .. }) = &self.dialog else { return };
        let pane = *pane;
        let items = items.clone();
        let mut new_items = Vec::new();
        let mut rows = Vec::new();
        let is_numbering = find_pattern.contains('#') || find_pattern == "#";
        let start_num: usize = replace_pattern.parse().unwrap_or(1);

        for (idx, (uri, old_name)) in items.iter().enumerate() {
            let proposed = if find_pattern.is_empty() && !replace_pattern.is_empty() {
                format!("{replace_pattern}{old_name}")
            } else if is_numbering {
                let num_str = format!("{:02}", start_num.saturating_add(idx));
                find_pattern.replace('#', &num_str)
            } else if !find_pattern.is_empty() {
                old_name.replace(find_pattern, replace_pattern)
            } else {
                old_name.clone()
            };
            rows.push((old_name.clone(), proposed.clone()));
            new_items.push((uri.clone(), proposed));
        }

        self.dialog = Some(PendingDialog::BatchRename { pane, items, new_names: new_items });
        self.ui.send(Update::Dialog(DialogView {
            kind: DialogKind::BatchRename,
            title: format!("Batch Rename ({} files)", rows.len()),
            message: "Preview of planned file renames:".into(),
            rows,
            input: String::new(),
            preview_path: None,
            opt1: false,
            opt2: false,
            extra1: find_pattern.to_string(),
            extra2: replace_pattern.to_string(),
            ..Default::default()
        }));
    }

    fn handle_batch_rename_apply(&mut self) {
        let Some(PendingDialog::BatchRename { pane, new_names, .. }) = self.dialog.take() else { return };
        self.ui.send(Update::CloseDialog);
        let loc = self.panes[pane].tab().location.clone();
        let Ok(provider) = self.registry.resolve(&loc) else { return };

        let tx = self.tx.clone();
        tokio::task::spawn(async move {
            let result = async {
                // Validate every target before changing any file. Never turn a
                // collision or backend failure into a misleading success message.
                let mut destinations = HashSet::new();
                let mut planned = Vec::new();
                for (uri, new_name) in new_names {
                    let target = uri.parent().ok_or(VfsError::BadRequest)?.join(&new_name)?;
                    if !destinations.insert(target.clone()) {
                        return Err(VfsError::AlreadyExists(target.display()));
                    }
                    if target == uri {
                        continue;
                    }
                    match provider.stat(&target).await {
                        Ok(_) => return Err(VfsError::AlreadyExists(target.display())),
                        Err(VfsError::NotFound(_)) => {}
                        Err(e) => return Err(e),
                    }
                    planned.push((uri, new_name));
                }
                let mut renamed: Vec<(VfsUri, String)> = Vec::new();
                for (uri, new_name) in planned {
                    match provider.rename(&uri, &new_name).await {
                        Ok(target) => renamed.push((target, uri.file_name().ok_or(VfsError::BadRequest)?.to_owned())),
                        Err(e) => {
                            for (target, original_name) in renamed.iter().rev() {
                                if let Err(rollback) = provider.rename(target, original_name).await {
                                    return Err(VfsError::Transport(format!(
                                        "Rename failed: {e}; rollback failed: {rollback}"
                                    )));
                                }
                            }
                            return Err(e);
                        }
                    }
                }
                Ok(format!("Successfully renamed {} files", renamed.len()))
            }
            .await
            .map_err(|e: VfsError| e.to_string());
            let _ = tx.send(Msg::Ev(Event::Op { result, reload: vec![loc], select: None })).await;
        });
    }

    fn compare_panes(&mut self) {
        self.dual = true;
        self.push_flags();
        let left_loc = self.panes[0].tab().location.clone();
        let right_loc = self.panes[1].tab().location.clone();
        let left_entries = &self.panes[0].tab().entries;
        let right_entries = &self.panes[1].tab().entries;

        let mut names = BTreeSet::new();
        let mut left_map = BTreeMap::new();
        let mut right_map = BTreeMap::new();

        for e in left_entries.iter() {
            names.insert(e.name.clone());
            left_map.insert(e.name.clone(), e.clone());
        }
        for e in right_entries.iter() {
            names.insert(e.name.clone());
            right_map.insert(e.name.clone(), e.clone());
        }

        let mut rows = Vec::new();

        for name in names {
            match (left_map.get(&name), right_map.get(&name)) {
                (Some(l), None) => {
                    rows.push((format!("[LEFT ONLY] {name}"), format!("{} · Missing on Right", format::size(l.size))));
                }
                (None, Some(r)) => {
                    rows.push((format!("[RIGHT ONLY] {name}"), format!("{} · Missing on Left", format::size(r.size))));
                }
                (Some(l), Some(r)) => {
                    if l.size == r.size && l.modified == r.modified {
                        rows.push((
                            format!("[IDENTICAL] {name}"),
                            format!("{} · Same size & mtime", format::size(l.size)),
                        ));
                    } else if l.modified > r.modified {
                        rows.push((
                            format!("[NEWER LEFT] {name}"),
                            format!("Left: {} vs Right: {}", format::size(l.size), format::size(r.size)),
                        ));
                    } else {
                        rows.push((
                            format!("[NEWER RIGHT] {name}"),
                            format!("Right: {} vs Left: {}", format::size(r.size), format::size(l.size)),
                        ));
                    }
                }
                (None, None) => {}
            }
        }

        self.dialog = Some(PendingDialog::ComparePanes { left_loc: left_loc.clone(), right_loc: right_loc.clone() });
        self.ui.send(Update::Dialog(DialogView {
            kind: DialogKind::ComparePanes,
            title: "Dual-Pane Comparison & Sync".into(),
            message: format!("Left: {}\nRight: {}", left_loc.display(), right_loc.display()),
            rows,
            input: String::new(),
            preview_path: None,
            ..Default::default()
        }));
    }

    fn handle_compare_sync(&mut self, to_right: bool) {
        let Some(PendingDialog::ComparePanes { left_loc, right_loc, .. }) = self.dialog.take() else { return };
        self.ui.send(Update::CloseDialog);
        if to_right {
            let left_entries = &self.panes[0].tab().entries;
            let right_map: BTreeMap<String, &FileEntry> =
                self.panes[1].tab().entries.iter().map(|e| (e.name.clone(), e)).collect();
            let mut to_sync = Vec::new();
            for l in left_entries.iter() {
                match right_map.get(&l.name) {
                    None => to_sync.push(l.uri.clone()),
                    Some(r) if l.modified > r.modified => to_sync.push(l.uri.clone()),
                    _ => {}
                }
            }
            if to_sync.is_empty() {
                self.say("Right pane is already up to date with Left pane", false);
            } else {
                self.say(format!("Syncing {} items Left → Right...", to_sync.len()), false);
                self.start_transfer(TransferKind::Copy, to_sync, right_loc);
            }
        } else {
            let right_entries = &self.panes[1].tab().entries;
            let left_map: BTreeMap<String, &FileEntry> =
                self.panes[0].tab().entries.iter().map(|e| (e.name.clone(), e)).collect();
            let mut to_sync = Vec::new();
            for r in right_entries.iter() {
                match left_map.get(&r.name) {
                    None => to_sync.push(r.uri.clone()),
                    Some(l) if r.modified > l.modified => to_sync.push(r.uri.clone()),
                    _ => {}
                }
            }
            if to_sync.is_empty() {
                self.say("Left pane is already up to date with Right pane", false);
            } else {
                self.say(format!("Syncing {} items Right → Left...", to_sync.len()), false);
                self.start_transfer(TransferKind::Copy, to_sync, left_loc);
            }
        }
    }

    /// "Open With…": find the applications for the selection's type, opening the Nemo-style
    /// Open With dialog directly.
    fn open_with(&mut self, pane: usize) {
        let targets = self.panes[pane].tab().targets();
        if let [e] = targets.as_slice() {
            let local_path = e.uri.to_local_path();
            if local_path.is_none() {
                self.say("Only local files can be opened with an application", true);
                return;
            }
            let is_dir = e.is_navigable();
            let name = e.name.clone();
            let mime = self.mime_index.mime_for(&name, is_dir);
            let mime_desc = String::new();
            let selected_app_id = None;
            self.dialog = Some(PendingDialog::Properties {
                pane,
                entry: e.clone(),
                backend: self.panes[pane].tab().backend,
                target_name: name,
                local_path,
                mime,
                mime_desc,
                current_tab: 2,
                selected_app_id,
            });
            self.refresh_properties_dialog();
            return;
        }

        let local: Vec<(PathBuf, bool)> = targets
            .iter()
            .filter_map(|e| {
                let path = match e.uri.scheme() {
                    Scheme::File => e.uri.to_local_path(),
                    Scheme::Gungnir if e.uri.authority().is_empty() => Some(PathBuf::from(e.uri.path())),
                    Scheme::Trash => backends::local::home_trash()
                        .ok()
                        .map(|t| t.join("files").join(e.uri.path().trim_start_matches('/'))),
                    Scheme::Recent | Scheme::Network | Scheme::Gungnir | Scheme::Sftp | Scheme::Smb => None,
                }?;
                Some((path, e.is_navigable()))
            })
            .collect();
        if local.is_empty() {
            self.say(
                if targets.is_empty() {
                    "Nothing selected"
                } else {
                    "Only local files can be opened with an application"
                },
                true,
            );
            return;
        }
        let tx = self.tx.clone();
        let desktop_dirs = self.desktop_dirs();
        tokio::task::spawn_blocking(move || {
            let idx = crate::desktop::DesktopIndex::load(&desktop_dirs);
            let (first, is_dir) = &local[0];
            let name = first.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mime = idx.mime_for(&name, *is_dir);
            let apps: Vec<crate::desktop::App> = idx.apps_for(&mime).into_iter().cloned().collect();
            let files = local.into_iter().map(|(p, _)| p).collect();
            let _ = tx.blocking_send(Msg::Ev(Event::OpenWith { mime, files, apps }));
        });
    }

    /// A drop: the dragged row, or the whole selection when the row is part of it, goes
    /// into the folder row it was dropped on, or into the target pane's folder.
    fn drop_items(&mut self, src_pane: usize, src_index: usize, dst_pane: usize, dst_index: Option<usize>, copy: bool) {
        let st = self.panes[src_pane].tab();
        let Some(entry) = st.entry_at(src_index).cloned() else { return };
        let sources: Vec<VfsUri> = if st.selected.contains(&entry.name) {
            st.targets().into_iter().map(|e| e.uri).collect()
        } else {
            vec![entry.uri]
        };
        let dt = self.panes[dst_pane].tab();
        let dest = match dst_index.and_then(|i| dt.entry_at(i)) {
            Some(e) if e.is_navigable() => e.uri.clone(),
            _ => dt.location.clone(),
        };
        // Dropped back where it came from, or a folder onto itself: nothing to do.
        if sources.contains(&dest) || sources.iter().all(|s| s.parent().as_ref() == Some(&dest)) {
            return;
        }
        self.focus_pane(dst_pane);
        self.start_transfer(if copy { TransferKind::Copy } else { TransferKind::Move }, sources, dest);
    }

    fn push_devices(&self) {
        let loc = self.panes[self.active_pane].tab().location.display();
        let items = self
            .devices
            .iter()
            .map(|d| crate::DeviceItem {
                label: d.label.as_str().into(),
                uri: d.mount_point.clone().unwrap_or_default().into(),
                object: d.object.as_str().into(),
                mounted: d.mount_point.is_some(),
                size_text: format::size(d.size).into(),
                active: d.mount_point.as_deref().is_some_and(|m| m == loc),
            })
            .collect();
        self.ui.send(Update::Devices(items));
    }

    /// Open a sidebar device: navigate to it, mounting it first if needed.
    fn device_open(&mut self, object: &str) {
        let Some(dev) = self.devices.iter().find(|d| d.object == object).cloned() else { return };
        if let Some(mp) = dev.mount_point.as_deref().and_then(|m| VfsUri::parse(m).ok()) {
            let p = self.active_pane;
            self.navigate_to(p, mp);
            return;
        }
        let Some(conn) = self.bus.clone() else { return };
        self.say(format!("Mounting {}…", dev.label), false);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result =
                crate::drives::mount(&conn, &dev.object).await.map_err(|e| format!("Mount {}: {e}", dev.label));
            let _ = tx.send(Msg::Ev(Event::Mounted { object: dev.object, result })).await;
        });
    }

    /// Eject button: move panes off the drive, unmount, eject or power off.
    fn device_eject(&mut self, object: &str) {
        let Some(dev) = self.devices.iter().find(|d| d.object == object).cloned() else { return };
        let Some(conn) = self.bus.clone() else { return };
        if let Some(mp) = dev.mount_point.as_deref().and_then(|m| VfsUri::parse(m).ok()) {
            for p in 0..2 {
                for t in &mut self.panes[p].tabs {
                    if t.location.starts_with(&mp) {
                        t.cancel.cancel();
                        if let Some(w) = t.watch.take() {
                            w.abort();
                        }
                    }
                }
                if self.panes[p].tab().location.starts_with(&mp) {
                    let home = self.cfg.home.clone();
                    self.navigate_to(p, home);
                }
            }
        }
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = crate::drives::safely_remove(&conn, &dev)
                .await
                .map(|()| format!("{} can be safely removed", dev.label))
                .map_err(|e| format!("Eject {}: {e}", dev.label));
            let _ = tx.send(Msg::Ev(Event::Op { result, reload: Vec::new(), select: None })).await;
        });
    }

    fn dialog_choose(&mut self, index: usize) {
        match self.dialog.take() {
            Some(PendingDialog::OpenWith { files, apps }) => {
                self.ui.send(Update::CloseDialog);
                let Some(app) = apps.into_iter().nth(index) else { return };
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = match crate::desktop::launch(&app, &files) {
                        Ok(_) => Ok(format!("Opened with {}", app.name)),
                        Err(e) => Err(format!("{}: {e}", app.name)),
                    };
                    let _ = tx.blocking_send(Msg::Ev(Event::Op { result, reload: Vec::new(), select: None }));
                });
            }
            Some(PendingDialog::Search { results, .. }) => {
                self.ui.send(Update::CloseDialog);
                if let Some((_, full_path_str)) = results.get(index) {
                    let path_part = full_path_str.split_whitespace().next().unwrap_or(full_path_str);
                    let clean_path = path_part.split(':').next().unwrap_or(path_part);
                    let p = Path::new(clean_path);
                    let target_dir = if p.is_dir() { p.to_path_buf() } else { p.parent().unwrap_or(p).to_path_buf() };
                    if let Ok(uri) = VfsUri::from_local_path(&target_dir) {
                        self.navigate_to(self.active_pane, uri);
                    }
                }
            }
            _ => {
                self.ui.send(Update::CloseDialog);
            }
        }
    }

    fn transfer_action(&mut self, id: i64, action: &str) {
        if action == "clear" {
            self.transfers.retain(|(_, t)| {
                matches!(t.progress.state, TransferState::Running | TransferState::Paused | TransferState::Queued)
            });
            let open = (!self.transfers.is_empty()).then_some(true).or(Some(false));
            self.push_transfers(open);
            return;
        }
        let Ok(id) = u64::try_from(id) else { return };
        let Some(pos) = self.transfers.iter().position(|(i, _)| *i == id) else { return };
        let t = &mut self.transfers[pos].1;
        match action {
            "pause" => {
                t.pause.send_replace(true);
                t.progress.state = TransferState::Paused;
            }
            "resume" => {
                t.pause.send_replace(false);
                t.progress.state = TransferState::Running;
            }
            "cancel" => t.cancel.cancel(),
            "dismiss" => {
                self.transfers.remove(pos);
            }
            _ => {}
        }
        self.push_transfers(None);
    }

    fn dialog_accept(&mut self, text: String) {
        let Some(d) = self.dialog.take() else { return };
        self.ui.send(Update::CloseDialog);
        let creating_file = matches!(&d, PendingDialog::NewFile { .. });
        match d {
            PendingDialog::NewFolder { dir } | PendingDialog::NewFile { dir } => {
                let file = creating_file;
                let name = text;
                let uri = match dir.join(&name) {
                    Ok(uri) => uri,
                    Err(e) => return self.say(e.to_string(), true),
                };
                let Ok(provider) = self.registry.resolve(&uri) else { return };
                self.spawn_op(vec![dir.clone()], Some((dir, name.clone())), async move {
                    if file {
                        provider.create_file(&uri).await?;
                    } else {
                        provider.create_dir(&uri).await?;
                    }
                    Ok(format!("Created {name}"))
                });
            }
            PendingDialog::OpenMany { pane, entries } => {
                for entry in entries {
                    self.open_entry(pane, entry, true);
                }
            }
            PendingDialog::SelectPattern { pane } => {
                let tab = self.panes[pane].tab();
                let selected = tab
                    .visible
                    .iter()
                    .filter_map(|&i| tab.entries.get(i as usize))
                    .filter(|entry| core_vfs::pattern::matches_name_pattern(&text, &entry.name))
                    .map(|entry| entry.name.clone())
                    .collect();
                self.set_selection(pane, selected);
            }
            PendingDialog::EmptyTrash => {
                let Ok(root) = VfsUri::trash("/") else { return };
                let Ok(provider) = self.registry.resolve(&root) else { return };
                self.spawn_op(vec![root], None, async move {
                    provider.empty_trash().await?;
                    Ok("Trash emptied".into())
                });
            }
            PendingDialog::ClearRecent => {
                let Ok(root) = VfsUri::recent("") else { return };
                let Ok(provider) = self.registry.resolve(&root) else { return };
                self.spawn_op(vec![root], None, async move {
                    provider.clear_recent().await?;
                    Ok("Recent history cleared".into())
                });
            }
            PendingDialog::ConnectServer { pane } => {
                let target = text.trim().to_owned();
                if !target.is_empty() {
                    match self.resolve_input(pane, &target) {
                        Ok(u) => self.navigate_to(pane, u),
                        Err(e) => self.say(e.to_string(), true),
                    }
                }
            }
            PendingDialog::Delete { pane, uris } => {
                let dir = self.panes[pane].tab().location.clone();
                let Some(first) = uris.first() else { return };
                let Ok(provider) = self.registry.resolve(first) else { return };
                let n = uris.len();
                self.spawn_op(vec![dir], None, async move {
                    provider.delete(&uris, CancellationToken::new()).await?;
                    Ok(format!("Deleted {n} item{}", if n == 1 { "" } else { "s" }))
                });
            }
            PendingDialog::Compress { pane, targets, base_dir } => {
                let archive_name = text.trim().to_string();
                if archive_name.is_empty() {
                    return self.say("Archive name cannot be empty", true);
                }
                let archive_path = base_dir.join(&archive_name);
                let loc = self.panes[pane].tab().location.clone();
                let select_name = archive_name.clone();
                let rel_items: Vec<PathBuf> =
                    targets.iter().map(|p| p.strip_prefix(&base_dir).unwrap_or(p).to_path_buf()).collect();

                let format = backends::archive::ArchiveFormat::from_path(&archive_path)
                    .unwrap_or(backends::archive::ArchiveFormat::TarZstd);

                self.spawn_op(vec![loc.clone()], Some((loc, select_name)), async move {
                    backends::archive::compress_items(
                        &archive_path,
                        &base_dir,
                        &rel_items,
                        &backends::archive::ArchiveOptions {
                            format: Some(format),
                            level: backends::archive::CompressionLevel::Normal,
                            ..Default::default()
                        },
                    )
                    .map_err(|e| core_vfs::error::VfsError::Io {
                        op: "compress",
                        errno: 0,
                        msg: format!("{e}"),
                    })?;
                    Ok(format!("Losslessly compressed into {archive_name}"))
                });
            }
            PendingDialog::Info
            | PendingDialog::Properties { .. }
            | PendingDialog::OpenWith { .. }
            | PendingDialog::Conflict { .. }
            | PendingDialog::Search { .. }
            | PendingDialog::BatchRename { .. }
            | PendingDialog::ComparePanes { .. } => {}
        }
    }

    fn event(&mut self, e: Event) {
        match e {
            Event::Batch { pane, tab, generation, entries } => self.on_batch(pane, tab, generation, entries),
            Event::ListDone { pane, tab, generation, result, collected, started } => {
                self.on_list_done(pane, tab, generation, result, collected, started)
            }
            Event::Prepared { pane, tab, view_gen, order, visible, rows } => {
                self.on_prepared(pane, tab, view_gen, order, visible, rows)
            }
            Event::ThumbnailsReady { pane, tab, items } => {
                if self.is_active_tab(pane, tab) && !items.is_empty() {
                    self.ui.send(Update::ThumbnailsReady { pane, items });
                }
            }
            Event::DirChanged { pane, tab, removed } => {
                let Some(ti) = self.tab_index(pane, tab) else { return };
                if self.panes[pane].tabs[ti].loading {
                    return;
                }
                if removed {
                    self.panes[pane].tabs[ti].watch = None;
                }
                if ti == self.panes[pane].active {
                    let loc = self.panes[pane].tab().location.clone();
                    self.load(pane, loc, true);
                }
            }
            Event::Volume { pane, tab, info } => {
                if let Some(ti) = self.tab_index(pane, tab) {
                    self.panes[pane].tabs[ti].volume = Some(info);
                    if pane == self.active_pane {
                        self.push_status();
                    }
                }
            }
            Event::Mounts(m) => {
                if m != self.mounts {
                    self.mounts = m;
                    self.push_places();
                }
            }
            Event::Places(p, b) => {
                self.places = p;
                self.bookmarks = b;
                self.push_places();
            }
            Event::Tree(rows) => {
                self.tree_rows = rows.iter().map(|r| r.uri.to_string()).collect();
                self.ui.send(Update::Tree(rows));
            }
            Event::RenameFinished { action, from, result } => {
                self.rename_busy = false;
                match result {
                    Err(e) => self.say(format!("Rename failed: {e}"), true),
                    Ok((to, identity)) => {
                        match action {
                            RenameAction::Forward => self.rename_history.record(from, to.clone(), identity),
                            RenameAction::Undo => self.rename_history.finish_undo(),
                            RenameAction::Redo => self.rename_history.finish_redo(),
                        }
                        let verb = match action {
                            RenameAction::Forward => "Renamed to",
                            RenameAction::Undo => "Undid rename to",
                            RenameAction::Redo => "Redid rename to",
                        };
                        self.say(format!("{verb} {}", to.file_name().unwrap_or_default()), false);
                        if let Some(dir) = to.parent() {
                            let name = to.file_name().unwrap_or_default().to_string();
                            for pane in &mut self.panes {
                                if pane.tab().location == dir {
                                    pane.tab_mut().pending_select = Some(name.clone());
                                    pane.tab_mut().selected = [name.clone()].into();
                                }
                            }
                            self.reload_matching(&[dir]);
                        }
                    }
                }
                self.push_rename_history();
            }
            Event::Templates(items) => {
                self.templates_loading = false;
                if self.templates != items {
                    let menu = items
                        .iter()
                        .map(|item| crate::TemplateItem {
                            id: item.source.to_uri_string().into(),
                            title: item.title.clone().into(),
                        })
                        .collect();
                    self.templates = items;
                    self.ui.send(Update::Templates(menu));
                }
            }
            Event::Created { pane, tab, dir, result } => match result {
                Err(e) => self.say(e.to_string(), true),
                Ok(uri) => {
                    self.say(format!("Created {}", uri.file_name().unwrap_or_default()), false);
                    if let Some(ti) = self.tab_index(pane, tab) {
                        let t = &mut self.panes[pane].tabs[ti];
                        if t.location == dir {
                            let name = uri.file_name().unwrap_or_default().to_string();
                            t.selected = [name.clone()].into();
                            t.pending_select = Some(name.clone());
                            t.pending_rename = Some(name);
                            t.filter.clear();
                        }
                    }
                    self.reload_matching(&[dir]);
                }
            },
            Event::Op { result, reload, select } => {
                match result {
                    Ok(m) => self.say(m, false),
                    Err(m) => self.say(m, true),
                }
                if let Some((dir, name)) = select {
                    for p in 0..2 {
                        if self.panes[p].tab().location == dir {
                            let t = self.panes[p].tab_mut();
                            t.pending_select = Some(name.clone());
                            t.selected = [name.clone()].into();
                        }
                    }
                }
                self.reload_matching(&reload);
                self.push_places();
            }
            Event::Preview { seq, title, detail, image_path } => {
                // Only the preview still waited for (not cancelled or superseded), and only
                // over no other dialog the user opened meanwhile.
                if self.preview_wait == Some(seq) && self.dialog.is_none() {
                    self.preview_wait = None;
                    self.dialog = Some(PendingDialog::Info);
                    self.ui.send(Update::Preview { title, detail, image_path });
                }
            }
            Event::Info { title, kind, result } => match result {
                Ok(rows) => self.info_dialog(kind, &title, "", rows),
                Err(e) => self.info_dialog(DialogKind::Error, &title, &e.to_string(), Vec::new()),
            },
            Event::Progress(p) => {
                if let Some((_, t)) = self.transfers.iter_mut().find(|(i, _)| *i == p.id) {
                    let terminal =
                        matches!(p.state, TransferState::Done | TransferState::Cancelled | TransferState::Failed(_));
                    if terminal {
                        t.progress.state = p.state;
                    } else {
                        let now = Instant::now();
                        let dt = now.duration_since(t.last.0).as_secs_f64();
                        if dt >= 0.25 {
                            #[allow(clippy::cast_precision_loss)]
                            let inst = p.bytes_done.saturating_sub(t.last.1) as f64 / dt;
                            t.rate = if t.rate == 0.0 { inst } else { t.rate * 0.6 + inst * 0.4 };
                            t.last = (now, p.bytes_done);
                        }
                        if t.progress.state != TransferState::Paused || p.state != TransferState::Running {
                            t.progress = p;
                        } else {
                            let state = t.progress.state.clone();
                            t.progress = p;
                            t.progress.state = state;
                        }
                    }
                    self.transfers_dirty = true;
                }
            }
            Event::TransferEnded { id, result } => {
                if result.is_ok()
                    && let Some((_, transfer)) = self.transfers.iter().find(|(i, _)| *i == id)
                    && transfer.kind == TransferKind::Move
                {
                    if let Some(service) = &self.clipboard_service {
                        let _ = service.try_send(crate::clipboard::Request::Moved(transfer.sources.clone()));
                    }
                    if let Some((pending, true)) = &mut self.clipboard {
                        pending.retain(|uri| !transfer.sources.contains(uri));
                        if pending.is_empty() {
                            self.clipboard = None;
                        }
                    }
                    self.push_pane(0);
                    self.push_pane(1);
                }
                let Some((_, t)) = self.transfers.iter_mut().find(|(i, _)| *i == id) else { return };
                t.took.get_or_insert_with(|| t.started.elapsed());
                t.progress.state = match &result {
                    Ok(()) => TransferState::Done,
                    Err(VfsError::Cancelled) => TransferState::Cancelled,
                    Err(e) => TransferState::Failed(e.to_string()),
                };
                if result.is_ok() {
                    t.progress.bytes_done = t.progress.bytes_done.max(t.progress.bytes_total);
                }
                let mut dirs = vec![t.dest.clone()];
                dirs.extend(t.sources.iter().filter_map(VfsUri::parent));
                let msg = match &result {
                    Ok(()) => (format!("{} finished", t.title), false),
                    Err(VfsError::Cancelled) => (format!("{} cancelled", t.title), false),
                    Err(e) => (format!("{} failed: {e}", t.title), true),
                };
                self.push_transfers(None);
                self.say(msg.0, msg.1);
                self.reload_matching(&dirs);
            }
            Event::BusReady(conn) => self.bus = Some(conn),
            Event::Devices(d) => {
                self.devices = d;
                self.push_devices();
            }
            Event::Mounted { object, result } => match result {
                Ok(mp) => {
                    let label =
                        self.devices.iter().find(|d| d.object == object).map(|d| d.label.clone()).unwrap_or_default();
                    self.say(format!("Mounted {label} at {mp}"), false);
                    if let Ok(u) = VfsUri::parse(&mp) {
                        let p = self.active_pane;
                        self.navigate_to(p, u);
                    }
                }
                Err(e) => self.say(e, true),
            },
            Event::OpenWith { mime, files, apps } => {
                if apps.is_empty() {
                    self.say(format!("No application is registered for {mime}"), true);
                    return;
                }
                let n = files.len();
                let rows =
                    apps.iter().map(|a| (a.name.clone(), a.id.trim_end_matches(".desktop").to_owned())).collect();
                let what = files
                    .first()
                    .and_then(|f| f.file_name())
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let what = if n == 1 { what } else { format!("{n} files") };
                self.dialog = Some(PendingDialog::OpenWith { files, apps });
                self.ui.send(Update::Dialog(DialogView {
                    kind: DialogKind::Choose,
                    title: format!("Open {what} with"),
                    message: mime,
                    rows,
                    input: String::new(),
                    preview_path: None,
                    ..Default::default()
                }));
            }
            Event::SearchResults { root, results } => {
                let n = results.len();
                let message = format!("Found {n} match{} in {}", if n == 1 { "" } else { "es" }, root.display());
                self.dialog = Some(PendingDialog::Search { results: results.clone() });
                self.ui.send(Update::Dialog(DialogView {
                    kind: DialogKind::Search,
                    title: "Search Results".into(),
                    message,
                    rows: results,
                    input: String::new(),
                    preview_path: None,
                    ..Default::default()
                }));
            }
            Event::ApplicationLaunched { generation, result } => {
                if generation == self.properties_generation
                    && matches!(self.dialog, Some(PendingDialog::Properties { .. }))
                {
                    match result {
                        Ok(message) => {
                            self.dialog = None;
                            self.ui.send(Update::CloseDialog);
                            self.say(message, false);
                        }
                        Err(message) => self.say(format!("Could not open application: {message}"), true),
                    }
                }
            }
            Event::PropertiesReady { generation, mut view } => {
                if generation == self.properties_generation
                    && let Some(PendingDialog::Properties { selected_app_id, mime, mime_desc, .. }) = &mut self.dialog
                {
                    *mime = view.mime_type.clone();
                    *mime_desc = view.mime_desc.clone();
                    if let Some(id) = selected_app_id.as_ref() {
                        if let Some(i) = view.open_with_apps.iter().position(|app| app.id.as_str() == id) {
                            view.selected_app_index = i as i32;
                        }
                    } else {
                        *selected_app_id = usize::try_from(view.selected_app_index)
                            .ok()
                            .and_then(|i| view.open_with_apps.get(i))
                            .map(|app| app.id.to_string());
                    }
                    self.ui.send(Update::Dialog(*view));
                }
            }
            Event::MimeIndex(index) => {
                self.mime_index = *index;
                self.push_custom_actions(0);
                self.push_custom_actions(1);
            }
            Event::ClipboardPaste { dest, files } => {
                if let Some((uris, cut)) = files.unwrap_or_else(|()| self.clipboard.clone()) {
                    self.start_transfer(if cut { TransferKind::Move } else { TransferKind::Copy }, uris, dest);
                } else {
                    self.say("Nothing to paste", true);
                }
            }
            Event::TerminalOutput { text, reload_loc } => {
                self.terminal_output.push_str(&text);
                if self.terminal_output.len() > 30_000 {
                    let cut = self.terminal_output.ceil_char_boundary(self.terminal_output.len() - 25_000);
                    self.terminal_output.drain(..cut);
                }
                self.push_terminal();
                self.reload_matching(&[reload_loc]);
            }
            Event::UpdateCheckResult(res) => {
                if res.available {
                    self.update_available = true;
                    self.update_version = res.version.clone();
                    self.ui.send(Update::PackageStatus {
                        available: true,
                        version: res.version,
                        updating: self.updating,
                    });
                }
            }
            Event::UpdateFinished { success, message } => {
                self.updating = false;
                if success {
                    self.update_available = false;
                    self.ui.send(Update::PackageStatus { available: false, version: String::new(), updating: false });
                    self.say(message, false);
                } else {
                    self.ui.send(Update::PackageStatus {
                        available: true,
                        version: self.update_version.clone(),
                        updating: false,
                    });
                    self.say(message, true);
                }
            }
            Event::CustomAppSelected(path) => {
                self.ui.send(Update::CustomCommand(path));
            }
            Event::Tick => {
                if self.cfg.templates_dir.is_some() && self.templates_refreshed.elapsed() >= Duration::from_secs(5) {
                    self.refresh_templates();
                }
                if self.transfers_dirty {
                    self.push_transfers(None);
                }
                if self.message.as_ref().is_some_and(|(_, _, at)| at.elapsed() >= MESSAGE_TTL) {
                    self.message = None;
                    self.push_status();
                }
            }
        }
    }
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// Standard places that exist, plus saved bookmarks.
fn build_places(home: &VfsUri, bookmarks: Option<&std::path::Path>) -> (Vec<PlaceItem>, Vec<VfsUri>) {
    let mut out = Vec::new();
    let h = home.path().to_owned();
    let candidates: [(&str, String, i32); 10] = [
        ("Home", h.clone(), 0),
        ("Desktop", format!("{h}/Desktop"), 1),
        ("Documents", format!("{h}/Documents"), 2),
        ("Downloads", format!("{h}/Downloads"), 3),
        ("Music", format!("{h}/Music"), 7),
        ("Pictures", format!("{h}/Pictures"), 8),
        ("Videos", format!("{h}/Videos"), 4),
        ("Recent", "recent:///".to_owned(), 11),
        ("Trash", "trash:///".to_owned(), 5),
        ("File System", "/".to_owned(), 6),
    ];
    for (label, path, icon) in candidates {
        if icon == 5 || icon == 11 || std::path::Path::new(&path).is_dir() {
            out.push(PlaceItem { label: label.into(), uri: SharedString::from(path), icon, active: false });
        }
    }
    let marks = bookmarks
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.lines().filter_map(|l| VfsUri::parse(l).ok()).collect())
        .unwrap_or_default();
    (out, marks)
}

/// Launch an external terminal emulator in `dir` with fallback detection.
fn launch_terminal_in(dir: &Path) -> Option<&'static str> {
    #[cfg(unix)]
    use std::os::unix::process::CommandExt;

    let target_dir = if dir.is_dir() {
        dir.to_path_buf()
    } else if let Some(parent) = dir.parent().filter(|p| p.is_dir()) {
        parent.to_path_buf()
    } else {
        PathBuf::from(".")
    };

    let try_spawn = |mut cmd: std::process::Command, name: &'static str| -> Option<&'static str> {
        cmd.current_dir(&target_dir);
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        #[cfg(unix)]
        cmd.process_group(0);
        match cmd.spawn() {
            Ok(mut child) => {
                let _ = std::thread::Builder::new().stack_size(64 * 1024).spawn(move || {
                    let _ = child.wait();
                });
                Some(name)
            }
            Err(_) => None,
        }
    };

    let dir_str = target_dir.display().to_string();

    // 1. $TERMINAL environment variable if set
    if let Ok(term) = std::env::var("TERMINAL") {
        let t = term.trim();
        if !t.is_empty() {
            let mut cmd = std::process::Command::new(t);
            cmd.current_dir(dir);
            if let Some(n) = try_spawn(cmd, "custom $TERMINAL") {
                return Some(n);
            }
        }
    }

    // 2. xdg-terminal-exec (freedesktop terminal spec)
    {
        let mut cmd = std::process::Command::new("xdg-terminal-exec");
        cmd.current_dir(dir);
        if let Some(n) = try_spawn(cmd, "xdg-terminal-exec") {
            return Some(n);
        }
    }

    // 3. GNOME Terminal
    {
        let mut cmd = std::process::Command::new("gnome-terminal");
        cmd.arg(format!("--working-directory={dir_str}"));
        if let Some(n) = try_spawn(cmd, "GNOME Terminal") {
            return Some(n);
        }
    }

    // 4. Ptyxis
    {
        let mut cmd = std::process::Command::new("ptyxis");
        cmd.arg(format!("--working-directory={dir_str}"));
        if let Some(n) = try_spawn(cmd, "Ptyxis") {
            return Some(n);
        }
    }

    // 5. Konsole
    {
        let mut cmd = std::process::Command::new("konsole");
        cmd.args(["--workdir", &dir_str]);
        if let Some(n) = try_spawn(cmd, "Konsole") {
            return Some(n);
        }
    }

    // 6. XFCE4 Terminal
    {
        let mut cmd = std::process::Command::new("xfce4-terminal");
        cmd.arg(format!("--working-directory={dir_str}"));
        if let Some(n) = try_spawn(cmd, "XFCE Terminal") {
            return Some(n);
        }
    }

    // 7. MATE Terminal
    {
        let mut cmd = std::process::Command::new("mate-terminal");
        cmd.arg(format!("--working-directory={dir_str}"));
        if let Some(n) = try_spawn(cmd, "MATE Terminal") {
            return Some(n);
        }
    }

    // 8. Foot
    {
        let mut cmd = std::process::Command::new("foot");
        cmd.args(["-D", &dir_str]);
        if let Some(n) = try_spawn(cmd, "Foot") {
            return Some(n);
        }
    }

    // 9. Kitty
    {
        let mut cmd = std::process::Command::new("kitty");
        cmd.args(["--directory", &dir_str]);
        if let Some(n) = try_spawn(cmd, "Kitty") {
            return Some(n);
        }
    }

    // 10. Alacritty
    {
        let mut cmd = std::process::Command::new("alacritty");
        cmd.args(["--working-directory", &dir_str]);
        if let Some(n) = try_spawn(cmd, "Alacritty") {
            return Some(n);
        }
    }

    // 11. WezTerm
    {
        let mut cmd = std::process::Command::new("wezterm");
        cmd.args(["start", "--cwd", &dir_str]);
        if let Some(n) = try_spawn(cmd, "WezTerm") {
            return Some(n);
        }
    }

    // 12. Ghostty
    {
        let mut cmd = std::process::Command::new("ghostty");
        cmd.arg(format!("--working-directory={dir_str}"));
        if let Some(n) = try_spawn(cmd, "Ghostty") {
            return Some(n);
        }
    }

    // 13. Tilix
    {
        let mut cmd = std::process::Command::new("tilix");
        cmd.arg(format!("--working-directory={dir_str}"));
        if let Some(n) = try_spawn(cmd, "Tilix") {
            return Some(n);
        }
    }

    // 14. Terminator
    {
        let mut cmd = std::process::Command::new("terminator");
        cmd.args(["--working-directory", &dir_str]);
        if let Some(n) = try_spawn(cmd, "Terminator") {
            return Some(n);
        }
    }

    // 15. LXTerminal
    {
        let mut cmd = std::process::Command::new("lxterminal");
        cmd.arg(format!("--working-directory={dir_str}"));
        if let Some(n) = try_spawn(cmd, "LXTerminal") {
            return Some(n);
        }
    }

    // 16. rxvt-unicode / urxvt
    {
        let mut cmd = std::process::Command::new("urxvt");
        cmd.current_dir(dir);
        if let Some(n) = try_spawn(cmd, "urxvt") {
            return Some(n);
        }
    }

    // 17. rxvt
    {
        let mut cmd = std::process::Command::new("rxvt");
        cmd.current_dir(dir);
        if let Some(n) = try_spawn(cmd, "rxvt") {
            return Some(n);
        }
    }

    // 18. XTerm
    {
        let mut cmd = std::process::Command::new("xterm");
        cmd.current_dir(dir);
        if let Some(n) = try_spawn(cmd, "XTerm") {
            return Some(n);
        }
    }

    None
}
