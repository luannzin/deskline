#!/usr/bin/env bash
# Builds dist/deskline.exe: one self-contained exe that runs portable and installs itself
# (its Install button). Run in Git Bash with the MSVC Rust toolchain; nasm makes the encoder fast.
#   tools/build-windows.sh
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release
mkdir -p dist
cp target/release/deskline.exe dist/deskline.exe
# the updater refuses a download that doesn't say it's the release's version
said=$(timeout 60 dist/deskline.exe --version | tr -d '\r' | awk '{print $NF}')
[ "$said" = "$version" ] || { echo "deskline.exe --version said '$said', not $version" >&2; exit 1; }
echo "Built dist/deskline.exe ($version)"
