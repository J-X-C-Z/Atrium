#!/usr/bin/env bash
# Build the native release app, an AppImage and a Debian package on Linux.
# APPIMAGETOOL must point at appimagetool; the release workflow downloads it.
# Set APPIMAGETOOL_EXTRACT_AND_RUN=1 when the tool is a FUSE AppImage and the
# runner has no FUSE.
set -euo pipefail
cd "$(dirname "$0")/.."

CHECK=0
for arg in "$@"; do
  case "$arg" in
    --check) CHECK=1 ;;
    -h|--help)
      echo "usage: $0 [--check]"
      echo "  --check  print the toolchain and output names without building"
      exit 0
      ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

case "$(uname -s):$(uname -m)" in
  Linux:x86_64) TARGET=x86_64-unknown-linux-gnu; ARCH=x86_64; DEB_ARCH=amd64; APPIMAGE_ARCH=x86_64 ;;
  Linux:aarch64|Linux:arm64) TARGET=aarch64-unknown-linux-gnu; ARCH=aarch64; DEB_ARCH=arm64; APPIMAGE_ARCH=aarch64 ;;
  *) echo "Build the Linux packages on Linux x86_64 or aarch64." >&2; exit 1 ;;
esac

METADATA=$(cargo metadata --locked --no-deps --format-version 1)
VERSION=$(python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="atrium-app"))' <<< "$METADATA")
TARGET_DIR=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])' <<< "$METADATA")

APPIMAGETOOL="${APPIMAGETOOL:-}"
if [[ -z "$APPIMAGETOOL" && -x "dist/tools/appimagetool" ]]; then
  APPIMAGETOOL="$PWD/dist/tools/appimagetool"
fi
if [[ -z "$APPIMAGETOOL" && -x "dist/tools/appimagetool-x86_64.AppImage" ]]; then
  APPIMAGETOOL="$PWD/dist/tools/appimagetool-x86_64.AppImage"
fi

if [[ "$CHECK" -eq 1 ]]; then
  for tool in cargo python3 install; do
    command -v "$tool" >/dev/null || { echo "missing: $tool" >&2; exit 1; }
  done
  command -v dpkg-deb >/dev/null || echo "warning: dpkg-deb missing; the .deb cannot be built" >&2
  echo "target: $TARGET"
  echo "version: $VERSION"
  echo "appimagetool: ${APPIMAGETOOL:-not found (set APPIMAGETOOL)}"
  echo "outputs: Atrium-$VERSION-linux-$ARCH.AppImage, Atrium-$VERSION-linux-$ARCH.deb"
  exit 0
fi

for tool in cargo python3 install dpkg-deb; do
  command -v "$tool" >/dev/null || { echo "missing: $tool" >&2; exit 1; }
done
if [[ -z "$APPIMAGETOOL" || ! -x "$APPIMAGETOOL" ]]; then
  echo "APPIMAGETOOL must point at an executable appimagetool." >&2
  exit 1
fi

cargo build --locked --release --target "$TARGET" \
  -p atrium-app --bin atrium -p atrium-node --bin atrium-node
BIN="$TARGET_DIR/$TARGET/release/atrium"
NODE_BIN="$TARGET_DIR/$TARGET/release/atrium-node"

mkdir -p dist
STAGE=$(mktemp -d dist/.atrium-linux.XXXXXX)
trap 'rm -rf "$STAGE"' EXIT

# Debian package.
DEB_ROOT="$STAGE/deb"
install -Dm755 "$BIN" "$DEB_ROOT/usr/bin/atrium"
install -Dm755 "$NODE_BIN" "$DEB_ROOT/usr/bin/atrium-node"
install -Dm644 packaging/linux/atrium.desktop "$DEB_ROOT/usr/share/applications/atrium.desktop"
install -Dm644 packaging/icons/atrium.png "$DEB_ROOT/usr/share/icons/hicolor/512x512/apps/atrium.png"
mkdir -p "$DEB_ROOT/DEBIAN"
cat > "$DEB_ROOT/DEBIAN/control" <<CONTROL
Package: atrium
Version: $VERSION
Section: utils
Priority: optional
Architecture: $DEB_ARCH
Maintainer: Atrium
Installed-Size: $(du -sk "$DEB_ROOT/usr" | cut -f1)
Depends: libc6 (>= 2.35)
Recommends: libxkbcommon0, libwayland-client0, libx11-6, libvulkan1
Description: Run Codex and ACP agents as chats or as projects
 Atrium is a desktop workspace for Codex app-server and ACP agents.
 The package also installs atrium-node, the per-user device service.
CONTROL
DEB="Atrium-${VERSION}-linux-${ARCH}.deb"
dpkg-deb --root-owner-group -Zxz --build "$DEB_ROOT" "$STAGE/$DEB" >/dev/null
dpkg-deb --info "$STAGE/$DEB" >/dev/null
dpkg-deb --contents "$STAGE/$DEB" >/dev/null

# AppImage.
APPDIR="$STAGE/AppDir"
install -Dm755 "$BIN" "$APPDIR/usr/bin/atrium"
install -Dm755 "$NODE_BIN" "$APPDIR/usr/bin/atrium-node"
install -Dm644 packaging/linux/atrium.desktop "$APPDIR/usr/share/applications/atrium.desktop"
install -Dm644 packaging/icons/atrium.png "$APPDIR/usr/share/icons/hicolor/512x512/apps/atrium.png"
cp packaging/linux/atrium.desktop "$APPDIR/atrium.desktop"
cp packaging/icons/atrium.png "$APPDIR/atrium.png"
cat > "$APPDIR/AppRun" <<'APPRUN'
#!/bin/sh
HERE=$(dirname "$(readlink -f "$0")")
exec "$HERE/usr/bin/atrium" "$@"
APPRUN
chmod 755 "$APPDIR/AppRun"
APPIMAGE="Atrium-${VERSION}-linux-${ARCH}.AppImage"
if [[ "${APPIMAGETOOL_EXTRACT_AND_RUN:-0}" == "1" ]]; then
  ARCH="$APPIMAGE_ARCH" "$APPIMAGETOOL" --appimage-extract-and-run "$APPDIR" "$STAGE/$APPIMAGE"
else
  ARCH="$APPIMAGE_ARCH" "$APPIMAGETOOL" "$APPDIR" "$STAGE/$APPIMAGE"
fi
test -s "$STAGE/$APPIMAGE"
chmod 755 "$STAGE/$APPIMAGE"

mv "$STAGE/$DEB" "dist/$DEB"
mv "$STAGE/$APPIMAGE" "dist/$APPIMAGE"
echo "Built dist/$DEB"
echo "Built dist/$APPIMAGE"
