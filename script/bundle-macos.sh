#!/usr/bin/env bash
# Builds Atrium.app (release) into dist/. Unsigned: right-click > Open the first time,
# or sign it with `codesign --deep -s "Developer ID Application: ..." dist/Atrium.app`.
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="atrium-app"))')

cargo build --release -p atrium-app

APP=dist/Atrium.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/atrium "$APP/Contents/MacOS/Atrium"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Atrium</string>
  <key>CFBundleDisplayName</key><string>Atrium</string>
  <key>CFBundleIdentifier</key><string>dev.atrium</string>
  <key>CFBundleExecutable</key><string>Atrium</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST
echo "Built $APP"
