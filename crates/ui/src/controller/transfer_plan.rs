//! File collision preflight and per-file decisions for copy and move requests.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use core_vfs::{Scheme, TransferKind, VfsUri, format};
use tokio::task::JoinError;

use super::{Controller, Event, Msg, PendingDialog};
use crate::DialogKind;
use crate::bridge::{DialogView, Update};

#[derive(Debug)]
struct TransferIntent {
    kind: TransferKind,
    sources: Vec<VfsUri>,
    destination: VfsUri,
}

#[derive(Debug)]
struct Conflict {
    source_index: usize,
    src_size: u64,
    dst_size: u64,
    src_mtime: Option<SystemTime>,
    dst_mtime: Option<SystemTime>,
}

impl Conflict {
    fn source_is_newer(&self) -> bool {
        matches!((self.src_mtime, self.dst_mtime), (Some(src), Some(dst)) if src > dst)
    }
}

#[derive(Clone, Copy, Debug)]
enum Decision {
    KeepBoth,
    Replace,
    Skip,
}

#[derive(Clone, Copy)]
enum Action {
    Replace,
    ReplaceIfNewer,
    AutoRename,
    Skip,
}

impl Action {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "replace" => Some(Self::Replace),
            "replace-if-newer" => Some(Self::ReplaceIfNewer),
            "auto-rename" => Some(Self::AutoRename),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }

    fn decide(self, conflict: &Conflict) -> Decision {
        match self {
            Self::Replace => Decision::Replace,
            Self::ReplaceIfNewer if conflict.source_is_newer() => Decision::Replace,
            Self::AutoRename => Decision::KeepBoth,
            Self::ReplaceIfNewer | Self::Skip => Decision::Skip,
        }
    }
}

#[derive(Debug)]
pub(crate) struct TransferPlan {
    intent: TransferIntent,
    conflicts: Vec<Conflict>,
    current_conflict: usize,
    decisions: Vec<Decision>,
}

struct TransferBatches {
    kind: TransferKind,
    destination: VfsUri,
    keep_both: Vec<VfsUri>,
    replace: Vec<VfsUri>,
}

fn collision_path(uri: &VfsUri) -> Option<PathBuf> {
    match uri.scheme() {
        Scheme::File => uri.to_local_path(),
        Scheme::Gungnir if uri.authority().is_empty() => Some(PathBuf::from(uri.path())),
        _ => None,
    }
}

impl TransferPlan {
    fn probe(intent: TransferIntent) -> Self {
        let mut conflicts = Vec::new();
        if let Some(directory) = collision_path(&intent.destination) {
            for (source_index, source) in intent.sources.iter().enumerate() {
                let Some(name) = source.file_name() else { continue };
                let Ok(destination) = directory.join(name).symlink_metadata() else { continue };
                let source = collision_path(source).and_then(|p| p.symlink_metadata().ok());
                conflicts.push(Conflict {
                    source_index,
                    src_size: source.as_ref().map_or(0, std::fs::Metadata::len),
                    dst_size: destination.len(),
                    src_mtime: source.and_then(|m| m.modified().ok()),
                    dst_mtime: destination.modified().ok(),
                });
            }
        }
        let decisions = vec![Decision::KeepBoth; intent.sources.len()];
        Self { intent, conflicts, current_conflict: 0, decisions }
    }

    fn current(&self) -> Option<&Conflict> {
        self.conflicts.get(self.current_conflict)
    }

    fn resolve(&mut self, action: Action, apply_all: bool) {
        let end = if apply_all { self.conflicts.len() } else { self.current_conflict + 1 };
        for conflict in &self.conflicts[self.current_conflict..end] {
            self.decisions[conflict.source_index] = action.decide(conflict);
        }
        self.current_conflict = end;
    }

    fn into_batches(self) -> TransferBatches {
        let mut keep_both = Vec::new();
        let mut replace = Vec::new();
        for (source, decision) in self.intent.sources.into_iter().zip(self.decisions) {
            match decision {
                Decision::KeepBoth => keep_both.push(source),
                Decision::Replace => replace.push(source),
                Decision::Skip => {}
            }
        }
        TransferBatches { kind: self.intent.kind, destination: self.intent.destination, keep_both, replace }
    }
}

enum Preflight {
    Running,
    Ready(Result<TransferPlan, JoinError>),
}

