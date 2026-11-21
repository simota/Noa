---
name: noa-orchestration
description: Delegate work to other coding agents (claude, codex, agy) or long commands in Noa worker panes via `"$NOA_BIN" +ctl`, each on a model tier sized to its task. Use in a Noa pane when work splits into parallel parts, when a part suits codex, agy or a cheaper or faster model, or when asked to delegate, fan out or orchestrate (並列で, 別エージェントに任せて, codexに投げて). Not outside Noa (NOA_BIN unset).
---

# Orchestrating worker panes in Noa

You are the orchestrator. Your first spawn opens a board pane next to yours
where the human watches every worker, most urgent first. Each worker is a
floating pane stowed on that board, started and controlled with
`"$NOA_BIN" +ctl`; your own pane stays clear. Every command prints one JSON object per
line, except `read`, which prints the pane's text. Full command reference,
output shapes and exit codes: [reference.md](reference.md).

## Before starting

1. Check that you can: `"$NOA_BIN" +ctl self`.
   - `NOA_BIN` unset: you are not in a Noa pane. Stop; this skill does not apply.
   - Exit status 3 with "sandbox likely blocks loopback" (`Operation not
     permitted`): your own shell's sandbox refuses the connection, not Noa. Run
     every `+ctl` call outside the sandbox, and tell the human once, so they can
     allow it instead of approving each call.
   - Exit status 3 otherwise: read the stderr line. "not listening" means Noa's
     server is off; a scope or disabled-server message names the config line to
     add (`server-enable = true`, `server-scopes = read,control,input`). Ask the
     human to add it; do not edit their Noa config yourself.
   - "does not report surface ids", or "this pane … is not among the panes": you
     reached a different Noa than the one you run in. Usually an older Noa or a
     second instance still holds the control port, so this one could not
     listen. Find it with `lsof -nP -iTCP:<port> -sTCP:LISTEN` and tell the
     human which process holds the port. Do not kill it yourself.
2. Decide what each worker gets: which agent (or command), which model tier
   (see "Matching the worker to the task"), which directory or worktree (see
   "Giving a worker its own worktree"), and a task that stands on its own. A
   worker sees nothing of your conversation. End every task with "Write your
   final report to the file named by $NOA_RESULT." Write task texts and other
   scratch files under `d=$(mktemp -d)`, never in the repository.
3. Keep the number of workers small (20 at most: the board holds 20; a
   worker keeps its slot until you `close` it, also after it exits) and
   tell the human what you are about to start, with each worker's tier, and
   that the board will show it.
4. Leave the board to the human. It opens by itself on your first spawn; if they
   closed it, `"$NOA_BIN" +ctl board --open` brings it back. Never run
   `+ctl board` without `--open` yourself: it takes over the pane it runs in.

## Matching the worker to the task

Give each task the cheapest, fastest worker that can do it, and keep the
expensive models for work that needs them. Pick a tier per task:

| Tier | Task looks like | Worker |
|---|---|---|
| none | Build, test, lint, benchmark, any command whose output you only read | No agent: `spawn -- cargo test …`, then `wait --for exit` |
| `light` | Fully specified and easy to check: rename, mechanical edits across files, boilerplate tests from a given pattern, find-and-summarise, docs from facts you hand over | Smallest model, low effort |
| `standard` | Ordinary implementation or a fix whose cause is known, with a clear done-condition | Mid model, medium effort |
| `deep` | Design choices, a bug whose cause is unknown, cross-cutting refactors, concurrency, security, reviewing other workers' output | Largest model, high effort |

- Set the tier with `spawn --tier light|standard|deep -- claude` (or `codex`,
  `agy`): it adds the model and effort flags listed in reference.md, "Model
  tiers". For a read-only `light` task on Claude Code, write
  `-- claude --model haiku --effort low` yourself instead. Pass flags on the
  command line only; do not change the human's agent config.
- A model or effort the human named for the task wins over the table.
- When a task fits two tiers, take the higher one. A retry costs more than the
  difference.
- When a `light` or `standard` worker's result fails your check, retry with
  a new worker one effort level higher on the same model first; if that also
  fails, go one tier up. Do not repeat the same model and effort.
- Split a mixed task so the mechanical part goes to `light` and only the hard
  part to `deep`, when the parts are independent.
- Your own check of each result does not get cheaper with the worker's tier.

## Giving a worker its own worktree

Workers that share one checkout overwrite each other's edits, see each
other's half-done changes in builds and tests, and race on the git index.
Decide per worker:

