#!/bin/sh
# Build companion gungnird daemon for Gungnir FM.
# Supports sibling ../gungnir-vfs checkout, custom $GUNGNIR_VFS_DIR, or automatic
# shallow clone of https://gitlab.com/exalted6165/gungnir-vfs.git into .build/gungnir-vfs.
# Fully portable across POSIX /bin/sh on Linux, FreeBSD, OpenBSD, NetBSD, and DragonFly BSD.
set -eu

here="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
sibling="${GUNGNIR_VFS_DIR:-$here/../gungnir-vfs}"

if [ ! -f "$sibling/Cargo.toml" ]; then
    if [ -f "$here/.build/gungnir-vfs/Cargo.toml" ]; then
        sibling="$here/.build/gungnir-vfs"
    elif command -v git >/dev/null 2>&1; then
        echo "==> gungnir-vfs not found at $sibling"
        echo "==> Automatically cloning gungnir-vfs from https://gitlab.com/exalted6165/gungnir-vfs.git..."
        mkdir -p "$here/.build"
        git clone --depth 1 https://gitlab.com/exalted6165/gungnir-vfs.git "$here/.build/gungnir-vfs" || {
            echo "Error: failed to clone gungnir-vfs repository." >&2
            exit 1
        }
        sibling="$here/.build/gungnir-vfs"
    else
        echo "gungnir-vfs not found at $sibling (clone https://gitlab.com/exalted6165/gungnir-vfs there)" >&2
        exit 1
    fi
fi

echo "==> Building companion gungnird daemon..."
cargo build --release --locked --bin gungnird \
    --manifest-path "$sibling/Cargo.toml" \
    --target-dir "$here/.build/gungnird" || exit 1

echo "$here/.build/gungnird/release/gungnird"
