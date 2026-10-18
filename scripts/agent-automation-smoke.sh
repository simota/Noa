#!/usr/bin/env bash
# Real-machine smoke checks for the agent-automation spec
# (docs/specs/agent-automation.md, acceptance TC-002…TC-031). Drives a
# *running, bundled, TCC-authorized* Noa.app via `osascript` and System
# Events; it cannot run headless or in CI.
#
#   scripts/bundle-macos.sh && open target/release/Noa.app
#   scripts/agent-automation-smoke.sh
#
# Run it from a terminal that holds Automation (Noa, System Events) and
# Accessibility permission. Leave Noa alone while it runs: it creates and
# closes windows, tabs and splits. Each check prints PASS/FAIL with its TC id.
# TC-007 (font size) and TC-014 (Dvorak) are visual and not covered here; the
# existing scripts/applescript-smoke.sh covers TC-024.
set -uo pipefail

APP="Noa"
pass=0
fail=0
WORK="$(mktemp -d "${TMPDIR:-/tmp}/noa-smoke.XXXXXX")"

osa() { osascript -e "$1" 2>&1; }
# Run a multi-line AppleScript from stdin.
osa_block() { osascript - 2>&1; }

ok() { echo "PASS  $1  ${2:-}"; pass=$((pass + 1)); }
bad() { echo "FAIL  $1  ${2:-}"; fail=$((fail + 1)); }

contains() {
  # contains <tc> <haystack> <needle>...
  local tc="$1" hay="$2"
  shift 2
  for needle in "$@"; do
    if ! printf '%s' "$hay" | grep -qF -- "$needle"; then
      bad "$tc" "(missing '$needle' in: $(printf '%s' "$hay" | tail -c 300))"
      return 1
    fi
  done
  ok "$tc"
}

expect_errno() {
  # expect_errno <tc> <errno> <applescript>
  local tc="$1" want="$2" got
  got="$(osa "$3")"
  if printf '%s' "$got" | grep -qF -- "($want)"; then
    ok "$tc" "($want)"
  else
    bad "$tc" "(want error $want, got: $got)"
  fi
}

# A fresh window with one terminal, made front; prints its window id.
fresh_window() {
  osa_block <<EOF
tell application "$APP"
  activate
  set w to new window
  delay 1
  return id of w
end tell
EOF
}

close_window_id() { osa "tell application \"$APP\" to close window (window id $1)" >/dev/null; }

# AX elements of the front Noa window: one line per terminal,
# "identifier|role description|focused|x,y|w,h|value". The value's own line
# breaks are flattened to "⏎" so every output line is one element.
ax_terminals() {
  osa_block <<'EOF'
on flat(t)
  set saved to AppleScript's text item delimiters
  set AppleScript's text item delimiters to {linefeed, return}
  set parts to text items of t
  set AppleScript's text item delimiters to "⏎"
  set r to parts as text
  set AppleScript's text item delimiters to saved
  return r
end flat
set out to ""
tell application "System Events" to tell process "Noa"
  repeat with e in (entire contents of window 1)
    try
      if role of e is "AXTextArea" then
        set p to position of e
        set s to size of e
        set out to out & (value of attribute "AXIdentifier" of e) & "|" & (role description of e) & "|" & (value of attribute "AXFocused" of e) & "|" & (item 1 of p) & "," & (item 2 of p) & "|" & (item 1 of s) & "," & (item 2 of s) & "|" & (my flat(value of e)) & linefeed
      end if
    end try
  end repeat
end tell
return out
EOF
}

echo "== Noa agent-automation smoke =="
osa "tell application \"$APP\" to activate" >/dev/null

# TC-002: pid / tty.
w="$(fresh_window)"
info="$(osa_block <<EOF
tell application "$APP"
  set t to focused terminal of selected tab of window id $w
  return (pid of t as text) & "|" & (tty of t)
