#!/bin/sh
# Build gungnird from the read-only sibling checkout ../gungnir-vfs into
# ./.build/gungnird (nothing is written inside ../gungnir-vfs: --target-dir points
# here and --locked keeps its Cargo.lock as is).
# Fully portable across POSIX /bin/sh.
set -eu

here="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
sibling="${GUNGNIR_VFS_DIR:-$here/../gungnir-vfs}"

if [ ! -f "$sibling/Cargo.toml" ]; then
    echo "gungnir-vfs not found at $sibling (clone https://gitlab.com/exalted6165/gungnir-vfs there)" >&2
    exit 1
fi

cargo build --release --locked --offline --bin gungnird \
    --manifest-path "$sibling/Cargo.toml" \
    --target-dir "$here/.build/gungnird" || exit 1

echo "$here/.build/gungnird/release/gungnird"
