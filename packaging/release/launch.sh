#!/bin/sh
# Private ELF loader keeps bundled libraries out of host utilities' search path.
set -eu
prefix=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
private="$prefix/lib/gungnir-fm"
name=${0##*/}
case "$name" in
    gungnird) exec "$private/runtime/ld-linux-x86-64.so.2" --library-path "$private/runtime" "$private/bin/gungnird" "$@" ;;
    gio)
        export GIO_MODULE_DIR="$private/gio-modules"
        exec "$private/runtime/ld-linux-x86-64.so.2" --library-path "$private/runtime" "$private/bin/gio" "$@"
        ;;
esac
export PATH="$prefix/bin:$PATH"
export XDG_DATA_DIRS="$prefix/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
export FONTCONFIG_PATH="$prefix/share/fontconfig"
export FONTCONFIG_FILE="$prefix/share/fontconfig/fonts.conf"
export QT_PLUGIN_PATH="$private/qt-plugins"
export QT_QPA_PLATFORM_PLUGIN_PATH="$private/qt-plugins/platforms"
export GUNGNIR_FM_LAUNCHER="${APPIMAGE:-$prefix/bin/gungnir-fm}"
export GUNGNIR_PACKAGE_FORMAT="${GUNGNIR_PACKAGE_FORMAT:-native}"
# Explicit opt-out remains useful for troubleshooting a missing daemon.
export GUNGNIR_FM_BACKEND="${GUNGNIR_FM_BACKEND:-live}"
export GIO_EXTRA_MODULES="$prefix/lib/gio/modules${GIO_EXTRA_MODULES:+:$GIO_EXTRA_MODULES}"
export GIO_USE_VFS="${GIO_USE_VFS:-gungnir}"
case "${1:-}" in --help|-h|--version|-V) diagnostic=1 ;; *) diagnostic=0 ;; esac
if [ "$diagnostic" = 0 ] && [ "$GUNGNIR_FM_BACKEND" = live ]; then
    export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    export GUNGNIR_SOCKET="${GUNGNIR_SOCKET:-$XDG_RUNTIME_DIR/gungnir/gungnird.sock}"
    socket_dir=${GUNGNIR_SOCKET%/*}
    if [ ! -d "$socket_dir" ]; then
        (umask 077; mkdir -p -- "$socket_dir")
    fi
    if [ ! -S "$GUNGNIR_SOCKET" ]; then
        "$prefix/bin/gungnird" --idle-timeout 300 >/dev/null 2>&1 &
    fi
fi
exec "$private/runtime/ld-linux-x86-64.so.2" --library-path "$private/runtime" "$private/bin/gungnir-fm" "$@"
