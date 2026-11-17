---
name: noa-orchestration
description: Run other coding agents (claude, codex, agy) or commands in worker panes of the Noa terminal and drive them from this pane with `"$NOA_BIN" +ctl`: spawn a worker, send it a task, wait for its turn to end, read its output, close it. Use when you are running inside a Noa pane and the task splits into parts that other agents or long commands can do in parallel, or when asked to delegate, fan out, or orchestrate agents in Noa.
---

# Orchestrating worker panes in Noa

You are the orchestrator. Each worker is a pane next to yours, started and
controlled with `"$NOA_BIN" +ctl`. Every command prints one JSON object per
line, except `read`, which prints the pane's text. Full command reference,
output shapes and exit codes: [reference.md](reference.md).

## Before starting

1. Check that you can: `"$NOA_BIN" +ctl self`.
   - `NOA_BIN` unset: you are not in a Noa pane. Stop; this skill does not apply.
   - Exit status 3: the control server is off or lacks a scope. The stderr line
     names the config line to add (`server-enable = true`,
     `server-scopes = read,control,input`). Ask the human to add it; do not edit
     their Noa config yourself.
   - "does not report surface ids", or "this pane … is not among the panes": you
     reached a different Noa than the one you run in. Usually an older Noa or a
     second instance still holds the control port, so this one could not
     listen. Find it with `lsof -nP -iTCP:<port> -sTCP:LISTEN` and tell the
     human which process holds the port. Do not kill it yourself.
2. Decide what each worker gets: which agent (or command), which directory, and a
   task that stands on its own. A worker sees nothing of your conversation.
3. Keep the number of workers small (the default limit is 8 per orchestrator) and
   tell the human what you are about to start.

## One worker, one round

```sh
noa() { "$NOA_BIN" +ctl "$@"; }
pane=$(noa spawn --label fix-tests --cwd "$PWD" -- claude | jq -r .paneId)
noa send "$pane" --enter --wait --timeout 1800 "Fix the failing test in crates/foo. Report what you changed."
noa read "$pane" --scrollback --max-bytes 65536
noa close "$pane"
```

- Always `send … --wait`. It subscribes before typing, so a fast reply cannot be
  missed. A separate `send` then `wait` can miss a turn that ends in between.
- `read` the pane before every `send`, not only the first. The first screen may
  be a trust or login prompt. Later, the input line may still hold text (a
  draft or a suggestion), and `send` types after it rather than replacing it.
  If the input line is not empty, do not send; tell the human what is there.
- Give every wait a timeout that fits the task, and see "When no report comes"
  below if it runs out.

## Several workers in parallel

Run one `send --wait` per worker in the background and collect each result.
Waiting on `--children` after separate sends has the same race as above.

```sh
for p in "$a" "$b"; do
  noa send "$p" --enter --wait --timeout 1800 "$(cat "task-$p.txt")" > "result-$p.json" &
done
wait   # the shell's wait: every worker's turn has ended
```

For "whichever stops first", use `noa wait --children` only for panes whose
current turn started before the wait (for example, the next time round a loop).

## When no report comes

A wait ends only on a report, a notification or an exit. If a worker never
reports the end of its turn, the wait runs until its timeout, so do not use an
unbounded wait. When a wait times out:

1. `noa ls --children`: note `agent.state` and `agent.seq` for each pending pane.
   Do not judge by `busy`: it only says a shell reported a running command, so
   it stays `false` for an agent started directly (`spawn -- claude`).
2. `noa read PANE`: look at the screen. If the agent is back at its prompt, treat
   the turn as over and continue from what the screen shows.
3. If the screen shows it still working, wait again with a new timeout.

Tell the human when a worker's reports are missing; its hooks may need setting
up (see reference.md, "Which agents report turns").

## Reading why a wait ended

Each line has `paneId` and `reason`:

| reason | What to do |
|---|---|
| `finished` | Turn ended. `read` the pane and check the work yourself |
| `permission` / `input` | The agent is blocked on a prompt. `read` it. Approve or answer only if the human's instructions already cover it; otherwise ask the human |
| `error` | The agent reported an error. `read` it, then retry, re-scope or report |
| `attention` | A desktop notification arrived. Not proof the agent is blocked or done: `read` and decide |
| `exit` | The worker's process ended (`exitCode` when known) or the pane is gone (`closed: true`). It will not report again |

Exit status 2 means the timeout passed. The line `{"timeout":true,"pending":[…]}`
lists the panes still running. Decide whether to wait longer, `read` them, or
close them.

## Rules

- Only `send` to and `close` your own workers. `--any-pane` exists for a pane the
  human pointed you at, never to drive panes you found in `ls`.
- Do not pass auto-approve flags to a worker agent or enable Noa's auto-approve
  unless the human asked for it.
- Close every worker you started when you are done, also after a failure
  (`noa close --children`). Read what you need first: closing discards the
  output.
- `wait` can only see turns an agent reports. Claude Code reports through
  Noa's hook adapter, and codex/agy through hooks that call `+ctl report`,
  when the human has configured them. Without reports, a worker ends a wait
  only with `exit` or `attention`. For plain commands, that is enough:
  `spawn -- make test`, then `wait PANE --for exit`.
