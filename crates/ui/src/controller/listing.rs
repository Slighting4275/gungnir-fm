//! Per-tab directory work and the view published from its current listing.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use core_vfs::sort::sort_order;
use core_vfs::{
    BackendKind, CancellationToken, FileEntry, ListOptions, ListSummary, SortKey, VfsError, VfsResult, VfsUri,
    VolumeInfo, WatchEvent, format,
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::{Controller, Event, Msg, RenameEdit, WATCH_DEBOUNCE};
use crate::bridge::Update;
use crate::rows::{self, RowItem};

/// The tab and mapping currently published to a pane's row model.
#[derive(Clone, Copy, Default)]
pub(super) struct PaneRows {
    pub(super) tab: Option<u64>,
    pub(super) revision: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StreamView {
    show_hidden: bool,
    filter: String,
    sort: SortKey,
}

#[derive(Debug)]
pub(crate) struct ListingBatch {
    entries: Vec<FileEntry>,
    visible: Vec<u32>,
    rows: Vec<RowItem>,
    view: StreamView,
}

impl ListingBatch {
    fn prepare(mut entries: Vec<FileEntry>, view: StreamView, first: bool) -> Self {
        if first {
            core_vfs::sort::sort_entries(&mut entries, view.sort);
        }
        let order = (0..u32::try_from(entries.len()).unwrap_or(u32::MAX)).collect::<Vec<_>>();
        let visible = rows::visible(&entries, &order, view.show_hidden, &view.filter);
        let rows = rows::rows(&entries, &visible, &HashSet::new());
        Self { entries, visible, rows, view }
    }
}

struct FolderWatch {
    task: JoinHandle<()>,
    registered: watch::Receiver<bool>,
}

pub(super) struct Tab {
    pub(super) id: u64,
    pub(super) location: VfsUri,
    pub(super) back: Vec<VfsUri>,
    pub(super) forward: Vec<VfsUri>,
    /// Arrival order; never reordered (sorting produces `order`).
    entries: Arc<Vec<FileEntry>>,
    /// Sorted permutation of `entries` (arrival order while streaming).
    order: Vec<u32>,
    /// Shown entries, display order (subset of `order`).
    visible: Vec<u32>,
    /// A silent refresh waiting for a display order. The old view remains usable.
    refreshed: Option<Arc<Vec<FileEntry>>>,
    stream_view: Option<watch::Sender<StreamView>>,
    pending_view: Option<u64>,
    pub(super) selected: HashSet<String>,
    pub(super) cursor: usize,
    pub(super) anchor: Option<usize>,
    pub(super) sort: SortKey,
    pub(super) filter: String,
    pub(super) loading: bool,
    pub(super) error: String,
    load_gen: u64,
    view_gen: u64,
    cancel: CancellationToken,
    watch: Option<FolderWatch>,
    watch_gen: u64,
    watch_dirty: bool,
    pub(super) volume: Option<VolumeInfo>,
    pub(super) backend: Option<BackendKind>,
    /// Reselect this name after the next load.
    pub(super) pending_select: Option<String>,
    pub(super) pending_rename: Option<String>,
}

impl Tab {
    pub(super) fn new(id: u64, location: VfsUri) -> Self {
        Self {
            id,
            location,
            back: Vec::new(),
            forward: Vec::new(),
            entries: Arc::new(Vec::new()),
            order: Vec::new(),
            visible: Vec::new(),
            refreshed: None,
            stream_view: None,
            pending_view: None,
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
            watch_gen: 0,
            watch_dirty: false,
            volume: None,
            backend: None,
            pending_select: None,
            pending_rename: None,
        }
    }

    pub(super) fn title(&self, home: &VfsUri) -> String {
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

    pub(super) fn entry_at(&self, visible_index: usize) -> Option<&FileEntry> {
        self.visible.get(visible_index).and_then(|&i| self.entries.get(i as usize))
    }

    pub(super) fn entries(&self) -> &[FileEntry] {
        &self.entries
    }

    pub(super) fn visible_len(&self) -> usize {
        self.visible.len()
    }

    pub(super) fn visible_entries(&self) -> impl Iterator<Item = &FileEntry> {
        self.visible.iter().filter_map(|&i| self.entries.get(i as usize))
    }

    /// The selection as entries, or the cursor row when nothing is selected.
    pub(super) fn targets(&self) -> Vec<FileEntry> {
        let sel: Vec<FileEntry> = self.visible_entries().filter(|e| self.selected.contains(&e.name)).cloned().collect();
        if sel.is_empty() { self.entry_at(self.cursor).cloned().into_iter().collect() } else { sel }
    }

    pub(super) fn needs_load(&self) -> bool {
        self.load_gen == 0
    }

    fn stream_view(&self, show_hidden: bool) -> StreamView {
        StreamView { show_hidden, filter: self.filter.clone(), sort: self.sort }
    }

    fn stop_watch(&mut self) {
        self.watch_gen += 1;
        if let Some(watch) = self.watch.take() {
            watch.task.abort();
        }
    }

    pub(super) fn stop_listing(&mut self) {
        self.cancel.cancel();
        if !self.needs_load() {
            self.load_gen += 1;
        }
        self.view_gen += 1;
        self.loading = false;
        self.refreshed = None;
        self.stream_view = None;
        self.pending_view = None;
        self.stop_watch();
    }

    fn clear_entries(&mut self) {
        self.entries = Arc::new(Vec::new());
        self.order.clear();
        self.visible.clear();
    }

    fn commit_view(&mut self, order: Option<Vec<u32>>, visible: Vec<u32>) {
        let cursor_entry = self.pending_select.take().or_else(|| self.entry_at(self.cursor).map(|e| e.name.clone()));
        let anchor_entry = self.anchor.and_then(|row| self.entry_at(row)).map(|e| e.name.clone());
        if let Some(entries) = self.refreshed.take() {
            self.entries = entries;
        }
        if let Some(order) = order {
            self.order = order;
        }
        self.visible = visible;
        let names: HashSet<&str> =
            self.visible.iter().filter_map(|&i| self.entries.get(i as usize)).map(|e| e.name.as_str()).collect();
        self.selected.retain(|name| names.contains(name.as_str()));
        let row_of = |name: &str| self.visible_entries().position(|e| e.name == name);
        let anchor = anchor_entry.as_deref().and_then(row_of);
        let cursor = cursor_entry.as_deref().and_then(row_of);
        self.anchor = anchor;
        self.cursor = cursor.unwrap_or(self.cursor).min(self.visible.len().saturating_sub(1));
    }
}

impl Drop for Tab {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.stop_watch();
    }
}

impl Controller {
    pub(super) fn publish_rows(&mut self, pane: usize, tab: Option<u64>, rows: Vec<RowItem>) {
        self.next_rows_revision = self.next_rows_revision.wrapping_add(1);
        let revision = self.next_rows_revision;
        self.pane_rows[pane] = PaneRows { tab, revision };
        self.ui.send(Update::Rows { pane, revision, rows });
    }

    /// Tab IDs survive promotion of the right pane. The pane index captured by a
    /// worker may refer to a different tab by the time its result arrives.
    fn tab_position(&self, id: u64) -> Option<(usize, usize)> {
        self.panes
            .iter()
            .enumerate()
            .find_map(|(pane, p)| p.tabs.iter().position(|t| t.id == id).map(|index| (pane, index)))
    }

    pub(super) fn tab_index(&self, pane: usize, id: u64) -> Option<usize> {
        self.panes[pane].tabs.iter().position(|t| t.id == id)
    }

    fn is_active_tab(&self, pane: usize, id: u64) -> bool {
        self.panes[pane].tab().id == id
    }

    /// Load `location` into the active tab of `pane`. `silent` keeps the current rows
    /// on screen until the new listing is complete (reloads after changes).
    pub(super) fn load(&mut self, pane: usize, location: VfsUri, silent: bool) {
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
        let show_hidden = self.show_hidden;
        let tx = self.tx.clone();
        let t = self.panes[pane].tab_mut();
        t.cancel.cancel(); // pre-empt whatever was loading
        t.cancel = CancellationToken::new();
        t.load_gen += 1;
        t.view_gen += 1;
        t.refreshed = None;
        t.pending_view = None;
        let (stream_view, view) = watch::channel(t.stream_view(show_hidden));
        t.stream_view = Some(stream_view);
        let same_place = t.location == location;
        t.location = location.clone();
        t.loading = true;
        t.watch_dirty = false;
        t.error.clear();
        t.backend = Some(provider.kind());
        if !same_place {
            t.pending_rename = None;
            t.stop_watch();
            t.volume = None;
        }
        if !silent {
            t.clear_entries();
            if !same_place {
                t.selected.clear();
                t.cursor = 0;
                t.anchor = None;
            }
        }
        let (tab, generation, cancel) = (t.id, t.load_gen, t.cancel.clone());
        if !silent && self.is_active_tab(pane, tab) {
            self.publish_rows(pane, Some(tab), Vec::new());
        }
        self.push_pane(pane);
        self.push_status();

        let watcher = self.start_watch(pane, tab);
        let started = Instant::now();
        tokio::spawn(async move {
            // Register before enumerating, so a change between scanning and
            // publishing the rows always has a watcher to report it.
            if let Some(mut registered) = watcher {
                tokio::select! {
                    () = cancel.cancelled() => return,
                    _ = registered.wait_for(|ready| *ready) => {}
                }
            }
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
                    }
                    match collected.as_mut() {
                        Some(c) => c.extend(batch),
                        None => {
                            let view = view.borrow().clone();
                            let batch =
                                match tokio::task::spawn_blocking(move || ListingBatch::prepare(batch, view, first))
                                    .await
                                {
                                    Ok(batch) => batch,
                                    Err(error) => {
                                        cancel.cancel();
                                        return Err(VfsError::Transport(format!("listing worker: {error}")));
                                    }
                                };
                            if tx.send(Msg::Ev(Event::Batch { tab, generation, batch })).await.is_err() {
                                return Err(VfsError::Cancelled);
                            }
                        }
                    }
                    first = false;
                }
                Ok(collected)
            };
            let (result, collected) = tokio::join!(list, forward);
            let (result, collected) = match collected {
                Ok(collected) => (result, collected),
                Err(error) => (Err(error), None),
            };
            let _ = tx.send(Msg::Ev(Event::ListDone { tab, generation, result, collected, started })).await;
        });
    }

    pub(super) fn on_batch(&mut self, tab: u64, generation: u64, batch: ListingBatch) {
        let Some((pane, ti)) = self.tab_position(tab) else { return };
        let active = self.is_active_tab(pane, tab);
        let t = &mut self.panes[pane].tabs[ti];
        if t.load_gen != generation {
            return;
        }
        // A prepared full view for a shorter prefix must be refreshed.
        t.view_gen += 1;
        let start = u32::try_from(t.entries.len()).unwrap_or(u32::MAX);
        let ListingBatch { entries, visible, mut rows, view } = batch;
        Arc::make_mut(&mut t.entries).extend(entries);
        t.order.extend(start..u32::try_from(t.entries.len()).unwrap_or(u32::MAX));

        if t.stream_view(self.show_hidden) == view {
            t.visible.extend(visible.into_iter().map(|relative| start.saturating_add(relative)));
            if active && self.pane_rows[pane].tab == Some(tab) && !rows.is_empty() {
                for row in &mut rows {
                    row.selected = t.selected.contains(&row.name);
                }
                self.ui.send(Update::AppendRows { pane, rows });
            }
        } else if t.pending_view.is_none() {
            self.prepare(pane, tab, true);
        }
        if active && pane == self.active_pane {
            self.push_status();
        }
    }

    pub(super) fn on_list_done(
        &mut self,
        tab: u64,
        generation: u64,
        result: VfsResult<ListSummary>,
        collected: Option<Vec<FileEntry>>,
        started: Instant,
    ) {
        let Some((pane, ti)) = self.tab_position(tab) else { return };
        let t = &mut self.panes[pane].tabs[ti];
        if t.load_gen != generation {
            return;
        }
        t.loading = false;
        t.stream_view = None;
        match result {
            Err(VfsError::Cancelled) => return,
            Err(e) => {
                t.error = e.to_string();
                if collected.is_some() {
                    // A failed silent reload: the folder is gone or unreadable now.
                    t.clear_entries();
                    t.selected.clear();
                    t.cursor = 0;
                    t.anchor = None;
                    t.view_gen += 1;
                    t.pending_view = None;
                    if self.is_active_tab(pane, tab) {
                        self.publish_rows(pane, Some(tab), Vec::new());
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
        if std::mem::take(&mut t.watch_dirty) && self.panes[pane].active == ti {
            let location = self.panes[pane].tabs[ti].location.clone();
            self.load(pane, location, true);
            return;
        }
        t.refreshed = collected.map(Arc::new);
        self.prepare(pane, tab, true);
        let (provider, loc) =
            (self.registry.resolve(&self.panes[pane].tabs[ti].location), self.panes[pane].tabs[ti].location.clone());
        if let Ok(provider) = provider {
            let tx = self.tx.clone();
            tokio::spawn(async move {
                if let Ok(info) = provider.volume(&loc).await {
                    let _ = tx.send(Msg::Ev(Event::Volume { tab, generation, info })).await;
                }
            });
        }
        if self.sidebar && self.sidebar_mode == 1 && pane == self.active_pane {
            self.rebuild_tree();
        }
    }

    /// Recompute order (if `resort`), visible rows and row data on a blocking thread.
    pub(super) fn prepare(&mut self, pane: usize, tab: u64, resort: bool) {
        let Some(ti) = self.tab_index(pane, tab) else { return };
        let show_hidden = self.show_hidden;
        let t = &mut self.panes[pane].tabs[ti];
        t.view_gen += 1;
        t.pending_view = Some(t.view_gen);
        if let Some(sender) = &t.stream_view {
            sender.send_replace(t.stream_view(show_hidden));
        }
        let resort = resort || t.refreshed.is_some();
        let (view_gen, entries, sort, filter, selected) = (
            t.view_gen,
            Arc::clone(t.refreshed.as_ref().unwrap_or(&t.entries)),
            t.sort,
            t.filter.clone(),
            t.selected.clone(),
        );
        let order = (!resort).then(|| t.order.clone());
        let build_rows = self.is_active_tab(pane, tab);
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let order = order.unwrap_or_else(|| sort_order(&entries, sort));
            let visible = rows::visible(&entries, &order, show_hidden, &filter);
            let rows = if build_rows { rows::rows(&entries, &visible, &selected) } else { Vec::new() };
            let _ = tx.blocking_send(Msg::Ev(Event::Prepared {
                tab,
                view_gen,
                order: resort.then_some(order),
                visible,
                rows,
            }));
        });
    }

    pub(super) fn on_prepared(
        &mut self,
        tab: u64,
        view_gen: u64,
        order: Option<Vec<u32>>,
        visible: Vec<u32>,
        mut rows: Vec<RowItem>,
    ) {
        let Some((pane, ti)) = self.tab_position(tab) else { return };
        let active = self.is_active_tab(pane, tab);
        let t = &mut self.panes[pane].tabs[ti];
        if t.pending_view != Some(view_gen) {
            return;
        }
        t.pending_view = None;
        if t.view_gen != view_gen {
            self.prepare(pane, tab, true);
            return;
        }
        t.commit_view(order, visible);
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
            self.publish_rows(pane, Some(tab), rows);
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
        let view_gen = t.view_gen;
        let entries = Arc::clone(&t.entries);
        let visible = t.visible.clone();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let candidates = visible.into_iter().filter_map(|idx| {
                let entry = entries.get(idx as usize)?;
                let category = format::category(entry);
                let extension = format::extension(&entry.name);
                if !crate::thumbnails::is_previewable(category, extension.as_deref()) {
                    return None;
                }
                let path = entry.uri.to_local_path()?;
                let mtime = entry
                    .modified
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .and_then(|duration| i64::try_from(duration.as_secs()).ok())
                    .unwrap_or(0);
                let uri = entry.uri.to_uri_string();
                if crate::thumbnails::lookup_cached(&uri, mtime).is_some() {
                    return None;
                }
                let video = category == core_vfs::format::Category::Video
                    || crate::thumbnails::is_video_ext(extension.as_deref());
                Some((&entry.name, path, uri, mtime, video))
            });
            let mut batch = Vec::new();
            for (name, path, uri, mtime, is_vid) in candidates {
                if let Ok(thumb) = crate::thumbnails::generate_thumbnail(&path, &uri, mtime, is_vid) {
                    batch.push((name.clone(), thumb));
                    if batch.len() >= 16 {
                        let _ = tx.blocking_send(Msg::Ev(Event::ThumbnailsReady {
                            tab,
                            view_gen,
                            items: std::mem::take(&mut batch),
                        }));
                    }
                }
            }
            if !batch.is_empty() {
                let _ = tx.blocking_send(Msg::Ev(Event::ThumbnailsReady { tab, view_gen, items: batch }));
            }
        });
    }

    fn start_watch(&mut self, pane: usize, tab: u64) -> Option<watch::Receiver<bool>> {
        let ti = self.tab_index(pane, tab)?;
        let t = &mut self.panes[pane].tabs[ti];
        if let Some(watch) = &t.watch
            && !watch.task.is_finished()
        {
            return Some(watch.registered.clone());
        }
        let location = t.location.clone();
        let provider = self.registry.resolve(&location).ok()?;
        let tx = self.tx.clone();
        let watch_gen = t.watch_gen;
        let (ready, registered) = watch::channel(false);
        let task = tokio::spawn(async move {
            let watch = tokio::task::spawn_blocking(move || provider.watch(&location)).await;
            ready.send_replace(true);
            let Ok(Ok(Some(mut watch))) = watch else { return };
            let _guard = watch.guard;
            while let Some(ev) = watch.events.recv().await {
                let mut removed = ev == WatchEvent::Removed;
                // Debounce bursts (an extraction writing 10k files is one reload).
                tokio::time::sleep(WATCH_DEBOUNCE).await;
                while let Ok(ev) = watch.events.try_recv() {
                    removed |= ev == WatchEvent::Removed;
                }
                if tx.send(Msg::Ev(Event::DirChanged { tab, watch_gen, removed })).await.is_err() {
                    return;
                }
            }
        });
        t.watch = Some(FolderWatch { task, registered: registered.clone() });
        Some(registered)
    }

    pub(super) fn on_dir_changed(&mut self, tab: u64, watch_gen: u64, removed: bool) {
        let Some((pane, index)) = self.tab_position(tab) else { return };
        let t = &mut self.panes[pane].tabs[index];
        if t.watch_gen != watch_gen {
            return;
        }
        if removed {
            t.stop_watch();
        }
        if t.loading {
            t.watch_dirty = true;
        } else if index == self.panes[pane].active {
            let location = self.panes[pane].tab().location.clone();
            self.load(pane, location, true);
        }
    }

    pub(super) fn on_volume(&mut self, tab: u64, generation: u64, info: VolumeInfo) {
        let Some((pane, index)) = self.tab_position(tab) else { return };
        let t = &mut self.panes[pane].tabs[index];
        if t.load_gen == generation {
            t.volume = Some(info);
            if self.active_pane == pane && self.panes[pane].active == index {
                self.push_status();
            }
        }
    }

    pub(super) fn on_thumbnails(&self, tab: u64, view_gen: u64, items: Vec<(String, PathBuf)>) {
        let Some((pane, index)) = self.tab_position(tab) else { return };
        if self.panes[pane].active == index && self.panes[pane].tabs[index].view_gen == view_gen && !items.is_empty() {
            self.ui.send(Update::ThumbnailsReady { pane, items });
        }
    }
}
