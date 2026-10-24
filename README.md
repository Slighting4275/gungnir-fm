# Gungnir FM

A fast dual-pane Linux file manager written in Rust with [Slint](https://slint.dev).
It combines Nemo's layout and keyboard ergonomics with sub-millisecond responsiveness, a modern dark theme, and an architecture where the UI thread never touches disk or blocking I/O. It serves as the companion GUI for [Gungnir VFS](https://gitlab.com/exalted6165/gungnir-vfs).

Source repository & issue tracker: [https://gitlab.com/exalted6165/gungnir-fm](https://gitlab.com/exalted6165/gungnir-fm).

---

## Key Features

### 🗂️ Panes, Tabs & Navigation
- **Dual-Pane Layout (`F3`)**: Single-pane default with instantaneous split-pane toggling.
- **Notebook Tabs per Pane (`Ctrl+T` / `Ctrl+W`)**: Independent multi-tab strips for primary and secondary panes, each maintaining its own path, navigation history stack (back/forward buffers), scroll position, and display titles.
- **Hybrid Path Bar**: Interactive breadcrumbs with Nemo-style home routing and remote badges, or direct editable path entry (`Ctrl+L`) for POSIX paths, `gungnir://`, `sftp://`, and `smb://` URIs.
- **Virtualized Views**: Compact list, detailed list, and thumbnail grid views virtualized with Slint `ListView`. 100,000-entry folders scroll smoothly without UI stutters.
- **Sortable Columns**: Natural, folders-first ordering by name, size, type, modified time, POSIX permissions, and Gungnir VFS sync status.

### 🖼️ Visual Image & Video Previews
- **Multi-Format Thumbnails**:
  - **Images**: PNG, JPG/JPEG, WebP, GIF, BMP, ICO, SVG, and AVIF via the `image` crate with ImageMagick fallback.
  - **Videos**: MP4, MKV, WebM, MOV, AVI, FLV, WMV, M4V, TS, MPG/MPEG frame extraction via `ffmpegthumbnailer` or `ffmpeg`.
- **Integrated Previews**: Visible across List View, Grid View, the Properties dialog, and the Quick Preview overlay (`Spacebar`).
- **Disk Caching**: Asynchronous background generation cached under `~/.cache/gungnir-fm/thumbnails/` using source URI and modification timestamp keys.

### 🔒 File Hashing & Interactive Checksum Verification
- **Streaming Multi-Hash Engine**: Computes MD5, SHA-1, SHA-256, and SHA-512 concurrently in a single streaming pass over 64 KB buffers without loading entire files into memory.
- **Interactive Checksum Dialog**: Right-click any file → **"Check File Hash…"** displays all computed digests with an interactive input field. Pasting an expected hash provides immediate visual verification (green on match, red on mismatch).
- **Checksum File Verification**: Automatically parses and verifies `.sha256`, `.md5`, `.sha512`, `.sha1`, and `SHA256SUMS` files against referenced targets in the folder.

### 🌐 Built-in Remote Filesystems (SFTP / SSH & SMB / CIFS)
- **Native Remote VFS**: Built-in `RemoteVfsBackend` handling `sftp://`, `ssh://`, `smb://`, and `cifs://` schemes.
- **"Connect to Server…" Dialog**: Accessible from the File menu or command palette, enabling direct connections to remote network shares.
- **Full VFS Parity**: Remote directory streaming, stat inspection, file operations, remote breadcrumb parsing, and status badges.

### ⚡ Custom Actions Engine (Nemo Actions Equivalent)
- **Declarative TOML Actions**: Loads user-defined actions from `~/.config/gungnir-fm/actions/*.toml` (with built-in fallbacks).
- **Target & MIME Filtering**: Rules match on target kind (`file`, `directory`, `any`), file extensions, or MIME patterns.
- **Token Substitution**: Standard tokens `%d` (active directory), `%f` (single selection), `%F` (multi-selection), and `%u` (URI).
- **Asynchronous Execution**: Spawns processes in the background via Tokio without blocking the UI event loop.
- **Default Actions Included**:
  - *Open Terminal Here* (`alacritty`, `ptyxis`, `gnome-terminal`, etc.)
  - *Calculate SHA-256 to Clipboard*
  - *Verify Checksum File*

### 💾 Crash-Resilient Session Persistence & Recovery
- **Automatic State Saving**: Debounced auto-save (500 ms) to `~/.config/gungnir-fm/session.json`.
- **Atomic Writes**: Writes to temporary files and atomically renames to prevent corruption during unexpected shutdowns or crashes.
- **Persisted State**: Window size and maximized state, active pane, split view toggle, open tabs per pane, hidden file visibility (`Ctrl+H`), and sidebar state (`F9`).
- **Safe Recovery**: Automatically restores open tabs and paths on restart, safely falling back to the user's home directory if any saved path is unmounted or inaccessible.

### 🔌 Removable Drives & Desktop Integration
- **Removable Media**: D-Bus and `udisks2` integration displaying removable storage drives, partitions, and mount states in the sidebar.
- **"Open With…" Chooser**: Freedesktop `.desktop` parser ranking default handlers, explicit associations, and parent MIME handlers.
- **Drag-and-Drop & Clipboard**: Seamless drag-and-drop between panes and tabs, plus standard cut/copy/paste (`Ctrl+X`, `Ctrl+C`, `Ctrl+V`).

### 🔱 Gungnir VFS Companion
- **Native `gungnir://` Protocol**: Connects to the local `gungnird` daemon via a zero-copy shared-memory ring buffer (protocol v2).
- **Live Metadata**: Real-time `metadata::*` xattr tags, cluster node routing, and mount-health fail-fast monitoring.
- **Mock Mode**: Built-in mock provider for headless development and testing without a live daemon.

---

## Keyboard Shortcuts

| Shortcut | Action |
| --- | --- |
| `F3` | Toggle dual split view |
| `F9` | Toggle sidebar (Places, Gungnir, Tree, Devices) |
| `Ctrl+T` | Open new tab in current directory |
| `Ctrl+W` | Close active tab (collapses secondary pane if last tab) |
| `Ctrl+Tab` / `Ctrl+PageDown` | Switch to next tab |
| `Ctrl+Shift+Tab` / `Ctrl+PageUp` | Switch to previous tab |
| `Alt+1` .. `Alt+9` | Jump directly to tab index 1–9 |
| `Ctrl+L` | Focus location bar (type path or URI) |
| `Alt+Up` | Navigate up to parent directory |
| `Alt+Left` / `Alt+Right` | Navigate back / forward in tab history |
| `Ctrl+H` | Toggle hidden files |
| `Ctrl+1` / `Ctrl+2` / `Ctrl+3` | Switch view: Detailed list / Compact list / Grid |
| `Space` | Quick Preview overlay (image/video/text) |
| `F2` | Rename selected file or directory |
| `F5` / `F6` | Copy / Move selection to other pane |
| `F7` | Create new directory |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | Copy / Cut / Paste selection |
| `Delete` | Move selection to Trash |
| `Shift+Delete` | Permanently delete selection |
| `F10` | Open context menu |

---

## Building and Installation

### Build Requirements
- Linux (x86_64, AArch64)
- Rust 1.92+ (edition 2024)
- Slint runtime dependencies (fontconfig, xkbcommon, X11 or Wayland libraries)
- Optional tools for preview enhancements: `ffmpegthumbnailer`, `ffmpeg`, `imagemagick`

### Compile from Source
```sh
cargo build --release
target/release/gungnir-fm [LEFT_PATH] [RIGHT_PATH]
```

### User Installation
To install into `~/.local/bin` with the desktop icon and application launcher entry:
```sh
scripts/install-local.sh
```
To uninstall:
```sh
scripts/install-local.sh --uninstall
```

### Running Tests
The workspace includes unit and integration tests across all crates (sandboxed and isolated, with zero personal data access):
```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```

---

## License

GPL-3.0-or-later (see [LICENSE](LICENSE)). Slint is used under its GPLv3 option.
