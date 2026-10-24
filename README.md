# Gungnir FM

A fast dual-pane file manager for Linux, written in Rust with [Slint](https://slint.dev).
It has Nemo's layout and keyboard ergonomics, a modern dark theme, and a UI thread that
never touches the disk. It is the GUI companion of
[Gungnir VFS](https://gitlab.com/exalted6165/gungnir-vfs).

- Dual pane (F3), tabs in each pane, and compact, detailed or grid views.
- Directories stream in: the first 200 entries show at once, and 100,000-entry folders
  scroll smoothly in a virtualized list.
- Sortable columns: name, size, type, modified, permissions and VFS status.
- Hybrid path bar: breadcrumbs, or Ctrl+L for typed paths and `gungnir://` URIs.
- A non-modal transfer drawer with progress, throughput, pause and cancel.
- Three backends:
  - local POSIX, with inotify watching and the freedesktop trash;
  - a mock Gungnir backend with synthetic nodes and stress folders;
  - the live **gungnird** backend, which adds `metadata::*` tags, mount-health fail-fast
    and file operations run inside the daemon.

## Build and run

```sh
cargo build --release
target/release/gungnir-fm [LEFT] [RIGHT]
```

Install it for your user (binary in `~/.local/bin`, an icon and a menu entry; your
default file manager is not changed). Undo with `--uninstall`:

```sh
scripts/install-local.sh
```

The menu entry starts in `live-or-off` mode: Gungnir features when gungnird runs,
otherwise plain local files with no Gungnir section.

With a running gungnird the live backend is used automatically; otherwise the mock
backend fills the Gungnir section. See [ARCHITECTURE.md](ARCHITECTURE.md) for the
design, the Slint live-preview workflow and how to switch between backends.

## Requirements

Linux, Rust 1.92 or newer (edition 2024), and the usual Slint desktop dependencies
(fontconfig, xkbcommon, X11 or Wayland). Building the live daemon for the tests needs a
checkout of gungnir-vfs next to this one (`../gungnir-vfs`).

## License

GPL-3.0-or-later (see [LICENSE](LICENSE)). Slint is used under its GPLv3 option.
