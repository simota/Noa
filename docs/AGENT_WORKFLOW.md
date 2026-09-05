# Coding agent workflows

## Settings panel

Open **Settings** with **Cmd+,**. Use **Tab** to search for **File Link Editor**,
then **Enter** to select the row. Use **Left/Right** to choose **System Default**,
**VS Code**, **Cursor**, **Zed**, or **Sublime Text**, and **Enter** to save.
The editor changes on save without restarting Noa. **Escape** cancels changes;
**Delete** or **Cmd+Backspace** resets to System Default. **Cmd+Z** on the saved
settings toast restores the previous editor.

The **Agent Workflows** row opens this built-in guide with **Left/Right**.
Opening it keeps unsaved settings intact. Close the guide and Settings before
running the prompt/reader commands below from the command palette.

## Notifications

Unread notifications belong to individual panes. Selecting a pane acknowledges
only that pane. An unselected pane can become unread even while its window is
focused. Desktop notifications retain their existing window-level suppression.

Use **Cmd+Shift+J**, or **Go to Next Unread Notification** in the command palette,
to visit unread panes in the current window group. The configurable action is
`session.next-notification`.

## Writing a prompt

Open **Compose Prompt** (`agent.compose-prompt`) from the command palette.
The modeless native editor supports multiline text, Japanese IME, selection,
undo, and Find. Its title identifies the destination session, branch, and
directory. Selected terminal text becomes a quoted starting point when there
is no draft.

**Paste to Pane** uses the existing bracketed paste/protection path and does
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

Open **Read Output Snapshot** (`terminal.read-output`). It shows the selection,
or the retained output tail when nothing is selected, in a read-only native
view. Snapshots are limited to 1 MiB. Live output cannot move the reader's
selection or scroll position.

The view supports Find and copying, emphasizes Markdown headings, and uses
monospace for fenced code. **Copy Next Code Block** selects and copies each
fenced block in order, excluding fences. This is basic text/Markdown styling,
not a full HTML/Markdown renderer.

New terminal output adds **New output available** to the title. **Return to
Latest** focuses the source pane at the live tail. Reopen the snapshot to read
updated output.

## File navigation and preview

Choose **File Link Editor** in Settings, or configure it directly:

```conf
file-link-editor = code
```

Supported values: `default`, `code`, `cursor`, `zed`, `subl`. The editor CLI
must be in Noa's inherited PATH. Cmd-click passes the absolute filename, line,
and column as one argument; `code`/`cursor` also receive `--goto`. No shell
evaluates the path. Directories and failed launches use the default macOS
handler. `default` preserves ordinary opening without line navigation.

**Cmd+Option+click** previews a detected local path in a monospace panel.
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
Structured status currently feeds local cards; remote client metadata is unchanged.
