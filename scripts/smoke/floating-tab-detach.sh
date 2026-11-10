#!/bin/sh
# Manual regression check (macOS): a window-anchored floating pane stays in
# its window when the visible tab is moved to a new window
# (docs/specs/floating-panes.md L2.3).
#
# Runs a debug build in an isolated HOME so the user's session file and
# config are untouched, drives it with synthetic key events, and saves one
# screenshot per window. PASS: the float (title strip + "WINFLOAT") is in
# orig.png and absent from detached_*.png.
#
# Needs: `cargo build -p noa`, swiftc, and Accessibility + Screen Recording
# permission for the terminal running this script.
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${TMPDIR:-/tmp}/noa-smoke-floating-detach
rm -rf "$WORK"
mkdir -p "$WORK/home/.config/noa"
cat > "$WORK/home/.config/noa/config" <<'CONFIG'
window-save-state = never
keybind = cmd+alt+3=float.cycle-anchor
keybind = cmd+alt+6=tab.move-to-new-window
CONFIG
swiftc -O "$ROOT/scripts/smoke/cgevent.swift" -o "$WORK/cgevent"

HOME="$WORK/home" XDG_CONFIG_HOME="$WORK/home/.config" \
  "$ROOT/target/debug/Noa" --cols 100 --rows 30 > "$WORK/noa.log" 2>&1 &
PID=$!
trap 'kill "$PID" 2>/dev/null || true' EXIT
sleep 3

key() { "$WORK/cgevent" "$PID" key "$@"; sleep 0.8; }
ORIG=$("$WORK/cgevent" "$PID" windows | head -1 | cut -d' ' -f1)
key 16 cmd shift          # cmd+shift+y: new float
key 20 cmd alt            # pane -> tab
key 20 cmd alt            # tab -> window
"$WORK/cgevent" "$PID" type "echo WINFLOAT"
key 36                    # return
key 17 cmd                # cmd+t: new tab
sleep 1
key 22 cmd alt            # move the visible tab to a new window
sleep 1.5

for window in $("$WORK/cgevent" "$PID" windows | cut -d' ' -f1); do
  if [ "$window" = "$ORIG" ]; then name=orig; else name=detached_$window; fi
  screencapture -x -l"$window" "$WORK/$name.png"
  echo "$WORK/$name.png"
done
