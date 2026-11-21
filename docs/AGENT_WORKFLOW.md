# Coding agent workflows

## Fullscreen agents

Codex's fullscreen UI keeps its own conversation history. Output Snapshot and
Noa's search can access only the current alternate screen. Use Codex's search
or `/copy` for earlier messages, or choose Scrollback with `/tui`.

Cmd+C copies Noa's selection first. With no terminal selection, Noa forwards
Cmd+C to fullscreen applications that negotiate the Kitty keyboard protocol;
the application decides what to copy.

To let Codex extend its selection with Shift+click, set
`mouse-shift-capture = true` in Noa's config. The default reserves Shift for
Noa's selection unless the application requests it with XTSHIFTESCAPE.
`never` always reserves Shift for Noa; `always` always passes it to applications
with mouse reporting enabled.

Antigravity CLI's automatic image detection over SSH currently checks terminal
brands. On a remote host, run `CLI_GRAPHICS=kitty agy` to explicitly select Kitty
graphics supported by Noa. Multiplexers must also support image passthrough.

The macOS app bundle includes a microphone usage description and audio-input
entitlement for voice-capable terminal applications. macOS still controls the
microphone permission; packaging does not grant recording permission itself.

## Settings panel

Open Settings with Cmd+,. Use Tab to search for File Link Editor,
then Enter to select the row. Use Left/Right to choose System Default,
VS Code, Cursor, Zed, or Sublime Text, and Enter to save.
The editor changes on save without restarting Noa. Escape cancels changes;
Delete or Cmd+Backspace resets to System Default. Cmd+Z on the saved
settings toast restores the previous editor.

The Agent Workflows row opens this built-in guide with Left/Right.
Opening it keeps unsaved settings intact. Close the guide and Settings before
running the prompt/reader commands below from the command palette.

## Notifications

Unread notifications belong to individual panes. Selecting a pane acknowledges
only that pane. An unselected pane can become unread even while its window is
focused. Desktop notifications retain their existing window-level suppression.

Use Cmd+Shift+J, or Go to Next Unread Notification in the command palette,
to visit unread panes in the current window group. The configurable action is
`session.next-notification`.

## Writing a prompt

Open Compose Prompt (`agent.compose-prompt`) from the command palette.
The modeless native editor supports multiline text, Japanese IME, selection,
undo, and Find. Its title identifies the destination session, branch, and
directory. Selected terminal text becomes a quoted starting point when there
is no draft.

Paste to Pane uses the existing bracketed paste/protection path and does
not send Enter. If the foreground process changed, reopen the composer to
review the destination. Confirm an active IME composition before pasting.

Drafts stay in memory per pane, including across panel closes and pane moves.
They are removed on pane closure and are not saved across app restarts.
Prompts are limited to 1 MiB; oversized text must be shortened before pasting
or closing the editor. Optional shortcut:

```conf
keybind = cmd+shift+e=agent.compose-prompt
```

## Reading output

Open Read Output Snapshot (`terminal.read-output`). It shows the selection,
or the retained output tail when nothing is selected, in a read-only native
view. Snapshots are limited to 1 MiB. Live output cannot move the reader's
selection or scroll position.

The view supports Find and copying, emphasizes Markdown headings, and uses
monospace for fenced code. Copy Next Code Block selects and copies each
fenced block in order, excluding fences. This is basic text/Markdown styling,
not a full HTML/Markdown renderer.

New terminal output adds New output available to the title. Return to
Latest focuses the source pane at the live tail. Reopen the snapshot to read
updated output.

## File navigation and preview

Choose File Link Editor in Settings, or configure it directly:

```conf
file-link-editor = code
```

Supported values: `default`, `code`, `cursor`, `zed`, `subl`. The editor CLI
must be in Noa's inherited PATH. Cmd-click passes the absolute filename, line,
and column as one argument; `code`/`cursor` also receive `--goto`. No shell
evaluates the path. Directories and failed launches use the default macOS
handler. `default` preserves ordinary opening without line navigation.

Cmd+Option+click previews a detected local path in a monospace panel.
Only regular UTF-8 files are previewed, up to 1 MiB; a worker reads the file.
Remote-pane paths remain excluded from local opening.

