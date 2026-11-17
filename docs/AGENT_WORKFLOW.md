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

The included `scripts/noa-agent-hook.py` writes lifecycle reports to the hook's
controlling terminal, leaving hook stdout untouched. It reports only event
kinds and tool names, and reads no transcripts. Agent settings are not changed
automatically.

Merge the following with existing Claude Code hooks, replacing the example
script path with an absolute path to this checkout. Quote the script path
inside the command if it contains spaces. A detached hook without a controlling
terminal reports nothing.

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
        "matcher": "permission_prompt|idle_prompt|elicitation_dialog",
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

Any hook can also report with `"$NOA_BIN" +ctl report <state> [detail]`, which writes
the same sequence to the hook's controlling terminal. `NOA_BIN` is the absolute path
of the running Noa, set in every pane.

## Orchestrating worker panes

An orchestrator (an agent or a script in one pane) can start worker panes, type
into them, wait for their agents, read their output, and close them with
`Noa +ctl`. Each command prints one JSON object per line. Workers are floating
panes over the orchestrator, collapsed to their title strips, and the first spawn
opens a board next to the orchestrator that shows them all.

Enable the control server with the scopes the commands need:

```conf
server-enable = true
server-scopes = read,control,input
```

A typical round, from inside the orchestrator's pane:

```sh
noa() { "$NOA_BIN" +ctl "$@"; }
pane=$(noa spawn --label tests -- claude | jq -r .paneId)
noa send "$pane" --enter --wait --timeout 1800 "Fix the failing test in crates/noa-vt"
noa read "$pane" --scrollback --max-bytes 65536
noa close "$pane"
```

| Command | What it does |
|---|---|
| `spawn [--cwd D] [--env K=V] [--label L] [--input T] [--no-wait-after] [--focus] [--no-board] [--max-children N] [-- CMD...]` | Starts a worker in a floating pane over this pane, collapsed to its title strip (which shows the agent's state); open it from the strip or with the board's `g`. `--focus` opens it and moves focus to it; otherwise focus stays where it was. The first spawn also opens this pane's board unless `--no-board`. The command runs without a shell, found on the orchestrator's own `PATH`, and the worker gets the orchestrator's locale (`LANG`, `LC_*`), which a Noa started from the Dock lacks; the cwd defaults to the orchestrator's (a relative `--cwd` is taken from it). Finished workers stay open (with their exit code in `Panel.exit`) until closed, unless `--no-wait-after`. One pane holds at most 8 floats, and one orchestrator 8 workers unless `--max-children` says otherwise; the count is checked before each spawn, so spawns run in parallel can exceed it. `--split` and `--tab` are refused |
| `send PANE [--enter] [--raw] [--wait ...] [--answer] TEXT\|-` | Pastes `TEXT` (or stdin) and, with `--enter`, submits it. Refused while the pane's agent reports `permission`, checked before typing and again before Enter, unless `--answer`. Prints `{"paneId","seq"}`: pass `seq` to `wait --after`, which covers state reports but not `attention` (a notification-only worker that finishes before the wait starts is missed). `--wait` waits right away, with no chance to miss the reply |
| `wait PANE...\|--children [--after SEQ \| --after PANE=SEQ ...] [--for LIST] [--all] [--timeout S]` | Returns when one of the panes (all of them with `--all`) stops: `finished`, `permission`, `input`, `error`, `exit` (printed whatever `--for` lists when a pane is gone or has exited and stays open, since neither will report again; without `--all`, a pane that had already exited when the wait began is skipped while other panes are left), or `attention`. Prints one line per pane that stopped; exits 2 on timeout, printing each pending pane's report count as `baseline`, which a later wait takes as `--after PANE=SEQ` to carry on without missing a report in between |
| `read PANE [--scrollback] [--max-bytes N]` | Prints the screen, or the retained output tail |
| `close PANE...\|--children` | Closes workers without a confirmation dialog. A pane that is already gone prints `"gone":true` and is not an error; one failure does not stop the rest |
| `ls [--children]` / `self` | Lists panels / prints this pane's panel |

Put `--` before a `send` text (or `report` detail) that starts with `--`.

`send` and `close` refuse panes this orchestrator did not spawn unless `--any-pane`
is given. This guards against a wrong id; it is not a security boundary.

`wait` relies on agent-state reports. Claude Code reports on its own through
OSC 7501 (working, blocked on a prompt, done or failed), and also through the hook
adapter above when it is configured. For Codex and Antigravity CLI,
configure their hooks to call `+ctl report`; their documented events (as of this
writing; check your installed version) cover the start and end of a turn but not
approval prompts, which therefore surface as `attention` (the agent's own
notification). `attention` means a notification arrived, not that the agent is
blocked; read the pane to decide. Without any report, a worker can only be waited
on for `exit`.

| Agent | Run `"$NOA_BIN" +ctl report running` on | Run `"$NOA_BIN" +ctl report finished` on |
|---|---|---|
| Codex | `UserPromptSubmit` | `Stop` |
| Antigravity CLI | `PreInvocation` | `Stop` |

The event names come from third-party and vendor documentation that Noa does not
test against, and both tools change where and how hooks are configured between
versions. Check your installed version's hook documentation (`/hooks` lists
loaded hooks in agy) before relying on them.

Exit status: 0 success, 1 error (bad arguments, unknown pane), 2 `wait` timed out,
3 the server is disabled, unreachable, lacks a scope, or went away mid-command.

### Watching the workers

`Noa +ctl board` turns a pane into a live view of an orchestrator's workers.
The orchestrator's first `spawn` opens one to its right (a new tab when there is
no room). If it was closed, `"$NOA_BIN" +ctl board --open` from the orchestrator's
pane opens another (`--split down` or `--tab` to choose where). Run
in a pane of its own, `+ctl board` follows that pane's orchestrator, or every
worker when it has none; `--of PANE` picks one.

The list puts workers that need you first (an approval or question prompt, an
error, an unread notification), then those working, done, and exited. Each card
shows the worker's last report (or the last detail it gave), the last lines of
its work (above an agent's input box and status line), its report count, and
how long it has been in its state. Keys: `j`/`k`
select, `Enter` opens a worker's live screen and the reports seen since the board
opened, `g` moves to the worker's pane (answer prompts there), `s` types a
message (a paste can span lines) and sends it with Enter, except to a worker
waiting for an approval, `x` then `y` closes the worker, `Esc` goes back,
`q` quits. The board pane is not a worker: it does not count toward
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
