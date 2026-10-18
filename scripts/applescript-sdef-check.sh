#!/usr/bin/env bash
# agent-automation AC-033: with Ghostty's `close tab` / `close window`
# commands in the dictionary, the legacy `close tab 2 of window 1` stops
# compiling (spec Amendment 2, a documented breaking change) while the new
# forms compile and round-trip. Compiles each form against the bundled
# dictionary and decompiles it. Needs Apple Events (run outside any sandbox),
# not a running Noa.
#
#   scripts/bundle-macos.sh
#   scripts/applescript-sdef-check.sh target/release/Noa.app
#
set -uo pipefail

APP_PATH="$(cd "$(dirname "${1:-target/release/Noa.app}")" && pwd)/$(basename "${1:-target/release/Noa.app}")"
if [ ! -d "$APP_PATH" ]; then
  echo "no app bundle at $APP_PATH (run scripts/bundle-macos.sh first)" >&2
  exit 2
fi
WORK="$(mktemp -d "${TMPDIR:-/tmp}/noa-sdef.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

fail=0
roundtrip() {
  # roundtrip <label> <statement> <expected decompiled statement (regex)>
  local label="$1" statement="$2" want="$3" src="$WORK/$1.applescript" out="$WORK/$1.scpt" got
  printf 'tell application "%s"\n%s\nend tell\n' "$APP_PATH" "$statement" > "$src"
  if ! osacompile -o "$out" "$src" 2>"$WORK/$1.err"; then
    echo "FAIL  $label: does not compile: $(cat "$WORK/$1.err")"
    fail=1
    return
  fi
  got="$(osadecompile "$out" | sed -n 2p | sed 's/^[[:space:]]*//')"
  if printf '%s' "$got" | grep -qE -- "$want"; then
    echo "PASS  $label: $got"
  else
    echo "FAIL  $label: decompiled to '$got' (want /$want/)"
    fail=1
  fi
}

must_not_compile() {
  # must_not_compile <label> <statement>
  local label="$1" src="$WORK/$1.applescript"
  printf 'tell application "%s"\n%s\nend tell\n' "$APP_PATH" "$2" > "$src"
  if osacompile -o "$WORK/$1.scpt" "$src" 2>/dev/null; then
    echo "FAIL  $label: compiled, but Amendment 2 documents it as removed"
    fail=1
  else
    echo "PASS  $label: no longer compiles (documented breaking change)"
  fi
}

must_not_compile legacy-close-tab 'close tab 2 of window 1'
roundtrip close-tab-command 'close tab (tab 2 of window 1)' '^close tab \(tab 2 of window 1\)$'
roundtrip close-specifier 'close (tab 2 of window 1)' '^close (\()?tab 2 of window 1(\))?$'
roundtrip legacy-close-window 'close window 1' '^close window 1$'
roundtrip close-window-command 'close window (window 1)' '^close window \(window 1\)$'
roundtrip send-key 'send key "enter" modifiers "control" to terminal 1 of tab 1 of window 1' '^send key "enter"'
roundtrip new-tab-in 'new tab in window 1 with configuration {font size:20, wait after command:true}' '^new tab in window 1 with configuration'
roundtrip contents 'contents of terminal 1 of tab 1 of window 1' '^contents of terminal 1'

[ "$fail" -eq 0 ]
