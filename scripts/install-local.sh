#!/bin/sh
# Install Gungnir FM for the current user only (no root, nothing system-wide):
#   ~/.local/bin/gungnir-fm, an icon, and a menu entry.
# Fully portable across POSIX /bin/sh on Linux, BSD, and macOS.
# Undo: scripts/install-local.sh --uninstall
set -eu

here="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
bin="$HOME/.local/bin/gungnir-fm"
daemon_bin="$HOME/.local/bin/gungnird"
gio_module="$HOME/.local/lib/gio/modules/libgiogungnir.so"
icon="$HOME/.local/share/icons/hicolor/scalable/apps/gungnir-fm.svg"
desktop="$HOME/.local/share/applications/gungnir-fm.desktop"

if [ "${1:-}" = "--uninstall" ]; then
    rm -f -- "$bin" "$daemon_bin" "$gio_module" "$icon" "$desktop"
    if command -v gio-querymodules >/dev/null 2>&1; then
        gio-querymodules "$HOME/.local/lib/gio/modules" 2>/dev/null || true
    fi
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

# Install gungnir-vfs daemon and GIO module if available
if [ -f "$here/.build/gungnird/release/gungnird" ]; then
    cp -f "$here/.build/gungnird/release/gungnird" "$daemon_bin"
    chmod 755 "$daemon_bin"
elif [ -f "$here/../gungnir-vfs/.build/target/release/gungnird" ]; then
    cp -f "$here/../gungnir-vfs/.build/target/release/gungnird" "$daemon_bin"
    chmod 755 "$daemon_bin"
fi

vfs_so="$here/../gungnir-vfs/.build/zig-out/lib/libgiogungnir.so"
if [ -f "$vfs_so" ]; then
    mkdir -p "$(dirname -- "$gio_module")"
    cp -f "$vfs_so" "$gio_module"
    chmod 755 "$gio_module"
    if command -v gio-querymodules >/dev/null 2>&1; then
        gio-querymodules "$HOME/.local/lib/gio/modules" 2>/dev/null || true
    fi
fi

cp -f "$here/crates/ui/assets/gungnir-fm.svg" "$icon"
chmod 644 "$icon"

cat > "$desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Gungnir FM
GenericName=File Manager
Comment=Fast dual-pane file manager (Gungnir VFS companion)
Exec=env GIO_EXTRA_MODULES=$HOME/.local/lib/gio/modules GIO_USE_VFS=gungnir GUNGNIR_FM_BACKEND=live-or-off $bin %f
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
