# `Noa +ctl` reference

Run as `"$NOA_BIN" +ctl <command> [options]`. Noa sets `NOA_BIN` (its absolute
path) and `NOA_SURFACE_ID` (this pane's surface id) in every pane; `+ctl` uses
the second to find "this pane". Pane ids are strings in JSON (`"paneId":"7"`) and
plain numbers on the command line. Put `--` before a text or detail that starts
with `--`.

Design and rationale: `docs/specs/agent-orchestration.md`. Human-facing guide:
`docs/AGENT_WORKFLOW.md`, "Orchestrating worker panes".

## Setup (the human's Noa config)

```conf
server-enable = true
server-scopes = read,control,input
```

`+ctl` connects to `server-bind`/`server-port` and authenticates with
`server-token` or the token file. A spawn that runs a command, types initial input
or sets env needs the `input` scope on top of `control`.

## Commands

### `self`, `ls [--children]`

Print panels, one per line. `self` prints this pane's panel. `--children` lists
only the workers this pane spawned (including exited ones still open).

Panel fields that matter for orchestration:

```jsonc
{
  "paneId": "7",
  "surfaceId": "0x8dbed625d56b41f4",
  "label": "fix-tests",          // only when given at spawn
  "parentPaneId": "1",           // only for workers
  "process": "claude",           // foreground process
  "cwd": "/repo",
  "busy": true,                  // a shell reported a running command (OSC 133);
                                 // always false for a command spawned without one
  "attention": false,            // an unread notification (OSC 9 and the like)
  "agent": {"kind": "claude", "state": "finished", "detail": "", "seq": 7},
                                 // omitted for no known agent and no report;
                                 // state/detail only after a report
  "exit": {"code": 0}            // only when the child exited and the pane stays open
}
```

`agent.seq` counts reports for that pane and only grows. A report newer than the
`seq` you saw belongs to a later turn.

### `spawn [placement] [options] [-- COMMAND...]`

Starts a worker whose parent is this pane and prints `{"paneId":"…"}`.

| Option | Meaning |
|---|---|
| `--split auto` (default) | First worker to the right of this pane, later ones split the largest of this pane's workers along its longer side. With no room left, a new tab in the same window |
| `--split right` / `--split down` | Split this pane in that direction (fails when there is no room) |
| `--tab` | New tab in this pane's window |
| `--cwd DIR` | Working directory. Relative paths are taken from this process's cwd. Default: this process's cwd |
| `--env KEY=VALUE` | Repeatable. `PATH` defaults to this process's `PATH` when a command is given |
| `--label TEXT` | Shown in the panel's `label` |
| `--input TEXT` | Typed into the worker once it starts |
| `--no-wait-after` | Close the pane when its command exits. By default it stays open with `exit.code` readable |
| `--focus` | Move keyboard focus to the worker. By default focus stays where it was |
| `--max-children N` | Refuse when this pane already has N workers (default 8). Checked before each spawn, so parallel spawns can exceed it |

`COMMAND` runs without a shell (argv, looked up on `PATH`). Without one, the worker
gets the user's shell. For a shell pipeline, use `-- sh -c '…'`.

### `send PANE [--enter] [--raw] [--wait [wait options]] [--any-pane] [--] TEXT|-`

Types `TEXT` (or stdin with `-`) into the pane as a paste. `--enter` then sends
Enter 150 ms later, which submits a prompt in agent TUIs. `--raw` types the text
as keystrokes instead of a bracketed paste.

- Without `--wait`: prints `{"paneId":"…","seq":N}`, the report count before
  sending. `wait PANE --after N` then waits for this turn without missing a quick
  reply.
- With `--wait`: subscribes first, sends, then behaves like `wait PANE` with the
  given wait options, and prints its lines. This is the safe default.
- Refuses a pane this pane did not spawn unless `--any-pane` is given. That guards
  against a wrong id; it is not a security boundary.
- Input the pane refused (for example, its process has exited) is an error.

### `wait PANE...|--children [--after SEQ] [--for LIST] [--all] [--timeout SECS]`

Blocks until one pane (every pane with `--all`) stops, then prints one line per
pane that stopped:

```json
{"paneId":"7","reason":"finished","seq":8,"state":"finished","detail":"…"}
{"paneId":"6","reason":"exit","exitCode":3}
{"paneId":"5","reason":"exit","closed":true}
```

- `--for` takes a comma list of `finished,permission,input,error,exit,attention`
  (default: all of them).
- A report counts only when its `seq` is newer than the baseline. The baseline is
  the `seq` at the start of the wait, or `--after SEQ` (one pane only).
  `--after` does not apply to `attention`.
- `attention` counts only after it was seen cleared during the wait, so a
  notification left from an earlier turn does not end it. With `send --wait`, the
  baseline is taken before sending.
- A pane that is gone, or whose process exited and stays open, ends the wait with
  `exit` whatever `--for` lists, since it will not report again. Without `--all`,
  a pane that had already exited when the wait began is skipped while other panes
  are still running.
- Timeout: prints `{"timeout":true,"pending":["…"]}` and exits 2. Without
  `--timeout`, it waits indefinitely.

### `read PANE [--scrollback] [--max-bytes N]`

Prints the pane's visible screen as text, or with `--scrollback` its retained
output. `--max-bytes` bounds the output. Works on exited panes that are still open.

### `close PANE...|--children [--any-pane]`

Closes panes without a confirmation dialog and prints
`{"paneId":"…","closed":true}` for each. The pane's output is gone afterwards.

### `report STATE [[--] DETAIL]`

Writes an agent-state report for the pane this process runs in. `STATE` is
`running`, `permission`, `input`, `finished`, `error` or `clear`. It is meant for a
worker agent's hooks. Without a controlling terminal it writes nothing and
succeeds.

## Exit status

| Code | Meaning |
|---|---|
| 0 | Done |
| 1 | Error: bad arguments, unknown pane, refused input, a pane that is not yours |
| 2 | `wait` timed out |
| 3 | Server disabled, unreachable, missing a scope, or gone mid-command. stderr names the config to add |

When `+ctl` reaches a Noa that does not list this pane (stderr says "does not
report surface ids", exit 3, or "is not among the panes", exit 1), the control
port belongs to another Noa: an older one, or a second instance. The Noa this
pane runs in then failed to listen, and its Settings > Server Status reads
"Bind failed". Find the process that holds the port:

```sh
lsof -nP -iTCP:61771 -sTCP:LISTEN   # 61771 is the default server-port
```

Report it to the human; quitting the other Noa and restarting this one (or
choosing another `server-port`) is their call.

## Which agents report turns

| Agent | Reports | Approval or question prompts |
|---|---|---|
| Claude Code | On its own through OSC 7501 (Claude Code 2.1.295 and later): `running` while working, `finished` when a turn completes, `error` when it fails. Noa versions before the 7501 `done`/`error` fix left the state at `running` after a turn. The hook adapter `scripts/noa-agent-hook.py` reports the same, if configured (`docs/AGENT_WORKFLOW.md`) | `permission` / `input` |
| Codex | `running`/`finished` from hooks calling `+ctl report` (`UserPromptSubmit`, `Stop`), if configured | Only as `attention` |
| Antigravity CLI | `running`/`finished` from hooks calling `+ctl report` (`PreInvocation`, `Stop`), if configured | Only as `attention` |
| Anything else | Nothing | Only `exit`, or `attention` if it sends a desktop notification |

The Codex and Antigravity hook events come from their documentation and are not
tested by Noa. Check the installed version if waits never end with `finished`.

## Patterns

Several workers, each with a race-free wait:

```sh
noa() { "$NOA_BIN" +ctl "$@"; }
a=$(noa spawn --label api -- claude | jq -r .paneId)
b=$(noa spawn --label ui -- codex | jq -r .paneId)
noa send "$a" --enter --wait --timeout 1800 "…task A…" > a.json &
noa send "$b" --enter --wait --timeout 1800 "…task B…" > b.json &
wait
```

The same without background jobs, only for workers that report turns (see the
table above): keep each `seq` and wait per pane. `--after` covers reports only. A
worker that only raises `attention` and finishes before its `wait` starts is
never seen, and that wait runs until its timeout. Use the background
`send --wait` form for such workers.

```sh
sa=$(noa send "$a" --enter "…" | jq -r .seq)
sb=$(noa send "$b" --enter "…" | jq -r .seq)
noa wait "$a" --after "$sa" --timeout 1800
noa wait "$b" --after "$sb" --timeout 1800
```

A command instead of an agent:

```sh
t=$(noa spawn --label tests -- cargo test --workspace | jq -r .paneId)
noa wait "$t" --for exit --timeout 3600 | jq .exitCode
noa read "$t" --scrollback --max-bytes 65536 | tail -50
noa close "$t"
```

Clean up everything this pane started:

```sh
noa close --children
```

## Known limits

- Placing a worker in a tab where a pane is zoomed may leave the tab unzoomed.
  The code reads that way, but it has not been reproduced.
- Parallel spawns from one orchestrator can exceed `--max-children`.
- A spawn retried after a timeout can start the worker twice.
