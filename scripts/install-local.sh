#!/bin/sh
# Install Gungnir FM for the current user only (no root, nothing system-wide):
#   ~/.local/bin/gungnir-fm, an icon, and a menu entry.
# Fully portable across POSIX /bin/sh on Linux, BSD, and macOS.
# Undo: scripts/install-local.sh --uninstall
set -eu

here="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
bin="$HOME/.local/bin/gungnir-fm"
icon="$HOME/.local/share/icons/hicolor/scalable/apps/gungnir-fm.svg"
desktop="$HOME/.local/share/applications/gungnir-fm.desktop"

if [ "${1:-}" = "--uninstall" ]; then
    rm -f -- "$bin" "$icon" "$desktop"
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
    fi
    echo "Gungnir FM removed."
    exit 0
fi

cargo build --release --offline --manifest-path "$here/Cargo.toml" -p gungnir-fm || exit 1

mkdir -p "$HOME/.local/bin" "$(dirname -- "$icon")" "$(dirname -- "$desktop")"
cp -f "$here/target/release/gungnir-fm" "$bin"
chmod 755 "$bin"

cp -f "$here/crates/ui/assets/gungnir-fm.svg" "$icon"
chmod 644 "$icon"

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

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
fi

echo "Installed: $bin"
echo "Menu entry: $desktop"
