#!/bin/sh
# Gungnir FM Snap runtime entrypoint.
# Launches gungnird background daemon, configures GIO dynamic VFS,
# and executes Gungnir FM.
set -eu

# Runtime socket directory (0700 owned by user)
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
sock="${GUNGNIR_SOCKET:-$XDG_RUNTIME_DIR/gungnir/gungnird.sock}"
mkdir -p "$(dirname "$sock")"
chmod 700 "$(dirname "$sock")" 2>/dev/null || true

# GIO VFS modules and live backend configuration
export GIO_EXTRA_MODULES="$SNAP/lib/gio/modules:${GIO_EXTRA_MODULES:-}"
export GIO_USE_VFS="gungnir"
export GUNGNIR_FM_BACKEND="live"
export GUNGNIR_PACKAGE_FORMAT="snap"
export LD_LIBRARY_PATH="$SNAP/lib:$SNAP/usr/lib:$SNAP/usr/lib/x86_64-linux-gnu:${LD_LIBRARY_PATH:-}"

# Start gungnird daemon if not already running
if [ -x "$SNAP/bin/gungnird" ] && [ ! -S "$sock" ]; then
    "$SNAP/bin/gungnird" --idle-timeout 300 >/dev/null 2>&1 &
fi

exec "$SNAP/bin/gungnir-fm" "$@"
