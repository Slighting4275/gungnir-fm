#!/bin/sh
# Build companion gungnird daemon for Gungnir FM.
# Supports sibling ../gungnir-vfs checkout, custom $GUNGNIR_VFS_DIR, or automatic
# shallow clone of https://gitlab.com/exalted6165/gungnir-vfs.git into .build/gungnir-vfs.
# --pinned builds the test revision in a separate checkout and target directory.
# Fully portable across POSIX /bin/sh on Linux, FreeBSD, OpenBSD, NetBSD, and DragonFly BSD.
set -eu

here="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
sibling="${GUNGNIR_VFS_DIR:-$here/../gungnir-vfs}"

target="$here/.build/gungnird"
remote=https://gitlab.com/exalted6165/gungnir-vfs.git

case "${1:-}:$#" in
--pinned:1)
    # Committed gungnir-vfs revision exercised by the FM live tests.
    revision=155f2c234ae1c242a7285c958b7affc1501c0533
    target="$here/.build/gungnird-pinned"
    source="$here/.build/gungnir-vfs-pinned"
    mkdir -p "$here/.build"
    if [ ! -e "$source/.git" ]; then
        if [ -e "$sibling/.git" ] && git -C "$sibling" cat-file -e "$revision^{commit}"; then
            git clone --quiet --no-checkout "$sibling" "$source"
        else
            git init --quiet "$source"
        fi
    elif [ -n "$(git -C "$source" status --porcelain)" ]; then
        echo "Pinned gungnird source has local changes: $source" >&2
        exit 1
    fi
    if ! git -C "$source" cat-file -e "$revision^{commit}" 2>/dev/null; then
        git -C "$source" fetch --quiet --depth 1 "$remote" "$revision"
    fi
    git -C "$source" checkout --quiet --detach "$revision"
    sibling="$source"
    ;;
:0)
    if [ ! -f "$sibling/Cargo.toml" ]; then
        if [ -f "$here/.build/gungnir-vfs/Cargo.toml" ]; then
            sibling="$here/.build/gungnir-vfs"
        elif command -v git >/dev/null 2>&1; then
            echo "==> gungnir-vfs not found at $sibling"
            echo "==> Automatically cloning gungnir-vfs from https://gitlab.com/exalted6165/gungnir-vfs.git..."
            mkdir -p "$here/.build"
            git clone --depth 1 "$remote" "$here/.build/gungnir-vfs" || {
                echo "Error: failed to clone gungnir-vfs repository." >&2
                exit 1
            }
            sibling="$here/.build/gungnir-vfs"
        else
            echo "gungnir-vfs not found at $sibling (clone https://gitlab.com/exalted6165/gungnir-vfs there)" >&2
            exit 1
        fi
    fi

    ;;
*)
    echo "usage: $0 [--pinned]" >&2
    exit 2
    ;;
esac

if [ -e "$sibling/.git" ]; then
    revision="$(git -C "$sibling" rev-parse HEAD)"
    if [ -n "$(git -C "$sibling" status --porcelain)" ]; then
        revision="$revision (with local changes)"
    fi
else
    revision="unknown (source has no Git metadata)"
fi
echo "==> Building companion gungnird from $revision" >&2
cargo build --release --locked --bin gungnird \
    --manifest-path "$sibling/Cargo.toml" \
    --target-dir "$target" >&2

echo "$target/release/gungnird"