#[derive(Default)]
pub(super) struct TransferPlanning {
    queued: VecDeque<TransferIntent>,
    preflight: Option<Preflight>,
}

impl TransferPlanning {
    pub(super) fn complete(&mut self, result: Result<TransferPlan, JoinError>) {
        self.preflight = Some(Preflight::Ready(result));
    }
}

impl Controller {
    /// Capture the selected items and destination before probing on a worker.
    pub(super) fn start_transfer(&mut self, kind: TransferKind, sources: Vec<VfsUri>, destination: VfsUri) {
        if sources.is_empty() {
            return;
        }
        if sources.iter().any(|source| destination.starts_with(source)) {
            self.say("A folder cannot be copied or moved into itself", true);
            return;
        }
        if kind == TransferKind::Move && sources.iter().all(|source| source.parent().as_ref() == Some(&destination)) {
            return;
        }
        self.transfer_planning.queued.push_back(TransferIntent { kind, sources, destination });
    }

    /// Advance transfer requests in arrival order. Give an open dialog time to finish
    /// before presenting the next conflict.
    pub(super) fn advance_transfer_planning(&mut self) {
        if self.dialog.is_some() || matches!(&self.transfer_planning.preflight, Some(Preflight::Running)) {
            return;
        }
        if let Some(Preflight::Ready(result)) = self.transfer_planning.preflight.take() {
            match result {
                Ok(plan) if plan.current().is_some() => {
                    self.dialog = Some(PendingDialog::Conflict(plan));
                    self.show_conflict();
                    return;
                }
                Ok(plan) => self.dispatch_plan(plan),
                Err(error) => self.say(format!("Could not check for file conflicts: {error}"), true),
            }
        }
        if let Some(intent) = self.transfer_planning.queued.pop_front() {
            self.transfer_planning.preflight = Some(Preflight::Running);
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || TransferPlan::probe(intent)).await;
                let _ = tx.send(Msg::Ev(Event::TransferPlanned(result))).await;
            });
        }
    }

    fn show_conflict(&mut self) {
        let Some(PendingDialog::Conflict(plan)) = &self.dialog else { return };
        let Some(c) = plan.current() else { return };
        let total = plan.conflicts.len();
        let idx_1based = plan.current_conflict + 1;
        let source = &plan.intent.sources[c.source_index];
        let filename = source.file_name().unwrap_or_default();
        let format_time = |t: Option<SystemTime>| {
            t.and_then(|st| st.duration_since(UNIX_EPOCH).ok())
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
            if c.source_is_newer() { "[NEWER]" } else { "" }
        );
        let dst_mod = format!(
            "{} · Modified: {} {}",
            format::size(c.dst_size),
            format_time(c.dst_mtime),
            if !c.source_is_newer() { "[NEWER/SAME]" } else { "" }
        );
        let rows = vec![
            ("File name".to_string(), filename.to_owned()),
            ("Source file".to_string(), src_mod),
            ("Existing file".to_string(), dst_mod),
            ("Conflict".to_string(), format!("{idx_1based} of {total} collisions")),
        ];
        self.ui.send(Update::Dialog(DialogView {
            kind: DialogKind::Conflict,
            title: format!("File Collision: {filename}"),
            message: format!("A file named '{filename}' already exists in destination. Choose an action:"),
            rows,
            input: String::new(),
            preview_path: source.to_local_path(),
            opt1: false,
            ..Default::default()
        }));
    }

    pub(super) fn handle_conflict_action(&mut self, action: &str, apply_all: bool) {
        let Some(action) = Action::parse(action) else { return };
        let Some(PendingDialog::Conflict(plan)) = &mut self.dialog else { return };
        plan.resolve(action, apply_all);
        if plan.current().is_some() {
            self.show_conflict();
        } else if let Some(PendingDialog::Conflict(plan)) = self.dialog.take() {
            self.ui.send(Update::CloseDialog);
            self.dispatch_plan(plan);
        }
    }

    fn dispatch_plan(&mut self, plan: TransferPlan) {
        let TransferBatches { kind, destination, keep_both, replace } = plan.into_batches();
        if keep_both.is_empty() && replace.is_empty() {
            self.say("Transfer skipped (all conflicting files skipped)", false);
            return;
        }

        // Overwrite applies to the whole provider request, so submit the two groups separately.
        self.execute_transfer(kind, keep_both, destination.clone(), false);
        self.execute_transfer(kind, replace, destination, true);
    }
}
