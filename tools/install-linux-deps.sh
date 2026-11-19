#!/usr/bin/env bash
# Installs what building Deskline needs on Debian 12+ / Ubuntu 24.04+ (CI runs this too, as root
# in a debian:12 container; add new ones here): the C/C++ toolchain and dpkg-dev for the .deb,
# PipeWire headers (0.3.65+, what the pipewire crate needs) and libclang for its bindings, nasm
# for the fast H.264 encoder, GStreamer for the GPU's, and the keyboard, Wayland, X11 and sound
# headers.
#   tools/install-linux-deps.sh
set -euo pipefail
sudo=""
[ "$(id -u)" = 0 ] || sudo=sudo
$sudo apt-get update
$sudo apt-get install -y --no-install-recommends \
    build-essential ca-certificates curl \
    pkg-config nasm libclang-dev libpipewire-0.3-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    libxkbcommon-dev libwayland-dev \
    libxcb1-dev libxcb-shm0-dev libxcb-randr0-dev libxcb-xfixes0-dev libxcb-xtest0-dev \
    libasound2-dev