## Explicit agent state

Noa accepts this informational extension:

```text
ESC ] 777;noa-agent;<state>;<detail> ESC \
```

| State | Meaning |
|---|---|
| `running` | Processing a prompt or tool operation |
| `permission` | An explicit permission request is outstanding |
| `input` | An explicit user-input request is outstanding |
| `finished` | The response ended; this does not assert task completion |
| `error` | The turn ended with an error |
| `clear` | Clear status; detail must be empty |

Details are bounded to 160 characters with control characters removed.
Unknown states are ignored. Reports only update display state and never
authorize an operation or send input. Reading a notification does not resolve
a permission/input request. A `running` or `clear` report clears stale unread
state. Generic BEL/OSC notifications retain the neutral `notification` label.

### Claude Code hooks

Claude Code reports its state on its own through OSC 7501, so these hooks are
optional; they add tool names and the `PermissionRequest` moment.

The included `scripts/noa-agent-hook.py` reports only event kinds and tool
names, and reads no transcripts. Claude Code runs hooks without a controlling
terminal, so the script prints the report as the hook's `terminalSequence`
output, which Claude Code writes to the terminal for it (it writes to the
controlling terminal directly when it has one). Claude Code honours
`terminalSequence` only for hooks in user settings (`~/.claude/settings.json`),
not project settings. Agent settings are not changed automatically.

Merge the following with the existing hooks in `~/.claude/settings.json`,
replacing the example script path with an absolute path to this checkout. Quote
the script path inside the command if it contains spaces.

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "python3 /absolute/path/Noa/scripts/noa-agent-hook.py"
          }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "python3 /absolute/path/Noa/scripts/noa-agent-hook.py"
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": ".*",
        "hooks": [
          {
            "type": "command",
            "command": "python3 /absolute/path/Noa/scripts/noa-agent-hook.py"
          }
        ]
      }
    ],
    "PermissionRequest": [
      {
        "matcher": ".*",
        "hooks": [
          {
            "type": "command",
            "command": "python3 /absolute/path/Noa/scripts/noa-agent-hook.py"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": ".*",
        "hooks": [
          {
            "type": "command",
            "command": "python3 /absolute/path/Noa/scripts/noa-agent-hook.py"
          }
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "permission_prompt|idle_prompt|elicitation_dialog|elicitation_url_dialog",
        "hooks": [
          {
            "type": "command",
            "command": "python3 /absolute/path/Noa/scripts/noa-agent-hook.py"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "python3 /absolute/path/Noa/scripts/noa-agent-hook.py"
          }
        ]
      }
    ],
    "StopFailure": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "python3 /absolute/path/Noa/scripts/noa-agent-hook.py"
          }
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "python3 /absolute/path/Noa/scripts/noa-agent-hook.py"
          }
        ]
      }
    ]
  }
}
```

Event meanings follow the [Claude Code hook reference](https://code.claude.com/docs/en/hooks).
Other agents can emit the protocol; only a Claude Code adapter is included.
Structured status feeds local cards and, through `Panel.agent`, noa-server clients
(see below).

A process in the pane can also report with `"$NOA_BIN" +ctl report <state> [detail]`,
which writes the same sequence to its controlling terminal. `NOA_BIN` is the
absolute path of the running Noa, set in every pane. Claude Code and Codex start
hooks in a new session without a controlling terminal, so from their hooks
`+ctl report` reports nothing.

## Orchestrating worker panes

An orchestrator (an agent or a script in one pane) can start worker panes, type
into them, wait for their agents, read their output, and close them with
`Noa +ctl`. Each command prints one JSON object per line. The first spawn
opens a board next to the orchestrator, and workers are floating panes stowed
on that board: the board's rows show them, and a worker appears over the board
only when opened. The orchestrator's own pane stays clear.

Enable the control server with the scopes the commands need:

```conf
server-enable = true
server-scopes = read,control,input
```

A typical round, from inside the orchestrator's pane:

```sh
noa() { "$NOA_BIN" +ctl "$@"; }
noa spawn --label tests --settle 10 --timeout 540 \
  --task 'Fix the failing test in crates/noa-vt. Write your final report to the file named by $NOA_RESULT.' -- claude
