# Spec: Keyboard-Only Copy Mode (`copy-mode`)

## Metadata

- **slug**: copy-mode
- **title**: Keyboard-Only Copy Mode
- **status**: locked (2026-07-13)
- **owner**: shingo.imota
- **build-path**: apex (recommended for the record; execution requires separate instructions)

## L0 — Vision

- **Problem**: noa users (terminal users with keyboard-centric workflows) can currently select and copy text only with the mouse.
- **Target users**: noa users with keyboard-centric workflows.
- **Job to be done**: Select text on the screen or in scrollback and copy it to the clipboard without moving their hands away from the home row.
- **Definition of success**: Pressing `shift+Arrow` starts a selection, and the entire flow—select, copy, and return to the shell—can be completed using only the keyboard.
- **Design policy**: A minimal noa-specific design (tmux/vim compatibility is not a goal). It combines editor semantics (Arrow = move and clear selection; Shift+Arrow = extend selection) with a modeless feel (typing a character naturally exits the mode and passes the input through to the shell).
- **Parity note**: Ghostty itself does not have a copy mode. This is an intentional extension beyond strict cloning.

## Reusable Assets and Technical Constraints (`reuse-scan`)

All lower layers already exist. The implementation extends those layers without introducing a new dependency boundary. The risk review and independent correction passes expanded the safe, focused implementation footprint beyond the initial 9–10-file estimate; that expanded scope was explicitly approved.

- **Selection model**: `crates/noa-grid/src/selection.rs` (`SelectionPoint`/`Selection`, based on absolute rows) + the `Terminal::set_selection` family of APIs (`terminal.rs:235-277`).
- **Copy path**: `AppCommand::Copy` → `copy_selection_to_clipboard()` (`clipboard_confirm.rs:4-19` — `selected_text()` → `clipboard.set_text`).
- **Mode precedent**: The key-handling priority chain in `event_loop.rs:494-597` + `ActiveOverlay`/`active_overlay_gate` (`input_ops/overlays.rs`).
- **Viewport**: Independent scrolling APIs (`scroll_viewport_up/down`, etc.) and the scroll-lock path (`viewport_offset != 0`).
- **Eviction tracking**: Per-frame reindexing for mouse selections (`pointer.rs:29-48`, realigned using the delta from `selection_rows_evicted()`).
- **Rendering**: The `row_base` conversion pattern in `FrameSnapshot` (`snapshot.rs`), equivalent to `is_active_search_match`.
- **Config**: Ghostty-compatible `keybind` syntax (`noa-config/src/lib.rs:208-221`) + the `command_from_keybind_action` pattern (`keybind.rs:258/284`). Existing defaults: `shift+↑/↓` = line scrolling (`keybind.rs:67-79`, replaced by this specification).

## L1 — Functional Requirements

