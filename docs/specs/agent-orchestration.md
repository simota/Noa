# Agent orchestration

| | |
|---|---|
| slug | agent-orchestration |
| status | implemented (API/CLI phase, 2026-11-16; D7–D17 add the UI and improvements). No owner sign-off. Not accepted on a real machine (A1, A2, A6) |
| owner | simota |
| as-of | 2026-11-21 (HEAD 39d8d89, D17) |
| scope decision | Both an "agent-driven API" and a "built-in UI for humans". The API is fixed first; the UI is a later phase (elicited 2026-11-16) |

## L0: Vision

- Problem: a parent agent running in one pane has no way to coordinate agents running in other panes. Almost all the parts exist, but they are not connected.
  - A worker's state (`running`/`permission`/`input`/`finished`/`error`) is in `SessionCard.agent_status` (`crates/noa-app/src/session_store.rs:140`), but `card_to_panel` drops it, so it is not visible from outside (`crates/noa-app/src/ipc_bridge.rs:604-627`; the end of `docs/AGENT_WORKFLOW.md`, "remote client metadata is unchanged").
  - `noa.newTab`/`noa.split` accept no command, cwd or env (`crates/noa-ipc/src/backend.rs:74-76`). The AppleScript side has `SurfaceConfig` (`crates/noa-app/src/macos_applescript.rs:297`).
  - A process inside a pane has no way to learn its own paneId. `NOA_SURFACE_ID` only goes into the env (`crates/noa-app/src/app.rs:762-777`) and appears nowhere in the API.
  - There is no client that can be called from a shell. `noa_ipc::Client` has neither `send_text` nor `subscribe` (`crates/noa-ipc/src/client.rs:120-221`).
- Job to be done: an orchestrator adds, drives and removes several panes with shell commands alone, and gets the work done by choosing claude / codex / agy according to the task (elicited 2026-11-16). For one worker it runs the cycle "start → send an instruction → wait for the end of the turn (or for an approval prompt) → read the output → close". The orchestrator decides which agent to use for which task; Noa does not. A human can see the whole of it inside Noa and step in on a worker that has stopped.
- Success definition: `Noa +ctl spawn` → `send` → `wait` → `read` → `close` runs to completion as one shell script (`scripts/agent-orchestration-smoke.sh`).

## Glossary

| Term | Definition | Code identifier |
|---|---|---|
| orchestrator | The pane that the process running `Noa +ctl` belongs to | What `parentPaneId` points to |
| worker | A pane that has a `parentPaneId` | `Surface.orchestration.parent` |
| turn | The unit between agent state reports. `seq` advances by 1 per report | `SessionCard.agent_seq` |
| terminal state | A state that ends a wait: `finished`, `permission`, `input`, `error`, process exit | — |

## Decisions

### D1: Add 5 fields to Panel (additive only; `PROTOCOL_VERSION` stays 3)

```jsonc
{
  "surfaceId": "0x00c0ffee00c0ffee",          // same value as NOA_SURFACE_ID
  "agent": { "kind": "claude", "state": "finished", "detail": "", "seq": 7 },  // kind is claude|codex|agy. The whole key is omitted when the process is no known agent and nothing was reported. state/detail only when there is a report
  "exit": { "code": 0 },                       // only when the child exited and the pane remains
  "parentPaneId": "3", "label": "fix-tests"    // only when given at spawn
}
```

- `kind` comes from the foreground process name (`classify_agent`, `crates/noa-app/src/sidebar.rs:740`), so it appears even for an agent with no hooks configured.
- `seq` is a per-pane monotonic counter that advances by 1 on every OSC 777 report (including `clear`). Even if the previous turn's `finished` is still there, waiting for "a terminal state with `seq` greater than N" does not confuse the two. `agent_status_at` is a time and does not guarantee order, so it is not used.
- Why the version is not raised: hello rejects anything but an exact match (`crates/noa-ipc/src/server.rs:1478`), so raising it would break every existing v3 client. `Panel` does not reject unknown fields.
- `noa.stateChanged` is delivered on a `PartialEq` diff of `Panel` (`crates/noa-ipc/src/protocol.rs:163-165`), so once the fields are added, state changes are pushed as they are.

### D2: Add `config` to `noa.newTab`/`noa.split` (no new method)

```jsonc
{"paneId":"1","direction":"horizontal",
 "config":{"command":"claude","cwd":"/repo","env":{"K":"V"},"initialInput":"…","waitAfterCommand":true},
 "label":"fix-tests","parentPaneId":"1","focus":false}
```

