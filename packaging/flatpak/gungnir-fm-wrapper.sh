#!/bin/sh
# Gungnir FM Flatpak runtime entrypoint.
# Ensures gungnir-vfs daemon (gungnird) is launched, wires dynamic GIO VFS,
# and executes Gungnir FM inside the sandboxed environment.
set -eu

# Standard runtime socket directory (must be 0700 owned by user)
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
sock="${GUNGNIR_SOCKET:-$XDG_RUNTIME_DIR/gungnir/gungnird.sock}"
mkdir -p "$(dirname "$sock")"
chmod 700 "$(dirname "$sock")" 2>/dev/null || true

# Configure GIO dynamic VFS extension module and live backend
export GIO_EXTRA_MODULES="/app/lib/gio/modules:${GIO_EXTRA_MODULES:-}"
export GIO_USE_VFS="gungnir"
export GUNGNIR_FM_BACKEND="live"
export GUNGNIR_PACKAGE_FORMAT="flatpak"

# Start gungnird background daemon if not already running
if [ -x /app/bin/gungnird ] && [ ! -S "$sock" ]; then
    /app/bin/gungnird --idle-timeout 300 >/dev/null 2>&1 &
fi

exec /app/bin/gungnir-fm "$@"
