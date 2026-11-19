#!/usr/bin/env bash
# Builds Atrium.app (release) into dist/. Unsigned: right-click > Open the first time,
# or sign it with `codesign --deep -s "Developer ID Application: ..." dist/Atrium.app`.
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
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize"}' \
  | "$APP/Contents/MacOS/Atrium" mcp --project "$STAGE" --stream package-smoke \
  | python3 -c 'import json,sys; r=json.load(sys.stdin); assert r["jsonrpc"] == "2.0" and r["id"] == 1 and r["result"]["serverInfo"]["name"] == "atrium", r'

rm -rf dist/Atrium.app
mv "$APP" dist/Atrium.app
echo "Built dist/Atrium.app"