end tell
EOF
)"
pid="${info%%|*}"
tty="${info##*|}"
shell_name="$(basename "${SHELL:-zsh}")"
shell_pid="$(ps -o pid=,comm= -t "${tty#/dev/}" | awk -v s="$shell_name" '{c=$2; sub(/^-/, "", c); n=split(c, a, "/"); if (a[n] == s) print $1}' | head -1)"
if [ -n "$pid" ] && [ "$pid" = "$shell_pid" ] && [[ "$tty" == /dev/ttys* ]]; then
  ok TC-002 "(pid $pid, $tty)"
else
  bad TC-002 "(pid '$pid' vs shell '$shell_pid', tty '$tty')"
fi

# TC-003: focused terminal of the front window matches the AX-focused pane.
got="$(osa_block <<EOF
tell application "$APP"
  activate
  set t to split (focused terminal of selected tab of window id $w) direction right
  delay 1
  focus t
  delay 0.5
  return id of focused terminal of selected tab of front window
end tell
EOF
)"
sleep 0.3
ax_focused="$(ax_terminals | awk -F'|' '$3 == "true" {print $1}')"
if [ "noa-terminal-$got" = "$ax_focused" ]; then ok TC-003 "($ax_focused)"; else bad TC-003 "(AppleScript $got vs AX $ax_focused)"; fi

# TC-019 / TC-020(a,b): AX tree, roles, identifiers, focus.
ax="$(ax_terminals)"
count="$(printf '%s' "$ax" | grep -c '^noa-terminal-')"
roles="$(printf '%s' "$ax" | awk -F'|' '$2 != "terminal"' | grep -c . || true)"
if [ "$count" -eq 2 ] && [ "$roles" -eq 0 ]; then ok "TC-019 (2 terminals, role description terminal)"; else bad TC-019 "($ax)"; fi
focused_count="$(printf '%s' "$ax" | awk -F'|' '$3 == "true"' | grep -c . || true)"
if [ "$focused_count" -eq 1 ]; then ok "TC-020a"; else bad TC-020a "($focused_count focused)"; fi
osa "tell application \"Finder\" to activate" >/dev/null
sleep 0.5
focused_count="$(ax_terminals | awk -F'|' '$3 == "true"' | grep -c . || true)"
if [ "$focused_count" -eq 0 ]; then ok "TC-020b"; else bad TC-020b "($focused_count focused while inactive)"; fi
osa "tell application \"$APP\" to activate" >/dev/null
sleep 0.3

# TC-020(c..f): elements follow split / close / resize.
osa "tell application \"$APP\" to split (focused terminal of selected tab of window id $w) direction down" >/dev/null
sleep 0.25
n3="$(ax_terminals | grep -c '^noa-terminal-')"
osa "tell application \"$APP\" to close (focused terminal of selected tab of window id $w)" >/dev/null
sleep 0.25
n2="$(ax_terminals | grep -c '^noa-terminal-')"
osa "tell application \"System Events\" to set size of window 1 of process \"Noa\" to {900, 600}" >/dev/null
sleep 0.25
widths="$(ax_terminals | awk -F'|' '{split($5, s, ","); total += s[1]} END {print total}')"
if [ "$n3" -eq 3 ] && [ "$n2" -eq 2 ] && [ "${widths:-0}" -gt 800 ] && [ "${widths:-0}" -le 900 ]; then
  ok TC-020cdef "(3 -> 2 elements, widths sum $widths in a 900pt window)"
else
  bad TC-020cdef "(after split $n3, after close $n2, widths $widths)"
fi
close_window_id "$w"

# TC-005: with configuration on all three creation verbs.
got="$(osa_block <<EOF
tell application "$APP"
  activate
  set cfg to {initial working directory:"/usr", environment variables:{"FOO=bar"}, initial input:"printf 'CFG-%s-%s\\\\n' \"\$PWD\" \"\$FOO\"\n"}
  set w to new window with configuration cfg
  set wt to focused terminal of selected tab of w
  set b to new tab in w with configuration cfg
  set bt to focused terminal of b
  set s to split bt direction right with configuration cfg
  delay 2
  set out to (contents of wt) & "@@" & (contents of bt) & "@@" & (contents of s)
  close window w
  return out
end tell
EOF
)"
if [ "$(printf '%s' "$got" | grep -o 'CFG-/usr-bar' | wc -l)" -ge 3 ]; then ok TC-005; else bad TC-005 "($got)"; fi

# TC-006: wait after command.
got="$(osa_block <<EOF
tell application "$APP"
  activate
  set a to new tab with configuration {command:"/bin/echo done", wait after command:true}
  set ta to focused terminal of a
  set c to new tab with configuration {wait after command:true, initial input:"exit\n"}
  set ct to focused terminal of c
  set d to new tab with configuration {command:"/bin/echo done"}
  set dt to focused terminal of d
  delay 2
  -- Judge inside AppleScript: contents span several lines.
  set heldA to ((contents of ta) contains "done") and ((contents of ta) contains "Process exited")
  set heldC to (contents of ct) contains "Process exited"
  set goneD to not (exists dt)
  send key "a" to ta
  send key "a" to ct
  delay 1
  return (heldA as text) & "," & (heldC as text) & "," & (goneD as text) & "," & ((not (exists ta)) as text) & "," & ((not (exists ct)) as text)
