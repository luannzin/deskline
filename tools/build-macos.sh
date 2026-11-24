#!/usr/bin/env bash
# Builds dist/Deskline.app (universal: Apple Silicon + Intel) and dist/Deskline.dmg, the disk image
# you drag Deskline into Applications from. Run on a Mac (brew install nasm for the fast encoder).
#   tools/build-macos.sh
# With SIGN_IDENTITY set to a "Developer ID Application: ..." identity in the keychain it signs for
# notarization. Set to any other identity (the self-signed one release.yml makes) it signs with
# that, so macOS knows each update as the same app and keeps Screen Recording and Accessibility
# allowed. Unset, ad-hoc: those are asked again after every update.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin

app=dist/Deskline.app
rm -rf "$app" dist/Deskline.dmg
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/deskline" \
    target/aarch64-apple-darwin/release/deskline target/x86_64-apple-darwin/release/deskline

iconset=$(mktemp -d)/deskline.iconset
mkdir -p "$iconset"
for s in 16 32 128 256 512; do
    sips -z $s $s assets/icon.png --out "$iconset/icon_${s}x${s}.png" >/dev/null
    sips -z $((s * 2)) $((s * 2)) assets/icon.png --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/deskline.icns"

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Deskline</string>
    <key>CFBundleDisplayName</key><string>Deskline</string>
    <key>CFBundleIdentifier</key><string>io.github.luannzin.deskline</string>
    <key>CFBundleExecutable</key><string>deskline</string>
    <key>CFBundleIconFile</key><string>deskline</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>LSUIElement</key><false/>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSLocalNetworkUsageDescription</key><string>Deskline connects directly to computers on your network when it can.</string>
    <key>NSAudioCaptureUsageDescription</key><string>Deskline sends what this Mac plays to the people you let in.</string>
    <key>NSMicrophoneUsageDescription</key><string>Deskline sends this Mac's sound through a loopback device such as BlackHole when one is installed.</string>
</dict>
</plist>
EOF

if [[ "${SIGN_IDENTITY:-}" == "Developer ID"* ]]; then
    # hardened runtime + secure timestamp: what notarization requires. The runtime blocks sound
    # input (a loopback device) unless the entitlement says Deskline uses it.
    ent=$(mktemp)
    cat > "$ent" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.device.audio-input</key><true/>
</dict>
</plist>
EOF
    codesign --force --options runtime --timestamp --entitlements "$ent" --sign "$SIGN_IDENTITY" "$app"
elif [ -n "${SIGN_IDENTITY:-}" ]; then
    codesign --force --deep --sign "$SIGN_IDENTITY" "$app"
else
    # Ad-hoc signature: required for Apple Silicon. Not notarized, so a browser-downloaded copy needs
    # System Settings > Privacy & Security > Open Anyway once (or: xattr -dr com.apple.quarantine Deskline.app).
    codesign --force --deep --sign - "$app"
fi
# The designated requirement is what macOS remembers permissions by: it must not change between
# releases (an ad-hoc one names this build's hash).
codesign --verify --deep --strict "$app"
codesign --display --requirements - "$app"

stage=$(mktemp -d)
ditto "$app" "$stage/Deskline.app"
ln -s /Applications "$stage/Applications"
# hdiutil now and then fails with "Resource busy" on CI runners; a retry gets through
for try in 1 2 3; do
    hdiutil create -volname Deskline -srcfolder "$stage" -format UDZO -ov dist/Deskline.dmg && break
    [ "$try" = 3 ] && exit 1
    sleep 5
done
if [[ "${SIGN_IDENTITY:-}" == "Developer ID"* ]]; then
    codesign --force --timestamp --sign "$SIGN_IDENTITY" dist/Deskline.dmg
fi
echo "Built $app and dist/Deskline.dmg"
