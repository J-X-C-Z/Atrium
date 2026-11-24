#!/usr/bin/env bash
# Build the native release app, a ZIP that preserves its executable and
# signature, and optionally a DMG. With ATRIUM_SIGNING_IDENTITY set the app is
# signed with the Developer ID and (when the notary variables are also set)
# notarized; otherwise an ad-hoc signature is used and notarization is skipped.
set -euo pipefail
cd "$(dirname "$0")/.."

DMG=0
CHECK=0
for arg in "$@"; do
  case "$arg" in
    --dmg) DMG=1 ;;
    --check) CHECK=1 ;;
    -h|--help)
      echo "usage: $0 [--dmg] [--check]"
      echo "  --dmg    also build dist/Atrium-<version>-macos-<arch>.dmg"
      echo "  --check  print the toolchain, signing and notarization state without building"
      exit 0
      ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

case "$(uname -s):$(uname -m)" in
  Darwin:arm64) TARGET=aarch64-apple-darwin ;;
  Darwin:x86_64) TARGET=x86_64-apple-darwin ;;
  *) echo "Build Atrium.app on macOS." >&2; exit 1 ;;
esac

MIN_MACOS=12.0
METADATA=$(cargo metadata --locked --no-deps --format-version 1)
VERSION=$(python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="atrium-app"))' <<< "$METADATA")
TARGET_DIR=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])' <<< "$METADATA")

SIGNING_IDENTITY="${ATRIUM_SIGNING_IDENTITY:-}"
NOTARY_READY=0
if [[ -n "$SIGNING_IDENTITY" && -n "${APPLE_ID:-}" && -n "${APPLE_TEAM_ID:-}" && -n "${APPLE_APP_PASSWORD:-}" ]]; then
  NOTARY_READY=1
fi

if [[ "$CHECK" -eq 1 ]]; then
  for tool in cargo python3 plutil codesign; do
    command -v "$tool" >/dev/null || { echo "missing: $tool" >&2; exit 1; }
  done
  if [[ "$DMG" -eq 1 ]]; then command -v hdiutil >/dev/null || { echo "missing: hdiutil" >&2; exit 1; }; fi
  echo "target: $TARGET"
  echo "version: $VERSION"
  echo "signing identity set: $([[ -n "$SIGNING_IDENTITY" ]] && echo yes || echo no)"
  echo "notarization credentials set: $([[ "$NOTARY_READY" -eq 1 ]] && echo yes || echo no)"
  if [[ "$DMG" -eq 1 ]]; then
    echo "dmg: yes"
    command -v xcrun >/dev/null || echo "warning: xcrun missing; notarization cannot run" >&2
  fi
  exit 0
fi

MACOSX_DEPLOYMENT_TARGET="$MIN_MACOS" cargo build --locked --release --target "$TARGET" \
  -p atrium-app --bin atrium -p atrium-node --bin atrium-node

mkdir -p dist
STAGE=$(mktemp -d dist/.atrium-bundle.XXXXXX)
trap 'rm -rf "$STAGE"' EXIT
APP="$STAGE/Atrium.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/bin"
cp "$TARGET_DIR/$TARGET/release/atrium" "$APP/Contents/MacOS/Atrium"
# The desktop app embeds the node, but the standalone binary is shipped too so
# `atrium-node install` can register the user service from the installed app.
cp "$TARGET_DIR/$TARGET/release/atrium-node" "$APP/Contents/Resources/bin/atrium-node"
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
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>dev.atrium.link</string>
      <key>CFBundleURLSchemes</key><array><string>atrium</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist"

if [[ -n "$SIGNING_IDENTITY" ]]; then
  echo "Signing with Developer ID (identity value not printed)."
  codesign --force --options runtime --timestamp --sign "$SIGNING_IDENTITY" \
    "$APP/Contents/Resources/bin/atrium-node"
  codesign --force --options runtime --timestamp --sign "$SIGNING_IDENTITY" "$APP"
else
  echo "ATRIUM_SIGNING_IDENTITY is not set: using an ad-hoc signature; signing and notarization are skipped." >&2
  codesign --force --sign - --timestamp=none "$APP/Contents/Resources/bin/atrium-node"
  codesign --force --sign - --timestamp=none "$APP"
fi
codesign --verify --strict "$APP/Contents/Resources/bin/atrium-node"
codesign --verify --strict "$APP"

ZIP="Atrium-macos-$(uname -m).zip"
ditto -c -k --norsrc --noextattr --keepParent "$APP" "$STAGE/$ZIP"
ditto -x -k "$STAGE/$ZIP" "$STAGE/verify"
EXTRACTED="$STAGE/verify/Atrium.app"
codesign --verify --strict "$EXTRACTED"
codesign --verify --strict "$EXTRACTED/Contents/Resources/bin/atrium-node"
python3 script/smoke-desktop.py "$EXTRACTED/Contents/MacOS/Atrium"
"$EXTRACTED/Contents/Resources/bin/atrium-node" config path >/dev/null
echo "Bundled atrium-node starts and reports its config path."

DMG_NAME=""
if [[ "$DMG" -eq 1 ]]; then
  DMG_NAME="Atrium-${VERSION}-macos-$(uname -m).dmg"
  DMG_STAGE="$STAGE/dmg"
  mkdir -p "$DMG_STAGE"
  cp -R "$APP" "$DMG_STAGE/Atrium.app"
  ln -s /Applications "$DMG_STAGE/Applications"
  hdiutil create -volname "Atrium $VERSION" -srcfolder "$DMG_STAGE" \
    -fs HFS+ -ov -format UDZO "$STAGE/$DMG_NAME" >/dev/null
  if [[ -n "$SIGNING_IDENTITY" ]]; then
    codesign --force --timestamp --sign "$SIGNING_IDENTITY" "$STAGE/$DMG_NAME"
    codesign --verify --strict "$STAGE/$DMG_NAME"
  fi
  if [[ "$NOTARY_READY" -eq 1 ]]; then
    echo "Submitting the DMG for notarization."
    xcrun notarytool submit "$STAGE/$DMG_NAME" \
      --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
      --password "$APPLE_APP_PASSWORD" --wait
    xcrun stapler staple "$STAGE/$DMG_NAME"
    xcrun stapler validate "$STAGE/$DMG_NAME"
  elif [[ -n "$SIGNING_IDENTITY" ]]; then
    echo "Notary credentials are incomplete (APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD): notarization skipped." >&2
  fi
fi

rm -rf dist/Atrium.app
mv "$APP" dist/Atrium.app
mv "$STAGE/$ZIP" "dist/$ZIP"
echo "Built dist/Atrium.app and dist/$ZIP"
if [[ -n "$DMG_NAME" ]]; then
  mv "$STAGE/$DMG_NAME" "dist/$DMG_NAME"
  echo "Built dist/$DMG_NAME"
fi