end tell
EOF
)"
if [ "$got" = "true,true,true,true,true" ]; then
  ok TC-006
else
  bad TC-006 "($got)"
fi

# TC-008: new tab in window 2.
w1="$(fresh_window)"
w2="$(fresh_window)"
got="$(osa_block <<EOF
tell application "$APP"
  set before1 to count tabs of window id $w1
  set before2 to count tabs of window id $w2
  new tab in window id $w2
  delay 1
  return (before1 as text) & "," & (before2 as text) & "->" & (count tabs of window id $w1) & "," & (count tabs of window id $w2)
end tell
EOF
)"
if [ "$got" = "1,1->1,2" ]; then ok TC-008; else bad TC-008 "($got)"; fi
close_window_id "$w1"
close_window_id "$w2"

# TC-009 / TC-011: the targeted pane is the one that splits (left = terminal 1).
w="$(fresh_window)"
osa "tell application \"$APP\" to split right" >/dev/null
sleep 0.5
left_x() { ax_terminals | awk -F'|' '{split($4, p, ","); print p[1]}' | sort -n | head -1; }
x0="$(left_x)"
osa "tell application \"$APP\" to split (terminal 1 of selected tab of window id $w) direction down" >/dev/null
sleep 0.5
lefts="$(ax_terminals | awk -F'|' -v x="$x0" '{split($4, p, ","); if (p[1] == x) n++} END {print n + 0}')"
osa "tell application \"$APP\" to split right" >/dev/null
sleep 0.5
total="$(ax_terminals | grep -c '^noa-terminal-')"
if [ "$lefts" -eq 2 ] && [ "$total" -eq 4 ]; then ok TC-009; else bad TC-009 "($lefts panes at left edge, $total total)"; fi
close_window_id "$w"

w="$(fresh_window)"
osa "tell application \"$APP\" to split right" >/dev/null
sleep 0.5
x0="$(left_x)"
r="$(osa "tell application \"$APP\" to perform action \"new_split:down\" on terminal 1 of selected tab of window id $w")"
sleep 0.5
lefts="$(ax_terminals | awk -F'|' -v x="$x0" '{split($4, p, ","); if (p[1] == x) n++} END {print n + 0}')"
if [ "$r" = "true" ] && [ "$lefts" -eq 2 ]; then ok TC-011 "(on terminal 1 split the left pane)"; else bad TC-011 "(returned $r, $lefts at left edge)"; fi
expect_errno "TC-011 unknown action" -1708 "tell application \"$APP\" to perform action \"nonexistent\""
close_window_id "$w"

# TC-010: references returned by creation verbs work at once.
w0="$(fresh_window)"
got="$(osa_block <<EOF
tell application "$APP"
  activate
  set t to split (focused terminal of selected tab of front window) direction right
  input text "printf 'NOA-%s\\\\n' AC10\n" to t
  set w to new window
  set wt to focused terminal of selected tab of w
  input text "printf 'NOA-%s\\\\n' W10\n" to wt
  set b to new tab in w
  input text "printf 'NOA-%s\\\\n' B10\n" to focused terminal of b
  set bt to focused terminal of b
  delay 2
  set out to (contents of t) & "@@" & (contents of wt) & "@@" & (contents of bt)
  set out to out & "@@" & ((id of w) = (id of front window)) & "@@" & ((id of b) = (id of selected tab of front window))
  close window w
  return out
end tell
EOF
)"
if printf '%s' "$got" | grep -q 'NOA-AC10' && printf '%s' "$got" | grep -q 'NOA-W10' \
  && printf '%s' "$got" | grep -q 'NOA-B10' && printf '%s' "$got" | grep -q '@@true@@true$'; then
  ok TC-010
else
  bad TC-010 "($got)"
fi
close_window_id "$w0"

