# Gungnir FM architecture

Gungnir FM is a dual-pane Linux file manager written in Rust with the Slint UI toolkit
(1.18). It is the companion GUI of [Gungnir VFS](https://gitlab.com/exalted6165/gungnir-vfs).
It takes its layout and ergonomics from Nemo and uses a dark slate theme. The UI thread never
waits on disk, IPC, sorting or filtering.

## Workspace layout

```
gungnir-fm/
├── Cargo.toml                 workspace (edition 2024, shared lints: no unwrap/expect)
├── crates/
│   ├── core-vfs/              the contract: no I/O, no UI
│   │   └── src/
│   │       ├── provider.rs    VfsProvider async trait, VfsRegistry (scheme router)
│   │       ├── model.rs       FileEntry, VfsMetadata, VfsStatus, VfsCapabilities,
│   │       │                  TransferRequest/Progress/State, MountInfo, VolumeInfo
│   │       ├── uri.rs         VfsUri: file:// and gungnir:// (normalized, percent-coded)
│   │       ├── error.rs       VfsError + gungnird status codes 0..=16 mapped 1:1
│   │       ├── sort.rs        natural, folders-first ordering (sort_order = index permutation)
│   │       └── format.rs      sizes, ls-style modes, type labels, icon categories
│   ├── backends/
│   │   ├── build.rs           links native libarchive
│   │   ├── src/archive.rs     lossless compression & extraction engine (3-tier hybrid pipeline)
│   │   ├── src/local.rs       LocalPosixBackend (daily driver)
│   │   ├── src/mock.rs        MockGungnirBackend (synthetic gungnir:// nodes, stress dirs)
│   │   ├── src/remote.rs      RemoteVfsBackend (built-in sftp:// and smb://)
│   │   ├── src/gungnir/       GungnirVfsBackend (live gungnird client)
│   │   │   ├── proto.rs       protocol v2 wire format, mirrored from ../gungnir-vfs
│   │   │   ├── link.rs        the gungnir-link thread: ring I/O, seq routing, health
│   │   │   └── mod.rs         the VfsProvider on top of the link
│   │   ├── examples/gungnir_tag.rs   set/list metadata::* through the real protocol
│   │   └── tests/             local_posix.rs, gungnir_live.rs (spawns a real gungnird)
│   └── ui/                    binary `gungnir-fm`
│       ├── build.rs           compiles ui/main.slint (style fluent-dark)
│       ├── ui/                *.slint, every file previewable on its own
│       │   ├── main.slint     AppWindow: MenuBar, toolbar, panes, drawer, dialogs
│       │   ├── theme.slint    Theme global (colours, metrics, status colour functions)
│       │   ├── types.slint    structs shared with Rust (FileRow, PaneInfo, ...)
│       │   ├── widgets.slint  vector icons (no image assets), buttons, badges, bars
│       │   ├── pathbar.slint  hybrid breadcrumb / editable location bar
│       │   ├── sidebar.slint  Places, Gungnir mounts with status, directory tree
│       │   ├── pane.slint     tabs, sortable header, virtualized list / compact / grid
│       │   ├── statusbar.slint status bar and transfer drawer
│       │   └── dialogs.slint  in-window dialogs (new folder, delete, properties, inspect, hash, preview)
│       └── src/
│           ├── main.rs        runtime, backend selection, callback wiring
│           ├── controller.rs  the controller actor (all application state)
│           ├── bridge.rs      Update enum + the only code that touches models
│           ├── rows.rs        FileEntry -> FileRow, filtering
│           ├── actions.rs     custom actions engine (Nemo actions equivalent)
│           ├── session.rs     atomic crash-resilient session persistence
│           ├── thumbnails.rs  async image and video thumbnail generator and cache
│           ├── hash.rs        streaming multi-hash computation and checksum verification
│           ├── drives.rs      removable storage and partitions via udisks2 over D-Bus
│           ├── desktop.rs     freedesktop .desktop application launcher
│           └── controller_tests.rs  headless tests through a recording UiSink
└── scripts/build-gungnird.sh  builds gungnird from ../gungnir-vfs into ./.build
```

## Threads and the non-blocking rules

```
 Slint event loop (main thread)          tokio runtime (4 workers + blocking pool)
 ─────────────────────────────           ─────────────────────────────────────────
 callbacks ── try_send(Msg::Ui) ──────▶  Controller actor (one task, never blocks)
                bounded mpsc(4096)            │  spawns: list tasks, ops, transfers,
                                              │          watchers, mount poller, ticker
                                              │  spawn_blocking: sort, filter, format rows
 apply(Update) ◀── invoke_from_event_loop ────┘
 (set_vec / extend / set_row_data)       gungnir-link thread (futex waits, bounded)
```

1. **The UI thread does no I/O, IPC, sorting or filtering.** Callbacks only `try_send`
   a `Command` (a full queue drops it instead of stalling). The window is changed only in
   `bridge::apply`, which runs on the event loop through
   `slint::Weak::upgrade_in_event_loop` (`invoke_from_event_loop`). Every `Update` holds
   plain `Send` data built on worker threads, so the UI thread only moves vectors into
   models.
2. **The controller is an actor.** It owns panes, tabs, selection and transfers. It
   never awaits slow work: listings, file operations, transfers, inspection and the tree
   run as spawned tasks that report back as `Event`s on the same bounded channel.
3. **CPU work runs on the blocking pool.** Sorting (`core_vfs::sort::sort_order`, an index
   permutation, so 100k entries are never moved), filtering and building `FileRow`s all run
   in `spawn_blocking`.
4. **Every load can be pre-empted.** Each tab has a `CancellationToken` and two
   generation counters. Navigating cancels the in-flight listing at once (the backend
   stops reading), and late batches or re-views from older generations are dropped.
   `controller_tests::navigating_away_cancels_the_inflight_listing` checks this.
5. **Listings stream.** Backends send a first batch of 200 entries, sorted on arrival so
   the first screenful is in order, then batches of 2,000. Rows are appended as they come.
   When the listing completes, the whole folder is sorted and swapped in with one
   `set_vec`. Reloads caused by file-watch events or finished operations are silent: the
   old rows stay on screen until the new listing is complete.
6. **Rendering is virtualized.** Every view (detailed, compact, grid) is a Slint
   `ListView`, so only visible rows are instantiated. The grid view is a `ListView` over
   rows of N cells.
7. **gungnird's blocking primitives get their own thread.** The shared-memory ring uses
   futexes, so a dedicated `gungnir-link` thread owns it, sleeps in bounded 25 ms slices,
   and is woken by a no-op `FUTEX_WAKE` when work arrives. Async callers only await
   `oneshot`s.

## The VfsProvider contract (`core-vfs`)

```rust
#[async_trait]
pub trait VfsProvider: Send + Sync + 'static {
    fn kind(&self) -> BackendKind;                 // status-bar badge
    fn capabilities(&self) -> VfsCapabilities;    // watch, trash, pause, metadata, health
    fn handles(&self, uri: &VfsUri) -> bool;
    fn can_transfer(&self, src: &VfsUri, dest: &VfsUri) -> bool;
    async fn list_dir(&self, dir, ListOptions, EntrySink, CancellationToken) -> VfsResult<ListSummary>;
    async fn stat / create_dir / rename / delete / trash / transfer / volume / mounts / inspect;
    fn watch(&self, dir) -> VfsResult<Option<DirWatch>>;
}
```

`VfsRegistry::resolve(uri)` returns the first provider that handles a URI. Hidden
entries are always listed; the controller filters them, so Ctrl+H toggles without a
reload. Errors are `VfsError`; variants from `NotFound` to `WouldRecurse` correspond 1:1
to gungnird reply statuses (`VfsError::from_gungnir_status`).

## Backends

### LocalPosixBackend (`file://`)

- **Listing.** `readdir` on one blocking task, with chunks handed to 4 scoped threads
  that `fstatat` in parallel. The first 200 entries go out before the rest is even read.
- **Watching.** `notify` (inotify) on the open folder, debounced 300 ms by the controller.
- **Operations.** `mkdir`, rename via `renameat2(RENAME_NOREPLACE)` (never clobbers),
  delete (`remove_dir_all` does not follow symlinks), and trash per the freedesktop.org
  Trash spec (`.trashinfo` reserved with `O_EXCL`, cross-device refused with a hint).
- **Transfers.** Copy or move with 1 MiB chunks. Progress is reported every 100 ms with
  `try_send` (a busy UI never stalls a copy). Pause and cancel are checked between chunks.
  A cancelled copy removes its partial file. Modes and times are kept, symlinks are
  recreated, moves use `rename` and fall back to copy and delete across devices, a copy
  into itself is refused, and name clashes get `name (copy).ext`.

### MockGungnirBackend (`gungnir://<node>/<share>/...`)

- **Nodes.** `odin` (Online) has `asgard` and `valhalla`; `huginn` (Syncing) has
  `memory`; `muninn` (Disconnected) has `thought`, and every request there fails fast
  with `SuspectMount`, the same way gungnird answers for a hung mount.
- **Stress directories.** `gungnir://odin/asgard/stress-50k` (50,000 entries) and
  `gungnir://odin/valhalla/stress-100k` (100,000 entries) are generated on first open,
  off the async workers.
- **Simulation.** IPC latency (12 ms), batch latency (2 ms), transfers at 350 MB/s with
  pause and cancel, every VFS status (synced, syncing, cached, remote, conflict, tagged)
  and synthetic `gungnir::*` attributes for Inspect.

### GungnirVfsBackend (live gungnird)

What gungnird is today, from `../gungnir-vfs` at milestone 8 (commit `ff51079`): a
replacement for GVfs that runs on this machine and serves local paths. It does **not**
have remote nodes, shares or a directory-listing endpoint yet. So the live backend
serves `gungnir:///<absolute path>` (an empty node or `localhost`), lists through the
local filesystem, and adds what the daemon really provides:

| Feature | gungnird protocol (mirrored in `backends/src/gungnir/proto.rs`) | Source in `../gungnir-vfs/src/daemon-rust/src` |
|---|---|---|
| Handshake | `AF_UNIX` socket, 16-byte frames `GNGR v2`, memfd via `SCM_RIGHTS`, `SO_PEERCRED` same uid | `ipc.rs` |
| Ring transport | 266,240-byte sealed memfd, 2 × 32 slots × 4096 B, process-shared futex wake/wait | `ipc.rs` |
| `metadata::*` | identify 16, get 17, set 18, unset 19, list 20, removed 21, moved 22, reply 32 | `md.rs` |
| Mount health | query 23, subscribe 24, event 33 (snapshots: healthy/slow/hung/gone) | `health.rs` |
| File ops | start 25, cancel 26, progress 34, done 35 (copy, move, delete, delete-tree, fsync) | `ops.rs` |
| Trash | list 40, delete 41, restore 42, empty 43, beat 44, stat 45, children 46, stamp 47 | `vfssvc.rs`, `trash.rs` |
| Status codes | 0..=13 metadata, 14 cancelled, 15 OS error + errno, 16 would-recurse, 17 mount inside, 18 items remain | `md.rs`, `ops.rs`, `vfssvc.rs` |

The live backend is registered ahead of the local one, so `trash:` goes through gungnird: every volume's trash, original names, restore and empty with the daemon's guarantees, folders browsable, changes noticed by polling a stamp every 2 s. A home item's URI is its plain entry name; another volume's starts with the hex of its trash directory. Without the daemon, or with one older than the trash service, the local trash code serves the same URIs.

In practice:

- **Health gate.** Paths on mounts gungnird reports hung or gone fail at once with
  `SuspectMount`, before any syscall. The sidebar lists every unhealthy mount from the
  pushed snapshots.
- **Metadata.** Each listing identifies entries and lists their keys, 16 requests
  pipelined within a 250 ms budget. Tagged files show the `tagged` status. Inspect shows
  the identity (`fs_uuid`, subvolume, inode, generation, btime), every key and value,
  and the mount's health and probe latency. Renames are reported with `moved`.
- **Transfers.** Files are copied by gungnird (io_uring or its thread engine) with
  `ALL_METADATA`, so tags follow the copy. The backend creates the directories and
  copies each file through the daemon with progress. Moves use the daemon's rename,
  falling back to copy and delete-tree. Cancel sends `MSG_OP_CANCEL`. The daemon has no
  pause.
- **Resilience.** A dead daemon fails pending requests with `DaemonUnavailable` (under
  1 s) and listings fall back to plain POSIX. The link reconnects at most every 2 s once
  gungnird is back.

`tests/gungnir_live.rs` runs all of this against a real gungnird built from the sibling
checkout.

## Live-previewing the `.slint` files

Every component and the root `AppWindow` declare realistic sample data as property
defaults, so any `.slint` file renders populated with no Rust attached. At runtime,
`SlintSink::attach` replaces the samples with live models.

- **Editor (slint-lsp).** Open `crates/ui/ui/main.slint` (or any other `.slint` file) and
  run "Show Preview" from the Slint extension. Imports are all relative
  (`import { Theme } from "theme.slint";`) and `../assets/gungnir-fm.svg` is the only
  asset, so the language server resolves everything without include paths. To match the
  app, set the preview style to `fluent-dark` (VS Code: `"slint.preview.style": "fluent-dark"`).
- **Command line (slint-viewer).**

  ```sh
  cargo install --locked slint-viewer@1.18.1          # once
  slint-viewer --style fluent-dark --auto-reload crates/ui/ui/main.slint
  slint-viewer --style fluent-dark --component Pane crates/ui/ui/pane.slint
  ```

  `--auto-reload` re-renders on every save. Previewing a single component works for
  `Sidebar`, `PathBar`, `StatusBar`, `TransferDrawer`, `Pane` and `DialogOverlay`.

## Swapping MockGungnirBackend for GungnirVfsBackend

The swap is a runtime switch, not a code change:

```sh
# 1. Build gungnird from the sibling checkout (writes only into ./.build)
scripts/build-gungnird.sh

# 2. Run it (default socket: $XDG_RUNTIME_DIR/gungnir/gungnird.sock)
.build/gungnird/release/gungnird &

# 3. Start the file manager against it
GUNGNIR_FM_BACKEND=live cargo run --release -p gungnir-fm
```

`GUNGNIR_FM_BACKEND` selects the backend:

- `auto` (default): live if gungnird answers its handshake within 500 ms, else mock.
- `live-or-off`: live if gungnird answers, else no Gungnir at all (the sidebar section
  is hidden). The menu entry from `scripts/install-local.sh` uses this for daily use.
- `mock`: always the mock.
- `live`: always the live backend, which keeps retrying a missing daemon.
- `off`: local files only.

The sidebar header shows which one is active (`GUNGNIR VFS · LIVE` or `· MOCK`).
`GUNGNIR_SOCKET` overrides the socket path, with the same lookup order as gungnird.
Selection happens in `crates/ui/src/main.rs` (look for `GUNGNIR_FM_BACKEND`).

When development of gungnir-vfs resumes:

1. Pull `../gungnir-vfs`, then run `scripts/build-gungnird.sh` and
   `cargo test -p backends --test gungnir_live`. A protocol change shows up here first.
2. **Protocol version bump.** Mirror the new layout in `proto.rs` (`VERSION`, offsets,
   slot sizes) from `ipc.rs` and `AGENTS.md` §1.B.
3. **New message kinds** (for example remote listing or remote nodes):
   - add the constants and request/reply parsers to `proto.rs`;
   - route unsolicited kinds in `link.rs` `Driver::dispatch`;
   - implement the call in `GungnirVfsBackend`.
4. **Remote nodes.** Relax `local_path()` / `is_local_gungnir()` in `gungnir/mod.rs` to
   accept node authorities. Then `gungnir://odin/...` URIs, which today only the mock
   serves, go to the daemon unchanged. The UI, controller and URI scheme need no changes.
5. **Daemon-side listing.** Replace the local `list_dir` delegation in
   `GungnirVfsBackend::list_dir` with streamed daemon batches into the same `EntrySink`.

## Keyboard map

| Key | Action | Key | Action |
|---|---|---|---|
| F3 | dual pane on/off | Ctrl+L | edit location |
| F2 | inline rename | Ctrl+F | filter active pane |
| F5 / F6 | copy / move to other pane | Ctrl+H | hidden files |
| F7 | new folder | Ctrl+T / Ctrl+W | new / close tab |
| F9 | sidebar | Ctrl+1/2/3 | compact / detailed / grid |
| Tab | switch pane | Ctrl+I | Gungnir VFS Inspect |
| Alt+Up, Backspace | parent | Alt+Left / Alt+Right | back / forward |
| Enter | open | Delete / Shift+Delete | trash / delete permanently |
| Space | toggle selection | Ctrl+A, Escape | select all, clear |

F-keys and Ctrl shortcuts are `MenuBar` shortcuts, which Slint dispatches before
keyboard focus, so they work even while a text field is focused. Navigation keys that
must not steal text editing (Backspace, Tab, arrows, Delete) live in the window's
`FocusScope`.

## Verification

```sh
cargo test --workspace                              # 43 tests (unit, POSIX, live gungnird, controller)
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo build --release
GUNGNIR_FM_TRACE=1 target/release/gungnir-fm        # prints listing timings
```

## Measured performance (release build, this machine)

| Folder | First rows on screen | Whole folder listed and sorted |
|---|---|---|
| local, 100,000 empty files | 11.9 ms | 833 ms |
| mock `stress-100k` (first open, incl. generation) | 173 ms | 750 ms |

End and Home jump across 100,000 rows instantly (virtualized). RSS with two 100k
folders open is about 360 MB.

## Layout (Nemo conventions)

- **Single pane by default.** The file view fills everything between the sidebar and
  the window edge. F3 or the split button adds a second pane behind a 1px divider; the
  active pane gets a 2px accent line and the inactive one is dimmed slightly.
- **No cards.** Panes, headers and lists sit flush against the toolbar, sidebar and
  status bar.
- **Tabs.** The tab bar appears only when a pane has two or more tabs. Tabs are flat,
  with a top accent on the active one and a close button on hover.
- **Location bar.** A pencil button (or Ctrl+L) flips between connected breadcrumbs
  and a typed location. Folders below home start at a "Home" crumb, the file system
  root is a drive icon, and Gungnir nodes show the spear icon.
- **Rows.** Rows are 24px high. Selection is a solid steel-blue fill (`#27425f`), and
  the keyboard cursor on an unselected row is a faint outline.
- **Sidebar.** Section headers (PLACES, GUNGNIR VFS) collapse with chevrons. Places or
  the folder tree is chosen in the View menu.

## Known limits

- After End on a very long list, the last row can sit about 14px under the bottom
  edge. Slint's ListView clamps programmatic scrolling slightly short of the wheel
  limit; one wheel notch or arrow press reveals it.
- **Non-UTF-8 names.** File names that are not valid UTF-8 are shown lossily, and
  operations on them fail cleanly.
- **Cross-backend transfers.** Transfers between the mock and local files are refused.
  Live gungnir and `file://` transfers do work, through gungnird.
- **Trash across filesystems.** Trashing files on another filesystem than the home trash
  is refused; the error suggests Shift+Delete.
- **slint-lsp.** The Live-Preview data was verified with slint-viewer and the compiler,
  which use the same front end as slint-lsp, not inside an editor session.

## Lossless compression architecture & C23 policy

### Strict Architectural Rule
Every format supported by Gungnir FM's archive engine is strictly 100% BIT-PERFECT LOSSLESS.
Not a single byte, bit, frame, or audio sample of original data is ever degraded, quantized,
or discarded. Original checksums (SHA-256 and BLAKE3) remain identical after decode.

### 3-Tier Hybrid Pipeline
- **Tier 1 (In-Process `libarchive` Core):** Direct dynamic linking to `/usr/lib/libarchive.so`.
  Auto-detects format signatures via magic bytes. High-performance streaming reading and
  writing of `.tar.zst`, `.zip`, `.tar.xz`, `.tar.gz`, `.tar.bz2`, `.tar.lz4`, and `.tar`
  with zero temporary disk footprint.
- **Tier 2 (In-Process Native Streaming Engines):** Dedicated single-file streaming
  compressors and decompressors for `.zst`, `.xz`, `.gz`, `.bz2`, `.lz4`, and `.br`.
- **Tier 3 (Asynchronous Background Worker Fallback):** Non-blocking subprocess execution
  for solid multi-file 7-Zip (`7z`), proprietary unrar, and specialized domain media
  transcoding (`ffmpeg` lossless FFV1, FLAC, WavPack, ALAC, JPEG XL modular mode).

### C Standard Compliance Policy
All C code written, generated, or compiled within the workspace strictly conforms to ISO/IEC 9899:2024
(C23 standard), utilizing `-std=c23`. All compiler tools (GCC 16.2.1, Clang 23.1.1) enforce C23
standards, guaranteeing memory safety, standard `nullptr`, modern type inference, and bit-level
determinism.
