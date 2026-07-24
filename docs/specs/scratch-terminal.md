# Scratch Terminal (transient popup terminal) — Draft Spec

- slug: scratch-terminal
- status: locked (2026-07-24)
- owner: simota
- build-path: apex

## L0 — Vision

**Problem**: Users want to instantly open a disposable shell in the focused session's current directory. A new tab or split disrupts the layout, and the existing QuickTerminal — designed to be "persistent, singleton, cwd-non-inheriting (launches in the process cwd)" — doesn't fit the "type a quick command where I'm currently working, then close it" use case. A lightweight, transient terminal: one action pops it up → run a command → it disappears on close. (User-confirmed 2026-07-24)

## Reuse / constraint findings (Lens)

**Reusable assets**:
- `App::focused_pane_cwd()` / `pane_cwd()` — crates/noa-app/src/app/lifecycle.rs:699-717. cwd sourced from OSC 7 + a `Path::is_dir` guard. This alone fully covers the cwd input.
- `spawn_pane_surface(..., cwd)` + `PtyConfig.cwd` + `CommandBuilder::cwd` — a complete pipeline for launching a shell in an arbitrary dir.
- `quick_terminal.rs` (847 lines) — a complete template for a borrowed winit window + dedicated surface/Renderer + float behavior + autohide + previous-app restore.
- `macos_window::configure_quick_terminal_window` / `show_quick_terminal_window` — `NSFloatingWindowLevel`, all-Spaces, borderless configuration.
- `AppCommand` + `commands.rs` dispatch + `keybind.rs` — in-app action/keybind registration.
- `macos_hotkey::HotkeyAction` + `GlobalHotKey::register` — global hotkey (if needed).
- `allocate_group_id` / `WindowGroupId` — group isolation.
- `anim` easing set.

