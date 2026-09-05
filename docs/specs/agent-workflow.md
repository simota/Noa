# Agent workflow improvements

Status: implemented (2026-09-05).

Scope: pane-scoped unread notifications and next-unread navigation; local
file links with editor line/column navigation; explicit agent status reports;
multiline prompt drafts and a readable output view. Project grouping and a
Git/test results panel remain outside this iteration unless requested.

Acceptance:

1. Selecting one pane acknowledges only that pane. A sibling pane in the
   focused window can still become unread. Next Notification visits unread
   panes in the current window group.
2. A configured editor receives the absolute filename, line, and column as
   arguments without shell evaluation. Missing editors fall back to the
   default file handler; directories use the default handler.
3. Explicit status reports distinguish running, permission/input waiting,
   response end, and error. Generic BEL/OSC notifications remain neutral.
   Acknowledging a notification does not resolve an outstanding agent request.
4. Prompt composition supports native multiline editing/IME and per-pane
   drafts, with an explicit destination and paste action.
5. A read-only output snapshot supports selection, copying, and search while
   the terminal continues receiving output.

Usage and integration setup: [Coding agent workflows](../AGENT_WORKFLOW.md).

Implemented:

- Pane acknowledgement and next-unread navigation (Cmd+Shift+J), including
  revealing the destination when another pane is zoomed.
- Configured editor line/column navigation and asynchronous local UTF-8 previews.
- Explicit agent states and a Claude Code lifecycle hook adapter.
- Native prompt drafts, read-only output snapshots, Find, and fenced code copying.
  Draft paste follows the existing protection path and never sends Enter.
- Settings exposes File Link Editor with save/cancel/reset/Undo support, plus
  an Agent Workflows action that opens the bundled guide without saving drafts.

Verification:

- `cargo test --workspace`: passed with local IPC sockets permitted.
- `cargo test -p noa-app --lib --quiet`: final app changes passed;
  1,182 tests passed, 6 ignored, including Settings search, editor save/reset/Undo,
  and preserving pending settings when opening the guide.
- `cargo build --workspace`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.
- `python3 -B -m unittest discover -s scripts -p test_noa_agent_hook.py`:
  4 tests passed.
- `bash scripts/test-native-text-panels.sh`: passed on macOS. Synthetic panels
  verify Japanese draft preservation, reader/guide modes, native Find/selection routing,
  and clean closure without launching a shell or modifying the clipboard.

Verification limits: interactive Japanese IME candidate selection, installed
editor CLI launches, and an actual Claude Code session were not exercised.
Editor arguments and hook payloads are covered by deterministic tests. Only
the Claude Code adapter is included; remote metadata has no structured status.
