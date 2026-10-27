#!/bin/sh
set -eu
export GUNGNIR_PACKAGE_FORMAT=flatpak
exec /app/bin/gungnir-fm "$@"
