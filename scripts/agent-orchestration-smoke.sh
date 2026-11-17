#!/bin/sh
# End-to-end smoke test for `Noa +ctl` (docs/specs/agent-orchestration.md).
# Run it from a shell inside a Noa pane whose config has
#   server-enable = true
#   server-scopes = read,control,input
# It spawns two workers next to this pane, drives them, and closes them.
# Needs no coding agent: the workers report state with `+ctl report`.
set -eu

: "${NOA_BIN:?run this inside a Noa pane (NOA_BIN is unset)}"
ctl() { "$NOA_BIN" +ctl "$@"; }
field() { python3 -I -c 'import json,sys; print(json.loads(sys.stdin.readline())[sys.argv[1]])' "$1"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

own=$(ctl self | field paneId)
echo "orchestrator pane: $own"

# 1. A command worker: its exit code is readable after it ends.
cmd=$(ctl spawn --label smoke-exit -- sh -c 'echo smoke-output; exit 3' | field paneId)
result=$(ctl wait "$cmd" --for exit --timeout 20)
echo "exit worker: $result"
[ "$(echo "$result" | field exitCode)" = 3 ] || fail "expected exit code 3"
ctl read "$cmd" | grep -q smoke-output || fail "worker output not readable"

# 2. A shell worker that reports agent state like a hook would.
sh_pane=$(ctl spawn --label smoke-report | field paneId)
sleep 1
result=$(ctl send "$sh_pane" --enter --wait --for finished,error --timeout 20 \
  '"$NOA_BIN" +ctl report running; sleep 1; "$NOA_BIN" +ctl report finished smoke')
echo "report worker: $result"
[ "$(echo "$result" | field reason)" = finished ] || fail "expected a finished report"
#    The exited command worker stays listed; waiting on every child still
#    waits for the shell worker's next turn instead of ending on that exit.
ctl send "$sh_pane" --enter 'sleep 1; "$NOA_BIN" +ctl report finished again' >/dev/null
result=$(ctl wait --children --for finished --timeout 20)
echo "children wait: $result"
[ "$(echo "$result" | field paneId)" = "$sh_pane" ] || fail "an earlier exit ended a wait on the children"

# 3. Placing another worker next to one with a pending notification does
#    not acknowledge it (auto placement focuses split candidates).
ping=$(ctl spawn --label smoke-notify -- sh -c 'sleep 1; printf "\033]9;smoke\007"; sleep 600' | field paneId)
ctl wait "$ping" --for attention --timeout 20 >/dev/null || fail "no attention from the notify worker"
extra=$(ctl spawn --label smoke-extra -- sh -c 'sleep 600' | field paneId)
ctl ls | grep "\"paneId\":\"$ping\"" | grep -q '"attention":true' || fail "auto placement cleared a worker's attention"
ctl close "$ping" "$extra"

# 4. A hookless agent (only a desktop notification per turn) can be waited
#    on turn after turn: sending to it acknowledges the last notification.
turns=$(ctl spawn --label smoke-turns -- sh -c 'while read l; do sleep 1; printf "\033]9;turn\007"; done' | field paneId)
sleep 1
for turn in 1 2; do
  ctl send "$turns" --enter --wait --for attention --timeout 20 "turn $turn" >/dev/null \
    || fail "turn $turn did not end on attention"
done
ctl close "$turns"
#    Input a pane refused (it already exited) answers nothing.
held=$(ctl spawn --label smoke-held -- sh -c 'printf "\033]9;held\007"; sleep 1' | field paneId)
ctl wait "$held" --for exit --timeout 20 >/dev/null
if ctl send "$held" x 2>/dev/null; then fail "send to an exited worker was accepted"; fi
ctl ls | grep "\"paneId\":\"$held\"" | grep -q '"attention":true' \
  || fail "refused input acknowledged the worker's notification"
ctl close "$held"

# 5. Both remaining workers are listed as this pane's children, then closed.
count=$(ctl ls --children | wc -l | tr -d ' ')
[ "$count" = 2 ] || fail "expected 2 workers, found $count"
ctl close --children
[ -z "$(ctl ls --children)" ] || fail "workers still listed after close"

# 6. The guard refuses a pane this orchestrator did not spawn.
if ctl send "$own" x 2>/dev/null; then fail "send to a non-worker was accepted"; fi

echo "agent-orchestration smoke: OK"