noa result PANE      # PANE: the "paneId" of the first line
noa close PANE
```

`--task` waits for the agent to be ready, types the task and waits for the turn
to end. When the agent's input box is not on screen in time (a trust or login
prompt), it types nothing and prints `"sent":false` with the `screen`; answer
the prompt and `send` the task yourself. Every worker gets its own report file
in `NOA_RESULT` (`<cwd>/.noa/result-LABEL.md`, with a random suffix when that
name is taken; `.noa/` ignores itself, so git
never sees it), which `result` prints.

| Command | What it does |
|---|---|
| `spawn [--cwd D \| --worktree] [--tier T] [--env K=V] [--label L] [--input T] [--no-wait-after] [--focus] [--no-board] [--max-children N] [--ready S] [--task T \| --task-file F [wait options]] [-- CMD...]` | Starts a worker in a floating pane stowed on this pane's board, opening the board first when none is open; the board's row shows the worker's state, and the board's `g` opens it over the lower part of the board. With `--no-board`, or when the board cannot open, the worker floats over this pane instead, collapsed to its title strip. `--focus` opens it and moves focus to it; otherwise focus stays where it was. The command runs without a shell, found on the orchestrator's own `PATH`, and the worker gets the orchestrator's locale (`LANG`, `LC_*`), which a Noa started from the Dock lacks; the cwd defaults to the orchestrator's (a relative `--cwd` is taken from it). A `--cwd` that is not a directory, or a command not found on the worker's `PATH`, is refused before anything starts. Finished workers stay open (with their exit code in `Panel.exit`) until closed, unless `--no-wait-after`. A board holds at most 20 stowed workers (its list scrolls), a pane at most 8 other floats, and one orchestrator 20 workers unless `--max-children` sets fewer; exited workers count until closed; Noa counts them too, one spawn at a time, so parallel spawns stay under the limit (an older Noa leaves only the count `+ctl` makes before each spawn). `--split` and `--tab` are refused, and so are a `--label` starting with `board:` and `--input` for an agent (it is typed as raw keys, each newline an Enter). `--worktree` (needs `--label L`, refuses `--cwd`) first runs `git worktree add -b orch/L <main>-wt/L HEAD`, next to the repository's main worktree, and starts the worker there; it prints `worktree` and `branch` too, and warns apart about tracked changes and untracked files the worktree lacks. `--ready S` waits up to S seconds for the worker to report or show a screen that stays still, and prints `ready` and the `screen`; `--task` (claude, codex or agy) then types the task and waits as `send --enter --wait` does. A spawn whose answer Noa did not confirm looks for its worker for about 3 s and prints `recovered:true` when it finds it. `--tier light\|standard\|deep` adds the model and effort flags of that tier to a `claude`, `codex` or `agy` command (refused when the command already picks a model or effort) |
| `send PANE [--enter] [--raw] [--replace] [--wait ...] [--answer] [--queue] TEXT\|-` | Pastes `TEXT` (or stdin) and, with `--enter`, submits it. Refused while the pane's agent waits for an approval or asks a question (`blocked:"question"`: Enter would pick its first choice), checked before typing and again before Enter, unless `--answer` (`--answer --enter ""` presses Enter alone); refused while the agent is working unless `--queue` (raw keys such as Esc still go); refused for a text over 250 KiB. A paste into an agent waits up to 15 s for it to enable bracketed paste, so a program still starting does not take each newline as Enter. Prints `{"paneId","seq"}`: pass `seq` to `wait --after`, which covers state reports but not `attention` (a notification-only worker that finishes before the wait starts is missed). `--wait` waits right away, with no chance to miss the reply, and says on stderr how to resume if it is killed. `--replace` first clears what the input line holds (Ctrl-E, then Ctrl-U in short writes, enough for 32 lines; checked on Claude Code, Codex and Antigravity CLI) |
| `wait PANE...\|--children [--after SEQ \| --after PANE=SEQ ...] [--for LIST] [--all] [--timeout S] [--tail N] [--stall S] [--settle S]` | Returns when one of the panes (all of them with `--all`) stops: `finished`, `permission`, `input`, `error`, `exit` (printed whatever `--for` lists when a pane is gone or has exited and stays open, since neither will report again; without `--all`, a pane that had already exited when the wait began is skipped while other panes are left), or `attention`. Prints one line per pane that stopped; exits 2 on timeout, printing each pending pane's report count as `baseline`, which a later wait takes as `--after PANE=SEQ` to carry on without missing a report in between. A named pane that is not listed is an error. `--tail N` adds each open pane's last N output lines as `tail`. `--stall S` also ends the wait, with `stalled` and `quietSecs`, for a pane whose screen has not changed for S seconds: a worker that stopped without reporting (Claude Code's working spinner keeps its screen changing; Codex and Antigravity CLI are not checked, so read a stalled pane before treating it as stopped). `--settle S` holds a `finished` until the work on screen has stayed still for S seconds. Lines carry the pane's `label`, the dialog as `prompt` for `permission` and `input`, `blocked`, `result` once the worker wrote its report file, and `agentLeft` when the agent quit to its shell. SIGTERM, SIGINT or SIGHUP prints the timeout line with `interrupted:true` |
| `read PANE [--scrollback] [--max-bytes N]` | Prints the screen, or the retained output tail; a cut says so on stderr, and so does a pane on the alternate screen, which keeps no history |
| `result PANE [--max-bytes N \| --path]` | Prints the worker's report file (`NOA_RESULT`); a cut keeps the start. `--path` prints only the file's path and a newline (exit 0 even if the file is empty or missing, without opening it; it does not go with `--max-bytes`, an argument error), e.g. to `tail -f` a command worker's log |
| `status [--answers]` | One line per worker with its state and what this pane's journal (`<data-dir>/noa/ctl/`) recorded: tier, last task, last outcome, answers, report file, and commits ahead and uncommitted changes for a worktree worker. `--answers` lists every prompt answered from this pane |
| `close PANE...\|--children [--prune]` | Closes workers without a confirmation dialog. A pane that is already gone prints `"gone":true` and is not an error; one failure does not stop the rest. `--prune` then removes the `spawn --worktree` worktree of each closed worker whose cwd is in it, found from that cwd (or from this pane's journal for a worker already gone), after moving its report files to the main worktree's `.noa/`, and deletes its branch (`git worktree remove`, `git branch -d`, never forced); what git refuses stays, named under `worktree.kept`, which also says why when no worktree was found |
| `ls [--children]` / `self` | Lists panels / prints this pane's panel |

Put `--` before a `send` text (or `report` detail) that starts with `--`.

`send` and `close` refuse panes this orchestrator did not spawn unless `--any-pane`
is given. This guards against a wrong id; it is not a security boundary.

`wait` relies on agent-state reports. Claude Code (2.1.295+), Codex (0.163+) and
Antigravity CLI (1.4+) report on their own through OSC 7501: working, blocked on
an approval or a question, done, or failed. No hooks are needed. Claude Code also
reports through the hook adapter above when it is configured. Older versions
surface approval prompts only as `attention` (the agent's own notification);
`attention` means a notification arrived, not that the agent is blocked, so
read the pane to decide. Without any report, a worker can only be waited on for
`exit`.

- Claude Code keeps reporting `working` (with "N still running") after a turn
  while background commands, monitors or MCP tasks it started are still running,
  so `wait` returns `finished` only once they end.
- Codex can report `blocked` up to a second before its approval modal is drawn,
  while it waits for typing to stop.
- agy reports can be turned off with `AGY_CLI_OSC7501=off`.

Exit status: 0 success, 1 error (bad arguments, unknown pane, refused input), 2
`wait` timed out or was interrupted, 3 the server is disabled, unreachable,
lacks a scope, went away mid-command, or has all its 32 connections open (after
3 s of retrying), or Noa did not confirm a request in time, which may still have
taken effect (stderr says which, including when the caller's own shell sandbox
blocks the loopback connection).

### Watching the workers

`Noa +ctl board` turns a pane into a live view of an orchestrator's workers.
The orchestrator's first `spawn` opens one to its right (a new tab when there is
no room). If it was closed, `"$NOA_BIN" +ctl board --open` from the orchestrator's
pane opens another (`--split down` or `--tab` to choose where). Run
in a pane of its own, `+ctl board` follows that pane's orchestrator, or every
worker when it has none; `--of PANE` picks one. It needs a terminal on stdin
and stdout and exits 1 without one: from a script, use `board --open`.

The board has three parts. At the top, a queue of the workers that need you
(an approval prompt, an error or a command that exited with a non-zero code,
then a question or an unread notification, the longest-waiting first within
each), whatever subtree they are in. Below it, the work under way, as a tree.
Last, one line that counts the finished and exited workers (and those closed
since the board opened); `Space` lists them. Each worker keeps its number from
the time it first appears, so `1`–`9` always pick the same one, and a closed
worker's number goes to no one else while the board is open (once 1–20 have all
been given, the one freed longest ago comes back). Each row shows
a mark and a word for the state (`!` approval, `?` a question dialog or a wait
for input, told apart by the word, `×` error, `●` working or a command
running, `✓` done, `·` exited or an agent that never reported), how long the worker has waited, or,
for a working worker, when its screen last moved ("8 秒前に出力"; "出力なし N
分" after 2 minutes), and its last report. The selected worker's screen is
previewed beside the list in a pane 110 columns or wider, under it from 70
columns (and 18 rows), and not at all in a narrower pane, where `Enter` shows it.
The preview's last line and the detail view add the worker's branch and, for a
worker spawned with a command, its model, effort and how long ago it started.
When the orchestrator itself needs you, it heads the queue as an unnumbered row
(the header shows its state in any case); the board never closes it. The board
rings the terminal bell once whenever a worker newly needs you.
The board's labels are in Japanese when the locale is Japanese (`LC_ALL`,
`LC_MESSAGES` or `LANG`, the first one set; with none, the system's preferred
language) and in English otherwise.
Keys: `j`/`k`
select, `n` jumps to the next worker in the queue, `Enter` opens a worker's live screen and the reports seen since the board
opened, `g` opens the worker over the lower part of the board, closing any
other worker it had open (answer prompts there; ⌘⇧I, ctrl+shift+i off macOS,
folds it and comes back to the board), `s` types a
message (a paste can span lines) and sends it with Enter, except to a worker
waiting for an approval or asking a question (Enter would pick the question's
first choice: `g` and answer it there); a message that could not go stays as
that worker's draft for the next `s`. `x` then `y` closes the worker (the prompt
says when its worktree and branch stay), `Esc` goes back,
`q` quits, asking again while workers are open: any other key, ^C included,
cancels, and when quitting is sure to close workers (workers of workers, or more
than the 8 floats the orchestrator's pane holds) it takes `y` instead of a second `q`.
If the board loses its connection it keeps the last list, dims what may be out
of date, says how long it has been and how old the list is, and keeps
retrying until you quit; it does not end on its own. Closing the board does not end its workers: they go back to the
orchestrator's pane as collapsed floats with title strips, and return to the
board when it opens again (except one that has focus). A worker may start workers of its own, one level deep: they are listed under
it on the same board, indented, with its row counting them (one that needs you
is in the queue too, named after its parent); `Space` on a subtree of the work
under way folds it, and anywhere else lists the finished and exited workers or
folds them again. Closing a worker closes the workers it started. The board pane is not a worker: it does not count toward
`--max-children` and `close --children` leaves it alone.

### A skill for the orchestrating agent

`skills/noa-orchestration/` is an agent skill: `SKILL.md` tells an agent when and
how to orchestrate workers, and `reference.md` is the full `+ctl` reference it
reads on demand. To make it available to Claude Code, link it into your skills
directory from a checkout of this repository:

```sh
scripts/install-agent-skill.sh               # ~/.claude/skills/noa-orchestration
scripts/install-agent-skill.sh --dir DIR     # another skills directory, e.g. a project's .claude/skills
scripts/install-agent-skill.sh --uninstall   # remove the link (add --dir DIR for another directory)
```

The skill is a symbolic link to the checkout, so it follows `git pull`. The
script leaves anything else already at the destination alone.
