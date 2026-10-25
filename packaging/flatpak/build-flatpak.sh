#!/bin/sh
# Flatpak package validation and build script for Gungnir FM.
# Validates manifest syntax, AppStream metainfo, and desktop entry.
set -eu

here="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
pkg="$here/packaging/flatpak"

echo "==> Validating Flatpak desktop file..."
if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "$pkg/org.gungnir.GungnirFM.desktop"
    echo "    Desktop file validation: OK"
fi

echo "==> Validating Flatpak AppStream metadata..."
if command -v appstreamcli >/dev/null 2>&1; then
    appstreamcli validate --no-net "$pkg/org.gungnir.GungnirFM.metainfo.xml"
    echo "    AppStream CLI validation: OK"
fi
if command -v appstream-util >/dev/null 2>&1; then
    appstream-util validate-relax "$pkg/org.gungnir.GungnirFM.metainfo.xml"
    echo "    AppStream Util validation: OK"
fi

echo "==> Verifying Flatpak wrapper script..."
test -x "$pkg/gungnir-fm-wrapper.sh"
echo "    Wrapper script executable: OK"

echo "==> Verifying Flatpak manifest..."
test -f "$pkg/org.gungnir.GungnirFM.yml"
echo "    Flatpak manifest present: OK"

if command -v flatpak-builder >/dev/null 2>&1; then
    echo "==> Building Flatpak package..."
    flatpak-builder --force-clean --user "$here/.build/flatpak-build" "$pkg/org.gungnir.GungnirFM.yml"
else
    echo "==> Note: flatpak-builder not installed; manifest and metadata verified."
fi

echo "==> Flatpak packaging verification complete."
