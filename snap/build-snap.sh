#!/bin/sh
# Snap package validation script for Gungnir FM.
set -eu

here="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"

echo "==> Verifying snapcraft.yaml..."
test -f "$here/snap/snapcraft.yaml"
echo "    snapcraft.yaml exists: OK"

echo "==> Verifying snap wrapper script..."
test -x "$here/snap/local/gungnir-fm-wrapper.sh"
echo "    Wrapper script executable: OK"

if command -v snapcraft >/dev/null 2>&1; then
    echo "==> Building snap package with snapcraft..."
    (cd "$here" && snapcraft)
else
    echo "==> Note: snapcraft CLI not installed on host; manifest and wrapper verified."
fi

echo "==> Snap packaging verification complete."