# TC-013: send key through the encoder, including DECCKM.
w="$(fresh_window)"
got="$(osa_block <<EOF
tell application "$APP"
  set t to focused terminal of selected tab of window id $w
  input text "cat -v\n" to t
  delay 0.5
  send key "arrowUp" to t
  send key "enter" to t
  send key "c" modifiers "control" to t
  delay 0.5
  input text "printf '\\\\e[?1h'; cat -v\n" to t
  delay 0.5
  send key "arrowUp" to t
  send key "enter" to t
  send key "c" modifiers "control" to t
  delay 0.5
  input text "printf 'DONE-%s\\\\n' 13\n" to t
  delay 0.5
  return contents of t
end tell
EOF
)"
contains TC-013 "$got" '^[[A' '^[OA' 'DONE-13'

# TC-015: mouse reports land on one interior cell; the system pointer stays.
before="$(osa 'use framework "AppKit"
set p to current application'"'"'s NSEvent'"'"'s mouseLocation()
return (x of p as text) & "," & (y of p as text)')"
got="$(osa_block <<EOF
tell application "$APP"
  activate
  set t to focused terminal of selected tab of window id $w
  input text "clear; printf '\\\\e[?1000h\\\\e[?1006h'; cat -v\n" to t
  delay 0.5
  send mouse position x 300 y 120 to t
  send mouse button left button to t
  send mouse button left button action release to t
  send mouse scroll x 0 y 1 to t
  delay 0.5
  set out to contents of t
  send key "c" modifiers "control" to t
  input text "printf '\\\\e[?1000l\\\\e[?1006l'\n" to t
  return out
end tell
EOF
)"
after="$(osa 'use framework "AppKit"
set p to current application'"'"'s NSEvent'"'"'s mouseLocation()
return (x of p as text) & "," & (y of p as text)')"
cell="$(printf '%s' "$got" | grep -o '\^\[\[<0;[0-9]*;[0-9]*M' | head -1 | sed 's/.*<0;\([0-9]*;[0-9]*\)M/\1/')"
if [ -n "$cell" ] && printf '%s' "$got" | grep -qF "^[[<0;${cell}m" && printf '%s' "$got" | grep -qF "^[[<64;${cell}M" \
  && [ "${cell%%;*}" -gt 1 ] && [ "${cell##*;}" -gt 1 ] && [ "$before" = "$after" ]; then
  ok TC-015 "(cell $cell, pointer unmoved)"
else
  bad TC-015 "(cell '$cell', pointer $before -> $after, contents: $(printf '%s' "$got" | tail -c 200))"
fi

# TC-016: a wheel without mouse tracking scrolls the viewport back.
got="$(osa_block <<EOF
tell application "$APP"
  set t to focused terminal of selected tab of window id $w
  input text "clear; seq 1 5000\n" to t
  delay 1
  set firstLine to paragraph 1 of (contents of t)
  send mouse scroll x 0 y 5 to t
  delay 0.3
  return firstLine & "|" & (paragraph 1 of (contents of t))
end tell
EOF
)"
b="${got%%|*}"; a="${got##*|}"
if [[ "$b" =~ ^[0-9]+$ && "$a" =~ ^[0-9]+$ ]] && [ "$a" -lt "$b" ]; then ok TC-016 "($b -> $a)"; else bad TC-016 "($got)"; fi

# TC-031: scrollback and a real selection (double-click on a printed word).
got="$(osa_block <<EOF
tell application "$APP"
  set t to focused terminal of selected tab of window id $w
  input text "clear; printf 'SEL-%s\\\\n' X\n" to t
  delay 0.5
  send mouse position x 12 y 6 to t
  send mouse button left button to t
  send mouse button left button action release to t
  send mouse button left button to t
  send mouse button left button action release to t
  delay 0.3
  set sb to scrollback of t
  return (length of sb as text) & "|" & (sb contains "5000") & "|" & (sb contains "SEL-X") & "|" & (selection of t)
end tell
EOF
)"
IFS='|' read -r sblen has5000 hassel sel <<< "$got"
if [ "${sblen:-0}" -le 262144 ] && [ "$has5000" = "true" ] && [ "$hassel" = "true" ] && [ -n "$sel" ]; then
  ok TC-031 "(scrollback $sblen chars, selection '$sel')"
else
  bad TC-031 "($got)"
fi

# TC-018: full-width text under Secure Keyboard Entry is not hidden.
got="$(osa_block <<EOF
tell application "$APP"
  set t to focused terminal of selected tab of window id $w
  input text "clear; printf '%s-%s\\\\n' あいう AC18\n" to t
  delay 0.5
  return contents of t