- **FR-1**: While the primary screen is displayed, pressing any of `shift+←/↑/→/↓` enters copy mode with the shell cursor position as the anchor and immediately extends the selection by one cell (direct gesture entry). On the alt screen, the gesture passes through according to FR-13.
- **FR-2**: The config entry `keybind = <chord>=copy_mode` (no default chord) enters copy mode with only a cursor and no selection (auxiliary action entry).
- **FR-3**: In copy mode, an unmodified Arrow key moves the cursor. If a selection exists, it is cleared before the cursor moves (editor semantics). When vertical movement crosses into a shorter row, the cursor's x coordinate is simply clamped to the end of that row (no sticky column; x is updated to the clamped position).
- **FR-4**: In copy mode, `shift+Arrow` extends the selection. If no selection exists, a new selection starts with the current cursor position as its anchor. The anchor lifetime is tied to the existence of the selection, not to Shift being held, and only the Shift modifier bit from each individual key event is inspected (to avoid modifier races). Vertical x clamping follows the same rule as FR-3.
- **FR-5**: When the cursor is at the top or bottom edge of the viewport and moves farther in the same direction, the viewport automatically scrolls by one row. If no further movement is possible at the oldest scrollback row or the live bottom edge, the operation is a no-op (both cursor and viewport remain unchanged).
- **FR-6**: In copy mode, keep the viewport fixed against scrolling/following caused by **pty output** (output continues flowing into the grid without loss). Viewport movement caused by user cursor movement under FR-5 remains allowed.
- **FR-7**: Track row eviction caused by scrollback pressure for the cursor and selection anchor using the same per-frame reindexing pattern as mouse selection. If the row containing the cursor or anchor itself is evicted, clamp it to the oldest surviving row (and keep the mode active).
- **FR-8**: `Enter` copies to the clipboard and exits if a selection exists. If no selection exists, it exits without copying.
- **FR-9**: `Esc` clears only the selection and remains in copy mode if a selection exists; otherwise, it exits (two-stage behavior).
- **FR-10**: Among key inputs other than those covered by FR-3/4/8/9, any **pty-bound input that does not resolve to a keybind** (printable characters, editing keys, etc.) clears the selection, exits copy mode, and passes through to the pty. Modifier-only events and unbound chords that produce no pty bytes leave copy mode unchanged. Chords resolved by config/default keybinds (`cmd+t`, `cmd+c`, etc.) execute normally while copy mode remains active, except that commands which replace the terminal selection or its coordinate space (`select_all`, `clear`, and `clear_scrollback`) exit copy mode before they run; focused-surface changes still exit according to FR-16.
- **FR-11**: Exit copy mode when a resize occurs (discard the selection and cursor; remapping the selection across reflow is out of scope).
- **FR-12**: If mouse-drag selection starts during copy mode, exit copy mode and transition to normal mouse selection.
- **FR-13**: While the alt screen is displayed, disable the direct `shift+Arrow` gesture and pass the key through to the TUI application (do not steal operations such as shift+Arrow from vim). Copy mode may be entered only through the `copy_mode` action (FR-2). Once entered, movement is limited to the visible grid (the alt screen has no scrollback).
- **FR-14**: Render the copy-mode cursor as a hollow block and hide the shell cursor while copy mode is active (prevent duplicate cursors). Do not add a new bind group.
- **FR-15**: Express the default `shift+Arrow` bindings from FR-1 as default bindings in the existing Ghostty-compatible `keybind` config, allowing them to be reassigned or unbound through config (the `copy_mode` action from FR-2 is assignable but has no default chord).
- **FR-16**: Copy mode is bound to the focused surface (pane/tab) at activation time. Exit copy mode when focus switches between tabs, panes, or windows.
- **FR-17**: Copy mode cannot be entered while another overlay (search, command palette, etc.) is displayed (both the `shift+Arrow` gesture and the `copy_mode` action defer to the overlay's normal handling).
- **FR-18**: Whenever copy mode exits (Enter/copy, Esc, passthrough exit, resize, mouse action, or focus switch), return the viewport to the live bottom (offset 0).

## L1 — Non-Functional Requirements

- **NFR-1**: Do not regress existing scrolling (`shift+PageUp/PageDown/Home/End`), mouse selection, or search overlay behavior.
- **NFR-2**: Keep selection, cursor movement, and eviction-tracking logic within the `noa-grid` layer so it can be tested headlessly without a GUI or GPU.
- **NFR-3**: Cover rendering changes with the GPU pipeline tests in `noa-render/tests/pipeline.rs`, avoiding the uniform layout / bind group visibility pitfalls documented in CLAUDE.md.
- **NFR-4**: Gate copy-mode key handling in the priority chain before keybind resolution and treat it in the same manner as existing modals such as `search_prompt`.

## L2 — Details (Wiring Only; No New Architecture)

- **Layer separation (the key to the headless testing strategy)**: Implement copy mode's **pure logic** (cursor movement + x clamping, selection start/extension/clearing, edge-triggered scroll decisions, boundary no-ops, and eviction reindexing) in a **GUI-independent module in `noa-grid`** (for example, a state machine in `noa-grid/src/copy_mode.rs`). The `CopyModeSession` in `noa-app` remains a thin layer that translates winit key events into abstract state-machine commands (`Move(dir, extend)` / `Copy` / `Cancel`). This enables `[headless]` coverage for AC-2/2b/3/18 and similar criteria.
- **event_loop**: Add a copy-mode branch to the key-handling priority chain (`event_loop.rs:494-597`) before keybind resolution. Add a `CopyMode` variant to `ActiveOverlay` and apply the same exclusive control as `search_prompt` (including `active_overlay_gate` and all its call sites; FR-17 entry exclusion is enforced here). For FR-10, first attempt keybind resolution; if no keybind resolves, perform exit-and-passthrough.
- **`CopyModeSession` (new, `noa-app`)**: Store the viewport offset and bound surface (FR-16) at entry, translate key events into abstract commands, and restore the viewport on exit (FR-18).
- **Selection model (reuse)**: Call `SelectionPoint`/`Selection` and the `Terminal::set_selection` family of APIs (`terminal.rs:235-277`) whenever the cursor moves. Do not introduce a new selection data structure.
- **Eviction tracking (FR-7)**: Apply the same pattern as per-frame mouse-selection reindexing (`pointer.rs:29-48` — realign using the delta between the captured and current `selection_rows_evicted()`, clamping on overflow) to the copy-mode cursor and anchor.
- **Viewport locking (FR-6)**: Add an explicit screen-level viewport lock so copy mode can pin the visible logical rows even when it starts at `viewport_offset == 0`. As new rows arrive, adjust the offset as needed to keep those rows visible. Release the lock on exit and return every screen to live view (FR-18).
- **Resize exit (FR-11)**: End `CopyModeSession` from the resize handler. Do not modify reflow (`selection = None` in `reflow.rs:150`); having copy mode exit first avoids silent disappearance.
- **Mouse exit (FR-12)**: At the start of the mouse-drag handler, detect and end `CopyMode`, then fall through to normal mouse-selection startup.
- **Clipboard (FR-8)**: Call `copy_selection_to_clipboard()` directly from the Enter handler. Copied content follows the existing `selected_text()` semantics (join multiple rows with `\n`; concatenate wrapped rows without a newline while preserving trailing spaces; trim trailing spaces from non-wrapped rows; skip `WIDE_SPACER`).
- **Rendering (FR-14)**: Add `copy_cursor: Option<SelectionPoint>` to `FrameSnapshot` (using the same `row_base` conversion pattern as `is_active_search_match`). Pass it to the cell shader as a per-cell instance flag, leaving the bind group layout unchanged. Reuse the existing selection foreground/background rendering mechanism for selection highlighting. Suppress the shell cursor when building the snapshot.
- **Config (FR-2/FR-15)**: Add `copy_mode` to `command_from_keybind_action`. Register `shift+←/↑/→/↓` from FR-1 in the default binding definitions (replacing the `shift+↑/↓` → `ViewportScroll` bindings in `keybind.rs:67-79`). Consolidate line scrolling under the existing `shift+PageUp/PageDown/Home/End` bindings.
- **In-mode keymap**: Fixed (hard-coded). Configurable sequential bindings are a future increment.

### Approved Build Clarifications (2026-07-13)

- **Viewport lock**: Copy mode uses an explicit grid-level viewport lock, including when it starts at offset `0`. Entry preserves an already-scrolled viewport. If the shell cursor is outside that viewport, the copy cursor is clamped to the nearest visible selectable cell.
- **Entry actions**: `copy_mode` is cursor-only. The direct gestures use separate directional actions (`copy_mode:left`, `copy_mode:right`, `copy_mode:up`, and `copy_mode:down`) that enter and immediately extend. Their canonical action names are `copy-mode.left`, `copy-mode.right`, `copy-mode.up`, and `copy-mode.down`.
- **Alt-screen passthrough**: Directional entry actions do not activate on the alt screen; both press and release events continue to the pty. Cursor-only `copy_mode` remains available there.
- **Input priority**: The effective order is modal UI → active copy-mode fixed keys → configured keybind → pty encoding → centralized exit-and-pty passthrough when a non-modifier event produced non-empty bytes. A keybind that opens an overlay or replaces the terminal selection or its coordinate space exits copy mode before the command runs.
- **Exit invariant**: One idempotent exit path clears the selection and copy cursor, unlocks the viewport, and returns it to offset `0`. Resize, mouse interaction, and surface/window focus changes all use this path.
- **Eviction atomicity**: Cursor and anchor eviction repair, clamping, and selection re-application happen while holding one terminal lock.
- **Selection semantics**: The first direct `shift+Arrow` movement creates the existing inclusive two-cell selection (anchor plus destination).
- **Cursor rendering**: The copy cursor is an independent, steady hollow block. Its presence suppresses the shell cursor and reuses the existing cell-instance pipeline and bind groups.
- **Semantic row ends**: Vertical and boundary clamping uses the last non-blank, non-`WIDE_SPACER` cell; an empty row clamps to column `0`, and a row ending in a wide glyph clamps to its lead cell.

## L3 — Acceptance Criteria

- **AC-1 (FR-1)** [manual]: With no selection in normal mode, pressing `shift+→` enters copy mode and creates a selection extending one cell right from the shell cursor position.
- **AC-2 (FR-4)** [headless]: With a selection active in copy mode, pressing `shift+Arrow` in the same direction extends the selection by one cell.
- **AC-2b (FR-5)** [headless]: When the cursor is at a viewport edge and moves farther in the same direction, the viewport automatically scrolls by one row, and the cursor/selection correctly moves or extends across the boundary.
- **AC-3 (FR-3)** [headless]: With a selection active, pressing an unmodified Arrow key clears the selection and moves only the cursor by one cell.
- **AC-4 (FR-10)** [manual]: Pressing a character key (for example, `a`) in copy mode exits, clears the selection, and sends that character to the shell.
- **AC-5 (FR-8)** [headless→manual]: With a multi-row selection (including wrapped rows and trailing spaces), pressing `Enter` copies exactly the text returned by `selected_text()` to the clipboard and exits.
- **AC-6 (FR-8)** [headless]: With no selection, pressing `Enter` exits without writing to the clipboard.
- **AC-7 (FR-9)** [headless]: With a selection active, pressing `Esc` clears only the selection and remains in copy mode; pressing `Esc` again exits.
- **AC-8 (FR-11)** [manual]: Resizing during copy mode exits and discards the selection and cursor.
- **AC-9 (FR-12)** [manual]: Starting a mouse-drag selection during copy mode exits copy mode, and the drag continues as a normal mouse selection.
- **AC-10 (FR-6)** [headless (grid layer)]: When a new row is added while copy mode holds the viewport, the same logical rows remain visible; `viewport_offset` increases as needed to compensate for appended rows (grid-layer verification; end-to-end confirmation of the fixed view is included in the manual checks for AC-15).
- **AC-11 (FR-7)** [headless]: If eviction occurs while the cursor/anchor refers to a scrollback row, it continues to refer to the same logical row; if that row itself is evicted, it is clamped to the oldest surviving row.
- **AC-12 (FR-13)** [manual]: After entering through the `copy_mode` action on the alt screen (for example, while vim is running), the cursor moves only within the visible grid.
- **AC-12b (FR-13)** [manual]: Pressing `shift+→` while the alt screen is displayed does not enter copy mode; the key event passes through to the TUI application (shift+Arrow works in vim).
- **AC-13 (FR-2)** [manual]: Assigning and pressing a chord for `copy_mode` in config enters copy mode with only a cursor and no selection.
- **AC-14 (FR-15)** [manual]: After unbinding or reassigning the default `shift+Arrow` bindings from FR-1, the default behavior does not trigger and only the reassigned chord works.
- **AC-15 (NFR-1)** [manual]: `shift+PageUp/PageDown/Home/End` scrolling, mouse selection, and the search overlay behave exactly as they did before this feature.
- **AC-16 (FR-14/NFR-3)** [headless (pipeline)]: With `FrameSnapshot.copy_cursor = Some(point)`, the GPU pipeline test renders one frame without errors or a new bind group.
- **AC-17 (NFR-2)** [headless]: Copy-mode selection extension, movement, and eviction-tracking logic can be verified without GUI/GPU dependencies using the equivalent of `cargo test -p noa-grid`.
- **AC-18 (NFR-4/FR-4)** [headless]: Given the input sequence Shift press → `→` → Shift release → unmodified `→`, the first input sets the anchor and extends by one cell, and the second clears the selection and only moves the cursor (no implicit re-anchoring caused by press/release timing).
- **AC-19 (FR-18)** [manual]: After navigating backward through scrollback in copy mode, exiting via Enter, Esc, or a character key returns the viewport to the live bottom.
- **AC-20 (FR-3)** [headless]: Moving vertically from a long row to a short row clamps the cursor's x coordinate to the end of the short row; moving back to the long row leaves x at the clamped position (no sticky column).
- **AC-21 (FR-17)** [manual]: Pressing `shift+→` while the search overlay is displayed does not enter copy mode; the key follows the search overlay's normal handling.
- **AC-22 (FR-10)** [manual]: Pressing `cmd+t` in copy mode opens a new tab (normal keybind execution) and does not pass through to the pty. Because focus moved, copy mode has exited according to FR-16.
- **AC-23 (FR-16)** [manual]: Switching tabs during copy mode exits copy mode and clears the selection.
- **AC-24 (FR-14)** [manual]: While copy mode is active, only the hollow-block copy cursor is visible; the shell cursor (filled block) is hidden. The shell cursor returns after exit.
- **AC-25 (FR-5)** [headless]: Moving farther in the same direction while the cursor is at the oldest scrollback row (or the live bottom edge) changes neither the cursor nor the viewport (no-op).

## Scope

**In scope**: Direct shift+Arrow gesture entry / `copy_mode` action entry / Arrow movement + x clamping + automatic edge scrolling + boundary no-op / Shift selection extension (editor semantics) / Enter to copy + return to live view / two-stage Esc / exit-and-passthrough for unbound keys / normal execution of keybind chords / fixed viewport against pty output / eviction tracking + clamping / exit on resize, mouse action, or focus switch / alt-screen guard (gesture passthrough; action entry only) / hollow-block cursor + shell-cursor suppression / overlay exclusion / reassignment and unbinding of default bindings through config.

**Out of scope (v1)**: Word/line motions (`w`/`b`/`e`, `0`/`$`/`V`, etc.) / page or boundary jumps (in-mode keys such as PageUp/`g`/`G`) / search integration (`/`, `n`, `N`) / remain in mode after yank (`y`) / configurable in-mode keys (sequential bindings) / selection remapping across reflow (preserving selection across resize) / rectangular selection / sticky column / mode indicator UI / changes to writing through OSC 52.

## Considered but Rejected

- **A (held-Shift-dependent extension model)**: Modifier races and silent re-anchoring dependent on Shift-release timing → revised and adopted as editor semantics (A′), tying anchor lifetime to the existence of the selection.
- **B (anchor-toggle model using `v`/Space)**: Safest to implement, but its additional key concept is less natural than following editor conventions directly.
- **Hybrid (B + Shift alias)**: Two input paths manipulate the same state, increasing both conceptual complexity and bug surface.
- **C (remain-after-yank model)**: Two copy keys (`y`/Enter) with different exit behavior violate the minimal-design policy.
- **Dedicated activation key (modal entry only)**: The original proposal. Revised at the user's direction to use the direct shift+Arrow gesture (modeless entry).
- **Enable the gesture on the alt screen**: Rejected because it would steal shift+Arrow operations from applications such as vim (entry through the action only is allowed).
- **Keep the viewport in place on exit**: Rejected because characters typed during a passthrough exit would not be visible (returning to live view was adopted).

## Open Questions / Deferred Decisions

- None (all 11 findings from the Spec Quality Gate have been resolved; zero items were parked).
- The order for revisiting future increments (out-of-scope items) will be decided separately at build time: word/line motions → search integration → selection remapping across reflow.

## Build-Path Decision

- Record **apex** as the recommendation (a single supervised run covering design → risk gate → implementation loop → AC verification → ship; it consumes this specification's L3 ACs as its verification contract). Execution requires separate instructions from the user.
- Alternative: orbit loop (long-running, unattended, and designed for interruption/resumption) / feature (guided collaboration).
