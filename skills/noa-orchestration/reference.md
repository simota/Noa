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
  "agent": {"kind": "claude", "state": "input", "detail": "", "seq": 7, "blocked": "question"},
                                 // omitted for no known agent and no report
                                 // (a worker spawned as claude, codex or agy
                                 // has its kind from the start);
                                 // state/detail only after a report; blocked
                                 // ("permission" or "question") only while the
                                 // last report was an OSC 7501 blocked report
  "exit": {"code": 0},           // only when the child exited and the pane stays open
  "spawn": {"argv": ["claude", "--model", "sonnet"], "startedAt": 1763700000}
                                 // only for a pane a spawn started with a command
}
```

`blocked` and `spawn` come from Noa itself; a Noa older than them omits both,
and `send` then refuses only `permission`.

`agent.seq` counts reports for that pane and only grows. A report newer than the
`seq` you saw belongs to a later turn.

### `spawn [options] [-- COMMAND...]`

Starts a worker whose parent is this pane and prints
`{"paneId":"…","cwd":"…","result":"…"}` (plus the fields of the options below);
`cwd` is the worker's directory, printed resolved. A worker
is always a floating pane. When this pane has no board open, the spawn first
opens one next to it (a new tab when there is no room), and the worker is
stowed on the board: collapsed, it shows only as the board's row; the board's
`g` opens it over the board. A board holds at most 20 stowed workers, so a
21st spawn fails; its list scrolls. If the board cannot open (stderr warns, the spawn still succeeds), or
with `--no-board`, the worker floats over this pane instead, collapsed to a
title strip that shows the agent's state; a pane holds at most 8 such floats.

A worker that spawns (one level of nesting) opens no board of its own: its
workers are stowed on its orchestrator's board, under its row, and refuse
`--no-board`. A worker's worker cannot spawn ("nesting goes one level deep").
Closing a worker closes the workers it started.
`--split` and `--tab` are refused; `--float` is
accepted and changes nothing.

| Option | Meaning |
|---|---|
| `--no-board` | Do not open the board; the worker floats over this pane |
| `--worktree` | Start the worker in a new git worktree: `git worktree add -b orch/LABEL <main>-wt/LABEL HEAD`, where `<main>` is the repository's main worktree (the same path from any worktree or subdirectory) and `HEAD` is your checkout's. Needs `--label` (letters, digits, `.`, `_`, `-`; not starting with `.` or `-`) and refuses `--cwd`. Prints `"worktree"` and `"branch"` too. A git failure (branch or path exists, not a repository) is exit 1 and starts nothing. What your checkout has that the commit lacks is warned on stderr, apart: `this checkout has uncommitted changes to N tracked files; they are not in the worktree` and `N untracked files are not in the worktree`. If Noa then refuses to start the worker, the fresh worktree and branch are removed again; if the answer is lost and the worker is not found ("Lost answer" below), they are kept, since the worker may be running there, and stderr says so. The board, when this spawn would open it, opens only after the worktree was made |
| `--tier light\|standard\|deep` | Add the tier's model and effort flags ("Model tiers" below) right after the program of a `claude`, `codex` or `agy` command. Refused for any other command, and for a command that already picks a model or effort, so a model the human named is never overridden. Prints `"tier"` too |
| `--cwd DIR` | Working directory. A relative path is taken from this process's cwd. An absolute path with no `.` or `..` component is used as given, symlinks kept; `.` components are dropped (`/tmp/.` gives `/tmp`). Each `..` removes the component before it, and that path is used when it names the same directory as the joined path (on macOS, `/tmp/../tmp` gives `/tmp`). When it does not, because a symlink comes before the `..`, the joined path is canonicalized and every symlink in it is resolved, prefix included (`/tmp/job/link/..` with `link -> real/sub` gives `/private/tmp/job/real`). Default: this process's cwd. One that is not a directory is refused (exit 1) before anything starts; `ls` shows it as the worker's `cwd` until the worker's shell reports its own |
| `--env KEY=VALUE` | Repeatable. `PATH` defaults to this process's `PATH` when a command is given, and `LANG` and `LC_*` to this process's, so a worker keeps your locale even when Noa itself has none |
| `--label TEXT` | Shown in the panel's `label`; a plain one (as for `--worktree`) also names the result file. One starting with `board:` is refused (exit 1): that names a board |
| `--input TEXT` | Written as raw keys right after the worker starts, so each newline is Enter. Refused (exit 1) for a `claude`, `codex` or `agy` command: use `--task`, or `send` once the agent is up |
| `--no-wait-after` | Close the pane when its command exits. By default it stays open with `exit.code` readable. A command that fails right after it starts (any exit other than code 0 within `abnormal-command-exit-runtime`, default 250 ms) stays open anyway, showing the failure |
| `--focus` | Open the worker's float and move keyboard focus to it. By default it stays collapsed and focus stays where it was |
| `--max-children N` | Refuse when this pane already has N workers (default and maximum 20, the workers a board holds). Exited workers count until closed. Sent to Noa as `maxChildren`, which counts one spawn at a time, so parallel spawns stay under it; a Noa older than that ignores it and only `+ctl`'s own count before the spawn applies. Refusal (exit 1): `this pane already has N workers[, M of them exited] (limit L); close finished ones after reading them[, or raise --max-children (at most 20)]` |
| `--ready SECS` | After the spawn, look every 250 ms, for at most SECS seconds, until the worker's agent sends a report other than `running` (`"ready":"report"`; Codex reports a `running` turn while it starts up, and a screen under a `running` report does not count as quiet) or its screen shows something and stays the same for 1.5 s (`"quiet"`); else `"timeout"`, or `"gone"` at once when the pane is no longer listed. Adds `"screen"`: the last 20 non-blank lines of the last screen read (`""` when none was). If a request fails meanwhile, the spawn line is printed without `ready` and the command exits with that failure's status |
| `--task TEXT` \| `--task-file F` | Only with a `claude`, `codex` or `agy` command (exit 1 otherwise); exclusive. Implies `--ready 30` unless `--ready` is given, and takes the wait options `--for`, `--all`, `--timeout`, `--tail`, `--stall`, `--settle` (refused without `--task`). The file is read and its size checked before anything starts. When ready is `report` or `quiet` and the screen ends in an agent's input box (lines between full-width rules, or Codex's `›` input line near the bottom, which has none; a numbered `› 1.` is a dialog's choice), types the task as `send --enter --wait` does (same refusals, paste-mode gate, journal and resume line) and prints the spawn line with `"sent":true`, then the wait lines. Otherwise types nothing: `"sent":false`, exit 0; read `screen` and send yourself. A send that is refused or fails prints `"sent":false` and exits with the send's status |

**Result file.** Every spawn whose cwd is known creates `<cwd>/.noa/` with a
`.gitignore` of `*` (an existing one is left alone) and gives the worker
`NOA_RESULT=<cwd>/.noa/result-LABEL.md` (a plain `--label`;
`result-LABEL-<8 hex>.md` when that name is taken; otherwise
`result-<8 random hex digits>.md`). The file is created empty at the spawn, which
reserves its name, so no two workers share one even before either writes. The line gains
`"result":"<path>"`. With `--worktree` the cwd is the worktree. `.noa/` never
shows in `git status` and does not stop `git worktree remove`. `--env
NOA_RESULT=…` wins and makes no directory. When `.noa` cannot be written, stderr
says `warning: the worker gets no $NOA_RESULT: cannot write <dir>/.noa: …`, the
line has no `result`, and the spawn goes on.

**Lost answer.** When Noa answers `-32009` or the client's 5 s deadline passes,
the spawn lists the panes up to 30 times, 100 ms apart. If exactly one new
worker of this pane appeared (with the same label, given `--label`), it prints
`{"paneId":"N","recovered":true,…}` and exits 0. Otherwise it exits 3 ("Noa did
not confirm the request in time (…); it may still have taken effect: check with
`ls` or `read` before retrying", or "Noa did not answer in time"). Another
unlabelled spawn of yours in the same seconds can be taken for it. When Noa
refused the spawn and this spawn had opened the board, the board is closed
again if this pane has no worker.

`COMMAND` runs without a shell (argv, looked up on `PATH`). A command without a
`/` that is not on the worker's `PATH` is refused (exit 1, "command not found on
PATH"). Without one, the worker gets the user's shell. For a shell pipeline,
use `-- sh -c '…'`.

### `send PANE [--enter] [--raw] [--replace] [--wait [wait options]] [--answer] [--queue] [--any-pane] [--] TEXT|-`

Types `TEXT` (or stdin with `-`) into the pane as a paste. `--enter` then sends
Enter 150 ms later, which submits a prompt in agent TUIs. `--raw` types the text
as keystrokes instead of a bracketed paste.

- Without `--wait`: prints `{"paneId":"…","seq":N}`, the report count before
  sending. `wait PANE --after N` then waits for this turn without missing a quick
  reply.
- With `--wait`: subscribes first, sends, then behaves like `wait PANE` with the
  given wait options, and prints its lines. This is the safe default.
- Refuses a pane this pane did not spawn unless `--any-pane` is given. That guards
  against a wrong id; it is not a security boundary. A pane that is not listed at
  all is refused with "pane N is not listed" (exit 1).
- Refused before anything is typed (exit 1):
  - A text over 250 KiB once JSON-encoded: `the text is N bytes after JSON
    encoding; one send takes at most 250 KiB. Nothing was sent: write the text
    to a file and send its path`.
  - A pane waiting for an approval (`agent.state` `permission`, or `blocked`
    `permission`): `pane N is waiting for an approval; read it and pass
    --answer. Nothing was sent.` Enter approves some prompts with their default
    choice.
  - A pane asking a question (`blocked` `question`, state `input`): `pane N is
    waiting for an answer to a question; read it and pass --answer. Nothing was
    sent.` A question dialog takes Enter as its first choice.
  - A pane whose agent is `running`, for pasted text or a bare Enter: `pane N is
    working; wait for its turn to end, or pass --queue (the wait may then end
    with the current turn)`. `--raw` keys with text (Esc, Ctrl-C) always go.
- `--answer` lifts the approval and question refusals, `--queue` the working
  one. With `--queue --wait`, every line the wait prints has `"queued":true`:
  the wait may end with the turn that was already running.
- An `input` without `blocked` is not refused: it also means an agent idle at its
  prompt (Claude Code's hook adapter reports `idle_prompt` as `input`, and an
  older Noa sends no `blocked`), so `read` an `input` pane before sending to it.
- The checks run again after `--replace` cleared the line (`pane N began waiting
  for an approval while its input line was cleared; the text was not typed.`)
  and before Enter (`… after the text was typed; Enter was not sent, so the text
  may still be in its input line.`), both exit 1.
- Pasted text (not `--raw`, not empty) into an agent's pane (`agent.kind` set),
  or one whose `process` is not listed yet and that no spawn started with a
  command (no `spawn` in its panel; a Noa too old to send `spawn` still holds
  those), first waits for the pane to enable
  bracketed paste: Noa is asked every 200 ms, for up to 15 s, before any key
  (also before `--replace`'s). Still off: exit 1, `pane N has not enabled
  bracketed paste yet (its program may still be starting); nothing was sent`. A
  Noa without `requirePasteMode` lets it through at once.
- `--wait` prints `Noa +ctl: sent to pane N; resume with: wait N --after SEQ`
  on stderr once the text (and Enter) is in, before waiting: the way back if
  your shell tool kills the command.
- Noa answering `-32009` while typing: exit 3, "Noa did not confirm the input to
  pane N in time (…); it may already be typed: `read` the pane before sending
  again".
- `--replace` clears the input line before typing: Ctrl-E, then Ctrl-U in
  writes of 16 (Claude Code takes a longer burst as a paste and keeps the
  draft), enough for a 32-line draft. Checked on Claude Code 2.1.329, Codex
  0.170.0 and Antigravity CLI 1.5.13, with typed text, pasted lines and Claude
  Code's collapsed `[Pasted text]`. Text after the cursor on later lines (a
  draft someone moved the cursor up in) can stay. Refused with `--answer`: a
  prompt has no input line.
- Answering a prompt: `send P --answer --enter ""` presses Enter alone, which
  takes the selected choice; `send P --answer --raw 2` types a key;
  `send P --answer --raw $'\e'` sends Esc.
- Input to a pane whose process has exited is refused with "pane N's process
  has exited (code C)"; `read` it, then `close` it.

### `wait PANE...|--children [--after SEQ | --after PANE=SEQ ...] [--for LIST] [--all] [--timeout SECS] [--tail N] [--stall SECS] [--settle SECS] [--any-pane]`

Blocks until one pane (every pane with `--all`) stops, then prints one line per
pane that stopped:

```json
{"paneId":"7","reason":"finished","label":"api","seq":8,"state":"finished","detail":"…","result":{"path":"/repo/.noa/result-api.md","bytes":812}}
{"paneId":"8","reason":"input","label":"ui","seq":4,"state":"input","blocked":"question","prompt":"Which file?\n1. a.rs\n2. b.rs"}
{"paneId":"6","reason":"exit","exitCode":3}
{"paneId":"5","reason":"exit","closed":true}
{"paneId":"4","reason":"exit","agentLeft":true,"seq":9}
```

- `label`: the pane's label, also once it has closed (the one seen when the
  wait began). `blocked`: what the agent reported it is blocked on.
- `prompt`, on every `permission` or `input` line with or without `--tail`: the
  screen's block below the work (the dialog or input box), rules and blank lines
  removed, at most 20 lines. Absent when the screen has no rule-delimited box.
- `result`: the pane's result file (spawn, "Result file") once it was written
  after the wait began (with `send --wait`, after just before typing; for a pane
  given `--after PANE=SEQ`, after the journal's send with that `seq` before it,
  in whole seconds). Timeout lines have none.
- `agentLeft`: a pane that had an agent when the wait began now has none (the
  agent quit to its shell); like any `exit`, whatever `--for` lists.
- `wait PANE` (named, not `--children`) refuses a listed pane that is neither
  this pane nor one of its workers: `pane N is not a worker of this pane (pass
  --any-pane to target it)`. Outside a listed Noa pane the check is skipped.

- A pane named by id that is not listed when the wait starts is an error
  (exit 1, "pane N is not listed"), so a stale or mistyped id does not pass for
  a finished worker. One that goes away during the wait ends it with `exit`.
- With `--children`, a pane `--after` names that is no longer listed (a worker
  that closed since the timeout line that gave its baseline) is reported as
  `{"paneId":"…","reason":"exit","closed":true}`, like a worker that goes away
  during the wait. One that is listed but is not this pane's worker is an error
  (exit 1, "not waited on").
- A request Noa does not answer within the client's 5 s deadline is retried on
  the next tick until the wait's `--timeout`; a lost connection still ends the
  wait with exit 3.
- `--tail N` adds `"tail"` for each line whose pane is still open: on an agent's
  pane, except for `permission` and `input`, the last N lines above its input
  box; otherwise the last N lines of retained output. A read that fails keeps
  the line, with `"tailError":"<message>"` instead. A pane on the alternate
  screen gets `Noa +ctl: pane N is on the alternate screen; its program keeps no
  history here: only the screen is returned` on stderr.
- `--settle SECS` (also on `send --wait` and `spawn --task`) holds a `finished`
  for SECS seconds. A newer report drops the hold and the wait goes on; a
  change in the work above the input box starts the SECS again; a `permission`
  that arrives meanwhile still ends it. One that held is printed with `"settledSecs":SECS`. A timeout
  during a hold gives the baseline from before that `finished`.
- `--for` takes a comma list of `finished,permission,input,error,exit,attention`
  (default: all of them), and `stalled` with `--stall`.
- `--stall SECS` also ends the wait for a pane whose visible screen has not
  changed for SECS seconds while it is open:
  `{"paneId":"7","reason":"stalled","quietSecs":120,"seq":5,"state":"running"}`.
  The quiet time counts from the start of the wait or the last change. Claude
  Code's working spinner shows elapsed seconds, so a working Claude Code does
  not stall; one that went back to its prompt without reporting, or sits on a
  prompt it does not report, does. Whether Codex and Antigravity CLI keep
  their screens changing while they work is not checked, so `read` a stalled
  pane before treating it as stopped. A report or exit seen in the same pass
  wins. `--for stalled` without `--stall` is an argument error.
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
  `{"timeout":true,"pending":["7"],"baseline":[{"paneId":"7","seq":5,"label":"api","state":"running"}]}`
  and exits 2 (`label` and `state` only when the pane has them). A pane that had
  already exited when the wait began is listed under `"exited"`, not
  `"pending"`, and has no `baseline` entry. `baseline` is the report count each pending pane was waited from
  (with `send --wait`, the count before sending). Pass it back as
  `--after PANE=SEQ` and the next wait also sees a report that landed in
  between. Without `--timeout`, it waits indefinitely.
- SIGTERM, SIGINT or SIGHUP during `wait` or `send --wait` prints the same line
  with `"interrupted":true` (and `"queued":true` under `--queue`) and exits 2,
  up to about a second later. SIGKILL prints nothing. A signal before the
  wait's first listing came back lists every pane under `pending`, and
  `baseline` only the `seq`s already known (from `--after`, or what
  `send --wait` saw before typing).

### `read PANE [--scrollback] [--max-bytes N]`

Prints the pane's visible screen as text (without the blank rows below it), or
with `--scrollback` its retained output. `--max-bytes` bounds the output
(default 256 KiB, at most 1 MiB); a cut keeps the end and prints
`output truncated` on stderr. Works on exited panes that are still open. A
pane on the alternate screen (most agents' TUIs) keeps no history:
`--scrollback` then says on stderr that only its screen came back.

### `result PANE [--max-bytes N | --path]`

Prints the pane's result file (spawn, "Result file"), with a newline added if
it lacks one. The path is the one this pane's journal recorded at the spawn (or
where `close --prune` moved it), else `<its cwd>/.noa/result-LABEL.md` for a listed pane with a plain label.
`--max-bytes` keeps the **start** (a report's summary is at its top) and says
`Noa +ctl: result cut; printed the first N bytes of <path>` on stderr. Exit 1:
`pane N has no result: <path> does not exist (the worker writes its report to
$NOA_RESULT)`, `pane N has no result yet: <path> is empty (…)` for a worker that
has not written its report, `cannot read pane N's result <path>: …`, or `pane N has no
known result file: …`. Works on closed workers the journal knows.

`--path` prints only the path of the result file and a newline, without opening
the result file. It exits 0 whenever a path is known, whether the file exists, is
empty or is missing, so a path can name a file that is not written yet. When no
path is known it exits 1 with `pane N has no known result file: …`. Other
failures keep the exit codes of "Exit status" below. `--path` with `--max-bytes` is an
argument error (also exit 1):
`--path and --max-bytes conflict: --path reads no file`. Use it to follow a
command worker's log, which `--max-bytes` cannot show the end of:
`tail -f "$("$NOA_BIN" +ctl result PANE --path)"`.

### `status [--answers]`

One line per live worker of this pane, in listing order: `paneId`, `cwd`, and
when known `label`, `state`, `seq`, `blocked`, `exit`, `branch`; from the
journal `tier`, `argv` (else Noa's `spawn.argv`), `lastTask` (at most 200 characters
of the last text sent that was not an answer, with any `[noa-timeline …]` header
removed), `lastSentSeq` (the `seq` before the last send),
`lastOutcome` (`{reason,seq,ts}`), `answers` (a count), `result` (where
`close --prune` moved it, after a prune), `worktree`, `retryOf` (the pane a
`retry` replaced).
A worktree worker also gets `ahead` (commits on its branch that your `HEAD`
lacks) and `dirty` (uncommitted changes in its worktree), each left out when
git cannot say. Then the journal's workers that are no longer listed:
`{"paneId":"…","gone":true,"label"?,"closed"?:true,"cwd"?,…}`. Only a pane this
pane spawned is a worker here: one it merely sent to, waited on or tried to close
is not listed.

`--answers` prints every prompt answered from this pane (`send --answer`),
oldest first: `{"ts":…,"paneId":"…","label"?:…,"text":…,"screen":…}`, where
`screen` is the prompt's last 20 lines as read just before answering.

**Journal.** Each `+ctl` command run in a Noa pane appends to
`<data-dir>/noa/ctl/<NOA_SURFACE_ID>.jsonl` (`~/Library/Application
Support/noa/ctl/` on macOS), one line per event: `spawn`, `send` (written only
once typed; its first 200 characters as `text`, `bytes`, `raw`, and the whole
text as `full` when it is longer than that and at most 64 KiB, for `retry`),
`outcome` (each wait line with a `reason`), `close`. Over 4 MiB it loses its oldest half. A new journal is
0600, in a 0700 `ctl/` when that is new too (existing ones are not chmod-ed);
appends and the halving hold a lock on `<NOA_SURFACE_ID>.jsonl.lock` beside it.
A journal that cannot be written warns once (`warning: cannot write the journal …; carrying on
without it`) and never fails the command. Without `NOA_SURFACE_ID` nothing is
kept.

### `close PANE...|--children [--any-pane] [--prune]`

Closes panes without a confirmation dialog and prints
`{"paneId":"…","closed":true}` for each. The pane's output is gone afterwards.

A pane that is no longer listed (closed by hand, or exited without being held)
prints `{"paneId":"…","closed":true,"gone":true}` and is not an error, so a
mistyped id also shows up as `gone`. A pane that could not be closed is named on
stderr; the rest are still closed and the exit status is 1. A live pane that is
not this pane's worker is refused before anything is closed.

Noa answering `-32009` for a pane that is still listed afterwards: no line for
it, stderr "Noa did not confirm closing pane N in time (…); it may still close:
check with `ls` before retrying", the rest are still closed, and the exit
status is 3.

`--prune`: for each closed worker whose label `L` has a worktree
`<main>-wt/L` on branch `orch/L`, and whose cwd is in it (as `spawn --worktree` made it; the
repository is found from the pane's cwd, or its nearest existing parent, not
from yours), first move every `.noa/result-*.md` of the worktree into
`<main>/.noa/` (a name taken there gets `-<8 hex>`), then run
`git worktree remove` and `git branch -d` in the main worktree, never forced. A
worker that is already gone is pruned too when this pane's journal recorded its
label and worktree (the worktree is then its cwd). The line gains
`"worktree":{"path":…,"removed":true,"branchDeleted":true,"result":…}`, where
`result` is the new path of this worker's own report, and `moved` lists every
file moved (`[{"from":…,"to":…,"paneId"?:…}]`, `paneId` for a report of another
worker that ran in that worktree); `result PANE` and `status` read each worker's
report at its new path. A result file that cannot be moved keeps the worktree
and branch, with `kept` saying why (`cannot move the result file <from> to
<to>: …`, `cannot make <main>/.noa: …`, `cannot read <wt>/.noa: …`); the files
are moved even when git then refuses. When git refuses
(uncommitted changes, a branch not merged into your `HEAD`), the work stays,
`"kept"` gives git's reason, and the exit status is unchanged. When the worktree
and branch exist but the pane's cwd was not in it, nothing is removed and the line
gains `"worktree":{"path":…,"removed":false,"branchDeleted":false,"kept":"the pane's cwd is not in this worktree"}`.
A labelled pane (a label `--worktree` takes) with no such worktree gets
`"worktree":{"removed":false,"branchDeleted":false,"kept":"<reason>"}`:
`no worktree <main>-wt/L on orch/L in this repository`, or `no orch/L worktree
found from the pane's cwd <cwd>: <git's error>`. A worktree an older `+ctl` made
inside another worktree (`<repo>-wt/A-wt/L`) is not found this way.
A pane that went away between the listing and its close (`"gone":true`) is pruned
the same way. Merge the
branch first, or read what would be lost before removing it by hand.

### `retry PANE [--tier light|standard|deep] [--task TEXT | --task-file F] [--ready SECS] [wait options] [--no-board]`

Starts one of this pane's `claude`, `codex` or `agy` workers again, for a
result that failed your check: one command for `close` (no `--prune`), `spawn`
in the same directory under the same label, and `send` with the task. PANE must
be a worker this pane's journal recorded a spawn of (listed or already gone);
any other is refused (exit 1, `pane N is not a worker this pane spawned`). Each
of these is also exit 1 with nothing closed or started:

- PANE is listed and its agent is `running`: wait for its turn to end, or
  `close` it first (a gone worker is retried from the journal).
- No command is known (journal `argv`, else Noa's `spawn.argv`), or it is not
  `claude`, `codex` or `agy`: the task has to be typed.
- `--tier` on a command that picks its own model or effort (as `spawn --tier`
  refuses it), or the old tier's flags are not right after the program.
- No task: without `--task`/`--task-file`, the old worker's last task is sent,
  which is the last `send` that was not `--answer`, not `--raw` keys, not empty,
  whole from the journal (`full`). One the journal kept only the start of (over
  64 KiB, or sent by an older `+ctl`) asks for `--task-file`. After follow-up
  sends, the last one is all it has: pass the whole task with `--task-file`.
- The directory is gone (a pruned worktree), the program is not on `PATH`, or
  a program given by its absolute path is no longer an executable file.
- The text with its preface is over 250 KiB.
- `--no-board` from a pane that is itself a worker (as `spawn` refuses it).

Then it closes PANE if it is still listed (journaled as a `close`, not printed,
never pruned) and spawns:

- Command: the journal's `argv` with the old `--tier`'s flags removed, then
  `--tier`'s added (default: the old tier again; none when it had none).
- Directory and label: the old worker's cwd and label. No worktree is made:
  a `spawn --worktree` worker goes on in its worktree and on `orch/LABEL`, so
  `close --prune` later finds it as before. Its `--env`, `--no-wait-after` and
  `--focus` are not in the journal and are not carried over.
- Task: a preface, a `---` line, then the task. The preface says a previous
  worker (pane N; its program and tier, or the model it named) worked on this
  task in this directory and stopped (`its last outcome: <reason>` when the
  journal has one); its changes, committed or not, are still here, so check
  `git status` and `git log` first and build on them; and `Its report: <path>`
  when its result file holds one. Retrying a retry replaces the earlier
  preface instead of stacking it.
- From there it is `spawn --task`: `--ready` (default 30), the wait options
  `--for`, `--all`, `--timeout`, `--tail`, `--stall`, `--settle`,
  `--no-board`, `"sent"`, the wait lines and exit status. The new worker gets
  a result file of its own (`result-LABEL-<8 hex>.md` while the old one is
  there).

The spawn line has `"retryOf":"N"`, `"tier"` when set, and `"worktree"` and
`"branch"` when the old worker had them; the journal's `spawn` has them too, so
`status` shows `retryOf` and the worktree's `ahead`/`dirty`. When the spawn
fails after the close (no spawn line printed), PANE stays closed with its work
in the directory, and `retry PANE` again starts from the journal; once a spawn
line was printed, the new worker exists: go on with it, not with PANE.

### `board [--of PANE]`, `board --open [--split right|down | --tab]`

A live view for people, not for scripts. The first `spawn` opens one by itself.
`board --open` starts a board pane next to this pane (a new tab when there is no room) and prints its `{"paneId":"…"}`;
when this pane's board is already open it opens nothing and prints
`{"paneId":"…","existing":true}`. The board is not one of this pane's workers. `board` without `--open` takes over the
pane it runs in until `q`, and is refused (exit 1) unless stdin and stdout are
a terminal, so a script or an agent's tool shell cannot start it by mistake. Workers that went back to this pane when its
board closed return to the board that `board --open` (or the next `spawn`)
opens, up to its 20 slots; one that has keyboard focus stays where it is.

The board shows what a worker was asked and whether it reported, from the
same files `status` and `result` read: the last task in its parent's journal
(the first 200 characters, as journaled) and its result file (`$NOA_RESULT`:
the journal's, else `<cwd>/.noa/result-<label>.md` for a plain label). The
preview's last line says `result written` once the file is non-empty; the
detail view (`Enter`) has a Task and a Result section (not created, empty
(not written yet), or N bytes written how long ago, and the path), and `r`
there swaps the screen box for the file's start (up to 16 KiB and 200 lines;
`j`/`k` scroll it). Only a regular file is read. The board looks at journals
and result files every 2 seconds, reads a journal again only when its time or
length changed, and reads a result's text only while it is shown, so a task
sent or a report written shows within about 2 seconds.

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
| 1 | Error: bad arguments, unknown pane, refused input, a pane that is not yours, a spawn over `--max-children`, a request over Noa's size limit ("Noa closed the connection because a request was over its size limit"). An argument error prints only its message and a pointer to `--help` |
| 2 | `wait` timed out, or was interrupted (`"interrupted":true`) |
| 3 | Server disabled, unreachable, missing a scope, gone mid-command, full, or not answering a request within 5 s ("did not answer in time"), or a request Noa did not confirm in time (`-32009`, "it may still have taken effect"). After either of the last two, check with `ls` or `read` before retrying. stderr says which: "sandbox likely blocks loopback" (your shell's sandbox refused the connection; run `+ctl` outside it), "not listening" (Noa's server is off), "too many control connections open" (see below), or a message naming the config to add |

Noa holds 32 connections at once; each running `wait`, `send --wait`, board and
attach holds one. A connect refused at that limit, or one Noa drops during the
WebSocket handshake (it does so past 4 refusals at once), is retried for 3 s
(100 ms, doubling), then exits 3 with "Noa has too many control connections open (each
running `wait`, `send --wait`, board and attach holds one); end some, or try
again later", or for a dropped handshake "cannot reach Noa at <url>: the
server dropped the connection during the websocket handshake: …; Noa may be at
its connection limit: …". A Noa older than 1013 drops every such connection
unanswered, which is retried the same way.

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

Flags that pick a worker's model and effort. `spawn --tier T -- claude` adds
the cells below (for Claude Code's `light`, the code-editing cell); write the
flags yourself (`spawn --label rename -- claude --model haiku --effort low`)
for another choice, such as Haiku for a read-only task. Tiers
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

Several workers, handling whichever stops first (workers that report turns):

```sh
noa() { "$NOA_BIN" +ctl "$@"; }
a=$(noa spawn --label api --ready 30 -- claude | jq -r .paneId)   # check "ready" and "screen"
b=$(noa spawn --label ui --ready 30 -- codex | jq -r .paneId)
sa=$(noa send "$a" --enter - < "$d/task-api.txt" | jq -r .seq)
sb=$(noa send "$b" --enter - < "$d/task-ui.txt" | jq -r .seq)
noa wait --children --after "$a=$sa" --after "$b=$sb" --settle 10 --stall 300 --timeout 100
```

Run the `wait` as its own tool call and repeat it. Each time, handle the lines
it printed, then pass every pane's newest base: the `seq` a line or a later
`send` printed for a pane you dealt with, the timeout line's `baseline` for the
rest. Drop panes you closed. `--after` covers reports only: a worker that only
raises `attention` and finishes before a `wait` starts is never seen. For such
workers run one `send --wait` each with your tool's background mode:

```sh
noa send "$a" --enter --wait --timeout 1800 - < "$d/task-a.txt" > "$d/a.json" &
noa send "$b" --enter --wait --timeout 1800 - < "$d/task-b.txt" > "$d/b.json" &
wait
```

A wait longer than your shell tool allows: one `--timeout 100` call per tool
call, carrying the timeout line's `baseline` into the next. A loop inside one
call is only for your tool's background mode, since the tool's limit covers the
whole call:

```sh
out=$(noa send "$a" --enter --wait --timeout 100 "…task…")
while [ $? -eq 2 ]; do
  seq=$(echo "$out" | jq -r '.baseline[0].seq')
  out=$(noa wait "$a" --after "$a=$seq" --timeout 100)
done
```

A command instead of an agent:

```sh
t=$(noa spawn --label tests -- cargo test --workspace | jq -r .paneId)
noa wait "$t" --for exit --timeout 100 | jq .exitCode   # exit 2: run it again in a new tool call
noa read "$t" --scrollback --max-bytes 65536 | tail -50
noa close "$t"
```

Clean up everything this pane started:

```sh
noa close --children
```

## Known limits

- With a Noa older than `maxChildren`, parallel spawns from one orchestrator can
  exceed `--max-children`.
- A spawn or send that exits 3 may still have taken effect: `ls --children` or
  `read` before retrying, or the worker starts (or the text is typed) twice.