| Worker | Directory |
|---|---|
| Edits files while another worker (or you) edits the same repo | Its own worktree |
| Builds or tests a branch other than the one checked out | Its own worktree |
| Makes an experimental change you may throw away | Its own worktree |
| Only reads: review, search, summarise, answer a question | The shared checkout |
| The only one editing, and you wait for it before editing yourself | The shared checkout |
| A command that tests the current tree while nobody edits | The shared checkout |

Let `spawn --worktree` create it, so you know its path and branch. Do not use
an agent's own worktree flag: you would not know where it put the work.

```sh
# $d/$label.txt: "You are in a git worktree on branch orch/$label. …task…
# Commit your work on this branch. Do not push or switch branches.
# Write your final report to the file named by $NOA_RESULT."
noa spawn --worktree --label "$label" --tier standard --task-file "$d/$label.txt" \
  --settle 10 --stall 300 --timeout 80 -- claude   # first line also has "worktree" and "branch"
```

It runs `git worktree add -b orch/$label <main>-wt/$label HEAD`: next to the
main worktree wherever you run it, branching from your checkout's `HEAD`. A git
failure (the branch or directory already exists) starts nothing. A new
directory may show a trust prompt, and the spawn line then has `"sent":false`
(see "One worker, one round").

- A worktree starts from a commit: your uncommitted changes to tracked files are
  not in it (`spawn --worktree` warns on stderr). Commit them first, or tell the
  worker what is missing. Untracked files are warned about on a line of their
  own; with your scratch files outside the repository, that line means files
  the worker may need.
- A worktree has no build outputs, installed dependencies or ignored files
  (`.env`, local config). Its first build is cold; budget the timeout for it,
  and keep the number of worktrees that build at once small.
- After a worker finishes, check its branch from your checkout
  (`git log -p HEAD..orch/$label`, then build and test it) before you merge.
  Merge branches one at a time and run the checks after each. A conflict goes
  to you or to a `deep` worker, not back to the worker that wrote one side.
- Clean up with `noa close --prune PANE` once the branch is merged (or
  dropped): it closes the pane, finds the worktree from the pane's cwd, then
  runs `git worktree remove` and `git branch -d`, never forced. When git refuses
  (uncommitted changes, branch not merged), or no worktree is found, the line
  says why under `worktree.kept` and the work stays. Do not remove it with
  `--force` or `-D` without reading what would be lost; keep it and tell the
  human instead.
- To retry a worktree worker, `close` it without `--prune`, then
  `spawn --cwd <its worktree> --label <same label> --tier <one up>`: the new
  worker carries on in the same worktree and branch, and `--prune` still finds
  it later. Say in the task that the previous attempt's changes are there.
  Its `$NOA_RESULT` is the same file, so trust it only once a wait line names
  it as `result`. Discarding the changes (`git reset --hard`) is the human's
  call. A new label with `--worktree` would split the work over two branches.
- If `spawn --worktree` fails with a permission error, your own shell may be
  sandboxed away from `.git/worktrees`; say so rather than working around it.

## One worker, one round

```sh
noa() { "$NOA_BIN" +ctl "$@"; }
d=$(mktemp -d)
printf '%s\n' "Fix the failing test in crates/foo. Write your final report to the file named by \$NOA_RESULT." > "$d/fix-tests.txt"
noa spawn --label fix-tests --task-file "$d/fix-tests.txt" --settle 10 --stall 300 --timeout 80 -- claude
noa result PANE            # PANE: "paneId" of the first line
noa close PANE
```

`spawn --task-file` waits for the agent to be ready, types the task, and waits
for the turn like `send --enter --wait`. The first line is the spawn's
(`paneId`, `result`, `ready`, `screen`, `sent`), the rest are wait lines.

- `"sent":false`: nothing was typed (the screen was not an agent's input box
  in time). Read `screen`. A trust prompt is the human's to answer unless their
  instructions cover it, or the directory is a worktree you made with
  `spawn --worktree` (see "Running without the human"); a login prompt is the
  human's. Then send the task yourself:
  `noa send PANE --enter --wait --settle 10 --stall 300 --timeout 100 - < "$d/fix-tests.txt"`.
- `--settle 10` holds a `finished` until the work above the input box has
  stayed still for 10 s: Claude Code workers have been seen to keep working
  after reporting `finished`.
- Read the report with `noa result PANE`, not the screen: `read --scrollback`
  on an agent returns only about its last screen. A wait line has
  `"result":{"path","bytes"}` once the worker wrote it during that wait; without
  it, the file is missing or left from an earlier task.
