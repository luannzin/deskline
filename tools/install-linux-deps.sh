#!/usr/bin/env bash
# Installs what building Deskline needs on Ubuntu/Debian (CI runs this too; add new ones here):
# PipeWire headers and libclang for its bindings, nasm for the fast H.264 encoder, and the
# keyboard, Wayland, X11 and sound headers.
#   tools/install-linux-deps.sh
set -euo pipefail
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
    pkg-config nasm libclang-dev libpipewire-0.3-dev \
    libxkbcommon-dev libwayland-dev \
    libxcb1-dev libxcb-shm0-dev libxcb-randr0-dev libxcb-xfixes0-dev libxcb-xtest0-dev \
    libasound2-dev
