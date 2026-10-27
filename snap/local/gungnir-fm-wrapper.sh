#!/bin/sh
set -eu
export GUNGNIR_PACKAGE_FORMAT=snap
exec "$SNAP/bin/gungnir-fm" "$@"