- For a later task, `send PANE --enter --wait …`. It subscribes before typing,
  so a fast reply cannot be missed; a separate `send` then `wait` can miss a
  turn that ends in between (except with `--after`, "Several workers").
- `read` the pane before every later `send`. `send` itself refuses a pane whose
  agent is working (wait for its turn to end; `--queue` only when you mean to
  queue, and then the wait may end with the turn already running) and one
  waiting for an approval or a question (`--answer`, see "Reading why a wait
  ended"). What it cannot see is the input line: it may still hold text, and
  `send` types after it. When the text there is yours (a `send` whose Enter did
  not go through), send again with `--replace`, which clears the input line
  first. When someone else typed it (the human, in an opened worker), do not
  send; tell the human what is there. Claude Code shows a dim suggestion there
  (`❯ Try "…"`, or a proposed next prompt) that `read` prints like typed text;
  it is replaced when you type, so it does not count as a draft.
- `--tail N` adds the last N lines above an agent's input box to each wait line
  as `tail`; a `permission` or `input` line carries the dialog as `prompt`
  anyway.
- Give every wait a timeout, and see "When no report comes" below if it runs
  out.
- Your shell tool has a time limit (Claude Code's Bash: 2 minutes by default).
  Keep every foreground wait under it: `--timeout 100`, or 80 with `--task`,
  whose ready wait (up to 30 s) comes first. Carry on from the timeout line in
  a new call: `noa wait PANE --after PANE=SEQ --timeout 100` with each pane's
  `baseline`. A wait the tool kills with SIGTERM still prints that line, with
  `"interrupted":true`; `send --wait` also prints `resume with: wait N --after
  SEQ` on stderr once the text is in. A notification (`attention`) is not
  carried over: for a worker that only notifies, `read` it after a timeout.

## After your context was compacted

Run `noa status` before anything else: one line per worker with its state,
`lastTask`, `lastSentSeq`, `lastOutcome`, `result`, and for a worktree worker
`ahead` and `dirty`; workers that are gone come last. A worker whose
`lastOutcome.seq` is above its `lastSentSeq` already ended the turn of your
last send: take its `result` before anything else. Otherwise wait with
`--after PANE=<lastSentSeq>`.

## Several workers in parallel

Handle whichever worker stops first, so one stuck on a prompt does not wait
for the slowest:

```sh
noa spawn --label api --ready 30 -- claude   # "paneId" A; check "ready" and "screen" as above
noa spawn --label ui --ready 30 -- codex     # B
noa send A --enter - < "$d/api.txt"          # {"paneId":"A","seq":SA}
noa send B --enter - < "$d/ui.txt"           # SB
noa wait --children --after A=SA --after B=SB --settle 10 --stall 300 --timeout 100
```

Repeat the `wait`, one tool call each time. Handle the lines it printed (take a
`finished` worker's `result` and check it; answer or ask about a prompt), then
pass every pane's newest base: the `seq` your latest `send` to it printed, else
the `seq` of the line you handled, else its `baseline` from the timeout line.
Leave out panes you closed. `--after` covers reports only, so for workers that
never report (an agent too old, or one that only notifies), run one
`send --wait` per worker with your tool's background mode instead:

```sh
noa send A --enter --wait --timeout 1800 - < "$d/api.txt" > "$d/api.json" &
noa send B --enter --wait --timeout 1800 - < "$d/ui.txt" > "$d/ui.json" &
wait
```

## When no report comes

A wait ends only on a report, a notification, an exit, or with `--stall
SECS` a screen that stopped changing. If a worker never reports the end of its
turn, a wait without `--stall` runs until its timeout, so do not use an
unbounded wait. Add `--stall 300` (or what fits the task) to long waits. Claude Code's
working spinner shows elapsed seconds, so its screen keeps changing while it
works, and `stalled` there means it went back to its prompt without reporting
or sits on a prompt it does not report. For Codex and Antigravity CLI this is
not checked: a quiet screen may also be an agent still working. In every case
`read` the pane and continue as in step 2 below. When a wait times out:

1. `noa ls --children`: note `agent.state` and `agent.seq` for each pending pane.
   Do not judge by `busy`: it only says a shell reported a running command, so
   it stays `false` for an agent started directly (`spawn -- claude`).
2. `noa read PANE`: look at the screen. If the agent is back at its prompt, treat
   the turn as over and continue from what the screen shows.
3. If the screen shows it still working, wait again with a new timeout, passing
   the timeout line's `baseline` as `--after PANE=SEQ`.

Tell the human when a worker's reports are missing: its agent may be older than
the versions that report on their own (see reference.md, "Which agents report
turns").

## Reading why a wait ended

Each line has `paneId` and `reason`:

| reason | What to do |
|---|---|
| `finished` | Turn ended (and held still for `settledSecs` with `--settle`). `noa result P` when the line has `result`, else `read` it; check the work yourself |
| `permission` | Waiting for an approval, shown in `prompt` (`read` the pane if it looks cut). Approve or answer only if the human's instructions already cover it, or under "Running without the human"; otherwise ask the human. To accept the selected choice: `send P --answer --enter ""`; to type a choice's key or Esc: `send P --answer --raw 2` or `--raw $'\e'`. `read` again afterwards |
| `input` with `"blocked":"question"` | A question dialog, in `prompt`. Enter picks its first choice, so answer with its key (`--answer --raw 2`), under the same rules as an approval |
| `input` without `blocked` | Idle at its prompt (or a Noa too old to say `blocked`): `read` it before sending |
| `error` | The agent reported an error. `read` it. A login, plan, quota or unavailable-model message goes to the human, not to a retry; otherwise retry, re-scope or report |
| `attention` | A desktop notification arrived. Not proof the agent is blocked or done: `read` and decide |
| `exit` | The worker's process ended (`exitCode` when known) or the pane is gone (`closed: true`). It will not report again. With `agentLeft: true` the agent quit to its shell and the pane is open: `read` why, then close it or start a new worker |
| `stalled` | Only with `--stall`: the screen has not changed for `quietSecs`. `read` it; see "When no report comes" |

Exit status 2 means the timeout passed. The line `{"timeout":true,"pending":[…]}`
lists the panes still running, each with its `seq` and agent `state` under
`baseline`; panes that had already exited are under `exited`. Decide whether to wait longer, `read` them, or
close them.

## Running without the human

When the human has said to carry on without them (they are away, or asked you
to run the work unattended), answer worker prompts yourself instead of
stopping at each one, within these limits:

- Approve what stays inside the worker's own worktree and its task: reading
  files, editing files in the worktree, building, testing, linting,
  formatting, committing on its `orch/…` branch, and writing its
  `$NOA_RESULT`.
- Refuse (Esc, or the prompt's "no" choice) anything that pushes, publishes or
  deploys, deletes or writes outside the worktree, installs or downloads,
  changes the human's config, credentials or agent settings, or acts on other
  panes. Then tell the worker what to do instead, or leave that part for the
  human.
- Accept the trust prompt of a worktree you made with `spawn --worktree` from
  the repository you are working in. Any other trust prompt, and any login,
  plan or quota prompt, is the human's.
- Anything you cannot place in those lists: leave it waiting, keep the other
  workers going, and ask the human when they are back.
- Workers in the shared checkout get none of this: their prompts are the
  human's.
- Give the human `noa status --answers` with your report: every prompt you
  answered, with its screen and your answer.

This is not leave to pass auto-approve flags or to enable Noa's auto-approve
(see "Rules"); you still read each prompt before answering it.

## Rules

- Exit 3 from `spawn`, `send` or `close` can mean Noa did it without confirming
  in time. `ls --children` or `read` before retrying, or you start a worker or
  type a task twice.
- Only `send` to and `close` your own workers. `--any-pane` exists for a pane the
  human pointed you at, never to drive panes you found in `ls`.
- Do not pass auto-approve flags to a worker agent or enable Noa's auto-approve
  unless the human asked for it.
- Close every worker you started when you are done, also after a failure
  (`noa close --children`). A worker that is already gone is not an error. Read what you need first: closing discards the
  output. The board stays; it is the human's to close (closing it hands
  any remaining workers back to your pane, still running).
- Remove every worktree you created once its branch is merged or dropped
  (`noa close --prune`), and list any you kept (with their branch) for the
  human.
- Workers are always floats: `spawn` has no `--split` or `--tab`. Do not open
  workers in panes or tabs some other way.
- `wait` can only see turns an agent reports. Current Claude Code, Codex and
  Antigravity CLI report on their own, with no hooks. Without reports, a worker
  ends a wait only with `exit`, `attention`, or `stalled` (with `--stall`). For plain commands, that is
  enough: `spawn -- make test`, then `wait PANE --for exit`.
- Nesting goes one level deep. A worker may start workers of its own (tell it
  so in its task when you want that); they appear under it on your board and
  share its 20 slots. A worker's worker cannot start more. Closing a worker
  also closes the workers it started.
