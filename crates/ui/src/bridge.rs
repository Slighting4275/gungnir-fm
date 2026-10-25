//! The only way data reaches the UI: `Update`s applied on the Slint event loop via
//! `slint::Weak::upgrade_in_event_loop` (`invoke_from_event_loop`). Everything in an
//! `Update` is `Send` plain data built on worker threads; the UI thread only moves
//! it into models (`VecModel::set_vec` / `extend` / `set_row_data`).

use std::path::PathBuf;
use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::rows::{RowItem, to_file_row};
use crate::{
    AppWindow, Crumb, DialogKind, FileRow, KeyValue, MountItem, PaneInfo, PlaceItem, StatusInfo, TabInfo, TransferRow,
    TreeRow,
};

/// Pane header data without models (so it is `Send`).
#[derive(Debug, Clone, Default)]
pub struct PaneView {
    pub tabs: Vec<TabInfo>,
    pub active_tab: i32,
    pub location: String,
    pub crumbs: Vec<Crumb>,
    pub sort_column: i32,
    pub sort_ascending: bool,
    pub loading: bool,
    pub error: String,
    pub filter: String,
    pub can_back: bool,
    pub can_forward: bool,
    pub cursor: i32,
}

#[derive(Debug, Clone)]
pub struct DialogView {
    pub kind: DialogKind,
    pub title: String,
    pub message: String,
    pub rows: Vec<(String, String)>,
    pub input: String,
    pub preview_path: Option<PathBuf>,
    pub opt1: bool,
    pub opt2: bool,
    pub extra1: String,
    pub extra2: String,
}