- It uses the same spawn path (`ScriptSpawn`) as AppleScript v2's `with configuration`. `macos_applescript` compiles as a no-op shim outside macOS, so the types did not need to move.
- `config.command` is an argv, not a string. It starts without a shell, and `argv[0]` is looked up in `config.env.PATH` (portable-pty uses the `PATH` in the child's env). `+ctl spawn` passes the orchestrator's own `PATH`, so a command is never missing because of the GUI app's minimal `PATH`.
- If any of `config.command`/`initialInput`/`env` is present, the `input` scope is needed in addition to `control`, because running a command carries the same authority as input.
- Automatic placement is `direction:"auto"` on `noa.split` rather than a new parameter (it is exclusive with the existing `direction`). The first worker whose parent is `paneId` splits to the right of the parent; each later one splits the largest pane among the workers with the same parent along its long side. If none has room, it opens in a new tab of the same window. `Panel` has no dimensions, so the client cannot decide; the app chooses.
- A request with no extensions goes to the existing `IpcBackend::new_tab`/`split`; only one with extensions goes to `IpcBackend::spawn`, which has a default implementation. Existing backend implementations stay source compatible.
- With `focus:false`, focus returns to where it was after creation. The current IPC split moves focus to the target pane before splitting (`crates/noa-app/src/app/ipc.rs:349-356`), so doing nothing would take focus away from the pane a human is typing in.

### D3: No wait on the server; the client builds it with subscribe

The procedure is "`subscribe(state_changed, paneIds)` → `listPanels` → check the condition → recheck on each notification". On a dropped marker, check the state in that notification, then redo `listPanels` (dropped marks that earlier notifications were lost; the notification itself is valid). `send_text`, `subscribe` and notification receipt were added to `noa_ipc::Client`. Notifications that arrive while a response is awaited are buffered, not dropped (limit 1024; on overflow, dropped is set on the next notification).

- `stateChanged` has no removal event, so a closed pane is not seen through notifications. For that reason, if no notification arrives for 1 second during a wait, the client rereads with `listPanels`. Only on this point does it depart from the original success condition of no polling at all.
- `attention` ends a wait only when it becomes `true` after `false` was observed once during the wait. A notification left over from the previous turn does not end it. `send --wait` takes the panel captured before sending as its baseline, so it does not miss an `attention` raised right after sending. IPC `sendText` marks that pane's notification read only when the input was accepted (the same as focus; if input is refused, as for an exited pane, it stays unread). Even for an agent with no hooks, `attention` drops once on every send, so `send --wait` from the second turn on also ends on `attention`.
- The list is reread once 1 second has passed since the last read, even if notifications are arriving (so that a vanished pane is not missed while another pane's notifications keep coming).
- `Panel` is built from the session card (created on first output), so on its own a worker that has not printed yet is not listed, and `wait` wrongly judges it closed. It is listed without a card: workers with a `parentPaneId` or `label`, and also normal panes in windows that can hold cards, so that an orchestrator itself, before any output, can find itself through `NOA_SURFACE_ID` (quick terminal, scratch popups and floats excluded). Spawn rebuilds the list before it responds.
- `seq` counts reports, not output batches (two reports in the same batch advance it by 2).
- A pane that exited and was kept (`Panel.exit` present) never reports again, like a vanished pane, so it ends the wait as `exit` regardless of `--for`. However, a pane that had already exited when the wait began does not end a wait without `--all` while other panes remain to wait for (otherwise one kept pane would end every later `wait --children` at once).
- The same pane given more than once is waited for as one. A huge `--timeout` is treated as an argument error or as "no deadline", and does not panic.
- A spawn that failed to create (such as a fixed direction with no room to split) returns focus to where it was, whatever `focus` says, because the split moves focus to the target before it checks for room.
- Every tab in which the spawn moved the focused pane (the spawn's target, and `auto` candidates that had no room, including the tab with OS focus) is returned to its original pane, except the tab that hands focus to the new pane. When focus goes to the new pane and that pane was created in a background tab, that tab is brought to the front. On macOS, when a new tab is created in the group of another OS window, the tab selected in that group is not restored (not supported).
- `auto` candidates are chosen from all the workers the parent started (including those that overflowed into a new tab), not only those in the parent's tab.
- A relative `--cwd` is made absolute against the orchestrator's cwd on the CLI side before it is sent (Noa would resolve it against its own process's cwd).
- During a wait, a state received in a notification is checked before the list is reread (so that the reread does not overwrite a transition).
- Not supported: the worker limit is only counted by the CLI in a listing before the spawn, so parallel spawns from the same parent can exceed it. Serializing per parent on the server would prevent it, but it cannot be reproduced without a GUI and is not in yet. (Resolved in D17: spawn sends `maxChildren`, and Noa counts per parent on the UI thread and refuses with `-32011`. A Noa without support behaves as before.)
- Even when the spawn's placement moves focus, unread notifications (`attention` and bell) are not marked read. Moving focus to a split candidate makes `focus_pane` clear notifications, so the unread flags from before the spawn are restored after it.
- Bringing a tab to the front while restoring focus does not activate the app (like ASSUME-6 on the AppleScript path, it does not take focus from a user working in another app).

### D4: The CLI is `Noa +ctl` (on the existing `+action` mechanism)

It was first written as `noa ctl`, but the binary already has Ghostty's compatible `Noa +<action>` mechanism (`crates/noa-app/src/cli.rs`), so it follows that.

| Command | Behavior |
|---|---|
| `self` | Returns the pane whose `surfaceId` matches `NOA_SURFACE_ID` |
| `ls [--children]` | Panel list (`--children`: only those whose parent is the caller) |
| `spawn [--cwd D \| --worktree] [--tier T] [--env K=V] [--label L] [--input T] [--no-wait-after] [--focus] [--no-board] [--max-children N] [--ready S] [--task T \| --task-file F [wait options]] [-- CMD…]` | Calls D2's `direction:"float"` (D8, D9: a worker is always a float). `parentPaneId` is the caller, `focus` is false, cwd is the caller's cwd. An exited worker is kept by default (so its exit code can be read; `--no-wait-after` disables this). Refuses if the caller has 8 or more workers (`--max-children N` changes this). If there is no board, opens one first and stows the worker on it (D10; `--no-board` does not open one and places the worker on the caller). `--split`/`--tab` are refused. (Revised in D12, D14, D17: the limit is 20 both by default and at most, and Noa also counts with `maxChildren`. `--worktree`/`--tier` are D14. `--ready` waits until the worker is ready and prints `ready` and `screen`; `--task` then sends the work and waits. `NOA_RESULT` is put in the worker's env. A `--label` starting with `board:` and `--input` to an agent are refused) |
| `send PANE [--enter] [--raw] [--replace] [--wait …] [--answer] [--queue] [--any-pane] TEXT\|-` | `noa.sendText`. Does not send to a pane awaiting approval (`permission`). Checks right before sending and right before Enter (for the same reason as the board, D7). `--answer` sends anyway. `--enter` sends `\r` with `paste:false` 150 ms after the paste. Prints the `seq` from before the send. `--wait` subscribes, then sends, then waits (the recommended path that misses nothing). (Revised in D17: a question dialog (`agent.blocked:"question"`) is also refused without `--answer`, and a paste or an Enter-only send to an agent at work (`running`) is refused without `--queue`. A paste to an agent waits until bracketed paste is enabled, and text over 250 KiB is refused. `--wait` prints a resume line on stderr) |
| `wait PANE…\|--children [--for finished,permission,input,error,exit,attention] [--after SEQ \| --after PANE=SEQ …] [--all] [--timeout S] [--tail N] [--stall S] [--settle S] [--any-pane]` | D3. Without `--after`, the baseline is the `seq` when the wait starts (the value right before another process's `send` cannot be known; to miss nothing, use `send --wait` or `--after`). `--after SEQ` is for a single pane only; for several panes, repeat `--after PANE=SEQ`. The timeout line prints each waited pane's baseline in `baseline`, so passing that to `--after` waits again without missing reports that arrived in between. A vanished pane is always returned as `exit` (`closed:true`). (Added in D11, D14, D17: `--tail`, `--stall`, `--settle`. A `wait` that names panes refuses a pane that is not the caller's worker; `--any-pane` lifts this. Ending on a signal also prints a timeout line with `interrupted:true`) |
| `read PANE [--scrollback] [--max-bytes N]` | `noa.getText` |
| `close PANE…\|--children [--any-pane] [--prune]` | `noa.closePane`. A pane not in the list succeeds with `gone:true`, and one failure does not stop the rest from closing (exit 1 if any failed). A live pane that is not the caller's worker is refused before anything is closed. (`--prune` added in D14. Revised in D17 to look up the worktree from the closed pane's cwd) |
| `result PANE [--max-bytes N]` | (Added in D17) Prints the worker's result file (`NOA_RESULT`). `--max-bytes` keeps the beginning |
| `status [--answers]` | (Added in D17) One line per worker of the caller, combining the listed state and the journal (records of `spawn`, `send`, `outcome`, `close`). `--answers` prints the prompts answered, oldest first |
| `report STATE [DETAIL]` | Writes `OSC 777;noa-agent` to the controlling terminal. Called from each agent's hooks (D6) |

- Output is one JSON object per line (only `read` prints text). Exit codes are 0 (success), 1 (arguments, unknown pane, and so on), 2 (timeout), 3 (server disabled, cannot connect, missing scope; a config line that fixes it is printed on stderr). (Revised in D17: a request with an unknown outcome (`-32009`) and a server full at its connection limit are also 3. On `-32009` it says the request "may have been carried out")
- The connection target comes from `noa-config`'s `server-bind`/`server-port` (an unspecified address is read as the loopback of the same address family: `0.0.0.0` becomes `127.0.0.1`, `::` becomes `::1`, because a v6only `::` does not accept IPv4) and from `server-token` or the token file (`noa_config::server_token_path()`). The token file is only read, never created.
- Commands that need the caller's own pane (`self`, `spawn`, `--children`, the checks in `send`/`close`) wait for it if it is not yet in the list, rereading for up to 3 seconds (30 times at 100 ms intervals), because a script started with `Noa -e` can run before its pane is first listed. `ls`/`read` from a pane that is never listed (such as a float) do not wait.
- `send`/`close` refuse by default a pane whose `parentPaneId` is not the caller (`--any-pane` lifts this; D17 made a `wait` that names panes do the same). This prevents mistakes; it is not a security boundary (see "Considered but rejected" below).
- No code that puts `noa` on the child's PATH has been found (0 hits from grepping `crates/noa-pty/src`, `crates/noa-app/src/app.rs`, `shell-integration/`, `scripts/bundle-macos.sh`; not checked on a real machine, A4). So `NOA_BIN` (the absolute path of the executable) is passed to the child in the same place as `NOA_SURFACE_ID`.

### D5: The seam with the UI phase is `Surface` and `SessionCard`

The plan was first to put all three on `SessionCard`, but `SessionCard` is updated only by io-thread deltas. `parent`/`label` (and the surface id and exit information), which are decided on the main thread at spawn, live in `Surface.orchestration` and similar places, and only `agent_seq`, which grows with each report, lives on `SessionCard`. `Panel` only copies both, and the UI phase reads the same two places. The UI itself is decided in a separate spec (`docs/specs/orchestration-board.md`), but it assumes the following.

- ~~Nest worker cards under the orchestrator card in the sidebar, and show a tally on the parent (running / needs you / done / error). No new window or board.~~ Changed in D7 to a dedicated pane (2026-11-17).
- Add "all workers of this orchestrator" as a destination of Compose Prompt. The send path is the same as `+ctl send`.
- A worker whose parent closed returns to the top level. ipc ids are not reused (`crates/noa-app/src/ipc_bridge.rs:51-60`), so a stale `parentPaneId` never points at a different pane.

### D7: The UI is a TUI in a dedicated pane, `+ctl board` (2026-11-17)

The UI for humans is a dedicated pane placed beside the orchestrator, not nesting in the sidebar (the original plan in D5). The owner decided after seeing screen mockups.

- Every Noa pane is a `Surface` with a `Terminal`; there are no non-terminal panes. A new kind of pane would touch layout, input, drawing and session saving. Following the float title band, which already draws UI in cells, a TUI called `Noa +ctl board` runs in an ordinary pane (the owner chose the TUI option).
- Data is read from the same server as `+ctl`: `listPanels` every 0.5 seconds, and `getText` for the shown workers every 1 second. It does not subscribe (unread notifications would pile up while it draws).
- The order is needs you (permission / input / error / unread notification) → working (running and no report) → done → exited. finished often comes with a notification, so finished takes priority over an unread notification (on a real machine, a finished worker appeared under needs you).
- Actions are go to (`noa.focusPane`), send (the same paste + `\r` as `+ctl send --enter`) and close (with confirmation). The board does not approve; you move to the pane and a person answers. The send line does not open for a worker awaiting approval (permission), and if it starts awaiting approval while you type, the text is not sent (revised in D17: the same for a question dialog). Right before sending, and right before the Enter 150 ms after the paste, the list is taken again and checked (some prompts approve their default choice on Enter). Pastes are received as bracketed paste and go into the send line with their newlines (reading a newline as Enter would run the remaining characters as action keys).
- A board opened with `--open` has no `parentPaneId`. It is not counted as a worker and `close --children` does not close it.
- A card's preview uses the lines above the group of full-width rule lines at the bottom of the screen (the agent's input box), because the input box and status line are not the work itself. With no rule lines, the whole screen. A report with no detail shows the previous detail, or the state and elapsed time. `seq` counts reports, not turns, so it is shown as 「報告 N」 (`report N`). The time a state changed is remembered and shown as 「作業中 2 分」 (`working 2m`) or 「完了 3 分前」 (`done 3m ago`) (a state already present when the board opened is 「開始時から」 ("since the start"; the code prints 「開始時」, `at start`)).
- The report history holds only reports seen since the board opened, and exists only inside the board. A state already present when it opened is shown as 「開始時」 (`at start`), because its time is unknown.
- Not supported: detecting a half-written draft in a worker's input box (no method of inferring it from the screen has been checked), starting from a GUI keybinding or menu, persisting the history.
- UI improvements (2026-11-17, requested by the owner with the scope narrowed to the board): framed cards (5 lines each; 8 workers did not fit on one screen) became one line per worker with the tail of its work below it (the tail is normally 1 line, 3 lines when selected; for done and exited only when selected). The name, kind and state columns are aligned across workers, and the kind column appears only when different programs are mixed. `pane N` and 「報告 N」 (`report N`) appear only in the detail view. The header shows the orchestrator by pane name. Keys `1`–`9` (select by number), `n` (next needing you) and `?` (help) were added, and the footer shows as many keys as fit plus `? ヘルプ` (`? help`). Workers hidden by scrolling (counting only those whose first line is not visible) are shown as 「↑/↓ あと N 件 · うち要対応 M」 (`↑/↓ N more · M need you`), and a hidden closed section as 「閉じた K 件」 (`K closed`). A part with 0 is not shown. Selecting the last worker shows the closed section too; if it does not fit, pressing `j` again on the last worker scrolls past the closed section (`k` first scrolls back; the selection stays on the last worker). When there are 0 workers, `j`/`k` scroll the closed section. Workers that closed after the board opened keep their last state and detail, up to 5, in the 「閉じた」 (`Closed`) section. The send line accepts ^U (clear all) and ^W (delete one word), and shows the destination and action one line above. The confirmation for closing a worker at work says so. The pane title (OSC 2) becomes 「ボード · 要対応 N」 (`Board · N need you`) and is restored with `CSI 22/23 t` on exit.

### D8: Workers can be placed in floats (2026-11-17)

At the owner's request to hide the worker panes while watching the board, `spawn --float` (`direction:"float"` on `noa.split`) was added.

- (Revised in D10: the anchor is normally the board) A worker opens as a float anchored to the orchestrator's pane. With `focus:false` (the `+ctl spawn` default) it starts folded to its title band; with `focus:true` (`--focus`) it starts open with focus. `direction:"float"` requires `parentPaneId` (invalid params without it). Only worker floats are listed, so a float without a parent cannot be created over IPC. The title band shows the agent's state and the needs-you color, so a stopped worker is visible even while folded. It opens from the title band or from the board's `g` (`focus_pane` opens a folded float).
- floating-panes REQ-023 is revised: IPC lists only floats that have a `parentPaneId` (workers). AppleScript, AX and Overview still show no floats at all. A float that is not a worker is not visible over IPC either.
- Float workers are removed from the `auto` split candidates (they are not in the split layout).
- A pane can hold 8 floats (`MAX_PER_ANCHOR`); beyond that, spawn fails (from D10 on, this applies to the board's pane). This is the same number as the `--max-children` default.
- Session restore does not restore orchestration information, so a restored float is no longer a worker and is not visible over IPC. (Revised in D17: floats of workers started over IPC are not saved in the session)

### D9: Workers are always floats; the first spawn opens the board (2026-11-17)

On the owner's instruction, `+ctl spawn` always starts the worker as a float and refuses `--split` and `--tab` (`--float` is accepted for compatibility and does nothing). Once orchestration starts, the board opens too.

- This is enforced in the `+ctl` layer. The server's `noa.split` (`horizontal`/`vertical`/`auto`) and the `noa.newTab` extensions remain for other API clients.
- (Revised in D10: the board opens before the spawn) After the spawn, if that orchestrator has no board, one opens with the same placement as `board --open` (split to the right, or a new tab if there is no room). The board is identified by the label `board:<orchestrator's pane id>`, and is not opened if it already exists. If it cannot open, a warning is printed and the spawn still succeeds. `--no-board` does not open it.
- The board is not a worker (no `parentPaneId`), so `close --children` does not close it. A person closes it. When it closes, the workers stowed on it return to the orchestrator (D10).
- Workers no longer split, so un-zooming caused by a split (a known limitation, not reproduced) does not happen in orchestration.
- The `--max-children` default of 8 is the same as the number of floats a pane can hold (8). (Revised to 20 in D12)

### D10: Workers are stowed on the board (2026-11-18)

The bands of folded workers (80 columns wide) stacked like stairs from the bottom right to the top left of the orchestrator's pane and hid its output. State was also split between two places, the bands and the board. The owner chose "merge into the board".

- `+ctl spawn` first finds or opens the orchestrator's board and passes that pane as `noa.split`'s `paneId` (the float's anchor). `parentPaneId` is the orchestrator as before. IPC is unchanged (the server already took the anchor from `paneId` and ownership from `parentPaneId`). With `--no-board`, the worker goes on the orchestrator as before. It does the same, after a warning, when the board cannot be checked or cannot be opened.
- A worker whose anchor pane is labelled `board:<parentPaneId>` is "stowed" (`Float.stowed`). A stowed worker shows no band when folded, and appears in no drawing, hit testing or cycling. Its state appears only in the board's row.
- An open stowed worker is placed across the full width of the board with the top 35% left free. It is not offset or stacked; it opens below the top of the board's list, which stays visible. It opens with the board's `g` (`focus_pane` → `reveal_float`), which folds any other stowed worker open on the same board (one open at a time per board). It returns with `float.toggle-focus` (⌘⇧I), the fold button, or a double click. Focus returns to the return target (the board). (Revised in D17: ⌘⇧I folds the open stowed worker before returning. Before, the worker stayed open)
- When the board's pane or tab closes (including `q` and ⌘W), stowed workers are reattached to the parent if the parent pane remains and has room (as ordinary folded floats, with bands). This is an exception to floating-panes REQ-013. Workers that are reattached are not counted in the close confirmation's count of running programs. If the parent closes with it in the same tab, the parent is a float, or the parent is full, they close as before.
- Stowed workers do not outlive the orchestrator. When the orchestrator's split pane or its tab closes, they close with it wherever the board is, and are counted in the close confirmation's count of running programs (the same as when, before D10, they closed under REQ-013 as floats on the orchestrator). When the orchestrator is a float, they do not close (as before D8, the workers belong to the split pane below).
- When focus moves to a stowed worker by any path (`g`, `spawn --focus`), its return target is that board.
- When a whole window closes, workers that return to an orchestrator in another window are not counted in the confirmation's count of running programs.
- A worker taken off the board by hand through anchor cycling (REQ-006) becomes an ordinary float.
- Known limitation: reopening the board with `board --open` does not move workers that already returned to the parent onto the board (spawns from then on go on the board) (resolved in D14). Session restore does not restore orchestration, so a restored float is not stowed (as in D8; in D17 worker floats are no longer saved).

### D11: Fix the friction found in real use (2026-11-20)

At the owner's request ("smarter, easier to use, easier to read"), an orchestrator actually ran 3 workers (board, CLI and skill) to find friction, and items with evidence (repro steps or `file:line`) were fixed.

- CLI:
  - A connection error states its cause (EPERM: the calling shell's sandbox; ECONNREFUSED: the server is not listening).
  - `spawn` refuses, before the RPC, a `--cwd` that is not a directory and a command that is not on the worker's `PATH`. The former used to start silently in a different directory. The reason a float did not open (the pty error) is carried in the RPC error.
  - `Panel.cwd` shows the spawn's cwd until the shell reports one with OSC 7.
  - `wait` fails when it is given an id that is not in the first listing, so that a stale id is not read as "exited".
  - The `--max-children` limit is 8 (the board's float count), and the refusal message gives the number of exited workers.
  - `read` drops blank lines at the bottom of the screen, and says on stderr when it truncated.
  - Argument errors do not print the full usage, and quote the value.
  - The timeout line separates panes that had exited before the wait into `exited`, and adds `state` to `baseline`.
  - `board --open` does not open a board when one already exists, and returns `existing:true`.
  - `--tail N` is added to `wait`/`send --wait`.
  - A send to an exited pane says that its process has exited.
- `send` still refuses only `permission` (revised in D17: it also refuses an `input` whose OSC 7501 `blocked` indicates a question, and an agent at work; an `input` without `blocked` is still not refused). Refusing `input` too was not adopted. `input` is also used for the hook adapter's `idle_prompt` (waiting at the prompt) (`scripts/noa-agent-hook.py`), and refusing it would stop ordinary follow-up sends.
- Board:
  - It does not quit when fetching the list fails. It keeps the last list and retries for up to 30 seconds. (Revised in D17: it does not stop at 30 seconds and keeps retrying until `q` is pressed)
  - Numbers are fixed in order of first appearance.
  - A non-zero exit code is shown under needs you in red as 「失敗」 (`failed`).
  - Within needs you, approvals, errors and failures come first, then the rest by longest wait.
  - The previous state's detail is not reused. Elapsed time with an unknown start is not shown. In narrow widths the state is abbreviated.
  - The notice line is placed at the top so that an open stowed worker does not hide it.
  - `s` on an exited worker is refused. `q` asks for confirmation while workers remain. (Revised in D17: during the confirmation, every key other than the quit key, ^C included, cancels, and when quitting always closes workers it asks for `y`)
  - A worker at work whose screen has not changed for 2 minutes or more shows 「出力なし N 分」 (`quiet Nm`).
  - Screens are fetched only for workers needing you, workers at work, and the selected worker.
- Not supported (awaiting decision):
  - Nested orchestration. When a float worker spawns, its worker goes on the parent's board and shares the 8 slots. The skill says "do not give a worker workers of its own".
  - `send --replace` (clear the input box with Ctrl-U). Whether it works in each agent is not checked. → Implemented in D14.
  - `spawn --worktree` and `--tier`. Whether `+ctl` should know about git and model names is a design decision. → D14 gave it that knowledge.
  - Stowing workers again when the board is reopened (D10's known limitation). → Implemented in D14.
- Not checked visually in the GUI: whether the notice line is hidden by stowed workers, colors, appearance at narrow widths.

### D12: A board holds 20 workers (2026-11-20)

On the owner's instruction, the number of workers one board can stow went from 8 to 20. The board's list already scrolls.

- Stowed workers draw no title band (D10). REQ-019's 8 floats is a limit to keep bands from stacking like stairs, so it does not apply to stowed workers. The limit for adding stowed workers to a board pane is 20 (`MAX_STOWED_PER_BOARD`). Floats that are not stowed are still limited to 8, even on the board.
- `--max-children` is 20 both by default and at most. With `--no-board`, workers go on the orchestrator's pane, so the server's limit of 8 applies first.
- Board numbers become two digits. Typing two digits within 1 second selects 10–20. If no such number exists, the second digit alone selects instead. `0` is used only as the second digit.
- When the board closes, stowed workers return to the parent pane. Only as many as there are free ordinary float slots (at most 8) can return; the rest close (the same as D10's "the parent is full"). The `q` confirmation says so. The close confirmation's count of running programs includes those that close because they cannot return (behavior from before D10).
- Not checked:
  - Load with 20 agents running at once (20 pairs of pty and `Terminal`; the board fetches screens only for workers needing you and workers at work).
  - Appearance in the GUI.

### D13: Nested orchestration goes one level deep, shown as a tree on the root's board (2026-11-20)

The owner chose display option A (show a tree on the root's board). The owner did not answer on limiting nesting to one level, so the proposed default was taken.

Terms:
- Root: an orchestrator in a split pane.
- Child: a worker of the root.
- Grandchild: a worker spawned by a child.

Behavior so far:
- When a child (a stowed float) spawns, `float_host` (`crates/noa-app/src/app/ipc.rs:1040`) replaces the anchor with the root's board. But `stows_on` matches only `board:<parent>`, so the grandchild was not stowed and appeared on the board as a float with a band.
- Without `--no-board`, a board for the child opened in another tab.
- Closing a child did not close its grandchildren. `discard_float` (`crates/noa-app/src/app/floating.rs:895`), which closes a float, does not clean up stowed workers.

Decisions:
- Placement (server): a spawn whose `parentPaneId` points at a child (a float stowed on the root's board) is stowed on that board. `stows_on` treats as stowed not only "the anchor's label is `board:<parent>`" but also "`board:<parent's parent>`". If a grandchild tries to spawn (the parent's parent is a worker), it is refused with invalid params. The message is "nesting goes one level deep".
- `+ctl spawn` (client): if the caller is a worker (has a `parentPaneId`), it neither looks for nor opens its own board. It passes itself as `paneId` and leaves placement to the server. A worker that gives `--no-board` gets an error (a worker's worker always goes on the root's board).
- Slots: the board's 20 slots (D12) are shared by children and grandchildren. `--max-children` counts the direct workers of the one spawning.
- Closing:
  - When a child closes, its grandchildren close too, whatever the path (closed by hand, `close`, closed on process exit).
  - When the root closes, the children close, and their grandchildren too.
  - When the board closes, children return to the root as in D10. Grandchildren close because their parent is a float (D10's "the parent is a float"). The `q` confirmation says so.
- `ls --children` and `close --children` apply only to direct workers. The root's `close --children` closes the children, and through them the grandchildren.
- Board:
  - The `--of R` list shows R's children and grandchildren. A grandchild appears right below its parent's row, indented, with tree lines (`│`, `└`).
  - Order: a child goes into a section by the most urgent member of its subtree (itself and its grandchildren together). Within a section, D11's rules apply. Grandchildren are ordered under their parent by the same rules.
  - The row of a child with grandchildren shows a tally of the grandchildren and of those needing you (「子 3 · 要対応 1」, `3 workers · 1 need you`).
  - `Space` folds and unfolds a subtree. The default is unfolded. A folded row is marked `▸`, an unfolded one `▾`. `←`/`→` are not used, because D7 uses them to move between the list and the detail view.
  - Numbers (1–20), `n`, `g`/`s`/`x` and the header tally include grandchildren. Needs-you items in a folded subtree are unfolded and selected with `n`.
- Not checked: appearance in the GUI.

### D14: Let it run to the end without a person (2026-11-20)

The owner's request, "I want to improve the autonomy of the orchestration feature", was taken as all three axes (elicited 2026-11-20): A, get past approval prompts without a person; B, give `+ctl` the setup steps; C, run long jobs unattended to the end. The three items awaiting decision in D11 were settled by the owner choosing B and C in this request.

- A:
  - `send --replace`: clears the input box before typing. It sends Ctrl-E then Ctrl-U, 64 in all (32 lines' worth), in chunks of at most 16 every 50 ms. In all three, Ctrl-U clears only the current line, and at the start of a line one more joins it to the previous line. Claude Code treats 65 bytes received at once as a paste and keeps the draft (up to 33 bytes cleared), so they are sent in chunks. Versions checked: Claude Code 2.1.329, Codex 0.170.0, agy 1.5.13. Checked with hand-typed input, a multi-line paste, Claude Code's `[Pasted text]` display, and an empty input box. A draft on lines after the cursor can remain. It cannot be combined with `--answer` (a prompt has no input box).
  - Approval without a person is decided in the skill (`skills/noa-orchestration/SKILL.md`, "Running without the human"). Reads, edits, builds, tests and commits to that branch that stay inside the worker's own worktree are approved; push, publishing, writes or deletes outside the worktree, installs, and changes to the person's settings or credentials are refused. Anything that fits neither is left waiting and the person is asked. The orchestrator may answer the trust prompt for a worktree made with `spawn --worktree`. Auto-approve flags and Noa's auto-approve are used, as before, only when the person asks. Noa's own behavior is unchanged.
- B:
  - `spawn --worktree`: requires `--label L` and cannot be combined with `--cwd`. It runs `git worktree add -b orch/L <top>-wt/L HEAD` in the repository of this process's cwd and starts the worker there (revised in D17: the location is `<main>-wt/L` beside the main worktree). `worktree` and `branch` are added to the output. If git fails, nothing starts. Checks such as the limit are done first, and the worktree is created before the board opens (if git refuses, the board does not open either). If the server refuses the start (RPC error), the just-created worktree and branch are removed. If no response arrived (connection failure), the worker may have started, so they are not removed and stderr says so. A warning on stderr says that uncommitted changes do not go into the worktree.
  - `close --prune`: for the label L of a closed worker, if `<top>-wt/L` holds a worktree on `orch/L` and the pane's cwd is inside it, it runs `git worktree remove` and `git branch -d`. Neither is forced. If refused, the work is kept and git's reason appears in the line's `worktree.kept`. The exit status does not change. (Revised in D17: the worktree is looked up from the closed pane's cwd, and if none is found that too is reported in `kept`)
  - `spawn --tier light|standard|deep`: for `claude` / `codex` / `agy` commands, inserts that tier's model and effort flags right after the program name. It refuses if the command already specifies a model or effort (it does not silently override what a person specified). Claude Code's `light` is the code-editing column's `sonnet` / `low`; for read-only work, write `haiku` yourself. Codex and agy model names change between versions, so the `+ctl` table (`crates/noa-app/src/ctl/worktree.rs`) and the skill's reference.md are updated together.
  - The per-agent launch profiles in "Considered but rejected" were partly adopted, in the form of adding flags without creating aliases. The orchestrator decides how to pick a model; Noa only looks up the table.
- C:
  - `wait --stall SECS` (`send --wait` too): if the text of an open pane's screen does not change for SECS seconds, the wait ends with `stalled` (with `quietSecs`). Screens are fetched only when `--stall` is given, at least 1 second apart. A report or exit in the same round takes priority. Claude Code shows elapsed seconds on its working spinner, so its screen keeps changing; it becomes `stalled` only when it returns to the prompt without reporting, or stops at a prompt that does not report. Whether Codex and agy keep changing their screens while working is not checked (they could become `stalled` while working), so the skill says to check with `read` after `stalled`.
  - Reopening the board (D10's known limitation): when a pane labelled `board:<P>` is created through the IPC spawn path, workers that an earlier board returned to P (a mark is put on the `Float` when it is returned) are moved to the new board, folded and stowed. The surface and process carry over as they are. Moving to another window uses D10's reattachment in reverse. Floats started with `--no-board`, floats with focus, and any beyond the 20 slots are not moved. A float moved by hand loses the mark.
- Found in real use: 3 Claude Code workers kept working after reporting `finished` (`--tail` showed a line running a commit). The cause is not confirmed (possibly the user's Stop hook). The skill says to `read`, and if the worker is still working, wait again with `--after`. (`wait --settle` was added in D17)
- Checked: unit tests (arguments, tier insertion, porcelain parsing, add and remove with real git, `branch -d` refusing an unmerged branch, stall detection, choosing what to restow). On a real machine (Noa 0.9.3's server and `+ctl` built from this branch), the flow `spawn --worktree --tier light -- claude` → `send --replace --enter --wait` after typing a draft → `wait --stall 4` → `close --prune` was checked through to the worktree and branch being removed.
- Not checked: how restowing looks in the GUI (needs the app rebuilt), the load of `--stall` with 20 workers.

### D15: Make the board "needs-you queue + working tree + preview" (2026-11-20)

The proposal the owner brought in, "Noa Mission Control" (a control tower that highlights only the moments that need a person's judgment), was adopted as a change to the board's display layer only. The protocol, `+ctl`, the fetch intervals (list 500 ms, screens 1 second) and reconnection are unchanged. The owner left the 4 open points with "settle them and implement", so the following defaults were taken.

- Sections (revising D13's "a child goes into a section by the most urgent member of its subtree"):
  - Workers needing you (`Group::Needs`), children or grandchildren, are listed flat in the **queue** at the top. The order follows D11's rules (approvals, errors and failures first, then by longest wait). A grandchild is written "parent › name".
  - Everything else is in the **tree** at the bottom (「バックグラウンド」, "background"). A child goes here if its subtree has a member at work or with no report, or if it needs you itself and has grandchildren. A grandchild needing you is also shown under its parent, but drawn dim, says only 「↑ 要対応に表示」 (`↑ in the queue above`), and cannot be selected (`j`/`k` skip it). A child needing you that has no grandchildren appears only in the queue.
  - A child whose whole subtree is done or exited is counted only, on the last line (「✓ 完了 N · 終了 N · 閉じた N」, `✓ done N · exited N · closed N`). Unfolding it lists that child and its grandchildren, and the closed workers. The default is folded.
- Grandchildren: D13's "nesting goes one level deep" is used as is. The `--of R` list already shows only children and grandchildren, so more levels are not needed.
- `Space`: if the selected worker is in a subtree of the tree (a parent, or a grandchild at work; parents needing you included), it folds or unfolds that subtree; otherwise the done/exited line. The key list at the bottom and the section's rule line say what will happen. When done/exited is folded with the selection inside it, the selection moves to the last remaining row. `j` past the last row unfolds it. Selecting a done worker by number also unfolds it. Only the working tree folds; when a subtree moves to done/exited, its folded mark is cleared too (the done/exited list shows the grandchildren too, and if it goes back to work it reappears unfolded). When no worker remains, the closed workers are always listed and `Space` is not suggested. When a grandchild selected in the queue goes back to work, its parent's subtree is unfolded to show the selection. The parent's name in a grandchild's queue row is cut at 10 columns.
- States that go into the queue: the current needs-you set as is (awaiting approval, awaiting input, error, non-zero exit, notified). Not narrowed to the proposal's 3 kinds. Showing failures under needs you is D11's decision, and dropping notified would hide D1's attention.
- State symbols: `!` awaiting approval / notified, `?` awaiting input, `×` error / failure, `●` working, `✓` done, `·` exited / no report. Color is never the only distinction.
- A working row shows the time the screen last moved instead of the time since it entered the state (「8 秒前に出力」, English `working, output 8s ago`). The first read says nothing about when it moved, so the time is shown only when a change was seen between two consecutive reads less than 3 seconds apart since it entered its current state (a change spanning a time when the detail view was open, or when it was not at work, has no known time and is cleared). Changes from before the current state are not shown. When unknown, no time at all is shown (it does not fall back to the working time either; D11's "elapsed time with an unknown start is not shown"). In narrow widths, 「作業中 8 秒前」 (`working 8s ago`). After 2 minutes without movement, 「出力なし N 分」 (`quiet Nm`) as before.
- Preview: the tail of the selected worker's screen is shown to the right of the list (2/5 of the width) at 110 columns or more, below the list (1/3 of the body, 3–8 lines) at 70–109 columns and 18 rows or more, and not at all when narrower (see it in the `Enter` detail view). For a worker needing you, the question is in the input box or a dialog, so the preview goes to the bottom of the screen; for a worker at work, it shows what is above the input box (`content_lines`). At the bottom: "kind · state · report N · cwd". With this, the 1–3 lines of screen tail under each row were removed. When a row's detail is empty, a worker needing you shows the last line of the screen (excluding rules; the question is in the input box), and a worker at work shows the last line of work above the input box.
- Screens are read for every worker needing you or at work, and for the selected worker (including those inside folded subtrees, to show freshness on the row).
- Language (an additional request from the owner): board labels are in Japanese when the locale is Japanese, otherwise in English. The check follows POSIX order (the first of `LC_ALL`, `LC_MESSAGES`, `LANG` that has a value); Japanese if it starts with `ja`. If none is set, macOS's preferred language (the first of `NSLocale.preferredLanguages`) is used, because Noa started from the Dock has no locale variables and the board starts without a shell. `board --open` passes the orchestrator's `LANG` and `LC_*` to the board's pane, as for workers (before, only `PATH`). Noa's other UI and `+ctl`'s output were already in English.
- Header: 「NOA / CONTROL」, with 「ワーカー N · 同期 いま」 (`workers N · synced now`) on the right (「同期 まだ」, `synced never`, if the list has never been read). In narrow widths, only 「ボード」 (`Board`) and the count.
- Not adopted:
  - The mockup's outer frame (`│` on the left and right). It uses 2 columns on every row, so rules are used only between sections.
  - Button-like boxes around choices, and badges with background colors. They would mean parsing the agent's screen, which breaks with each version. The preview shows the screen's text as it is.
  - A hint line inside the preview (「g 実画面へ移動して回答」, "g: go to the real screen and answer"). It duplicates the key list at the bottom.
  - A dedicated layout for 44 columns or fewer. The queue is always on top and columns shrink by the existing fallbacks (abbreviated states, kind and tally abbreviated), so it draws the same as 45–69 columns.
- Checked: unit tests (placement and full-row widths per width, queue and echo, the target of `Space`, freshness, preview slicing).
- Not checked: appearance in the GUI (colors, rules, the bottom preview hidden by an open worker).

### D16: Fix the instability found by review and on a real machine (2026-11-20)

Three reviewers were run with `+ctl` (`ctl.rs`, `board.rs`, the server side), and boundary cases were tried on a real machine in parallel. Of the reviews' 21 findings, 19 were fixed, plus 2 found on the real machine (the wording of `send` to a pane not in the list, and exited panes appearing in `baseline`). No new features were added.

- Waiting:
  - `wait --children --after PANE=SEQ` failed with exit 1 when `--after` included a child that closed after the timeout line, which broke the resume loop. A pane not in the list is reported as `{"reason":"exit","closed":true}`. A pane in the list that is not the caller's worker is still an error.
  - When one request went past its 5-second deadline, the wait ended as "connection lost" (exit 3) and no timeout line was printed. An expired deadline is now `ClientError::Timeout`, separate from a lost connection, and only inside the wait loop is it retried on the next tick. Outside a wait it is still exit 3, but the wording is "did not answer in time".
  - Panes printed in `exited` were removed from the timeout line's `baseline`.
- Server: a request for which the UI thread returns no result within 2 seconds is answered with `-32009` (outcome unknown) instead of `-32603`. `spawn --worktree` does not read this as a refusal and keeps the worktree (before, it could remove the ground under a running worker). The board's automatic open does not fall back to a tab in this case either (it does not make a second board).
- `close`: when its output closes (`close --children | head -1`), it does not panic and closes every pane. `--prune` also runs for panes that vanished between the listing and the close, and when it skips because the cwd is outside the worktree it gives the reason in `kept`.
- Guards: a `send` to a pane not in the list says "pane N is not listed" (it does not suggest `--any-pane`). The exit on hitting a Noa that returns no surface id is now 3 for `send`/`close` too.
- Board:
  - It does not start on a stdin/stdout that is not a terminal (exit 1). Running `board` from an agent's tool shell left it stuck after writing escapes to a pipe (seen on a real machine).
  - When the destination of the send line or the close confirmation vanishes, keys are discarded until Esc is pressed. Before, it silently returned to normal mode and the following keys acted on a different worker.
  - If the reconnection target is a different Noa (different `serverInstanceId`), it quits. (Revised in D17: it does not quit, and keeps reconnecting until the original Noa answers) This is so that it does not close a worker of another instance whose pane id happens to match.
  - The `q` confirmation is skipped only when the latest list fetch succeeded and showed 0 workers. The confirmation text puts what will close first (revised in D17: puts the keys first), and does not promise how many can return (the parent's 8 float slots also count existing floats, which the board cannot know). The full text fits in 80 columns.
  - Screen fetching stops for that round at the first lost connection or expired deadline (with 20 workers, keys could stall for up to 100 seconds).
  - When the parent pane closes, it says so. On SIGTERM, SIGHUP and SIGINT it restores the terminal before quitting. Two-digit number input discards the first digit on a non-digit key, and measures the 1 second by key arrival time. No history line is made for an agent that has never reported.
- Not adopted:
  - When `running` comes in the same tick right after `finished`, the waiter sees only `running` (the option of keeping the last stopped state in `Panel`). The wait ends when the next turn ends, so it was judged not worth adding to the protocol. To be reconsidered if a case where it matters appears.
  - Closing on the server side the race where the state moves to `permission` between the board's paste and Enter. It stays a client-side recheck, as in `send`.
- Skill: an agent's `read --scrollback` returned only about the last screen (seen with Claude Code 2.1.329 and Codex 0.171.0). SKILL.md now says to have long reports written to a file. (Revised in D17: spawn hands over the file in `NOA_RESULT`. On the alternate screen, `alternateScreen` tells you so)
- Checked: unit tests (at least 1 per fix), `scripts/verify.sh`. On a real machine (this branch's `+ctl` against a 0.9.5 server): refusing `board` without a terminal, `send` to a pane not in the list, `baseline`, `--after` including a closed child, `close` to a closed pipe, the `q` confirmation text at 80 columns, returning from the alternate screen after SIGTERM.
- Not checked: the `-32009` path (a server-side change; the running Noa is the old version), a reconnection target that is a different instance, the display when a destination vanishes (unit tests only).

### D17: Fixes from 3 audits (UI/UX, reliability, autonomy) (2026-11-21)

After D16, code-reading audits were run on 3 axes (HEAD 118c12c): the UI/UX of the board and stowed workers (U1–U14), the reliability of the control path (REL-01–REL-17), and the autonomy to run to the end without a person (AU-01–AU-16). Findings with evidence (`file:line` or a scenario) were fixed on 5 branches across the server, CLI and board. The protocol is additive only, and `PROTOCOL_VERSION` stays 3.

- Waiting and sending:
  - `send` refuses a question dialog (`agent.blocked:"question"`) as well as an approval prompt (`--answer` sends anyway; AU-01). Claude Code's question dialog is `input` in OSC 7501, and Enter selects the first choice. It checks 3 times: before sending, after clearing with `--replace`, and right before Enter. D11's reason for not refusing `input` (the hook adapter's `idle_prompt`) still holds, so only an `input` with `blocked` is refused. An `input` without `blocked`, and old servers that return no `blocked`, are sent to as before.
  - A paste, or an Enter-only send, to an agent at work (`running`) is refused; `--queue` sends anyway (AU-05), because `--wait` would take the end of the current turn for the reply to the text sent. Every line of `--queue --wait` carries `"queued":true`. `--raw` keys with text (Esc, Ctrl-C) are always sent, because they are the way to stop an agent at work.
  - A paste to an agent's pane (`agent.kind` present), or to a pane whose process is not listed yet (except a pane with `Panel.spawn`, which spawn started with a command), waits until that pane enables bracketed paste (every 200 ms, up to 15 seconds; REL-01). A program still starting read newlines as Enter, and a turn began with only the first part of the instruction. An empty paste with `requirePasteMode:true` asks, and once `-32010` stops coming back, the `--replace` keys and the text are sent. Old servers ignore this field, so the question passes at once. If it is not enabled within 15 seconds, nothing is sent and it exits 1.
  - Text over 250 KiB as JSON (`noa_ipc::MAX_SEND_TEXT_BYTES`) is refused with exit 1 before anything is typed (REL-04). Before, the 256 KiB frame limit cut the whole connection and it was reported as "cannot reach Noa" (exit 3).
  - `--replace` checks again for an approval prompt and a question after clearing the input box (REL-16).
  - `send --wait` prints `sent to pane N; resume with: wait N --after SEQ` on stderr once the text is in (REL-11). `wait` and `send --wait` on SIGTERM, SIGINT or SIGHUP print the timeout line with `"interrupted":true` and exit 2 (AU-08). Even when killed by a shell tool's time limit, the `baseline` for waiting again remains. Signals are checked between requests, so they can be up to about 1 second late (or, in the middle of a request, until that request ends). If one arrives before the first listing returns, every pane is printed as `pending`, and only the known `seq` values (from `--after`, or seen by `send --wait` before typing) go into `baseline`. Signals ignored at startup stay ignored. SIGKILL cannot be caught.
  - `wait --settle SECS` (also `send --wait` and `spawn --task`): `finished` is not printed at once but held for SECS seconds. If a new report arrives in that time, the hold is dropped and the wait goes on; if the text of the work above the input box changes, SECS seconds are counted again from there. If it holds, it is printed with `"settledSecs"` (AU-12; addresses D14's "keeps working after `finished`"). When the hold is dropped, the baseline is the `seq` of the held `finished`, so a `permission` that arrived during the hold ends the wait as before.
  - Added to wait lines: `label` for a pane with a label (also in each entry of the timeout line's `baseline`), `blocked` for what it is stopped on, and `prompt` on `permission`/`input` lines (with or without `--tail`; up to 20 lines from the group of the input box or dialog, rules and blank lines removed; AU-11). For an agent's pane, `--tail` on lines for other reasons is the last N lines above the input box. If fetching `--tail` fails, the result line that succeeded is not discarded; it gets `tailError` instead of `tail` (REL-05). If a pane that was an agent when the wait began returns to a shell, the wait ends as `exit` with `"agentLeft":true` (REL-12).
  - A `wait PANE` that names panes refuses a listed pane that is neither the caller nor the caller's worker (`--any-pane` lifts this; the second half of REL-09). A wait from outside a Noa pane (no `NOA_SURFACE_ID`, or not found in the list) is not checked, so as not to break waits from hooks or outside scripts.
  - `-32009` (outcome unknown) is exit 3, and the message says it may still have taken effect: check with `ls` or `read` before retrying (AU-06). Before, it was exit 1, read as a refusal, and resent. `send` says the text may already have been typed, `close` says the pane may still close, and `close` goes on closing the other panes.
- spawn:
  - `spawn --ready SECS`: after starting, waits up to SECS seconds for a report from the agent other than `running` (`"ready":"report"`; Codex 0.171 reports a `running` turn while starting up) or for a screen that shows something to stay unchanged for 1.5 seconds (`"quiet"`) (`"timeout"` past that, `"gone"` if the pane vanishes). The last screen's 20 non-empty lines are added to the line as `"screen"` (AU-02). The model sequence `spawn → read → send` raced with trust prompts and logins drawn late.
  - `spawn --task TEXT | --task-file F` (only for claude, codex and agy commands): without `--ready`, waits 30 seconds. If ready is `report` or `quiet` and the bottom of the screen is the agent's input box (a group enclosed by rules), it sends the work by the same path as `send --enter --wait`, and prints the wait lines after the spawn line (`"sent":true`). Otherwise it types nothing and exits 0 with `"sent":false`; the orchestrator reads `screen` and sends it itself. It takes the wait options (`--for --all --timeout --tail --stall --settle`). The file is read and its size checked before the start.
  - Result file (AU-03): every spawn whose cwd is known creates `<cwd>/.noa/` and a `.noa/.gitignore` containing only `*` (left alone if they exist). The worker's env gets `NOA_RESULT=<cwd>/.noa/result-<label>.md` (for a plain label; if that file already exists, `result-<label>-<8 hex digits>.md`; with no label, `result-<8 random hex digits>.md`). It is one file per worker so that workers sharing a cwd do not write to the same file. The name is reserved by creating an empty file at spawn time (reports are written last, so checking existence alone handed the same path to a worker with the same label started while the earlier one was still working; pointed out in the PR #210 review). A file that is still empty is not treated as a report: `result` says it has not been written yet, and `close --prune` does not move it. `.noa/` ignores itself, so it does not show in `git status` and does not block `git worktree remove` (`close --prune`). As is, the report would be removed with the worktree, so `close --prune` moves it first (see the worktree item). If the caller passes `--env NOA_RESULT=…`, that is used. If it cannot be written, a warning is printed and the spawn goes on. With D16's "have long reports written to a file", the orchestrator chose the file's location, which conflicted both with `close --prune` and with the unattended rule (do not write outside the worktree).
  - `--input` to an agent's command is refused (AU-14). `--input` is written as raw keys right after the start, so every newline becomes Enter. `--label board:…` is refused too (REL-14; the server also refuses a `board:` label with a `parentPaneId` with `-32602`).
  - `--max-children` is also sent to the server as `maxChildren`. The server counts children per parent on the UI thread and refuses with `-32011` (REL-02; closes D3's unsupported item). The client's precheck stays for old servers, and the wording of `-32011` matches the client's.
  - A spawn that got `-32009` or a client deadline rereads the list every 100 ms for up to about 3 seconds. If exactly one new child of its own (with the same label, if `--label` was given) is found, it succeeds with `"recovered":true` (AU-06). If not, exit 3.
  - If a spawn the server refused had opened a board on the spot, that board is closed only when this pane has no workers at all (REL-13). A new board takes over workers returned from an earlier board (D14), so existing workers may be on it.
- Results and journal:
  - A wait line (`wait`, `send --wait`, `spawn --task`) has `"result":{"path","bytes"}` if the pane's result file was written after the wait began (for `send --wait`, from just before typing), so that a previous job's result is not read as the new one. For a pane waited on again with `--after PANE=SEQ`, it counts from the time of the journal's `send` taken before that `seq` was typed (in seconds, rounded down).
  - `+ctl result PANE [--max-bytes N]`: prints the result file. Unlike `read`, `--max-bytes` keeps the beginning (a report's summary is usually at the top).
  - Journal (AU-04): for each orchestrator pane, `spawn`, `send`, `outcome` and `close` are appended one per line to `<data-dir>/noa/ctl/<surface id>.jsonl` (the same `noa/` as `session.json`). The client writes it, not the server, so that an orchestrator whose context was compacted can recover what it started, what it sent, what it saw and what it answered. If it cannot be written, a warning is printed once and the command does not fail. If it is over 4 MiB when opened, the older half is dropped. Appending and dropping the half are done after taking an exclusive lock on the neighboring `<surface id>.jsonl.lock`. A newly created journal is 0600 and a newly created `noa/ctl` is 0700 (existing permissions are not changed), because it holds the text sent and the screens answered.
  - `+ctl status`: one line per worker of the caller with the listed state and the journal's contents (`tier`, `argv`, `lastTask`, `lastSentSeq`, `lastOutcome`, `answers`, `result`, `worktree`). `lastTask` is at most 200 characters of the last task text, with any timeline header (`[noa-timeline board=… run=…] …`) removed. A worktree worker gets `ahead` (the number of commits in `HEAD..<branch>`) and `dirty`. Workers in the journal but not in the list are printed last with `"gone":true`. `status --answers` prints the prompts answered from here, with the screen just before each answer, oldest first. D14's "hand the answered prompts to the person" had been only a promise in the skill.
- worktree (REL-08, AU-07, AU-15):
  - The location of `spawn --worktree` is beside the main worktree (`<main>-wt/<label>`), not beside the caller's worktree. When the orchestrator's cwd was inside a worker's worktree, it created a nested location. The branch is still cut from the caller's HEAD.
  - `close --prune` looks up the worktree from the closed pane's cwd (or, if gone, its nearest existing ancestor), not from the caller's cwd, and runs git in the main worktree. If the label passes `--worktree`'s rules but no worktree is found, it does not stay silent but gives the reason in `worktree.kept` (exit unchanged). A worktree that an old `+ctl` created in a nested location is not found and becomes `kept`. Before removing, every `.noa/result-*.md` in the worktree is moved to `<main>/.noa/` (with `-<8 hex digits>` added if the name exists), and where that worker's own report went is given in `worktree.result`. `result` and `status` read it from the journal's `close`. If they cannot be moved, the worktree and branch are kept and the reason is given in `kept`. A worker already gone from the list is cleaned up the same way, using the label and worktree in the journal as its cwd.
  - The warning about uncommitted changes is split into changes to tracked files and untracked files (AU-15).
- Server and protocol (additive only):
  - `Panel.agent.blocked`: `"permission"` or `"question"`. Present only when the latest report is an OSC 7501 blocked.
  - `Panel.spawn`: `{argv, startedAt}`. Present only on a pane that an IPC spawn started with a command (used for the board's model, effort and start time).
  - `requirePasteMode` and `-32010` on `noa.sendText`, `maxChildren` and `-32011` on spawn, `alternateScreen` on `noa.getText`. `read --scrollback` and `wait --tail` say on stderr that a pane on the alternate screen returns only the screen (REL-07. The cause of D16's observed "returns only about one screen" is either the alternate screen or history erased by `CSI 3 J`, and which one applies to each agent is not checked. Only the former can be told by this).
  - A connection over the connection limit (32) is closed with close 1013 (`too many connections`) instead of being dropped silently. A frame or message that is too big is closed with 1009 (`message too big`) (REL-10, REL-04). `+ctl` retries 1013, and connections cut during the handshake (such as when more than 4 are refused at once), for up to 3 seconds; if still full, it exits 3 and says that each running `wait`, `send --wait`, board and attach holds one. 1009 is exit 1.
  - The cause of `agent.kind` missing with `spawn -- claude` was traced to the first process notification, arriving before the card, being discarded (from reading the code only; not observed on a real machine). When there is no process name, the kind is taken from OSC 7501's `app=`, then from `Panel.spawn.argv[0]`. `Panel.process` stays missing.
  - The sidebar's branch is fetched again also when a command or an agent's turn ends (REL-06).
  - Floats of workers started over IPC are not saved for session restore (REL-09). Before, up to 20 came back as plain shells.
- Board:
  - It does not quit on its own when the connection is lost (U1). It keeps retrying with the interval growing from 0.5 to 4 seconds, and the notice line says 「接続できません N 秒 · 表示は M 秒前のもの」 (`Cannot reach Noa for Ns · as of Ms ago`). When a different Noa answers, it does not quit either and keeps waiting for the original Noa. Meanwhile it holds no connection to the other Noa, and does not run `g`, `x` or sends (they would reach a pane of another instance with a matching pane id; pointed out in the PR #210 review). It ends only on `q` (and when stdin closes). Before the list is read it says 「読み込み中…」 (`Loading…`), and while the connection is lost each row's mark and state are dimmed with a `?` added (U12).
  - When a worker newly needs you, it emits one BEL (U2). Noa marks a BEL from a pane without focus as unread in the tab and the sidebar (what the audit read from the code; not seen on a real machine).
  - The orchestrator's own state is shown in the header, and when it needs you it appears at the top of the queue as an unnumbered row (U3). That row is not closed with `x` (closing it would close every worker).
  - Pressing ⌘⇧I (`float.toggle-focus`) on an open stowed worker folds that worker and returns to the board (U4). Before, the worker stayed open and covered the bottom 65% of the board. The close confirmation puts the keys right after the question.
  - Text in the send line that could not be sent is kept as a draft per worker and comes back on the next `s` (U5).
  - In the `q` confirmation, every key other than the quit key, ^C included, cancels (U7; pressing ^C twice used to quit). When quitting always closes workers (there are workers' workers, or more than 8 top-level workers), it asks for `y` instead of `q`. The confirmation text puts the keys first, and the full text fits in 80 columns.
  - A worker running a command that is not an agent is 「実行中」 (`running`; blue `●`, in the working section) (U8). 「報告なし」 (`no report`) now applies only to an agent that has never reported.
  - The branch is shown at the bottom of the preview and in the detail view. The confirmation for closing a worker running inside its label's worktree says 「worktree とブランチ orch/L は残ります」 (`Its worktree and branch orch/L stay.`) (part of U6).
  - The wording that inserts a state into a sentence is written separately for each state, and the section name 「バックグラウンド」 ("background") became 「作業中」 (`Working`) (U10). Help descriptions wrap instead of being cut (U11).
  - Frames are wrapped in synchronized output (DEC 2026), and unchanged frames are not written (U13).
  - A question dialog (`blocked:"question"`) is shown as 「質問」 (`question`; `?`, orange), separate from awaiting input. As with awaiting approval, the send line does not open, it checks right before sending and right before Enter, and it says to go to the pane with `g` and answer. In English, awaiting input without `blocked` is written `input` (Japanese stays 「入力待ち」 as before).
  - When `Panel.spawn` is present, the bottom of the preview and the detail view show the model and effort read from argv and the time since start (U9). The text of the work handed over is not on the wire, so it is not shown.
  - While the board is open, the numbers of closed workers are not reused (first half of U14). When 1–20 are used up, the number that became free earliest is used first. Rows in the closed section also show numbers.
- Found and fixed by an independent review (a review reading the `crates/` diff; each item above describes the behavior after these fixes):
  1. A worker started with a command (`spawn -- ./deploy.sh`) never shows a process name, so `send` waited 15 seconds and refused. A pane with `Panel.spawn` does not wait for paste mode.
  2. `close --prune` removed the report file too with `git worktree remove`. It is moved to `<main>/.noa/` first.
  3. A single redraw during a `--settle` hold consumed the `finished`, and a wait without `--timeout` never ended. A changed screen restarts the count.
  4. Waiting again with `wait --after` gave no `result` for a report written before the wait. It counts from the time of the `send` being waited on again.
  5. Workers with the same label in the same cwd shared one report file. If it already exists, `-<8 hex digits>` is added.
  6. When more than 4 were refused at once at the connection limit, the rest were cut during the handshake and exited 3 at once. These are also retried for up to 3 seconds.
  7. `close --prune` on a worker gone from the list kept the worktree without a word. It is looked up from the journal and cleaned up.
  8. When the first listing kept hitting its deadline, the wait did not end on SIGINT or SIGTERM. Signals are checked during that time too.
  9. While at the connection limit, the board's reconnection stopped keys and drawing each time with its 3-second retry. The board connects once and leaves the interval to its own retry.
  10. Dropping the journal's half could lose a concurrent append by another command. A lock file serializes appending and dropping the half.
  11. The journal was created with umask permissions. It is created 0600 and 0700.
- Found and fixed in the PR #210 review (claude, codex, agy). For each, a test reproducing the failure was written first and seen to fail before the fix:
  1. An agent right after start (before its first output) is listed as a bare Panel without `agent`, so `send` skipped the bracketed paste wait (REL-01 had no effect). The bare Panel also gets `agent.kind` from the spawn's argv, and `send` gates when the spawn's argv is an agent.
  2. Pressing Enter in the board's send line while a different Noa was answering lost the text, neither sent nor kept as a draft. It is kept as a draft.
  3. The listings `wait` takes before its loop (the parent check, the result file path) were outside signal handling and did not retry a 5-second deadline either. They are handled like the loop's first listing.
  4. When several workers used one worktree, of the reports `close --prune` moved, only the closed worker's was kept in the journal. Everything moved is given in `worktree.moved`, so each worker's report can be found from the journal.
  5. With 0 workers, the row of an orchestrator needing you was not drawn (the heading said 1 item, and keys acted on an invisible selection).
  6. In the list view, the orchestrator's screen was not fetched (the preview stayed loading).
  7. `status` showed the target of `send --any-pane`, the caller itself, and mistyped ids as "gone workers". Only panes with a spawn record are workers.
- Not adopted:
  - REL-03 (an idempotency key for spawn): no protocol or server state was added; `-32009` became exit 3, and spawn recovers from the listing (above). Recovery cannot tell this spawn from an unlabelled spawn made by the same orchestrator in the same 3 seconds. Scope's Out (no idempotency key, at-least-once) stands.
  - From U6, prune from the board (`worktree::prune` after closing) and an OSC 52 copy of the cwd. The board only says that the worktree and branch stay; cleanup is left to `close --prune`.
  - The second half of U14 (filtering with `/`, and the mouse).
  - AU-13 (detecting a worker a person opened, and a draft in the input box). It would need focus in `Panel` and `getGrid` in `Client`, and the premise that Claude Code draws its suggestions in faint is not checked on a real machine. A `read` before sending stays in the skill.
  - AU-09 (`wait --unread`). The parallel model became a `wait --children` loop that carries `--after` along (AU-10, skill only). Notifications (`attention`) are still lost across a timeout.
  - REL-15 (a running `wait` reconnecting when a `server-*` config change restarts the server). The wait still ends with exit 3 without printing `baseline`. `send --wait`'s resume line and `status`'s `lastSentSeq` and `lastOutcome` are the clues for waiting again.
  - Grandchildren's worktrees from REL-08. The root's `close --prune` handles only the children's labels, so a grandchild's worktree that a child made with `--worktree` remains.
  - REL-17 (unit tests for risky branches) was not taken as separate work. The `send` steps were extracted this time into `send_steps` (through `SendTarget`), and the size check, the refusal while at work, the paste-mode wait and the recheck after `--replace` are checked with mocks. The recheck right before Enter, and `open_board` not falling back to a tab on `-32009`, still have no unit tests.
- Checked: unit tests (at least 1 per item; with real git, that the result file does not show in `git status` and does not block `git worktree remove`, `ahead` and `dirty`), `scripts/verify.sh --full`. On a real machine (this branch's `+ctl` against a 0.9.5 server): refusing `send` to a pane at work, refusing a `board:` label, refusing `--input` to an agent, the `interrupted` line on SIGTERM, refusing 250 KiB.
- Not checked:
  - None of the server-side changes can be checked on a real machine until the app is rebuilt: `blocked`, `requirePasteMode`, `maxChildren`, `alternateScreen`, `Panel.spawn`, close 1013/1009, refetching the branch, session restore.
  - GUI: the unread mark from BEL, folding with ⌘⇧I, DEC 2026 drawing, colors.
  - `--task`'s input box detection and `prompt` extraction rely on agents enclosing the input box in full-width rules; Codex 0.171 draws none, so `--task` also takes its `›` input line near the bottom (not a numbered `› 1.` choice). Checked on 2026-11-22 with Claude Code 2.1.330, Codex 0.171.0 and Antigravity CLI 1.5.14 (each typed a task; in a new directory Claude Code and Antigravity CLI stopped at their trust prompt with `"sent":false`, while Codex showed none and took the task). Work could still be typed into a trust prompt drawn between rules, and a dialog without rules gets no `prompt`.

### D18: The board shows each worker's task and report (2026-11-23)

A person watching workers on `+ctl board` saw each worker's state and screen but not what it was asked to do, nor whether it had written its report (`$NOA_RESULT`) or what the report said; learning either meant leaving the board for `+ctl status`, `+ctl result` or the worker's pane. The board now shows both, from the files `+ctl` already keeps. The protocol, the server and `+ctl`'s other commands are unchanged.

- Sources (no new record): a worker's task is the last non-answer `send` in its parent's journal (`noa_config::ctl_journal_path` of the parent pane's `surfaceId`: the orchestrator's for a worker, the parent worker's for a worker's worker, D13), read with `journal::workers`. Its result file is the journal's `result`, else `<cwd>/.noa/result-<label>.md` for a plain label (`ctl::result_path`, as `+ctl result` finds it).
- Detail view (`Enter`): a Task section (the task on up to 3 lines, 2 under 32 rows and 1 under 24, cut with `…`; 「先頭 200 文字で切れています」 / `cut at the first 200 characters` when it is as long as the journal keeps, `journal::TEXT_CHARS`) and a Result section (「まだ作られていません」 / `not created`, 「空です(まだ書かれていません)」 / `empty (not written yet)` for the file reserved at spawn, or 「N バイト · 3 分前に書かれました」 / `N bytes · written 3m ago`, and the path). When there is nothing to show, a short dim reason: no journal (the parent pane is not listed), journal not found or unreadable, not in the journal, no task sent, no known result file, not a regular file, cannot read it.
- `r` in the detail view swaps the screen box for the result file's start, with its line position; `j`/`k` and the arrows scroll it instead of stepping to another worker; `r` again, `Esc` or `h` go back (leaving the detail view always returns to the screen next time). The footer and `?` help say so in both languages. `r` was free; in the list it does nothing.
- List: the preview's last line adds 「結果あり」 / `result written` (text, in green) after the report count once the file is non-empty, only where it fits, so narrow layouts keep their widths.
- Naming: on the board 「報告」 / `report` already meant an agent's OSC report (`report N`, 「報告の履歴」), so the `$NOA_RESULT` file is called 「結果」 / `result`, as `+ctl result` calls it.
- Cost: listings come every 500 ms and screens every second, and none of that reads these files. The board's loop looks at them at most every 2 seconds (`insight::FILES_EVERY`): one `stat` per journal, reading it again only when its modification time or length changed, and one `lstat` per worker's result file. The result's text is read only while `r` shows it, and again only when its time or length changed. A task sent or a report written shows within about 2 seconds.
- Safety: a result is read with `O_NOFOLLOW | O_NONBLOCK` and only if the opened file is regular (a FIFO would block the board, a device may never end, a link may lead out of the worker's file), at most 16 KiB and 200 lines from its start (`result::read_start`, as the Agent Dashboard's drawer reads it); the `lstat` calls a link not regular too. A journal is read only if regular. Text is stripped of control characters (tabs become spaces) before it is drawn.
- Not adopted: a result preview in the list's rows (the row's detail is the agent's last report; one more column would not fit 80 columns), reading the end of a long result (its start holds the summary by convention), and watching files with kqueue/FSEvents (polling every 2 seconds is enough for a person and needs no platform code).
- Checked: unit tests with temp journals and result files (both languages; widths at 80 columns, narrow panes and the minimum size; `r` toggling, `j`/`k` scrolling and its end; missing, empty, FIFO and unknown-parent reasons; a task cut at 200 characters; an unchanged journal not read again, a changed one read after the next 2-second look; a rewritten result read again), `result::read_start` bounds and refusals, `scripts/verify.sh`.
- Not checked: appearance in the GUI (colors, the swapped box) on a real machine, and the 2-second delay felt in real use.

### D19: `+ctl retry` (2026-11-23)

Retrying a worker whose result failed the orchestrator's check took four steps that the skill spelled out: `close` without `--prune`, `spawn --cwd <its worktree> --label <same label> --tier <one up>`, and the whole task sent again with a note that the previous attempt's changes are there. A new label split the work over two branches, and a forgotten note had the new worker redo or overwrite the work. `+ctl retry PANE [--tier T] [--task TEXT | --task-file F] [--ready SECS] [wait options] [--no-board]` does it in one command. Client only: no protocol or server change.

- Which worker: PANE must have a `spawn` in this pane's journal (`journal::workers`), listed or gone. A pane listed under another parent with that id (ids are given out again after Noa restarts) counts as gone and is not touched. A listed worker whose agent is `running` is refused: the orchestrator waits for its turn to end, or closes it.
- Command: the journal's `argv` (else `Panel.spawn.argv`), with exactly the flags `worktree::apply_tier` put right after the program for the journal's `tier` removed (`worktree::remove_tier`; refused when they are not there), then `--tier`'s, or the old tier's, put back by the spawn. `--tier` on a command that picks its own model or effort is refused, as `spawn --tier` refuses it. Only `claude`, `codex` and `agy` are retried, since the task is typed.
- Place: the old worker's cwd and label from the journal. No worktree is made; `orch/LABEL` and `<main>-wt/LABEL` go on being used, so `close --prune` later still finds them by label and cwd. The new spawn's journal event and line carry the old `worktree` and `branch`, and `"retryOf":"<old pane>"`, which `status` prints. The old pane, if listed, is closed after every check passed and before the spawn (a board holds 20 workers), journaled as a `close` and never pruned.
- Task: `--task`/`--task-file`, else the old worker's last task: the last `send` that was not `--answer`, not `--raw` keys and not empty. The journal's `send` kept only its first 200 characters, so it now also keeps the whole text as `full` when it is longer and at most 64 KiB (`journal::FULL_TEXT_BYTES`; the journal stays 0600), and `raw`. A task known only by its start (over the bound), or from an older journal (no `raw`, so `--raw` keys there look like a task), is refused with a request for `--task-file` before anything is closed. The text sent is a preface, a `---` line, and the task. The preface names the previous worker (pane, program, tier or model), its last outcome's reason when the journal has one, says its changes, committed or not, are in this directory and that `git status`/`git log` come first, and names its result file when that holds a report. A retry of a retry strips the earlier preface (it starts with a fixed phrase and ends at the first `---` line), so prefaces do not stack.
- Then it is `spawn --task` (`Session::spawn` with the extra line fields): the ready wait, `"sent"`, the wait lines, the journal, and the exit status are the same. The new worker gets its own result file (`result-LABEL-<8 hex>.md` while the old one is there).
- Chosen over the spec as written:
  - The whole last task, not the whole first one: a task given in follow-up sends is not rebuilt; the skill says to pass `--task-file` then.
  - `--raw` keys and empty sends do not count as the task (they were the "last text sent" `status` shows as `lastTask`; `lastTask` is now at most 200 characters with any timeline header removed), which needed `raw` in the `send` event.
  - The spawn's `--env`, `--no-wait-after` and `--focus` are not in the journal and are not carried over.
  - `check_spawn` (the directory exists, the program is on `PATH`) runs before the close, so a pruned worktree closes nothing.
- Checked: unit tests for argument parsing; tier flag removal and re-application for each tier pair and each agent (`worktree`, and through `retry::plan`); the refusals (not a worker, running, no command, not an agent, tier conflict, flags not where the tier put them, no directory, no whole task, a text too large, `--no-board` from a worker); the journal's `full` round trip, its 64 KiB bound, an older journal's cut task, and that answers, keys and empty sends are not the task; the preface and its stripping; the steps' order (check, close only when listed, spawn; a failed step stops the rest) with mocks as for `spawn_steps`; `scripts/verify.sh`.
- Board (with D18): the board's Task section shows a retried worker's whole task without the preface (from `full`), headed 「ペイン N の再試行」 / `retry of pane N`, and drops the "cut at 200 characters" note when the journal kept the task whole.
- Checked on a real machine (this branch's `+ctl` against Noa 0.9.14's server, Claude Code 2.1.330): `spawn --tier light --task …` then `retry PANE --tier standard` closed the old pane, started `claude --model sonnet --effort medium` in the same directory under the same label with a new result file, typed the preface and the task (the worker answered the task), and `status` showed the old worker gone with `closed` and the new one with `retryOf`; `retry` of a pane that is not a worker exited 1.
- Not checked: a retry in a worktree after the previous worker left commits there (whether the new worker builds on them is the agent's behavior, not Noa's).

### D20: A widget session is a board tab that orchestrates itself (2026-11-25)

The agent widgets (`widgets/discussion`, `widgets/review`, `widgets/verify`, `widgets/competition`) opened one tab per session and laid its agent panes out in splits (up to 3×3). The owner changed the spec: a session opens a new tab whose pane is a board (`+ctl board`, D7), and every agent pane is a float stowed on it (D10, D12, D13). A widget is a separate GUI process, not a pane, so no orchestrator pane exists: the board pane is its own orchestrator. This decision is the Noa side; the widgets move to it separately.

Protocol (additive; `PROTOCOL_VERSION` stays 3):

```jsonc
// noa.newTab — scope: control + input
{"windowId":"3",            // optional, as before
 "board":true,              // new
 "focus":false,             // optional, as in D2
 "autoApprove":true,        // optional, as before (needs input)
 "config":{"cwd":"/repo","env":{"PATH":"…","LANG":"ja_JP.UTF-8"}}}  // optional; only cwd and env
// → {"paneId":"42"}        // the board pane's IPC id
```

- With `board:true` the server opens a new tab whose pane runs this Noa's own binary as `<exe> +ctl board --of self` (no shell; `<exe>` is the running app's `current_exe`, the same path `NOA_BIN` carries). After minting the pane's IPC id, and in the same UI-thread step that answers the request, it labels the pane `board:<that id>` and gives it no parent. The label is the one D10's `board --open` sets, so `stows_on` and `+ctl` read the pane as the board of itself. The snapshot is rebuilt before the reply (as for any spawn), so a listing or a float spawned right after the reply already sees the label.
- `--of self` is a new `+ctl board` form: the board resolves its own pane by `NOA_SURFACE_ID` (waiting for it to be listed, as other commands that need their pane do) and shows that pane's workers. It does not go with `--open`.
- Refused with invalid params (`-32602`), since the server sets them: `label`, `parentPaneId`, `maxChildren` (it goes with a parent), `config.command`, `config.initialInput`, `config.waitAfterCommand`. `config.env` keys are checked as in D2. Without the `input` scope it is refused with `-32003`: the request runs a program in a new pane, as D2's `config.command` does.
- Workers: the client spawns each agent with `noa.split` `{"paneId":<board>,"direction":"float","parentPaneId":<board>, "config":{…}, "label":…, "focus":false, "maxChildren":20}`. `stows_on` sees anchor label `board:<parent>`, so the float is stowed. A worker that orchestrates in turn (D13) gets its workers stowed on the same board (`stows_on`'s `board:<parent's parent>` rule) and nesting stays one level deep.
- Client: `noa_ipc::Client::spawn_board_tab(&mut self, window: Option<u64>, params: &SpawnParams) -> Result<u64, ClientError>` sends `board:true` with `params` (`windowId` when `window` is given) and returns the board's IPC id. It adds the client process's `PATH`, `LANG` and `LC_*` to `config.env` where `params` sets none, as `+ctl board --open` gives a board its orchestrator's (`worker_env`): a Noa started from the Dock has neither. The board's language falls back to the system language without locale variables (`Lang::detect`), and `PATH` is what the board's `git` calls (D18 results) run on.
- Capability gate: a Noa that predates this ignores `board` and opens a plain tab (a login shell) instead. A Noa that takes it lists `boardTab` in `noa.hello`'s new `capabilities` array (`noa_ipc::CAPABILITY_BOARD_TAB`; the field is absent, read as empty, from an older server), and `Client::supports_board_tab()` checks for it. A widget checks it before asking for a board tab and otherwise starts no agents, with a notice. A capability rather than a version, so a development build, which keeps the last release's `serverVersion`, passes while a released Noa without the feature does not (PR #254 review).

Board behaviour when it is its own orchestrator (`of == me`):
- It lists its children and their children (D13), and never itself (it was already left out as `me`).
- Its own pane is no lead: it never heads the queue (U3), takes no number, and is not named 「オーケストレーター」 / `orchestrator` anywhere. The header's scope reads 「このボード」 / `this board` instead of the orchestrator's name, with no orchestrator state beside it. A bell the board itself rings (attention on its own pane) therefore puts nothing in the queue.
- With no worker it says 「ワーカーはまだいません。このボードに浮動ペインが開かれると、ここに並びます。」 / `No workers yet. They appear here once floating panes open on this board.`, not D17's "the orchestrator runs `+ctl spawn`". 「オーケストレーターのペインは閉じられました」 cannot appear: its orchestrator's pane is the one it runs in.
- `q` with workers asks for `y`, and says 「y で終了 · ワーカー N 件はボードと一緒にすべて閉じます」 / `y quits · all N workers close with the board`: no worker has an orchestrator to go back to (below).

Behaviours of the floats on such a board (checked in code; unit tests where marked):
- Auto approval: `autoApprove` on the `newTab` sets that tab's per-tab flag, and a float created in the tab takes the tab's flag (`create_float` passes `state.auto_approve_enabled`), so it applies to every agent on the board, nested ones included. Toggle Auto Approve on the tab changes it for all of them.
- Focus: `noa.focusPane` on a stowed worker goes through `focus_pane`, which reveals a collapsed float (`reveal_float`: expands it, folds any other open stowed worker on the board), then raises its tab. This is the path the board's own `g` takes, so a widget's "show this pane" works the same way.
- Close one worker: `noa.closePane` on a worker closes the float (`close_float`); its own workers close with it (D13).
- Close the board: when the board pane closes (`noa.closePane` on it, ⌘W on its tab, or its process ending, `q` included), its stowed workers have no orchestrator outside the closing tab to go back to (D10's `stowed_parent` needs a parent that outlives the close), so they close with it, and their workers with them (D13). Closing a float drops its `Pty`, which kills its child (`noa-pty`'s `Drop`). Nothing keeps running unseen. If the board is the tab's only split pane (the usual case), the tab closes. ⌘W counts the floats' running programs in its confirmation, as for any closing pane's floats; `noa.closePane` asks nothing (noa-server FR-13; `IpcActionKind::ClosePane`).
- Limit: the board holds 20 stowed workers (D12, `MAX_STOWED_PER_BOARD`), children and their children together; `maxChildren` with `parentPaneId = <board>` counts the board's direct workers on the server, atomically with the spawn (REL-02).
- Session restore: worker floats are not saved (REL-09, D17), and the board's command is not either, so after a relaunch the tab comes back as one plain shell tab in the board's cwd, as a D10 board pane does. The widget process is gone after a relaunch too, so nothing restows anything.
- `restow_handed_back_floats` (D14) runs only for a spawn whose request names a `board:<id>` label; a board tab names none, and no float can have been handed back to a pane that did not exist yet, so it does not run.

- Not adopted: a dedicated method (`noa.newBoardTab`): a `newTab` flag reuses D2's validation, window targeting, `focus` and `autoApprove`. Predicting the pane's IPC id so the server could pass `--of <id>` and set the label before the spawn: ids are minted only once the pane exists, so the board resolves itself (`--of self`) and the label is set after minting.
- Checked: unit tests for `board` parsing beside the spawn extensions, the version gate, the client's env fill, `+ctl board --of self` parsing (and its refusal with `--open`), the server's argv parsing back to `--of self`, the board label, `stows_on` for a board tab's floats and their workers, and the board with `of == me` (listing, queue, header, empty note, quit prompt); loopback tests (`crates/noa-ipc/tests/orchestration_tests.rs`) for the request reaching the backend as `SpawnTarget::BoardTab` and every refusal; `scripts/verify.sh`.
- Not checked on a real machine: the board tab opening and its floats stowing in the GUI, focus from a widget, close and ⌘W with running agents, auto approval of a stowed agent.

### D6: Confine per-agent differences to hook configuration

> Added 2026-11-18: all three now report their own state with OSC 7501 (Claude Code 2.1.295+, Codex 0.163+ with `app=codex`, agy 1.4+ with `app=antigravity`; all include `blocked:kind=permission|question`). So the hook paths in the table below are no longer needed. In addition, Claude Code and Codex start hooks in a new session (setsid), so `+ctl report` from a hook cannot open `/dev/tty` and nothing arrives. The Codex procedure never worked from the start. `scripts/noa-agent-hook.py` for Claude Code was changed to report through the hook's `terminalSequence` output. A6 was resolved in the form "hooks are not needed". agy's `PreInvocation` hook fires on every model call, and the top level of `.agents/hooks.json` is keyed by hook name (the description was removed from `docs/AGENT_WORKFLOW.md`).

All three report state through `OSC 777;noa-agent`. Each agent's hooks only call `"$NOA_BIN" +ctl report`, and neither Noa itself nor `+ctl wait` branches by kind. `scripts/noa-agent-hook.py` is kept so as not to break existing users' configuration, and `+ctl report` does the same thing (write to the controlling terminal; with no terminal, do nothing and succeed).

| kind | Source of `running`/`finished` | Source of approval/answer waits | Evidence |
|---|---|---|---|
| claude | Hooks (`UserPromptSubmit`, `Stop` and others) | Hooks (`PermissionRequest`, `Notification`) | Implemented in the repository (`scripts/noa-agent-hook.py`, `docs/AGENT_WORKFLOW.md`) |
| codex | Hooks (`UserPromptSubmit`, `Stop`) | No matching hook confirmed. The TUI's notification (OSC 9) becomes `attention`, which stands in | External articles only. How to enable it and where it is configured differ by version. Not verified (A6) |
| agy | Hooks (`PreInvocation`, `Stop`; `.agents/hooks.json`) | Not confirmed. `attention` stands in | Search results from the official docs only. Treating `Stop` as the end of a turn is a guess. Not verified (A6) |

- `attention` means "a notification was shown", not proof of "awaiting approval" (`docs/specs/agent-attention.md` L0). For codex and agy, when a wait ends on `attention`, the orchestrator looks at the screen with `read` and decides.
- There are two ways to avoid stopping at approval prompts, and a human decides whether to use either: pass each agent's own auto-approve flag in the `spawn` arguments, or enable Noa's auto-approve mode in the worker's tab. This spec enables neither by default.
- Whether the way of sending (`\r` after the paste) is the same for all three is not verified (A1). If it differs, `+ctl send` switches on `agent.kind`. This is the only place a branch goes.
- The hook configuration format for codex and agy is not documented (it differs by version and is not confirmed). `docs/AGENT_WORKFLOW.md` says only "which event calls what".

## Considered but rejected

- A server-side `noa.waitFor`: in a synchronous server with one thread per connection (NFR-3 in `crates/noa-ipc/src/lib.rs`), that connection is blocked while waiting. It would also duplicate what subscribe does.
- A new method `noa.spawn`: its placement arguments overlap with `newTab`/`split`. Its shape would diverge from AppleScript.
- Confining with per-pane tokens so that "only your own descendants can be touched": a process of the same user can type into any pane with `osascript` and no token (`docs/specs/agent-automation.md` L0). Changes to `auth.rs` are not worth paying for something that is not a boundary. This judgment holds only on macOS with `macos-applescript` at its default `true` (`docs/CONFIGURATION.md:264`, `crates/noa-app/src/app/applescript.rs:189`). In environments set to `false`, and on Linux, the token is the only gate, so reconsider if that needs protecting (A5).
- Server-side run/task objects (task queue, dependencies, retries): there is one caller, and the shell script can hold the order.
- Building on AppleScript: macOS only, and with no event subscription it would poll. `wait for` was already rejected (`docs/specs/agent-automation.md`, "Considered but rejected").
- An MCP server: for an agent with a shell, the CLI is enough.
- Inferring approval prompts from screen text (auto-approve's detector `crates/noa-app/src/auto_approve.rs:45-54` has signatures for codex and agy): today it runs only in tabs with auto-approve enabled (`crates/noa-app/src/io_thread/auto_approve.rs:54`). Running it for every worker would scan the screen on every pty feed, which touches the output throughput guarantee (`docs/positioning.md` Guardrails). Consider it once `attention` proves insufficient.
- Noa choosing the agent that fits a task: the orchestrator holds the criteria for choosing, not Noa.
- Per-agent launch profiles (aliases such as `--agent codex`): writing `-- codex …` is enough.

## Scope

- In: D1–D4 and D6, updates to `docs/api/noa-server-protocol.md` and `docs/AGENT_WORKFLOW.md`, the smoke script.
- Out: the UI (D5's separate spec), automatically writing hook configuration for codex and agy (configuration examples only go in the docs), enabling auto-approve for workers by default, session restore of `parent`/`label`, orchestration over a LAN, changing the default of `server-enable`, idempotency keys for mutations (stays at-least-once; resending `spawn` after a timeout can start two).

## Assumptions and falsification

| ID | Assumption | Cheap way to check | If it fails |
|---|---|---|---|
| A1 (key) | For each of claude / codex / agy, the hooks emit `running`→`finished` every turn, and a `sendText` paste + `\r` submits the prompt | On current main, send `noa.sendText` twice to each agent's pane and see whether the sidebar state goes back and forth twice (try codex and agy with a temporary configuration whose hook emits OSC 777 with `printf`) | D3's `seq` wait does not hold for that agent. Add output-idle detection, or drop it from scope |
| A2 | Restoring the original focus with `focus:false` does not break an IME composition or the split animation | Send `noa.split` during a composition | Drop `focus`, and build a separate split path that does not take focus |
| A3 | `stateChanged` is sent without delay even when only `agent`/`exit` change | Add one case to the `card_to_panel` diff test | Fix the delivery's throttling condition |
| A4 | `noa` is not on the PATH of a pane's child process | `command -v noa` in a pane on a real machine | If it is, `NOA_BIN` is not needed |
| A5 | `osascript` with no token can type into any pane (from reading the spec and the gate's code only; not run) | Run `osascript -e 'tell application "Noa" to input text …'` aimed at another pane | Reconsider the confinement option |
| A6 | codex and agy have hooks that call an external command when a prompt is submitted and when a turn ends | Check the official docs and `/hooks` for the installed version | That agent can be waited on only through `exit` and `attention` |

A1 and A6 are not verified. The whole design rests on these two.

## Implementation record (2026-11-16)

- `noa-ipc`: `Panel`'s 5 fields, `SpawnConfig`/`SpawnParams`/`SplitPlacement`, `IpcBackend::spawn`, `Client`'s `spawn_tab`/`spawn_split`/`send_text`/`subscribe`/`next_notification`. Tests are in `crates/noa-ipc/tests/orchestration_tests.rs`.
- `noa-app`: `execute_ipc_spawn` and automatic placement (`crates/noa-app/src/app/ipc.rs`), the mapping to `Panel` (`crates/noa-app/src/ipc_bridge.rs`), `SessionCard.agent_seq`, `NOA_BIN`, `Noa +ctl` (`crates/noa-app/src/ctl.rs`).
- `noa-pty`: `Pty::spawn_env` (to keep the surface id on the `Surface`).
- Checked by automated tests: argument parsing, wait conditions (not ended by an old report, `--after`, attention, exit and vanishing), the order of automatic placement, the mapping to `Panel`, the server's routing and scopes, notification buffering.
- Not checked: actual splitting and focus restore in the GUI (A2), sending and reporting with each agent (A1, A6). Running `scripts/agent-orchestration-smoke.sh` in a Noa pane checks, without agents, spawn → wait for exit → read → wait for a report → close → guards.

Next phase: write the spec for the UI (D5).
