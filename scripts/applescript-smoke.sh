#!/usr/bin/env bash
# AppleScript smoke checks for the Noa Apple Event bridge (applescript spec
# R-13 / AC-13). Manual / real-machine use only — it drives a *running,
# bundled, TCC-authorized* Noa.app via `osascript` and cannot run headless or
# in CI. Build and launch the bundle first:
#
#   scripts/bundle-macos.sh && open target/release/Noa.app
#   scripts/applescript-smoke.sh
#
# The first scripted command triggers the macOS Automation (TCC) prompt; grant
# it, then re-run. Each check prints PASS/FAIL; `input text` is observed by eye
# in the target terminal. Covers AC-3/5/6/9/10/15/16.
set -uo pipefail

APP="Noa"
pass=0
fail=0

osa() { osascript -e "$1" 2>&1; }

check() {
  # check <label> <expr> <expected-substring>
  local label="$1" expr="$2" want="${3:-}"
  local got
  got="$(osa "$expr")"
  if [ -n "$want" ]; then
    if printf '%s' "$got" | grep -qF -- "$want"; then
      echo "PASS  $label  ($got)"; pass=$((pass + 1))
    else
      echo "FAIL  $label  (got: $got, want ~ $want)"; fail=$((fail + 1))
    fi
  else
    if printf '%s' "$got" | grep -qiE "error|missing|doesn.t understand"; then
      echo "FAIL  $label  ($got)"; fail=$((fail + 1))
    else
      echo "PASS  $label  ($got)"; pass=$((pass + 1))
    fi
  fi
}

expect_error() {
  # expect_error <label> <expr>
  local label="$1" expr="$2" got
  got="$(osa "$expr")"
  if printf '%s' "$got" | grep -qiE "error|doesn.t understand|not handled|-1708"; then
    echo "PASS  $label  (rejected: $got)"; pass=$((pass + 1))
  else
    echo "FAIL  $label  (expected an AppleScript error, got: $got)"; fail=$((fail + 1))
  fi
}

echo "== Noa AppleScript smoke =="
osa "tell application \"$APP\" to activate" >/dev/null

# AC-3: new window / new tab with an initial working directory.
before="$(osa "tell application \"$APP\" to count windows")"
osa "tell application \"$APP\" to new window" >/dev/null
after="$(osa "tell application \"$APP\" to count windows")"
if [ "$after" -gt "$before" ] 2>/dev/null; then
  echo "PASS  AC-3 new window increases window count ($before -> $after)"; pass=$((pass + 1))
else
  echo "FAIL  AC-3 new window ($before -> $after)"; fail=$((fail + 1))
fi
osa "tell application \"$APP\" to new tab with properties {initial working directory:\"/tmp\"}" >/dev/null || \
  osa "tell application \"$APP\" to new tab initial working directory \"/tmp\"" >/dev/null
sleep 1
check "AC-3 new tab cwd is /tmp" \
  "tell application \"$APP\" to get working directory of terminal 1 of tab 1 of window 1" "/tmp"

# AC-5: split the focused terminal in each direction.
for dir in right left down up; do
  check "AC-5 split $dir" "tell application \"$APP\" to split $dir"
done

# AC-6: select tab / activate window.
check "AC-6 select tab 1 of window 1" "tell application \"$APP\" to select tab 1 of window 1"
check "AC-6 activate window 1" "tell application \"$APP\" to activate window 1"

# AC-9: perform a known action, reject an unknown one.
check "AC-9 perform toggle_fullscreen" "tell application \"$APP\" to perform action \"toggle_fullscreen\""
expect_error "AC-9 perform nonexistent rejected" "tell application \"$APP\" to perform action \"nonexistent\""

# AC-10: property reads reflect real state.
check "AC-10 application version" "tell application \"$APP\" to get version" ""
check "AC-10 application frontmost" "tell application \"$APP\" to get frontmost" ""
check "AC-10 tab index" "tell application \"$APP\" to get index of tab 1 of window 1" ""
check "AC-10 tab selected" "tell application \"$APP\" to get selected of tab 1 of window 1" ""
check "AC-10 working directory" \
  "tell application \"$APP\" to get working directory of terminal 1 of tab 1 of window 1" ""

# AC-15/AC-16: focus then close a specific split pane (needs >=2 terminals; the
# AC-5 splits above created them). Observe the cursor move for AC-15 by eye.
check "AC-15 focus terminal 2" "tell application \"$APP\" to focus terminal 2 of tab 1 of window 1"
check "AC-16 close terminal 2" "tell application \"$APP\" to close terminal 2 of tab 1 of window 1"

# AC-13 / R-7: input text lands on the pty (observe the target terminal by eye).
osa "tell application \"$APP\" to input text \"echo noa-applescript-smoke\n\"" >/dev/null
echo "NOTE  input text sent — confirm 'echo noa-applescript-smoke' ran in the focused terminal"

echo "== $pass passed, $fail failed =="
[ "$fail" -eq 0 ]
