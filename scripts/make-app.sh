#!/usr/bin/env bash
# Wrap the built binary in "dist/Workbench for LiveKit.app" (macOS).
#
#   scripts/make-app.sh [path/to/livekit-workbench]
#
# The bundle carries the camera usage text macOS needs to ask for camera
# access, and an ad-hoc signature so the permission is remembered.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXE="${1:-$ROOT/target/release/livekit-workbench}"
[[ -x "$EXE" ]] || { echo "No binary at $EXE; build first (cargo build --release)." >&2; exit 1; }

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
APP="$ROOT/dist/Workbench for LiveKit.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$EXE" "$APP/Contents/MacOS/livekit-workbench"

ICON_KEY=""
if [[ -f "$ROOT/assets/icon.icns" ]]; then
  cp "$ROOT/assets/icon.icns" "$APP/Contents/Resources/icon.icns"
  ICON_KEY="<key>CFBundleIconFile</key><string>icon</string>"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Workbench for LiveKit</string>
  <key>CFBundleDisplayName</key><string>Workbench for LiveKit</string>
  <key>CFBundleIdentifier</key><string>io.czoli1976.livekit-workbench</string>
  <key>CFBundleExecutable</key><string>livekit-workbench</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSCameraUsageDescription</key><string>Workbench for LiveKit publishes your camera to a LiveKit room so you can see what the video pipeline does with it.</string>
  $ICON_KEY
</dict>
</plist>
PLIST

codesign --force --deep -s - "$APP"
echo "Built $APP"
