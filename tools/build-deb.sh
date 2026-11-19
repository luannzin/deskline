#!/usr/bin/env bash
# Builds dist/deskline_amd64.deb. Build on Debian 12 (the oldest glibc and PipeWire we support)
# after tools/install-linux-deps.sh, so it runs on Debian 12, Ubuntu 24.04 and newer:
#   tools/build-deb.sh
# Install with: sudo apt install ./dist/deskline_amd64.deb
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
pkg="$work/deskline"
install -Dm755 target/release/deskline "$pkg/usr/bin/deskline"
install -Dm644 assets/icon.png "$pkg/usr/share/icons/hicolor/512x512/apps/deskline.png"
mkdir -p "$pkg/usr/share/applications" "$pkg/DEBIAN"
cat > "$pkg/usr/share/applications/deskline.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Deskline
GenericName=Remote Desktop
Comment=See and control another computer's screen, or let someone help on yours
Exec=deskline
Icon=deskline
StartupWMClass=deskline
Categories=Network;RemoteAccess;
Keywords=remote;desktop;support;screen;share;
Terminal=false
EOF

# Depends: the libraries the binary links (found by dpkg-shlibdeps), plus the ones the window
# and keyboard code load at runtime (OpenGL, X11, Wayland, xkbcommon are dlopen'd).
mkdir -p "$work/debian" && touch "$work/debian/control"
linked=$(cd "$work" && dpkg-shlibdeps -O deskline/usr/bin/deskline | sed -n 's/^shlibs:Depends=//p')
cat > "$pkg/DEBIAN/control" <<EOF
Package: deskline
Version: $version
Architecture: amd64
Maintainer: luannzin <luandaniel966@gmail.com>
Depends: $linked, libegl1, libgl1, libx11-6, libx11-xcb1, libxcb1, libxcursor1, libxi6, libxrandr2, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libwayland-cursor0, libwayland-egl1
Recommends: pipewire, xdg-desktop-portal, gstreamer1.0-plugins-bad, gstreamer1.0-vaapi
Section: net
Priority: optional
Homepage: https://github.com/luannzin/deskline
Description: remote desktop with no server to run
 Each computer has a code. Paste someone's code, they accept, and you see
 their screen; ask for control and they allow, and you drive their mouse and
 keyboard. Works on the same network or across the internet, end-to-end
 encrypted.
EOF

mkdir -p dist
dpkg-deb --root-owner-group -Zxz --build "$pkg" dist/deskline_amd64.deb
echo "Built dist/deskline_amd64.deb ($version)"
