---
project: gungnir-fm
effort: E5
phase: verify
updated: 2026-10-24
---

# Gungnir FM: Ideal State Artifact

## Problem

A daily-driver Linux file manager is needed that is faster than Nemo and is the GUI
for Gungnir VFS. Work on gungnir-vfs is paused until next week, so the GUI must be
usable and testable now without blocking on unfinished daemon endpoints.

## Vision

You open a 100,000-file folder and the first screenful is there before you notice it
loading. Scrolling never stutters, and a hung network mount never freezes a window. The
same app shows Gungnir tags, mount health and daemon-driven copies the moment gungnird
is running, with no rebuild.

## Out of Scope

- Changing anything inside `../gungnir-vfs` (read-only sibling).
- Remote Gungnir nodes for real (the daemon has none yet; the mock covers them).
- Thumbnails, archive browsing, network mounts (smb/sftp), drag and drop between apps.
- Committing or pushing without the owner's explicit go-ahead.

## Principles

- The UI thread only applies prepared data; every wait, sort and format happens elsewhere.
- Match the daemon's wire format from its source, not from assumptions.
- Fail fast and say why, never hang (Gungnir's own "never hesitates" rule).

## Constraints

- Rust edition 2024, Slint 1.18, tokio, tokio-util, async-trait, notify, thiserror.
- Updates reach the UI only through `slint::invoke_from_event_loop`.
- Bounded `tokio::sync::mpsc` between UI callbacks and workers.
- No `unwrap`/`expect` outside tests (workspace lint); no em dashes in user-facing text.
- Builds and binaries never run from `/tmp` (noexec on this host).

## Goal

All five phases of the brief are delivered and verified by tool output. The workspace
builds, tests and lints clean; the app runs against the local filesystem, the mock and a
real gungnird.

## Criteria

Phase 1, repo and scaffold
- [x] ISC-1: `../gungnir-vfs` cloned. Probe: `git -C ../gungnir-vfs log -1` = `ff51079`.
- [x] ISC-2: sibling untouched. Probe: `git -C ../gungnir-vfs status --porcelain --ignored` is empty.
- [x] ISC-3: workspace has core-vfs, backends and ui crates with the required deps. Probe: `Cargo.toml`.
- [x] ISC-4: `build.rs` compiles `ui/main.slint`. Probe: `cargo build`.

Phase 2, Slint UI
- [x] ISC-5: theme, main, sidebar, pane, pathbar, statusbar and dialogs `.slint` files exist. Probe: `ls crates/ui/ui`.
- [x] ISC-6: AppWindow renders a populated dual-pane window with no Rust data. Probe: screenshot of the bare window.
- [x] ISC-7: MenuBar with File, Edit, View, Go, Gungnir, Bookmarks and Help. Probe: screenshot.
- [x] ISC-8: hybrid path bar (crumbs plus Ctrl+L edit) accepts `gungnir://` URIs. Probe: xdotool Ctrl+L to stress-100k.
- [x] ISC-9: sidebar has Places, Gungnir mounts with status dots, and a tree toggle. Probe: screenshots.
- [x] ISC-10: column header click sorts ascending then descending. Probe: screenshots s2, s4.
- [x] ISC-11: ContextMenuArea offers the listed actions on right click and selects the row. Probe: screenshot c3.
- [x] ISC-12: status bar shows items, selection size, free space and backend badge. Probe: screenshots.
- [x] ISC-13: transfer drawer shows progress, MB/s, pause and cancel. Probe: screenshots q1 to q5.
- [x] ISC-14: compiles with zero Slint warnings (no binding loops, no deprecated properties). Probe: `cargo build`.

Phase 3, VFS core and backends
- [x] ISC-15: `VfsProvider` async trait with streaming `list_dir` and `CancellationToken`. Probe: `provider.rs`.
- [x] ISC-16: URI parser handles `file://`, bare paths and `gungnir://`, with normalization and percent coding. Probe: 5 uri tests.
- [x] ISC-17: every gungnird status 0..=16 maps to a typed `VfsError`. Probe: `every_gungnir_status_maps`.
- [x] ISC-18: POSIX listing streams 200, then ≤2,000 per batch. Probe: `lists_10k_in_batches_with_hidden_files`.
- [x] ISC-19: POSIX mkdir, rename (no clobber) and delete. Probe: `mkdir_rename_delete`.
- [x] ISC-20: POSIX copy and move with progress, unique names and recursion refusal. Probe: `copy_tree_then_move_with_progress`.
- [x] ISC-21: pause, then cancel, leaves no partial file. Probe: `pause_then_cancel_leaves_no_partial_file`.
- [x] ISC-22: trash follows the freedesktop spec. Probe: `trash_follows_the_freedesktop_spec`.
- [x] ISC-23: inotify watch reports changes. Probe: `watch_reports_changes`.
- [x] ISC-24: mock streams 50,000 entries in batches. Probe: `stress_dir_streams_50k_in_batches`.
- [x] ISC-25: mock disconnected node fails in under 50 ms. Probe: `disconnected_node_fails_fast`.
- [x] ISC-26: protocol layout equals gungnird (266,240 B shm, D2C slots at 135,168, `GNGR` v2). Probe: `layout_matches_gungnird`.
- [x] ISC-27: live handshake, metadata set/get/list, tagged listing and inspect against a real gungnird. Probe: `handshake_metadata_listing_and_inspect`.
- [x] ISC-28: live daemon copy carries `metadata::*` (ALL_METADATA) and delete-tree works. Probe: `daemon_file_ops_with_progress_and_metadata`.
- [x] ISC-29: dead daemon fails in under 1 s, listing still works. Probe: `a_dead_daemon_fails_fast_and_listing_still_works`.

Phase 4, controller wiring
- [x] ISC-30: first screenful arrives alone (200 rows), then the sorted whole. Probe: `streams_a_small_first_batch_then_the_sorted_whole`.
- [x] ISC-31: navigating away pre-empts an in-flight listing, with no stale rows. Probe: `navigating_away_cancels_the_inflight_listing`.
- [x] ISC-32: Ctrl+H toggles dotfiles without a reload; sort by size flips direction. Probe: `hidden_toggle_sorting_and_range_selection`.
- [x] ISC-33: shift-click range selection is pushed as row patches. Probe: same test.
- [x] ISC-34: F2, F3, F5, F6 (cross-backend refusal), F7, F9, Ctrl+L, Ctrl+T, Ctrl+W, Ctrl+F, Ctrl+H, Ctrl+I, Tab and Alt+Left verified by real keystrokes. Probe: xdotool screenshots.
- [x] ISC-35: live run: tags shown, Inspect shows the daemon identity, F5 through gungnird keeps tags, and kill/restart flips Disconnected/Online. Probe: screenshots v3, wall.

Phase 5, verification and docs
- [x] ISC-36: `cargo test --workspace` passes (43 tests). Probe: test output.
- [x] ISC-37: `cargo clippy --workspace --all-targets -- -D warnings` passes. Probe: exit 0.
- [x] ISC-38: `cargo fmt --all --check` passes.
- [x] ISC-39: `cargo build --release` passes. Probe: `.build/release.log` exit 0.
- [x] ISC-40: ARCHITECTURE.md covers the crate layout, preview workflow and mock-to-live swap. Probe: file.
- [x] ISC-41: slint-viewer 1.18.1 renders main.slint standalone, and F3 splits it there too. Probe: docs/preview.png, docs/preview-split.png.

Nemo layout refactor (second request, 2026-10-24)
- [x] ISC-44: single pane by default, full width between the sidebar and the window edge. Probe: screenshot x1.
- [x] ISC-45: F3 splits with a 1px divider and collapses again. Probe: screenshots x3, x4.
- [x] ISC-46: no card borders, radii or insets around the panes. Probe: `pane.slint` and screenshots.
- [x] ISC-47: tab bar hidden with one tab, flat tabs with a hover close button. Probe: screenshots.
- [x] ISC-48: Home, root and node crumbs plus an edit toggle. Probe: `crumbs_start_at_home_like_nemo`.
- [x] ISC-49: solid steel-blue selection fill, no glow outline. Probe: preview screenshot.
- [x] ISC-50: sidebar mode pills removed, collapsible section headers. Probe: preview screenshot.
- [ ] ISC-51: End scrolls the last row fully into view. Partial: about 14px short (Slint clamp), see ARCHITECTURE.md Known limits.
- Anti: ISC-42: no file under `../gungnir-vfs` modified. Probe: ISC-2.
- Anti: ISC-43: nothing outside the project is committed and no personal data is published (owner-authorized commit, noreply identity). Probe: `git show --stat HEAD`, privacy grep.

## Test Strategy

Unit tests sit next to pure code (uri, error, sort, format, proto, rows). Integration
tests drive real backends: the POSIX tests use real files only inside `$TMPDIR/gungnir-test-sandbox`, and
the live tests spawn the real gungnird built from the sibling. Controller behaviour is
tested headless through a recording `UiSink`. The UI was verified by driving the real
window with xdotool and reading screenshots.

## Features

| Feature | Criteria |
|---|---|
| Core contract | ISC-15, 16, 17 |
| POSIX backend | ISC-18 to 23 |
| Mock backend | ISC-24, 25 |
| Live gungnird backend | ISC-26 to 29, 35 |
| Controller | ISC-30 to 34 |
| Slint UI | ISC-5 to 14 |

## Decisions

- 2026-10-24 refined: the brief assumed `gungnir://node/share` remote URIs. The real
  gungnird (M8) serves local paths only, so the live backend serves
  `gungnir:///<path>` and the mock keeps node URIs. One registry slot; swapping is
  `GUNGNIR_FM_BACKEND`, not code.
- 2026-10-24: wrote a native Rust client of protocol v2 instead of linking the gungnird
  crate. That avoids a hard build dependency on the sibling, rusqlite/io-uring in the
  GUI, and the AGPL linkage. Conformance is proven by the live tests instead.
- 2026-10-24: sorting produces an index permutation (`sort_order`), so 100k entries are
  never cloned or moved for a re-sort.
- 2026-10-24: every criterion is tied to a concrete tool probe (test, command or screenshot).

## Changelog

- conjecture: the mock's 100k-entry generation was cheap enough to run inline.
  refutation: the first batch took 1.22 s (debug) on a tokio worker.
  learning: generation moved to `spawn_blocking` and attributes became lazy; the 50k
  first batch now takes 133 ms in debug.
- conjecture: a header `HorizontalLayout` with width-dependent columns is fine.
  refutation: Slint reported a binding loop through the window's layout info.
  learning: header cells are positioned explicitly, with no layout constraints.

## Verification

See the final report of the 2026-10-24 session; release-build timings are recorded in
ARCHITECTURE.md once measured.
