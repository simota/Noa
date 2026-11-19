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
   worker sees nothing of your conversation.
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
noa() { "$NOA_BIN" +ctl "$@"; }
out=$(noa spawn --worktree --label "$label" --tier standard -- claude) || exit 1
pane=$(echo "$out" | jq -r .paneId)      # $out also has "worktree" and "branch"
noa read "$pane"                         # a new directory may show a trust prompt first
noa send "$pane" --enter --wait --timeout 540 "You are in a git worktree on branch orch/$label. …task… Commit your work on this branch. Do not push or switch branches."
```

It runs `git worktree add -b orch/$label <repo>-wt/$label HEAD` from your
checkout, and a git failure (the branch or directory already exists) starts
nothing. `ls --children` shows each worker's `cwd`.

- A worktree starts from a commit: your uncommitted changes are not in it
  (`spawn --worktree` warns on stderr). Commit them first, or tell the worker
  what is missing.
- A worktree has no build outputs, installed dependencies or ignored files
  (`.env`, local config). Its first build is cold; budget the timeout for it,
  and keep the number of worktrees that build at once small.
- After a worker finishes, check its branch from your checkout
  (`git log -p HEAD..orch/$label`, then build and test it) before you merge.
  Merge branches one at a time and run the checks after each. A conflict goes
  to you or to a `deep` worker, not back to the worker that wrote one side.
- Clean up with `noa close --prune PANE` from your checkout once the branch is
  merged (or dropped): it closes the pane, then runs `git worktree remove` and
  `git branch -d`, never forced. When git refuses (uncommitted changes, branch
  not merged), the line says so under `worktree.kept` and the work stays. Do
  not remove it with `--force` or `-D` without reading what would be lost; keep
  it and tell the human instead.
- If `spawn --worktree` fails with a permission error, your own shell may be
  sandboxed away from `.git/worktrees`; say so rather than working around it.

## One worker, one round

```sh
noa() { "$NOA_BIN" +ctl "$@"; }
pane=$(noa spawn --label fix-tests --cwd "$PWD" -- claude | jq -r .paneId)
noa read "$pane"
noa send "$pane" --enter --wait --timeout 540 --tail 40 "Fix the failing test in crates/foo. Report what you changed."
noa read "$pane" --scrollback --max-bytes 65536
noa close "$pane"
```

- Always `send … --wait`. It subscribes before typing, so a fast reply cannot be
  missed. A separate `send` then `wait` can miss a turn that ends in between.
- `read` the pane before every `send`, not only the first. The first screen may
  be a trust or login prompt. Later, the input line may still hold text (a
  draft or a suggestion), and `send` types after it rather than replacing it.
  When the text there is yours (a `send` whose Enter did not go through), send
  again with `--replace`, which clears the input line first. When someone else
  typed it (the human, in an opened worker), do not send; tell the human what
  is there. Claude Code shows a dim suggestion there (`❯ Try "…"`, or a
  proposed next prompt) that `read` prints like typed text; it is replaced when
  you type, so it does not count as a draft. A trust prompt on the first screen
  is the human's to answer unless their instructions cover it, or the
  directory is a worktree you made with `spawn --worktree` (see "Running
  without the human").
- A `finished` does not always end the work: Claude Code workers have been
  seen to keep working after reporting it (the cause, possibly a hook, is not
  confirmed). If `read` shows the agent still working, wait again with
  `--after` the `seq` from the line.
- `--tail N` adds the pane's last N output lines to each wait line as `tail`,
  which often saves the `read` after a wait.
- Give every wait a timeout that fits the task, and see "When no report comes"
  below if it runs out.
- Your own shell tool has a time limit too (Claude Code's Bash: 2 minutes by
  default, 10 at most). A wait it kills prints nothing, so keep `--timeout`
  under that limit, or run the whole command with your tool's background mode
  (not a shell `&` inside a foreground call, which the limit still kills), and
  carry on from the timeout line: it lists each pending pane's `baseline`, and
  `noa wait PANE --after PANE=SEQ --timeout …` continues without missing a
  report that arrived in between. It does not cover a notification
  (`attention`): for a worker that only notifies, `read` it after a timeout.

## Several workers in parallel

Run one `send --wait` per worker in the background and collect each result.
Waiting on `--children` after separate sends has the same race as above. Run
this whole block with your tool's background mode, since it lasts as long as
the slowest worker.

```sh
for p in "$a" "$b"; do
  noa send "$p" --enter --wait --timeout 1800 --tail 40 - < "task-$p.txt" > "result-$p.json" &
done
wait   # the shell's wait: every worker's turn has ended
```

For "whichever stops first", use `noa wait --children` only for panes whose
current turn started before the wait (for example, the next time round a loop).

## When no report comes

A wait ends only on a report, a notification, an exit, or with `--stall
SECS` a screen that stopped changing. If a worker never reports the end of its
turn, a wait without `--stall` runs until its timeout, so do not use an
unbounded wait. Add `--stall 300` (or what fits the task) to long waits: an
agent's working spinner keeps its screen changing, so `stalled` means it went
back to its prompt without reporting, or sits on a prompt it does not report.
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
| `finished` | Turn ended. `read` the pane and check the work yourself |
| `permission` / `input` | The agent is blocked on a prompt, or (`input`) idle at its prompt. `read` it. Approve or answer only if the human's instructions already cover it, or under "Running without the human"; otherwise ask the human. `send` refuses a pane in `permission` unless you pass `--answer`. To accept the selected choice: `send P --answer --enter ""`; to type a choice's key or Esc: `send P --answer --raw 2` or `--raw $'\e'`. `read` again afterwards |
| `error` | The agent reported an error. `read` it. A login, plan, quota or unavailable-model message goes to the human, not to a retry; otherwise retry, re-scope or report |
| `attention` | A desktop notification arrived. Not proof the agent is blocked or done: `read` and decide |
| `exit` | The worker's process ended (`exitCode` when known) or the pane is gone (`closed: true`). It will not report again |
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
  formatting, and committing on its `orch/…` branch.
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
- Keep a list of every prompt you answered and how, and give it to the human
  with your report.

This is not leave to pass auto-approve flags or to enable Noa's auto-approve
(see "Rules"); you still read each prompt before answering it.

## Rules

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
