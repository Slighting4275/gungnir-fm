#!/usr/bin/env bash
# Install Gungnir FM for the current user only (no root, nothing system-wide):
#   ~/.local/bin/gungnir-fm, an icon, and a menu entry.
# The menu entry declares no MimeType, so it never becomes the default folder handler.
# It starts with GUNGNIR_FM_BACKEND=live-or-off: the live gungnird when it runs,
# otherwise local files only (no mock Gungnir entries).
# Undo: scripts/install-local.sh --uninstall
set -u
here="$(cd "$(dirname "$0")/.." && pwd)"
bin="$HOME/.local/bin/gungnir-fm"
icon="$HOME/.local/share/icons/hicolor/scalable/apps/gungnir-fm.svg"
desktop="$HOME/.local/share/applications/gungnir-fm.desktop"

if [ "${1:-}" = "--uninstall" ]; then
    rm -f -- "$bin" "$icon" "$desktop"
    update-desktop-database "$HOME/.local/share/applications" 2>/dev/null
    echo "Gungnir FM removed."
    exit 0
fi

cargo build --release --manifest-path "$here/Cargo.toml" -p gungnir-fm || exit 1
install -Dm755 "$here/target/release/gungnir-fm" "$bin" || exit 1
install -Dm644 "$here/crates/ui/assets/gungnir-fm.svg" "$icon" || exit 1
mkdir -p "$(dirname "$desktop")"
cat > "$desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Gungnir FM
GenericName=File Manager
Comment=Fast dual-pane file manager (Gungnir VFS companion)
Exec=env GUNGNIR_FM_BACKEND=live-or-off $bin %f
Icon=gungnir-fm
Terminal=false
Categories=System;FileTools;FileManager;
Keywords=files;folders;browser;explorer;gungnir;
StartupNotify=true
DESKTOP
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null
echo "Installed: $bin"
echo "Menu entry: $desktop"