end tell
EOF
)"
n="$(printf '%s' "$got" | grep -o 'あいう-AC18' | wc -l | tr -d ' ')"
if [ "$n" = "1" ]; then ok TC-018 "(turn Secure Keyboard Entry on in Noa first)"; else bad TC-018 "($n occurrences)"; fi

# TC-021: selected text and insertion point via AX.
osa "tell application \"$APP\" to input text \"clear; printf 'hello\\\\e[5;1H'; cat\n\" to focused terminal of selected tab of window id $w" >/dev/null
sleep 0.5
osa_block >/dev/null <<EOF
tell application "$APP"
  set t to focused terminal of selected tab of window id $w
  send mouse position x 10 y 6 to t
  send mouse button left button to t
  send mouse button left button action release to t
  send mouse button left button to t
  send mouse button left button action release to t
end tell
EOF
got="$(osa_block <<'EOF'
tell application "System Events" to tell process "Noa"
  repeat with e in (entire contents of window 1)
    try
      if role of e is "AXTextArea" then return (value of attribute "AXSelectedText" of e) & "|" & (value of attribute "AXInsertionPointLineNumber" of e)
    end try
  end repeat
end tell
EOF
)"
as_sel="$(osa "tell application \"$APP\" to selection of focused terminal of selected tab of window id $w")"
if [ "$got" = "hello|4" ] && [ "$as_sel" = "hello" ]; then ok TC-021; else bad TC-021 "(AX $got, AppleScript '$as_sel')"; fi
osa "tell application \"$APP\" to send key \"c\" modifiers \"control\" to focused terminal of selected tab of window id $w" >/dev/null

# TC-027: invalid input is rejected with the spec's error numbers.
t="focused terminal of selected tab of window id $w"
expect_errno "TC-027 unknown key" -1700 "tell application \"$APP\" to send key \"foo\" to $t"
expect_errno "TC-027 unknown modifier" -1700 "tell application \"$APP\" to send key \"a\" modifiers \"hyper\" to $t"
expect_errno "TC-027 bad env entry" -1700 "tell application \"$APP\" to new tab with configuration {environment variables:{\"NOEQ\"}}"
expect_errno "TC-027 missing y" -1715 "tell application \"$APP\" to send mouse position x 1 to $t"
gone="$(osa_block <<EOF
tell application "$APP"
  set s to split ($t) direction right
  delay 1
  set i to id of s
  close s
  delay 1
  return i
end tell
EOF
)"
expect_errno "TC-027 send key to closed terminal" -1728 "tell application \"$APP\" to send key \"a\" to terminal id $gone of selected tab of window id $w"
expect_errno "TC-027 contents of closed terminal" -1728 "tell application \"$APP\" to contents of terminal id $gone of selected tab of window id $w"
if pgrep -x Noa >/dev/null || pgrep -f 'Noa.app/Contents/MacOS' >/dev/null; then ok "TC-027 app alive"; else bad "TC-027 app alive"; fi
close_window_id "$w"

# TC-029: close tab / close window (both forms), count, exists.
w="$(fresh_window)"
got="$(osa_block <<EOF
tell application "$APP"
  new tab in window id $w
  delay 1
  set out to (count tabs of window id $w) as text
  set out to out & "," & (exists tab 3 of window id $w) & "," & (exists tab 1 of window id $w)
  close tab (tab 2 of window id $w)
  delay 1
  set out to out & "," & (count tabs of window id $w)
  new tab in window id $w
  delay 1
  set windowCount to count windows
  return out & "|" & windowCount
end tell
EOF
)"
legacy="$(osa "tell application \"$APP\" to close (tab 2 of window id $w)")"
sleep 1
tabs_after="$(osa "tell application \"$APP\" to count tabs of window id $w")"
before_windows="${got##*|}"
close_window_id "$w"
sleep 1
after_windows="$(osa "tell application \"$APP\" to count windows")"
if [ "${got%%|*}" = "2,false,true,1" ] && [ "$tabs_after" = "1" ] && [ "$after_windows" -eq $((before_windows - 1)) ]; then
  ok TC-029 "(close (tab 2 …): '${legacy:-ok}')"
else
  bad TC-029 "($got, legacy '$legacy' -> $tabs_after tabs, windows $before_windows -> $after_windows)"
fi

rm -rf "$WORK"
echo "== $pass passed, $fail failed =="
echo "Manual: TC-007 (font size), TC-014 (Dvorak), TC-026 (restart with macos-applescript = false)."
[ "$fail" -eq 0 ]
