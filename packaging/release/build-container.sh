#!/bin/sh
set -eu
cd /source/gungnir-fm
cargo build --offline --locked --release --target-dir /out/cargo-fm -p gungnir-fm
cargo build --offline --locked --release --manifest-path /source/gungnir-vfs/Cargo.toml --target-dir /out/cargo-vfs --bin gungnird
cd /source/gungnir-vfs
./zb -Doptimize=ReleaseSafe
python3 /source/gungnir-fm/packaging/release/stage.py /out/payload