**Hard constraints**:
- 1 winit window = 1 wgpu surface. Native overlay cards are display-only (can't be first responder) → the popup must be an independent winit window.
- winit can't do NSPanel → mimic a borderless NSWindow (the QT approach).
- `Pty` is Send/!Sync, moved to the io thread. Terminal state lives in `Arc<Mutex<Terminal>>`.
- cwd can be `None` (shell integration disabled, OSC 7 not reported, dir deleted) → a fallback is mandatory.
- In-app keybinds are currently compile-time defaults only → user configuration follows the config-only-key approach (QT precedent).
- QT is excluded from `window_order`, the sidebar, and the session store (`window_sidebar_eligible`) → the transient popup should be excluded the same way.
- No equivalent feature exists upstream in Ghostty → a pure noa feature with no parity constraint.

## CHALLENGE — selection and rejection

**Adopted (confirmed by user 2026-07-24)**: Option A, "Scratch Popup (small centered floating window)" + auto-close on exit + toggle/focus-loss close.

Decisions:
- Esc is not intercepted (passed straight through to the pty). Close methods: pressing the toggle key again / losing focus / shell exit.
- Losing focus = immediate teardown (no warning even if a child process is running).
- Single instance, toggle behavior.
- cwd: `focused_pane_cwd()` → falls back to the process cwd if `None`.
- Excluded from the sidebar/session store/window_order.

**Considered but rejected**:
- Option B, a second QT-style edge panel — visually confusable with QuickTerminal.
- Option C, Overlay Scratch (window stacking) — the added complexity of move-tracking and focus management isn't worth it.
- Esc-intercept close — would break legitimate Esc input in vim/TUIs.
- Double-Esc close — muddies the implementation and behavior.
- Warning on a running process — disposability consistency takes priority.
- Promote feature (promote to a tab) — out-of-scope, moved to Open Questions.

## L1 — Requirements

Functional requirements:
- **R1 Toggle command**: Add `AppCommand::ToggleScratchTerminal`. Default chord `cmd+shift+t` (confirmed no conflicts 2026-07-24). Overridable via the config key `scratch-terminal-key`; empty string/`none` disables it (following QT's `quick-terminal-hotkey` precedent).
- **R2 Presentation**: A borderless floating small window centered on the focused noa window (dedicated winit window + wgpu surface + Renderer). `NSFloatingWindowLevel`, no decoration (reusing a configuration equivalent to `configure_quick_terminal_window`). Default 100×25 cells, clamped to 90% of the focused window. Config key `scratch-terminal-size` (specified as `WxH` cells). No appearance animation (shown instantly, following QT's center precedent).
- **R3 cwd inheritance**: Launch the shell (`spawn_pane_surface(..., cwd)`) with the value from `focused_pane_cwd()`. `None` (shell integration disabled, OSC 7 not reported, dir deleted) falls back to the process cwd.
- **R4 Lifecycle**: A fresh spawn on every invocation. On display, the popup itself becomes the key window (equivalent to `makeKeyAndOrderFront`). Closed enumeration of close conditions: (a) pressing the toggle key again, (b) the **popup's own** loss of focus (not the anchor window's focus-loss event — this prevents the popup from destroying itself immediately after appearing; as with QT's `maybe_autohide_quick_terminal`, only a focus-loss on the popup's own window_id triggers this), (c) shell exit, (d) `cmd+w` inside the popup, (e) config reload, (f) app quit. Any of these immediately tears down the pty, window, and surface. No warning.
- **R5 Single instance**: Only one exists at a time, app-wide. Guaranteed by the type invariant `App::scratch_terminal: Option<_>` (the spawn path always checks `is_some()` first). Toggling while shown closes it.
- **R6 Exclusion**: Excluded from `window_order`, the sidebar, the session store, the tab overview, and tab cycling (following QT's `window_sidebar_eligible` precedent). **However, it does participate in keybind dispatch**: even while the popup is focused, command resolution via `KeybindEngine` proceeds as usual — (i) the toggle key and `cmd+w` trigger the R4 close, (ii) terminal-operation commands (copy/paste/clear/select-all/font-size) work, (iii) window/tab-management commands (new tab/window/split, tab cycling/selection, sidebar/overview toggle, etc.) are no-ops.
- **R7 Esc pass-through**: Esc is not intercepted (sent to the pty). The only close methods are the R4 enumeration.
- **R8 Keybind scope**: R1's chord is **in-app keybind only** (no global hotkey registration). When noa is unfocused, the chord doesn't reach noa and nothing happens (intentional).

Non-functional requirements:
- **NR1 Perceived instantaneity**: Using the existing spawn pipeline (including prewarming), the time from toggle press to first-frame display is **under 150ms** (measured on a dev machine, verified via debug timing logs).
- **NR2 No QT interference**: Has zero effect on QuickTerminal's behavior or config. Both can be shown simultaneously.

## L2 — Detail

- New file `crates/noa-app/src/app/scratch_terminal.rs` (a simplified `quick_terminal.rs`: no anim, no persistence, no global hotkey, no screen resolution). State is `App::scratch_terminal: Option<ScratchTerminalState>`.
- `ScratchTerminalState`: no need for a `window_id`/`visible`-equivalent (simplified to shown=Some, torn-down=None).
- Command wiring: `commands/command.rs` (enum + menu id + `action_name()` `"scratch-terminal.toggle"` + reverse parse) → default table in `commands/keybind.rs` → dispatch in `app/commands.rs`. Follows the 5-touch modal-addition convention.
- config: `noa-config` gets `scratch_terminal_key` (String, default `"cmd+shift+t"`) and `scratch_terminal_size` (default `100x25`). Supports partial override.
- spawn: pass `cwd = focused_pane_cwd().or(None → process cwd)` to `spawn_pane_surface`. cwd resolution is a **live check at toggle time** (the `Path::is_dir` guard in `pane_cwd` runs at evaluation time — not a cached value). Uses a dedicated `WindowGroupId` (`allocate_group_id`).
- Focus-loss detection: on the path equivalent to QT's `maybe_autohide_quick_terminal` (triggered by Focused(false) on the popup's own window_id), destroy instead of hide. Must not mis-teardown on a transient IME candidate-window focus shift (follows QT autohide's existing guard). Shell exit follows the same shape as QT's `destroy_quick_terminal` path.
- Position calculation: centered on the focused window's outer frame. Does not track window movement (stays put while shown).
- Fullscreen support: inherits `configure_quick_terminal_window`'s collectionBehavior (`canJoinAllSpaces | fullScreenAuxiliary`), so it can also display over a native-fullscreen anchor.
- config reload: `ConfigWatcher::reload_config_from_disk` (config_reload.rs:506) only scans `self.windows` and doesn't apply to the popup, so **the shown popup is torn down when a reload arrives** (R4-e; disposability consistency, avoiding exposure to stale config).
- App quit: explicitly tear down the popup's pty/window on the quit path (existing teardown doesn't catch it since it doesn't participate in `window_order` — R4-f). If the anchor window closes, the popup is naturally torn down via focus-loss (R4-b).

## L3 — Acceptance Criteria

| ID | Verification | Requirement | Method |
|----|------|---------|------|
| AC-1 | `cmd+shift+t` on the focused window → popup shown; pressing again → window disappears and the shell process exits | R1, R4, R5 | GUI + `ps` check |
| AC-2 | `scratch-terminal-key = ""` disables the feature (key passes straight to the pty) | R1 | unit + GUI |
| AC-3 | `cd /tmp` in a pane, then toggle → `pwd` inside the popup = `/tmp` | R3 | GUI |
| AC-4 | With the live check at toggle time, an unreported/deleted cwd doesn't crash and falls back to launching in the process cwd | R3 | unit (fallback function) + GUI |
| AC-5 | `exit` inside the popup → auto-closes and tears down | R4-c | GUI |
| AC-6 | Clicking another window (popup loses focus) → immediate teardown | R4-b | GUI |
| AC-7 | The `Option<ScratchTerminalState>` type invariant plus the `is_some()` guard at the top of the spawn path prevent duplicate creation. Two stress cases: rapid double-toggle, and re-toggle before spawn completes — neither creates 2 instances | R5 | code review + unit + GUI |
| AC-8 | The popup never appears in the sidebar, session save, tab overview, or `cmd+shift+]` cycling | R6 | GUI |
| AC-9 | Esc works as a mode switch in vim inside the popup, and the popup doesn't close | R7 | GUI |
| AC-10 | The popup is centered on the focused window at 100×25. On an 800×600 window, it's clamped to ≤90% of the interior size (verified via frame query) | R2 | GUI + frame query |
| AC-11 | Can be shown simultaneously with QuickTerminal. Existing noa-app test suite green + QT manual check (show/hide/autohide/screen resolution) | NR2 | machine + GUI |
| AC-12 | `cargo test/clippy --workspace` green | all | machine |
| AC-13 | The popup accepts keyboard input immediately after showing, and doesn't self-destruct the instant it opens (show → keystroke → stays shown) | R4-b, R2 | GUI |
| AC-14 | Toggle press → first-frame display under 150ms (debug timing log) | NR1 | machine (measured from logs) |
| AC-15 | The shown popup is torn down when a config reload arrives | R4-e | GUI |
| AC-16 | `cmd+w` inside the popup → closes; window-management commands like `cmd+t`/`cmd+n`/tab cycling are no-ops; copy/paste/font-size work | R4-d, R6 | GUI |
| AC-17 | The popup also displays over a native-fullscreen anchor | R2 | GUI |
| AC-18 | The popup's pty isn't orphaned on app quit | R4-f | GUI + `ps` |

## Scope

**In**: All of R1–R8, NR1–NR2. Includes teardown on config reload, fullscreen display, and quit-time teardown.
**Out**: Promotion to a tab, multiple instances, global hotkey, animation, history/restore, splits, cwd header display, window-move tracking, **live application of config reload to the popup** (superseded by teardown).

## Open Questions / Deferred Decisions

- Promote feature (popup → promote to a regular tab): future consideration. Requires designing pty transfer.
- Global hotkey support: deferred since the cwd source is ambiguous when unfocused (R8 already documents the in-app-only scope).
- cwd header display: deferred to the prompt display for v1.
- Interaction between the IME candidate window and focus-loss teardown: L2 already requires following the QT autohide guard. A manual check with Japanese input is recommended at implementation time (there's a history of past bugs rooted in IME preedit).

## Assumption Ledger

- ASSUME-1 (ratified): shell integration enabled is the standard environment. An unreported cwd degrading via fallback is acceptable.
- ASSUME-2 (elicited): "transient" = tearing down the pty along with everything else on close (confirmed at FRAME).
- ASSUME-3 (ratified): no-op window-management keybinds inside the popup are the expected behavior (specified in response to a quality-gate finding, approved by the user at LOCK).