impl Default for DialogView {
    fn default() -> Self {
        Self {
            kind: DialogKind::None,
            title: String::new(),
            message: String::new(),
            rows: Vec::new(),
            input: String::new(),
            preview_path: None,
            opt1: false,
            opt2: false,
            extra1: String::new(),
            extra2: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flags {
    pub active_pane: i32,
    pub dual_pane: bool,
    pub sidebar_visible: bool,
    pub show_hidden: bool,
    pub view_mode: i32,
    pub zoom_level: f32,
}

#[derive(Debug)]
pub enum Update {
    /// Replace every row of a pane.
    Rows {
        pane: usize,
        rows: Vec<RowItem>,
    },
    /// Append streamed rows.
    AppendRows {
        pane: usize,
        rows: Vec<RowItem>,
    },
    /// Loaded thumbnails ready to be shown.
    ThumbnailsReady {
        pane: usize,
        items: Vec<(String, PathBuf)>,
    },
    /// Display full quick-preview modal.
    Preview {
        title: String,
        detail: String,
        image_path: Option<PathBuf>,
    },
    /// Flip the selected flag of single rows.
    Selection {
        pane: usize,
        changes: Vec<(usize, bool)>,
    },
    Pane {
        pane: usize,
        view: PaneView,
    },
    Status(StatusInfo),
    Places(Vec<PlaceItem>),
    Mounts {
        items: Vec<MountItem>,
        label: String,
    },
    Tree(Vec<TreeRow>),
    Devices(Vec<crate::DeviceItem>),
    Transfers {
        rows: Vec<TransferRow>,
        active: i32,
        open: Option<bool>,
    },
    Flags(Flags),
    Dialog(DialogView),
    CloseDialog,
    Rename {
        pane: i32,
        index: i32,
    },
    CustomActions {
        pane: usize,
        actions: Vec<crate::CustomActionItem>,
    },
    Terminal {
        open: bool,
        cwd: String,
        output: String,
    },
    PackageStatus {
        available: bool,
        version: String,
        updating: bool,
    },
    Quit,
}

/// Where the controller sends updates (the window, or a test recorder).
pub trait UiSink: Send + Sync + 'static {
    fn send(&self, u: Update);
}

pub struct SlintSink {
    weak: slint::Weak<AppWindow>,
}

impl SlintSink {
    /// Install empty models on the window (replacing the Live-Preview sample data).
    /// Must run on the UI thread, before the event loop starts.
    pub fn attach(ui: &AppWindow) -> Self {
        ui.set_left_files(ModelRc::from(Rc::new(VecModel::<FileRow>::default())));
        ui.set_right_files(ModelRc::from(Rc::new(VecModel::<FileRow>::default())));
        ui.set_transfers(ModelRc::from(Rc::new(VecModel::<TransferRow>::default())));
        ui.set_active_transfers(0);
        ui.set_transfers_open(false);
        ui.set_places(ModelRc::default());
        ui.set_mounts(ModelRc::default());
        ui.set_tree(ModelRc::default());
        ui.set_devices(ModelRc::default());
        Self { weak: ui.as_weak() }
    }
}

fn files_model(ui: &AppWindow, pane: usize) -> ModelRc<FileRow> {
    if pane == 0 { ui.get_left_files() } else { ui.get_right_files() }
}

fn with_vec_model<T: Clone + 'static>(m: &ModelRc<T>, f: impl FnOnce(&VecModel<T>)) {
    if let Some(vm) = m.as_any().downcast_ref::<VecModel<T>>() {
        f(vm);
    }
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

pub fn apply(ui: &AppWindow, u: Update) {
    match u {
        Update::Rows { pane, rows } => {
            let file_rows: Vec<FileRow> = rows.iter().map(to_file_row).collect();
            with_vec_model(&files_model(ui, pane), |m| m.set_vec(file_rows));
        }
        Update::AppendRows { pane, rows } => {
            let file_rows: Vec<FileRow> = rows.iter().map(to_file_row).collect();
            with_vec_model(&files_model(ui, pane), |m| m.extend(file_rows));
        }
        Update::ThumbnailsReady { pane, items } => {
            let mut any = false;
            with_vec_model(&files_model(ui, pane), |m| {
                for (name, path) in items {
                    if let Ok(img) = slint::Image::load_from_path(&path) {
                        for i in 0..m.row_count() {
                            if let Some(mut r) = m.row_data(i)
                                && r.name.as_str() == name
                            {
                                r.thumbnail = img;
                                r.has_thumbnail = true;
                                m.set_row_data(i, r);
                                any = true;
                                break;
                            }
                        }
                    }
                }
            });
            if any {
                if pane == 0 {
                    ui.set_left_thumbnail_rev(ui.get_left_thumbnail_rev().wrapping_add(1));
                } else {
                    ui.set_right_thumbnail_rev(ui.get_right_thumbnail_rev().wrapping_add(1));
                }
            }
        }
        Update::Preview { title, detail, image_path } => {
            let img = image_path.and_then(|p| slint::Image::load_from_path(&p).ok()).unwrap_or_default();
            ui.set_preview_image(img);
            ui.set_preview_detail(SharedString::from(detail));
            ui.set_dialog_title(SharedString::from(title));
            ui.set_dialog_message(SharedString::new());
            ui.set_dialog_rows(model(Vec::new()));
            ui.set_dialog_input(SharedString::new());
            ui.set_dialog_kind(DialogKind::Preview);
        }
        Update::Selection { pane, changes } => with_vec_model(&files_model(ui, pane), |m| {
            for (i, sel) in changes {
                if let Some(mut r) = m.row_data(i)
                    && r.selected != sel
                {
                    r.selected = sel;
                    m.set_row_data(i, r);
                }
            }
        }),
        Update::Pane { pane, view } => {
            let info = PaneInfo {
                tabs: model(view.tabs),
                active_tab: view.active_tab,
                location: view.location.into(),
                crumbs: model(view.crumbs),
                sort_column: view.sort_column,
                sort_ascending: view.sort_ascending,
                loading: view.loading,
                error: view.error.into(),
                filter: view.filter.into(),
                can_back: view.can_back,
                can_forward: view.can_forward,
                cursor: view.cursor,
            };
            if pane == 0 { ui.set_left(info) } else { ui.set_right(info) }
        }
        Update::Status(s) => ui.set_status(s),
        Update::Places(p) => ui.set_places(model(p)),
        Update::Mounts { items, label } => {
            ui.set_mounts(model(items));
            ui.set_gungnir_label(label.into());
        }
        Update::Tree(t) => ui.set_tree(model(t)),
        Update::Devices(d) => ui.set_devices(model(d)),
        Update::Transfers { rows, active, open } => {
            let m = ui.get_transfers();
            match m.as_any().downcast_ref::<VecModel<TransferRow>>() {
                // Same length: update in place (no flicker of the drawer rows).
                Some(vm) if vm.row_count() == rows.len() => {
                    for (i, r) in rows.into_iter().enumerate() {
                        vm.set_row_data(i, r);
                    }
                }
                Some(vm) => vm.set_vec(rows),
                None => ui.set_transfers(model(rows)),
            }
            ui.set_active_transfers(active);
            if let Some(o) = open {
                ui.set_transfers_open(o);
            }
        }
        Update::Flags(f) => {
            ui.set_active_pane(f.active_pane);
            ui.set_dual_pane(f.dual_pane);
            ui.set_sidebar_visible(f.sidebar_visible);
            ui.set_show_hidden(f.show_hidden);
            ui.set_view_mode(f.view_mode);
            ui.set_zoom_level(f.zoom_level);
        }
        Update::Dialog(d) => {
            let img = d.preview_path.and_then(|p| slint::Image::load_from_path(&p).ok()).unwrap_or_default();
            ui.set_preview_image(img);
            ui.set_preview_detail(SharedString::new());
            ui.set_dialog_title(d.title.into());
            ui.set_dialog_message(d.message.into());
            ui.set_dialog_rows(model(
                d.rows.into_iter().map(|(k, v)| KeyValue { key: k.into(), value: v.into() }).collect(),
            ));
            ui.set_dialog_input(SharedString::from(d.input));
            ui.set_dialog_opt1(d.opt1);
            ui.set_dialog_opt2(d.opt2);
            ui.set_dialog_extra1(SharedString::from(d.extra1));
            ui.set_dialog_extra2(SharedString::from(d.extra2));
            ui.set_dialog_kind(d.kind);
        }
        Update::CloseDialog => {
            ui.set_preview_image(slint::Image::default());
            ui.set_preview_detail(SharedString::new());
            ui.set_dialog_kind(DialogKind::None);
            ui.set_dialog_opt1(false);
            ui.set_dialog_opt2(false);
            ui.set_dialog_extra1(SharedString::new());
            ui.set_dialog_extra2(SharedString::new());
        }
        Update::Terminal { open, cwd, output } => {
            ui.set_terminal_open(open);
            ui.set_terminal_cwd(cwd.into());
            ui.set_terminal_output(output.into());
        }
        Update::Rename { pane, index } => {
            ui.set_renaming_pane(pane);
            ui.set_renaming_index(index);
        }
        Update::CustomActions { pane, actions } => {
            if pane == 0 {
                ui.set_left_custom_actions(model(actions));
            } else {
                ui.set_right_custom_actions(model(actions));
            }
        }
        Update::PackageStatus { available, version, updating } => {
            ui.set_update_available(available);
            ui.set_update_version(version.into());
            ui.set_updating(updating);
        }
        Update::Quit => {
            let _ = slint::quit_event_loop();
        }
    }
}

impl UiSink for SlintSink {
    fn send(&self, u: Update) {
        // Queued onto the event loop; dropped silently once the window is gone.
        let _ = self.weak.upgrade_in_event_loop(move |ui| apply(&ui, u));
    }
}
