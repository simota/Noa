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

### `spawn [options] [-- COMMAND...]`

Starts a worker whose parent is this pane and prints `{"paneId":"…"}`. A worker
is always a floating pane. When this pane has no board open, the spawn first
opens one next to it (a new tab when there is no room), and the worker is
stowed on the board: collapsed, it shows only as the board's row; the board's
`g` opens it over the board. One pane holds at most 8 floats, so a ninth spawn
fails. If the board cannot open (stderr warns, the spawn still succeeds), or
with `--no-board`, the worker floats over this pane instead, collapsed to a
title strip that shows the agent's state. `--split` and `--tab` are refused; `--float` is
accepted and changes nothing.

| Option | Meaning |
|---|---|
| `--no-board` | Do not open the board; the worker floats over this pane |
| `--cwd DIR` | Working directory. Relative paths are taken from this process's cwd. Default: this process's cwd. One that is not a directory is refused (exit 1) before anything starts; `ls` shows it as the worker's `cwd` until the worker's shell reports its own |
| `--env KEY=VALUE` | Repeatable. `PATH` defaults to this process's `PATH` when a command is given, and `LANG` and `LC_*` to this process's, so a worker keeps your locale even when Noa itself has none |
| `--label TEXT` | Shown in the panel's `label` |
| `--input TEXT` | Typed into the worker once it starts |
| `--no-wait-after` | Close the pane when its command exits. By default it stays open with `exit.code` readable |
| `--focus` | Open the worker's float and move keyboard focus to it. By default it stays collapsed and focus stays where it was |
| `--max-children N` | Refuse when this pane already has N workers (default and maximum 8, the floats a board holds). Exited workers count until closed; the refusal says how many have exited. Checked before each spawn, so parallel spawns can exceed it |

`COMMAND` runs without a shell (argv, looked up on `PATH`). A command without a
`/` that is not on the worker's `PATH` is refused (exit 1, "command not found on
PATH"). Without one, the worker gets the user's shell. For a shell pipeline,
use `-- sh -c '…'`.

### `send PANE [--enter] [--raw] [--wait [wait options]] [--answer] [--any-pane] [--] TEXT|-`

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
- Refuses a pane whose agent reported `permission` (it waits for an approval),
  because Enter approves some prompts with their default choice. It checks before
  typing and again before Enter; if the approval appeared in between, Enter is not
  sent and the text stays in the input line. `--answer` sends anyway. Only agents
  that report `permission` are covered (see "Which agents report turns").
