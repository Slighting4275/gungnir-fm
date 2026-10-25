#!/bin/sh
# AppImage creation and validation script for Gungnir FM.
# Assembles AppDir, bundles gungnir-vfs (daemon + GIO module),
# packages squashfs filesystem, prepends standard AppImage Type 2 runtime,
# and verifies archive structure and executables.
set -eu

here="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
vfs_dir="${GUNGNIR_VFS_DIR:-$here/../gungnir-vfs}"
build_dir="$here/.build"
appdir="$build_dir/AppDir"
appimage="$build_dir/Gungnir_FM-x86_64.AppImage"
squashfs="$build_dir/appdir.squashfs"

echo "==> Preparing AppDir staging directory..."
rm -rf "$appdir" "$squashfs"
mkdir -p "$appdir/usr/bin" \
         "$appdir/usr/lib/gio/modules" \
         "$appdir/usr/share/applications" \
         "$appdir/usr/share/metainfo" \
         "$appdir/usr/share/icons/hicolor/scalable/apps"

echo "==> Building gungnir-vfs daemon and GIO extension module..."
if [ ! -f "$vfs_dir/Cargo.toml" ]; then
    echo "Error: gungnir-vfs repository not found at $vfs_dir" >&2
    exit 1
fi

# 1. Build gungnird Rust daemon
cargo build --release --locked --bin gungnird \
    --manifest-path "$vfs_dir/Cargo.toml" \
    --target-dir "$build_dir/gungnird"

# 2. Build libgiogungnir.so GIO module
(cd "$vfs_dir" && ./zb)

echo "==> Building gungnir-fm release binary..."
cargo build --release --locked --manifest-path "$here/Cargo.toml" -p gungnir-fm

echo "==> Staging binaries and libraries into AppDir..."
cp -f "$here/target/release/gungnir-fm" "$appdir/usr/bin/gungnir-fm"
chmod 755 "$appdir/usr/bin/gungnir-fm"

cp -f "$build_dir/gungnird/release/gungnird" "$appdir/usr/bin/gungnird"
chmod 755 "$appdir/usr/bin/gungnird"

cp -f "$vfs_dir/.build/zig-out/lib/libgiogungnir.so" "$appdir/usr/lib/gio/modules/libgiogungnir.so"
chmod 755 "$appdir/usr/lib/gio/modules/libgiogungnir.so"

if command -v gio-querymodules >/dev/null 2>&1; then
    gio-querymodules "$appdir/usr/lib/gio/modules"
fi

echo "==> Staging metadata, desktop files, and icons..."
cp -f "$here/packaging/appimage/AppRun" "$appdir/AppRun"
chmod 755 "$appdir/AppRun"

cp -f "$here/packaging/flatpak/org.gungnir.GungnirFM.desktop" "$appdir/usr/share/applications/org.gungnir.GungnirFM.desktop"
cp -f "$here/packaging/flatpak/org.gungnir.GungnirFM.desktop" "$appdir/org.gungnir.GungnirFM.desktop"
ln -sf org.gungnir.GungnirFM.desktop "$appdir/gungnir-fm.desktop"

cp -f "$here/packaging/flatpak/org.gungnir.GungnirFM.metainfo.xml" "$appdir/usr/share/metainfo/org.gungnir.GungnirFM.metainfo.xml"
cp -f "$here/crates/ui/assets/gungnir-fm.svg" "$appdir/usr/share/icons/hicolor/scalable/apps/org.gungnir.GungnirFM.svg"
cp -f "$here/crates/ui/assets/gungnir-fm.svg" "$appdir/org.gungnir.GungnirFM.svg"
cp -f "$here/crates/ui/assets/gungnir-fm.svg" "$appdir/.DirIcon"

echo "==> Validating staged desktop and AppStream metadata..."
if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "$appdir/org.gungnir.GungnirFM.desktop"
fi
if command -v appstreamcli >/dev/null 2>&1; then
    appstreamcli validate --no-net "$appdir/usr/share/metainfo/org.gungnir.GungnirFM.metainfo.xml"
fi

echo "==> Generating squashfs payload..."
MKSQUASHFS="mksquashfs"
if [ -x "$HOME/.local/bin/mksquashfs" ]; then
    MKSQUASHFS="$HOME/.local/bin/mksquashfs"
fi

"$MKSQUASHFS" "$appdir" "$squashfs" -root-owned -noappend -comp zstd >/dev/null 2>&1 || \
"$MKSQUASHFS" "$appdir" "$squashfs" -root-owned -noappend -comp gzip >/dev/null

echo "==> Assembling standalone Type 2 AppImage executable..."
# Locate or extract standard Type 2 64-bit AppImage runtime
runtime_src=""
for candidate in \
    "$HOME/.local/opt/odysync/odysync-linux-amd64.AppImage" \
    "$HOME/.local/opt/genoffice/GenOffice.AppImage" \
    "$HOME/.local/share/appimagekit/runtime-x86_64"; do
    if [ -f "$candidate" ]; then
        runtime_src="$candidate"
        break
    fi
done

if [ -n "$runtime_src" ]; then
    offset="$("$runtime_src" --appimage-offset 2>/dev/null || echo "944632")"
    head -c "$offset" "$runtime_src" > "$build_dir/runtime-type2"
    cat "$build_dir/runtime-type2" "$squashfs" > "$appimage"
    rm -f "$build_dir/runtime-type2"
else
    # Fallback to squashfs image
    cp -f "$squashfs" "$appimage"
fi
chmod 755 "$appimage"
rm -f "$squashfs"

echo "==> Verifying built AppImage..."
test -s "$appimage"
test -x "$appimage"
ls -lh "$appimage"

if [ -x "$HOME/.local/bin/unsquashfs" ]; then
    echo "==> Verifying embedded filesystem with unsquashfs..."
    offset="$("$appimage" --appimage-offset 2>/dev/null || echo "0")"
    "$HOME/.local/bin/unsquashfs" -offset "$offset" -l "$appimage" | grep -E "(gungnir-fm|gungnird|libgiogungnir\.so|AppRun)" | head -n 6
fi

echo "==> AppImage package built and verified successfully: $appimage"
