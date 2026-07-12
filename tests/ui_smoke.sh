#!/usr/bin/env bash
# UI smoke test for yt-gtk.
#
# Runs the app inside a nested X server (Xephyr), drives it with spoofed
# clicks/keystrokes (xdotool), and captures screenshots (ImageMagick `import`)
# so a human or agent can visually diff the results.
#
# The app runs against an isolated throwaway HOME/XDG environment, so it never
# touches real user data. It needs the repo's youtube-subs.txt, which the
# binary locates relative to itself.
#
# Usage: tests/ui_smoke.sh [output-dir]
# Requires: Xephyr, xdotool, import (imagemagick), a built target/debug/yt-gtk.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINARY="$REPO_ROOT/target/debug/yt-gtk"
OUT_DIR="${1:-$(mktemp -d /tmp/yt-gtk-ui-smoke.XXXXXX)}"
DISPLAY_NUM=":97"
SANDBOX_HOME="$(mktemp -d /tmp/yt-gtk-ui-home.XXXXXX)"

for tool in Xephyr xdotool import; do
    command -v "$tool" >/dev/null || { echo "SKIP: $tool not installed" >&2; exit 0; }
done
[[ -x $BINARY ]] || { echo "FAIL: build target/debug/yt-gtk first (cargo build)" >&2; exit 1; }

mkdir -p "$OUT_DIR"
XEPHYR_PID=""
APP_PID=""

cleanup() {
    [[ -n $APP_PID ]] && kill "$APP_PID" 2>/dev/null || true
    [[ -n $XEPHYR_PID ]] && kill "$XEPHYR_PID" 2>/dev/null || true
    rm -rf "$SANDBOX_HOME"
}
trap cleanup EXIT

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

assert_app_alive() {
    kill -0 "$APP_PID" 2>/dev/null || fail "app died during: $1"
}

shot() {
    DISPLAY=$DISPLAY_NUM import -window root "$OUT_DIR/$1.png"
}

click() {
    DISPLAY=$DISPLAY_NUM xdotool mousemove "$1" "$2" click "$3"
    sleep 1
}

Xephyr "$DISPLAY_NUM" -screen 1400x950 >/dev/null 2>&1 &
XEPHYR_PID=$!
sleep 2

HOME="$SANDBOX_HOME" \
    XDG_DATA_HOME="$SANDBOX_HOME/.local/share" \
    XDG_CACHE_HOME="$SANDBOX_HOME/.cache" \
    XDG_CONFIG_HOME="$SANDBOX_HOME/.config" \
    WAYLAND_DISPLAY= GDK_BACKEND=x11 \
    DISPLAY=$DISPLAY_NUM "$BINARY" >"$OUT_DIR/app.log" 2>&1 &
APP_PID=$!
sleep 4

assert_app_alive "startup"
DISPLAY=$DISPLAY_NUM xdotool search --name yt-gtk >/dev/null || fail "window did not appear"
shot 01-startup

# Header view-switcher tabs (1400px-wide window): Feed ~464, Search ~600, Watch Later ~736.
click 736 27 1
assert_app_alive "switching to Watch Later"
shot 02-watch-later

click 600 27 1
assert_app_alive "switching to Search"
# The search entry auto-focuses on page switch; typing must not need a click.
DISPLAY=$DISPLAY_NUM xdotool type --delay 30 "smoke test query"
shot 03-search-focused

click 464 27 1
assert_app_alive "switching back to Feed"

# Right-click inside the page body opens the card context menu when a card is
# present; with an empty sandbox feed it must simply not crash.
click 600 400 3
assert_app_alive "right-click on page body"
shot 04-context-click

echo "PASS: screenshots and app.log in $OUT_DIR"
