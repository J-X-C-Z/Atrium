#!/usr/bin/env bash
# Build the native release app and a ZIP that preserves its executable and signature.
set -euo pipefail
cd "$(dirname "$0")/.."

case "$(uname -s):$(uname -m)" in
  Darwin:arm64) TARGET=aarch64-apple-darwin ;;
  Darwin:x86_64) TARGET=x86_64-apple-darwin ;;
  *) echo "Build Atrium.app on macOS." >&2; exit 1 ;;
esac

METADATA=$(cargo metadata --locked --no-deps --format-version 1)
VERSION=$(python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="atrium-app"))' <<< "$METADATA")
TARGET_DIR=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])' <<< "$METADATA")
MIN_MACOS=12.0
MACOSX_DEPLOYMENT_TARGET="$MIN_MACOS" cargo build --locked --release --target "$TARGET" -p atrium-app --bin atrium

mkdir -p dist
STAGE=$(mktemp -d dist/.atrium-bundle.XXXXXX)
trap 'rm -rf "$STAGE"' EXIT
APP="$STAGE/Atrium.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$TARGET_DIR/$TARGET/release/atrium" "$APP/Contents/MacOS/Atrium"
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
  <key>LSMinimumSystemVersion</key><string>${MIN_MACOS}</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist"
codesign --force --sign - --timestamp=none "$APP"
ZIP="Atrium-macos-$(uname -m).zip"
ditto -c -k --keepParent "$APP" "$STAGE/$ZIP"
ditto -x -k "$STAGE/$ZIP" "$STAGE/verify"
EXTRACTED="$STAGE/verify/Atrium.app"
codesign --verify --strict "$EXTRACTED"
python3 script/smoke-desktop.py "$EXTRACTED/Contents/MacOS/Atrium"

rm -rf dist/Atrium.app
mv "$APP" dist/Atrium.app
mv "$STAGE/$ZIP" "dist/$ZIP"
echo "Built dist/Atrium.app and dist/$ZIP"
