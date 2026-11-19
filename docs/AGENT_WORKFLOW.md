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
pane=$(noa spawn --label tests -- claude | jq -r .paneId)
noa send "$pane" --enter --wait --timeout 540 --tail 40 "Fix the failing test in crates/noa-vt"
noa read "$pane" --scrollback --max-bytes 65536
noa close "$pane"
```

| Command | What it does |
|---|---|
| `spawn [--cwd D] [--env K=V] [--label L] [--input T] [--no-wait-after] [--focus] [--no-board] [--max-children N] [-- CMD...]` | Starts a worker in a floating pane stowed on this pane's board, opening the board first when none is open; the board's row shows the worker's state, and the board's `g` opens it over the lower part of the board. With `--no-board`, or when the board cannot open, the worker floats over this pane instead, collapsed to its title strip. `--focus` opens it and moves focus to it; otherwise focus stays where it was. The command runs without a shell, found on the orchestrator's own `PATH`, and the worker gets the orchestrator's locale (`LANG`, `LC_*`), which a Noa started from the Dock lacks; the cwd defaults to the orchestrator's (a relative `--cwd` is taken from it). A `--cwd` that is not a directory, or a command not found on the worker's `PATH`, is refused before anything starts. Finished workers stay open (with their exit code in `Panel.exit`) until closed, unless `--no-wait-after`. A board holds at most 20 stowed workers (its list scrolls), a pane at most 8 other floats, and one orchestrator 20 workers unless `--max-children` sets fewer; exited workers count until closed; the count is checked before each spawn, so spawns run in parallel can exceed it. `--split` and `--tab` are refused |
| `send PANE [--enter] [--raw] [--wait ...] [--answer] TEXT\|-` | Pastes `TEXT` (or stdin) and, with `--enter`, submits it. Refused while the pane's agent reports `permission`, checked before typing and again before Enter, unless `--answer` (`--answer --enter ""` presses Enter alone). Prints `{"paneId","seq"}`: pass `seq` to `wait --after`, which covers state reports but not `attention` (a notification-only worker that finishes before the wait starts is missed). `--wait` waits right away, with no chance to miss the reply |
| `wait PANE...\|--children [--after SEQ \| --after PANE=SEQ ...] [--for LIST] [--all] [--timeout S] [--tail N]` | Returns when one of the panes (all of them with `--all`) stops: `finished`, `permission`, `input`, `error`, `exit` (printed whatever `--for` lists when a pane is gone or has exited and stays open, since neither will report again; without `--all`, a pane that had already exited when the wait began is skipped while other panes are left), or `attention`. Prints one line per pane that stopped; exits 2 on timeout, printing each pending pane's report count as `baseline`, which a later wait takes as `--after PANE=SEQ` to carry on without missing a report in between. A named pane that is not listed is an error. `--tail N` adds each open pane's last N output lines as `tail` |
| `read PANE [--scrollback] [--max-bytes N]` | Prints the screen, or the retained output tail; a cut says so on stderr |
| `close PANE...\|--children` | Closes workers without a confirmation dialog. A pane that is already gone prints `"gone":true` and is not an error; one failure does not stop the rest |
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

Exit status: 0 success, 1 error (bad arguments, unknown pane), 2 `wait` timed out,
3 the server is disabled, unreachable, lacks a scope, or went away mid-command
(stderr says which, including when the caller's own shell sandbox blocks the
loopback connection).

### Watching the workers

`Noa +ctl board` turns a pane into a live view of an orchestrator's workers.
The orchestrator's first `spawn` opens one to its right (a new tab when there is
no room). If it was closed, `"$NOA_BIN" +ctl board --open` from the orchestrator's
pane opens another (`--split down` or `--tab` to choose where). Run
in a pane of its own, `+ctl board` follows that pane's orchestrator, or every
worker when it has none; `--of PANE` picks one.

The list puts workers that need you first (an approval prompt, an error or a
command that exited with a non-zero code, then a question or an unread
notification, the longest-waiting first within each), then those working, done, and
exited. Each worker keeps its number from the time it first appears, so `1`–`9`
always pick the same one. A working worker whose screen has not changed for 2
minutes reads "出力なし N 分". Each card
shows the worker's last report (or the last detail it gave), the last lines of
its work (above an agent's input box and status line), its report count, and
how long it has been in its state. Keys: `j`/`k`
select, `Enter` opens a worker's live screen and the reports seen since the board
opened, `g` opens the worker over the lower part of the board, closing any
other worker it had open (answer prompts there; ⌘⇧I, ctrl+shift+i off macOS,
comes back to the board), `s` types a
message (a paste can span lines) and sends it with Enter, except to a worker
waiting for an approval, `x` then `y` closes the worker, `Esc` goes back,
`q` quits (asking again while workers are open). If the board loses its
connection it keeps the last list and retries for 30 seconds. Closing the board does not end its workers: they go back to the
orchestrator's pane as collapsed floats with title strips. A worker may start workers of its own, one level deep: they are listed under
it on the same board, indented, with its row counting them; `Space` folds a
worker's subtree. Closing a worker closes the workers it started. The board pane is not a worker: it does not count toward
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
