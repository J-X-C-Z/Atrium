#!/usr/bin/env bash
# Capture the conversation list in English and Chinese against mock-codex.
#
# macOS (a normal desktop session):
#   script/screenshot-chat-list.sh
# Linux (headless; CI runs this under xvfb-run):
#   script/screenshot-chat-list.sh --linux
#
# Each run gets its own HOME and CODEX_HOME, so it never reads or writes the
# user's settings or mock threads. The images land in docs/evidence/ unless
# --output names another folder.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
target="${CARGO_TARGET_DIR:-$root/target}"
out="$root/docs/evidence"
linux=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --linux) linux=true ;;
        --output) out="$2"; shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done
mkdir -p "$out"

work=$(mktemp -d "${TMPDIR:-/tmp}/atrium-screenshot.XXXXXX")

echo "building atrium and mock-codex…"
# Touch the two entry points so a target directory shared with other
# checkouts cannot hand back their stale binaries.
touch "$root/crates/codex-client/src/bin/mock_codex.rs"
touch "$root/crates/atrium-app/src/main.rs"
cargo build -p codex-client --bin mock-codex
if [ "$linux" = true ]; then
    # Xvfb and ImageMagick capture the window; the in-app renderer is not
    # needed, so CI does not build its extra platform support.
    cargo build -p atrium-app
else
    cargo build -p atrium-app --features screenshots
fi
# The target directory is shared with other checkouts, whose builds can
# replace these paths. Take a copy of the two binaries this run built.
mkdir -p "$work/bin"
cp "$target/debug/mock-codex" "$work/bin/mock-codex"
cp "$target/debug/atrium" "$work/bin/atrium"
app_pid=""
cleanup() {
    if [ -n "$app_pid" ] && kill -0 "$app_pid" 2>/dev/null; then
        kill "$app_pid" 2>/dev/null || true
        wait "$app_pid" 2>/dev/null || true
    fi
    if [ "${ATRIUM_SCREENSHOT_KEEP:-0}" = "1" ]; then
        echo "kept $work"
    else
        rm -rf "$work"
    fi
}
trap cleanup EXIT

write_settings() {
    local home=$1 lang=$2
    local json
    json=$(cat <<EOF
{
  "install_id": "screenshot",
  "device_name": "Screenshot",
  "language": "$lang",
  "theme": "light",
  "sync_enabled": false,
  "pinned": ["demo-pinned"],
  "archived": ["demo-archived"],
  "chat_names": {"demo-pinned": "Launch checklist"}
}
EOF
)
    # macOS and Linux keep application settings in different folders.
    mkdir -p "$home/Library/Application Support/Atrium" "$home/.config/Atrium"
    printf '%s\n' "$json" >"$home/Library/Application Support/Atrium/settings.json"
    printf '%s\n' "$json" >"$home/.config/Atrium/settings.json"
}

wait_for_window() {
    local pid=$1
    if [ "$linux" = false ]; then
        # A desktop session has no reliable window query without the
        # Accessibility permission, so give the first paint enough time.
        sleep 10
        kill -0 "$pid" 2>/dev/null
        return
    fi
    for _ in $(seq 1 60); do
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "atrium exited before its window appeared" >&2
            return 1
        fi
        if xdotool search --onlyvisible --name '^Atrium$' >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    echo "Atrium's window did not appear" >&2
    return 1
}

capture_macos() {
    local file=$1
    local bounds
    bounds=$(osascript -e 'tell application "System Events" to tell process "atrium" to get {position, size} of window 1' 2>/dev/null || true)
    if [ -n "$bounds" ]; then
        local x y w h
        x=$(echo "$bounds" | cut -d, -f1 | tr -d ' ')
        y=$(echo "$bounds" | cut -d, -f2 | tr -d ' ')
        w=$(echo "$bounds" | cut -d, -f3 | tr -d ' ')
        h=$(echo "$bounds" | cut -d, -f4 | tr -d ' ')
        if [ -n "$x" ] && [ -n "$w" ] && [ -n "$h" ]; then
            screencapture -x -R "${x},${y},${w},${h}" "$file"
            return
        fi
    fi
    # No accessibility permission: take the whole screen.
    screencapture -x "$file"
}

capture_linux() {
    local file=$1
    local wid
    wid=$(xdotool search --onlyvisible --name '^Atrium$' | head -n1)
    if [ -n "$wid" ]; then
        import -window "$wid" "$file"
    else
        import -window root "$file"
    fi
}

capture() {
    local name=$1 lang=$2 file=$3
    rm -f "$file"
    local home="$work/$name"
    mkdir -p "$home" "$work/codex-$name"
    write_settings "$home" "$lang"

    local shot=""
    if [ "$linux" = false ]; then
        # macOS has no screen-recording permission here, so the app renders
        # its own window to the PNG and quits.
        shot="$file"
    fi
    HOME="$home" \
    XDG_CONFIG_HOME="$home/.config" \
    CODEX_HOME="$work/codex-$name" \
    CODEX_BIN="$work/bin/mock-codex" \
    ATRIUM_MOCK_CHAT_LIST=1 \
    ATRIUM_SCREENSHOT_WORKING_THREAD=demo-working \
    ATRIUM_SCREENSHOT_TO="$shot" \
    ATRIUM_NO_TAILSCALE=1 \
        "$work/bin/atrium" >"$work/$name.log" 2>&1 &
    app_pid=$!
    wait_for_window "$app_pid"
    if [ "$linux" = true ]; then
        # Let the threads arrive and the row animations settle.
        sleep 6
        capture_linux "$file"
    else
        for _ in $(seq 1 40); do
            [ -s "$file" ] && break
            sleep 1
        done
        if [ ! -s "$file" ]; then
            echo "in-app capture failed; falling back to screencapture" >&2
            capture_macos "$file"
        fi
    fi
    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
    app_pid=""
    if [ ! -s "$file" ]; then
        echo "no screenshot was written to $file" >&2
        return 1
    fi
    echo "wrote $file"
}

capture en "en" "$out/chat-list-en.png"
capture zh "zh-CN" "$out/chat-list-zh.png"