- Only `permission` is refused. `input` is not: it also means an agent idle at
  its prompt (Claude Code's hook adapter reports `idle_prompt` as `input`), so
  `read` an `input` pane before sending to it.
- Answering a prompt: `send P --answer --enter ""` presses Enter alone, which
  takes the selected choice; `send P --answer --raw 2` types a key;
  `send P --answer --raw $'\e'` sends Esc.
- Input to a pane whose process has exited is refused with "pane N's process
  has exited (code C)"; `read` it, then `close` it.

### `wait PANE...|--children [--after SEQ | --after PANE=SEQ ...] [--for LIST] [--all] [--timeout SECS] [--tail N]`

Blocks until one pane (every pane with `--all`) stops, then prints one line per
pane that stopped:

```json
{"paneId":"7","reason":"finished","seq":8,"state":"finished","detail":"…"}
{"paneId":"6","reason":"exit","exitCode":3}
{"paneId":"5","reason":"exit","closed":true}
```

- A pane named by id that is not listed when the wait starts is an error
  (exit 1, "pane N is not listed"), so a stale or mistyped id does not pass for
  a finished worker. One that goes away during the wait ends it with `exit`.
- `--tail N` adds `"tail"`: the pane's last N lines of retained output, for each
  line whose pane is still open.
- `--for` takes a comma list of `finished,permission,input,error,exit,attention`
  (default: all of them).
- A report counts only when its `seq` is newer than the baseline. The baseline is
  the `seq` at the start of the wait, or the one `--after` gives: `--after SEQ`
  for a single pane, `--after PANE=SEQ` (repeatable) for several.
  `--after` does not apply to `attention`.
- `attention` counts only after it was seen cleared during the wait, so a
  notification left from an earlier turn does not end it. With `send --wait`, the
  baseline is taken before sending.
- A pane that is gone, or whose process exited and stays open, ends the wait with
  `exit` whatever `--for` lists, since it will not report again. Without `--all`,
  a pane that had already exited when the wait began is skipped while other panes
  are still running.
- Timeout: prints
  `{"timeout":true,"pending":["7"],"baseline":[{"paneId":"7","seq":5,"state":"running"}]}`
  and exits 2 (`state` only for a pane with an agent state). A pane that had
  already exited when the wait began is listed under `"exited"`, not
  `"pending"`. `baseline` is the report count each pending pane was waited from
  (with `send --wait`, the count before sending). Pass it back as
  `--after PANE=SEQ` and the next wait also sees a report that landed in
  between. Without `--timeout`, it waits indefinitely.

### `read PANE [--scrollback] [--max-bytes N]`

Prints the pane's visible screen as text (without the blank rows below it), or
with `--scrollback` its retained output. `--max-bytes` bounds the output
(default 256 KiB, at most 1 MiB); a cut keeps the end and prints
`output truncated` on stderr. Works on exited panes that are still open.

### `close PANE...|--children [--any-pane]`

Closes panes without a confirmation dialog and prints
`{"paneId":"…","closed":true}` for each. The pane's output is gone afterwards.

A pane that is no longer listed (closed by hand, or exited without being held)
prints `{"paneId":"…","closed":true,"gone":true}` and is not an error, so a
mistyped id also shows up as `gone`. A pane that could not be closed is named on
stderr; the rest are still closed and the exit status is 1. A live pane that is
not this pane's worker is refused before anything is closed.

### `board [--of PANE]`, `board --open [--split right|down | --tab]`

A live view for people, not for scripts. The first `spawn` opens one by itself.
`board --open` starts a board pane next to this pane (a new tab when there is no room) and prints its `{"paneId":"…"}`;
when this pane's board is already open it opens nothing and prints
`{"paneId":"…","existing":true}`. The board is not one of this pane's workers. `board` without `--open` takes over the
pane it runs in until `q`.

### `report STATE [[--] DETAIL]`

Writes an agent-state report for the pane this process runs in. `STATE` is
`running`, `permission`, `input`, `finished`, `error` or `clear`. It is meant for a
script running in the pane (agent hooks that run without a controlling
terminal cannot use it). Without a controlling terminal it writes nothing and
succeeds.

## Exit status

| Code | Meaning |
|---|---|
| 0 | Done |
| 1 | Error: bad arguments, unknown pane, refused input, a pane that is not yours. An argument error prints only its message and a pointer to `--help` |
| 2 | `wait` timed out |
| 3 | Server disabled, unreachable, missing a scope, or gone mid-command. stderr says which: "sandbox likely blocks loopback" (your shell's sandbox refused the connection; run `+ctl` outside it), "not listening" (Noa's server is off), or a message naming the config to add |

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
| Codex | On its own through OSC 7501 (Codex 0.163 and later, `app=codex`) | `permission` / `input` |
| Antigravity CLI | On its own through OSC 7501 (agy 1.4 and later, `app=antigravity`; off with `AGY_CLI_OSC7501=off`) | `permission` / `input` |
| Anything else | Nothing | Only `exit`, or `attention` if it sends a desktop notification |

Older Codex and agy versions surface prompts only as `attention`. Hooks that
call `+ctl report` deliver nothing from Claude Code or Codex, which run hooks
without a controlling terminal; `+ctl report` is for scripts running in the
pane. Check the installed version if waits never end with `finished`.

## Model tiers

Flags that pick a worker's model and effort, appended to the agent command at
`spawn` (`spawn --label rename -- claude --model haiku --effort low`). Tiers
are described in SKILL.md, "Matching the worker to the task".

| Tier | Claude Code | Codex | Antigravity CLI |
|---|---|---|---|
| `light` | Reading only: `--model haiku --effort low`. Edits code: `--model sonnet --effort low` | `-m gpt-6-luna -c model_reasoning_effort=low` | `--model gemini-4-hydrogen-low` |
| `standard` | `--model sonnet --effort medium` | `-m gpt-6.2-sol -c model_reasoning_effort=medium` | `--model gemini-4-argon-medium` |
| `deep` | `--model opus --effort high` | `-m gpt-6.2-astra -c model_reasoning_effort=high` | `--model gemini-4-neon-high` |

- Claude Code: Anthropic advises keeping Haiku to work whose mistakes are
  quick to spot (finding files, running tests, reading logs, summarising),
  and code edits on a larger model, hence the split `light` cell.
- Antigravity: Google describes Gemini 4 Hydrogen as the fast model for
  everyday coding without a planning phase, Argon as faster and lighter on
  tokens than Carbon, and Neon, Carbon or Argon for work that needs the
  deepest reasoning at every step. Gemini 4 models depend on the plan
  (Hydrogen needs a non-trial Pro plan, Neon a paid plan, Carbon Ultra or
  Enterprise). Gemini 3.8 Flash is being retired, so no tier uses it.
  Gemini and Claude/GPT models draw on separate quotas.

- Claude Code takes the aliases `haiku`, `sonnet`, `opus` and `fable` for the
  latest model of each family, so its column does not go stale. `--effort`
  takes `low`, `medium`, `high`, `xhigh`, `max`.
- Codex and Antigravity model names change between releases. The names above
  are from Codex 0.170.0 and Antigravity CLI 1.5.12. List what the installed
  version offers before using them:
  - Codex: `codex debug models | jq -r '.models[] | [.slug, .description] | @tsv'`.
    The description says which models are the fast or affordable ones; each
    model's `supported_reasoning_levels` lists the effort values it takes.
  - Antigravity: `agy models`. The effort is part of the model id
    (`…-low`, `…-high`).
- `read` the pane after `spawn`, as before any first send: a model name the
  installed version or the account does not offer shows up there.

## Patterns

Several workers, each with a race-free wait:

```sh
noa() { "$NOA_BIN" +ctl "$@"; }
a=$(noa spawn --label api -- claude | jq -r .paneId)
b=$(noa spawn --label ui -- codex | jq -r .paneId)
noa send "$a" --enter --wait --timeout 1800 "…task A…" > a.json &
noa send "$b" --enter --wait --timeout 1800 "…task B…" > b.json &
wait   # run the whole block with your tool's background mode
```

The same without background jobs, only for workers that report turns (see the
table above): keep each `seq` and wait per pane. `--after` covers reports only. A
worker that only raises `attention` and finishes before its `wait` starts is
never seen, and that wait runs until its timeout. Use the background
`send --wait` form for such workers.

```sh
sa=$(noa send "$a" --enter "…" | jq -r .seq)
sb=$(noa send "$b" --enter "…" | jq -r .seq)
noa wait "$a" --after "$sa" --timeout 540
noa wait "$b" --after "$sb" --timeout 540
```

A wait longer than your shell tool allows: keep each wait short and carry on
from the timeout line. A report that arrives between the waits is not missed.
A notification is: `--after` covers reports only, so use this only for workers
that report turns (see the table above). For a worker that only raises
`attention`, `read` it after each timeout instead.

```sh
out=$(noa send "$a" --enter --wait --timeout 540 "…task…")
while [ $? -eq 2 ]; do
  seq=$(echo "$out" | jq -r '.baseline[0].seq')
  out=$(noa wait "$a" --after "$a=$seq" --timeout 540)
done
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

- Parallel spawns from one orchestrator can exceed `--max-children`.
- A spawn retried after a timeout can start the worker twice.
