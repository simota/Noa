# Floating panes

| | |
|---|---|
| slug | floating-panes |
| status | locked |
| owner | simota |
| reader | The implementer (decides what to build, what not to build, and how acceptance is judged) |
| as-of | 2026-11-10 |
| review trigger | When any of `SplitTree`, the renderer's draw plan, the session schema or `AgentState` changes |
| depth | standard |

## L0: Vision

- Problem: Split panes cut the screen into equal parts, so a terminal that only needs to run an agent on the side or be watched out of the corner of the eye forces the whole layout to be rearranged. The Scratch Terminal can be shown on top, but when it loses focus it is discarded together with its pty, its position is fixed, its size cannot be changed while it runs, and there is only one for the whole app (`docs/specs/scratch-terminal.md` R4/R5, `crates/noa-app/src/app.rs:396`).
- Audience: Users who run several agent CLIs (Claude Code, codex) side by side in noa.
- Job to be done
  - Primary: Run an agent in a small window without changing the split layout, watch its progress, and enlarge it only when needed.
  - Secondary: Leave `htop` or a log running and monitor it out of the corner of the eye.
- Success definition: An agent can be started without changing the split layout at all (AC-001), the agent's wait for approval can be noticed while the float stays collapsed (AC-016), and one operation switches between full-screen display and the original arrangement (AC-013).
- Existing assets & constraints: as-of main `c079459`. Paths start from `crates/`.
  - The Scratch Terminal is implemented as a separate OS window (`noa-app/src/app/scratch_terminal.rs:245`). The only thing that can be reused is the pty creation `spawn_pane_surface` (`noa-app/src/app/lifecycle.rs:1069`).
  - Drawing is one surface per window, and panes are scissor-split (`noa-render/src/renderer/mod.rs:900`, `noa-render/src/draw_plan.rs:74`). There is no mechanism for overdraw or z-order.
  - On macOS, one native tab owns one `WindowState` and one drawing surface (`noa-app/src/app/state.rs:271`). A window-anchored float will be drawn on the drawing surface of the visible tab.
  - There is no per-pane title band. In-window chrome is an AppKit card on macOS (`noa-app/src/macos_overlay/`, display only) and a wgpu card elsewhere (`noa-app/src/app/sidebar/palette.rs`).
  - There is no general drag mechanism. `SplitResizeDrag` (`noa-app/src/split_tree/tree/resize.rs:11`) and `SidebarDrag` exist separately.
  - `PaneId` is unique only within a process and does not span restarts. `MAX_PANES_PER_TAB` and `DIVIDER_HIT_ZONE_PX` are in `noa-app/src/split_tree/tree/types.rs`.
  - The hit test `hit_test` (`noa-app/src/split_tree/tree/hit_test.rs:11`) returns `Pane` / `Divider`.
  - The session schema is `noa-app/src/session.rs` (`SESSION_VERSION = 3`). `parse` returns `None` for a version outside the supported range and starts without a session (`session.rs:331`).
  - Sidebar cards are one per split pane (`session_card_id(window_id, pane_id)`, `noa-app/src/app/sidebar/state.rs:40`).
  - Pane enumeration has two routes: via `window_order` (session, scrollback, sidebar, Overview, AppleScript) and via all `windows` (AX, IPC, timers).
  - Commands are `AppCommand` (`noa-app/src/commands/command.rs:10`); action names are dot-separated and can be reassigned with `keybind =` (`noa-config/src/parser/overrides.rs:520`). An existing test verifies that `cmd+shift+f` is reserved and unassigned (`noa-app/src/commands/tests.rs:170`).
  - The close confirmation when a program is running is `close_confirm_message` in `noa-app/src/app/helpers/lifecycle.rs:30`.
  - Agent state is `AgentState { Running, Permission, Input, Finished, Error }` at `noa-grid/src/osc.rs:370`. In OSC 7501, `state=blocked:kind=permission` becomes `Permission` and `state=blocked` becomes `Input` (`noa-grid/src/osc.rs:475`).

## Glossary

| Term | Definition | Not to be called | Code identifier |
|------|-----------|------------------|-----------------|
| Float | A terminal pane with its own pty, placed on top of the split layout | popup, scratch, overlay | TBD(implementer), OQ-1 |
| Split pane | An existing pane that is a leaf of `SplitTree` | normal pane, underlying pane | `PaneId` / `SplitTree` |
| Anchor | The target a float belongs to, which determines its movable range, display condition and lifetime. Three kinds: pane / tab / window | parent, scope | TBD(implementer), OQ-1 |
| Anchor rect | The pixel rectangle the anchor occupies on screen. Pane = that split pane's rectangle; tab = the whole split area of that tab; window = the whole terminal area (excluding the tab bar and sidebar) | parent area | — |
| Return pane | The split pane in the window's visible tab that last had focus | most recent pane, underlying pane | `WindowState.focused_pane` |
| Title band | The band at the top edge of a float, one cell row high | header, title bar | — |
| Content area | The part of a float other than the title band, where the terminal is drawn | body | — |
| Collapse | The state where a float is only a title band | minimize, hide | — |
| Maximize | The state where a float fills the anchor rect | zoom, full screen | — |
| Hidden | The state of not being drawn because the display condition (L2.3) is not met. The float exists and its pty runs | collapse, discard | — |
| Attention display | The title band's background takes the colour that the sidebar card of a split pane with the same `AgentState` uses when in attention. It is cleared under the same conditions as that card | notification, badge | — |

## L1: Requirements

`T-n` in `Source` points to a row in the "Dialogue record" at the end.

### Functional (`REQ-n`)

| ID | Requirement | Priority | Source |
|----|-------------|----------|--------|
| REQ-001 | `float.new` creates one float anchored to the return pane and moves focus to it (the same when a float has focus). The size of the whole float (including the title band) is "content area 80×24 cells + title band"; any side that exceeds 90% of the matching side of the anchor rect is shrunk to that, and the grid is the largest columns × rows that fit in the content area. The position starts at the point touching the bottom-right corner of the anchor rect and is shifted left and up by "the title band height × the number of floats shown overlapping the anchor rect (regardless of anchor kind)", then clamped. The cwd is inherited from the anchor's split pane | CRITICAL | elicited T-3, T-11 / ratified T-6 (ASSUME-1, 3, 7), T-8 (ASSUME-18) / ASSUME-21, 28 |
| REQ-002 | A float always shows a title band. From the left: anchor kind icon, title (same title rules as split panes), agent state, maximize button, collapse button, close button | CRITICAL | elicited T-3 / T-6 (maximize button) |
| REQ-003 | Dragging the title band moves the float. It is clamped on all four sides to a position where the whole float fits inside the anchor rect. While collapsed, it is clamped by the title band's rectangle | CRITICAL | elicited T-3 / ASSUME-22 |
| REQ-004 | Dragging the four edges and four corners of a float (`DIVIDER_HIT_ZONE_PX` inside and outside) resizes it. The lower limit is a content area of 20×3 cells and the upper limit is the anchor rect. The grid is the largest columns × rows that fit in the content area, and when the grid changes the pty is notified of the window size. A float's hit area (including the outer hit zone) takes priority over dividers and over floats further back. Resizing is not accepted while collapsed or maximized | CRITICAL | elicited T-3 / ratified T-6 (ASSUME-9) / ASSUME-22, 26 |
| REQ-005 | Movable range, display condition, lifetime and following for each anchor kind follow the table in L2.3. While hidden, pty reading and terminal state updates continue | CRITICAL | elicited T-3 |
| REQ-006 | Clicking the anchor kind icon or running `float.cycle-anchor` switches in the order pane → tab → window → pane. On switching, the on-screen position and size are kept, and any part that does not fit in the new anchor rect is shrunk first and then clamped. When switching to pane, the anchor is the split pane under the centre point of the normal-state rectangle (collapsed and maximized floats are also judged by the normal-state rectangle). The switched float becomes the front-most of the new layer. Switching while maximized keeps the maximized state and fills the new anchor rect, and the same "keep the on-screen position, shrink, then clamp" is also applied to the retained normal-state rectangle | CRITICAL | elicited T-3 / ratified T-8 (ASSUME-12) / ASSUME-23, 25 |
| REQ-007 | Position and size are held as ratios to the anchor rect. When the anchor rect changes, they are recomputed from the ratios, the lower limit (REQ-004) is applied, and then they are clamped. This recomputation does not rewrite the held ratios (only the user's move, resize or anchor switch changes the ratios). While the anchor rect cannot hold a float of the lower-limit size, that float is hidden | CRITICAL | elicited T-3 / ratified T-8 (ASSUME-13) / ASSUME-27 |
| REQ-008 | Stacking order is fixed by layer (from top: window anchor, tab anchor, pane anchor). Within the same layer, the float last clicked or focused is front-most. The command palette, confirmation dialogs and Overview all appear above every float | CRITICAL | elicited T-3 |
| REQ-009 | The collapse button, a double-click on the title band, and `float.toggle-collapse` each toggle collapse and expand. On collapse only the title band remains with its top-left fixed, and the width keeps the previous width. A collapsed float receives no key input. Collapsing the float that has focus moves focus to the return pane, and collapsing a float that does not have focus does not move focus. Expanding returns to the state just before collapse (normal or maximized) and gains focus. If the rectangle after expansion does not fit in the anchor rect, it is clamped | CRITICAL | elicited T-5 / ratified T-6 (ASSUME-6), T-8 (ASSUME-14) / ASSUME-22, 24 |
| REQ-010 | The maximize button or `float.toggle-maximize` expands the float to fill the anchor rect. The title band remains, and the content area is the whole area beneath it. Running it again returns to the position and size before maximizing. Move and resize are not accepted while maximized. The maximize operation is not accepted while collapsed | HIGH | elicited T-6 (B1) / ratified T-8 (ASSUME-14) / ASSUME-22 |
| REQ-011 | Clicking a float's content area or title band brings that float to the front of its layer and gives it focus (clicking the title band of a collapsed float only brings it to the front). Clicking outside a float moves focus to the split pane at that position. The cycle targets are "floats that are shown and not collapsed", in creation order. `float.focus-next` / `float.focus-previous` cycle through the targets, and when run from a split pane they move to the first / last target. `float.toggle-focus` moves to the most recently focused target when a split pane has focus (the most recently created one if there is no focus history), and to the return pane when a float has focus. Mouse events (click, wheel, move) that occur in a float's hit area do not reach the split pane underneath | CRITICAL | ratified T-6 (ASSUME-7) / ASSUME-24, 26 |
| REQ-012 | The close button, or `tab.close` while a float has focus, closes only that float. If a program is running, the same confirmation as when closing a split pane is shown. A float whose shell has exited is handled the same as a split pane, including the settings. Focus after closing is the return pane | CRITICAL | ratified T-6 (ASSUME-4) |
| REQ-013 | When an anchor is closed, the floats belonging to it are also closed and their ptys are released. The "number of running programs" in the confirmation for closing a split pane, tab, window or the app includes the floats that will be closed together. Exception: a worker stowed on the board (agent-orchestration D10) is re-attached to its parent if the parent pane remains, and is not counted in the confirmation either | CRITICAL | elicited T-3 / ratified T-8 (ASSUME-20) |
| REQ-014 | The title band shows the `AgentState` of the agent inside the float as the string from `AgentState::label`. For states where `AgentState::needs_attention` is true (`Permission`, `Input`, `Error`), the title band is put in attention display. It does not expand automatically even when collapsed | CRITICAL | elicited T-5, T-6 (A1) / ASSUME-29 |
| REQ-015 | Auto-approve mode targets floats under the same conditions as split panes, and writes the approval input to that float's pty | HIGH | elicited T-5 (scope proposal) → approved at T-6 (C1) |
| REQ-016 | Each float has one sidebar card, marked as a float. Selecting the card and `NextNotification` make that float displayable (bring the window to the front, switch the tab, release the zoom of other split panes, expand the collapse) and focus it. When the anchor rect is below the lower limit and the float stays hidden, focus goes to the anchor's split pane (the return pane for tab and window anchors) | HIGH | elicited T-5 (scope proposal) → approved at T-6 (C1) / ratified T-8 (ASSUME-15) / ASSUME-30 |
| REQ-017 | When `window-save-state` is enabled, each float's ratio-based position and size, collapsed state and cwd are saved and restored at startup. The place a record is stored represents the anchor (a pane anchor is the leaf of that split pane, a tab anchor is the tab, a window anchor is under the window). Each record carries a serial number for creation order within the window and a stacking order, and after restore the creation order (cycle order) and stacking order are the same as before saving. A maximized float is saved with its pre-maximize position and size as the normal state, and a float collapsed from maximized is saved as "collapsed, expands to normal". Focus after restore is on a split pane. The shell is started fresh. Float content is not written even when `scrollback-persist = tail` | HIGH | ratified T-6 (ASSUME-8) / ASSUME-31 |
| REQ-018 | The seven actions in L2.2 are added as `AppCommand` and can be assigned with `keybind =`. The default codes are as in the L2.2 table and do not collide with existing default or reserved codes | HIGH | ratified T-6 (ASSUME-5) / ASSUME-32 |
| REQ-019 | A single anchor can have at most 8 floats (exception: workers stowed on the board are up to 20 per board, agent-orchestration D12). `float.new` on an anchor that has reached the limit creates nothing and tells the user of the limit through an in-window toast. `float.cycle-anchor` whose destination anchor has reached the limit does not change the kind and shows the same toast. Floats are not counted in `MAX_PANES_PER_TAB` and do not affect whether a split pane can be created | HIGH | ratified T-6 (ASSUME-2), T-8 (ASSUME-10) |
| REQ-020 | While a float has focus, every action whose name starts with `split.` does nothing | MEDIUM | ratified T-8 (ASSUME-17) |
| REQ-021 | In Scratch Terminal and Quick Terminal windows, `float.new` does nothing | MEDIUM | ratified T-8 (ASSUME-17) |
| REQ-022 | A float's background does not show through the content of split panes or other floats beneath it, regardless of the window's transparency settings | HIGH | ratified T-8 (ASSUME-19) |
| REQ-023 | Floats do not appear in AppleScript, in AX pane enumeration, or in Overview tiles. They also do not appear in IPC pane enumeration and output. Only floats of workers spawned by the orchestrator (those with `parentPaneId`) are enumerated and readable over IPC (revised 2026-11-17, agent-orchestration D8). The anchor (`paneId`) and the parent (`parentPaneId`) may differ (D10: stowed on the board) | MEDIUM | ratified T-8 (ASSUME-16), revised by the owner 2026-11-17 |
| REQ-024 | When the float that has focus becomes hidden (tab switch, anchor rect below the lower limit), focus moves to the return pane. Focus is not automatically given back when it is shown again | HIGH | ASSUME-24 |

### Cross-functional (`CFR-n`)

| ID | Requirement | Measure / criterion | Target / required outcome | Verified how | Priority | Source |
|----|-------------|---------------------|---------------------------|--------------|----------|--------|
| CFR-001 | Do not break existing tests and static checks | Existing automated tests | `cargo test --workspace` all pass, `cargo clippy --workspace` no warnings | Exit code of the commands | CRITICAL | grounded: `CLAUDE.md` Commands |
| CFR-002 | Collapse and expand do not disturb terminal content | Number of window-size notifications sent to the pty | 0 for a normal ⇄ collapsed round trip | Unit test that counts notifications | CRITICAL | elicited T-5 |
| CFR-003 | No drawing cost is paid for hidden floats | Number of floats included in one frame's draw plan | 0 for hidden floats | Unit test that inspects the draw plan | HIGH | ASSUME-33 |
| CFR-004 | Same behaviour on macOS and Linux | The must-have ACs of L3 | All PASS on both OSes | Run the acceptance test spec on each OS | HIGH | elicited T-5 (scope proposal) → approved at T-6 (C1) |

## L2: Detail

### L2.1 Data model

| Entity | Field | Type | Constraints | Default | Nullable | Owned by |
|--------|-------|------|-------------|---------|----------|----------|
| Float | id | `PaneId` | Unique within the process | `PaneId::alloc()` | no | noa-app |
| Float | anchor | pane (`PaneId`) \| tab \| window | For pane, an existing split pane | pane | no | noa-app |
| Float | origin | (x, y) ratio | Finite, each 0.0–1.0. From the anchor rect's top-left to the float's top-left | REQ-001 | no | noa-app |
| Float | size | (w, h) ratio | Finite, each greater than 0.0 and at most 1.0. The size in the normal state, including the title band | REQ-001 | no | noa-app |
| Float | state | normal \| collapsed \| maximized | — | normal | no | noa-app |
| Float | state_before_collapse | normal \| maximized | Meaningful only when state = collapsed | normal | no | noa-app |
| Float | z | Monotonically increasing integer | Larger is nearer the front within the same layer. Renumbered each time it is brought to the front | Numbered at creation | no | noa-app |
| Session | float record | origin, size, collapsed or not, cwd, stacking order | REQ-017. The place stored represents the anchor | — | — | `noa-app/src/session.rs` |

### L2.2 Interface contract

| Operation | Trigger | Input | Output | Errors | Idempotent | Auth |
|-----------|---------|-------|--------|--------|------------|------|
| `float.new` | Default macOS `cmd+shift+y` / Linux `ctrl+shift+y` | — | REQ-001 | Limit (REQ-019), pty creation failure (Behavior matrix), excluded window (REQ-021) | no | N/A (local operation) |
| `float.toggle-focus` | Default macOS `cmd+shift+i` / Linux `ctrl+shift+i` | — | REQ-011 | No cycle target → does nothing | no | Same |
| `float.focus-next` | No default | — | REQ-011 | Same | no | Same |
| `float.focus-previous` | No default | — | REQ-011 | Same | no | Same |
| `float.toggle-collapse` | No default, collapse button, band double-click | For the button and the band, that float. For the action, the focused float, or if none, the visible float last focused in that window | REQ-009 | No target → does nothing | no | Same |
| `float.toggle-maximize` | No default, maximize button | Same as `float.toggle-collapse` | REQ-010 | No target, or collapsed → does nothing | no | Same |
| `float.cycle-anchor` | No default, anchor kind icon | Same as `float.toggle-collapse` (read "button" as "icon") | REQ-006 | No target → does nothing. Destination at limit → REQ-019 | no | Same |

### L2.3 State & transitions

Behaviour per anchor kind (REQ-005):

| Anchor | Anchor rect | Display condition | Event that closes the float | Anchor movement |
|---------|-------------|---------|--------------------|---------------|
| Pane | That split pane's rectangle | The split pane is drawn on screen (the tab is visible and no other split pane is zoomed) | That split pane closes | When the split pane moves to another tab or window, the float moves with it, keeping its ratio |
| Tab | The whole split area of that tab | The tab is visible | The tab closes | When the tab moves to another window, the float moves with it |
| Window | The whole terminal area | Always (on top of the visible tab) | The window's last tab closes | Does not move |

Every kind is shown only when, in addition, the lower-limit condition of REQ-007 is met.

Display state transitions:

| From | Event | To | Guard | Side effect |
|------|-------|----|-------|-------------|
| normal | collapse operation | collapsed | — | state_before_collapse = normal. No pty size notification. Focus per REQ-009 |
| maximized | collapse operation | collapsed | — | state_before_collapse = maximized. Same as above |
| collapsed | expand operation | state_before_collapse | — | Focus goes to the float. No pty size notification |
| normal | maximize operation | maximized | — | origin and size are kept as they are. Recompute the grid and notify the pty |
| maximized | maximize operation | normal | — | Drawn with the retained origin and size. Recompute the grid and notify the pty |
| collapsed | maximize operation | collapsed | — | Does nothing |
| any | anchor switch | same state | Destination is under the limit | REQ-006 |
| any | close operation, discard by shell exit, anchor disappearance | (discarded) | Confirm if a program is running (REQ-012, REQ-013) | Abort any drag in progress |

### L2.4 Validation rules

| Field | Rule | On violation | Message source |
|-------|------|--------------|----------------|
| Session float record origin / size | The constraints of L2.1 | Only that float is not restored. The rest are restored | Log only |
| `float.*` in `keybind =` | Existing keybind validation | Existing diagnostic | Existing config diagnostics |

## Behavior matrix

| Condition | Applies to | Expected behavior | User-visible | Verifies |
|-----------|-----------|-------------------|--------------|----------|
| Empty state (0 floats) | REQ-018 | `float.*` other than `float.new` do nothing | No change | AC-023 |
| Invalid input (corrupt session record) | REQ-017 | Discard only the matching float, per L2.4 | That float does not appear | AC-021 |
| Unauthorized | — | N/A (a local single-user operation; there is no permission boundary) | — | — |
| Dependency unavailable (pty creation failure) | REQ-001 | Create no float, do not change focus, write the error to the log | No change | AC-024 |
| Concurrent modification (the float is discarded during a drag: anchor disappears or shell exits) | REQ-012, REQ-013 | Abort the drag | The float disappears | AC-025 |
| Concurrent modification (the anchor rect changes during a drag) | REQ-007 | Continue the drag and clamp to the new anchor rect | The float stays inside the anchor | AC-009 |
| Limit exceeded | REQ-019 | Do not create or switch; show a toast | Toast | AC-022 |
| A TUI redraws on collapse / expand | CFR-002 | Send no pty size notification | The screen is not disturbed | AC-012 |
| Native tab switch (macOS) | REQ-005 | A window-anchored float appears at the same position on top of the destination tab | The position does not change | AC-006 |
| Font size change | REQ-007 | Do not change the float's pixel rectangle; recompute only the grid and notify the pty | Only the character size changes | AC-044 |
| Anchor rect is below the lower limit | REQ-007 | Hidden. Shown again when it widens | The float disappears temporarily | AC-010 |
| The focused float becomes hidden | REQ-024 | Focus goes to the return pane | Keystrokes go into the split pane | AC-038 |

## Dependencies & integration points

| Dependency | Kind | We control? | Contract / version | Failure mode | Fallback |
|------------|------|-------------|--------------------|--------------|----------|
| `spawn_pane_surface` | Internal | yes | main `c079459` | pty creation failure | Behavior matrix |
| Renderer's draw plan | Internal | yes | Same. Overdraw is new | A wgpu validation error aborts at runtime | Detect by adding an overdraw frame to `noa-render/tests/pipeline.rs` |
| Session file | Internal | yes | `SESSION_VERSION` | Corrupt record | L2.4 |

## Migration & rollout

- Existing data: Backward compatible. `SESSION_VERSION` is raised to 4, and a version-3 file is read as 0 floats (AC-021).
- Staged release: No flag. Unless a float is created, existing screens and operations do not change.
- Rollback: A build from before this feature treats a version-4 file as no session, per existing behaviour (`session.rs:331`). Only the restore of window layout is lost; there is no irreversible change.

## L3: Acceptance Criteria

Common premise F-1: the tab's split area is 1000×600 px (origin at top-left), cells are 10×20 px, and the title band is 20 px. In an AC that does not mention A or B, there is one split pane, and it is the anchor rect. In an AC that mentions A and B, the area is split left and right in two, with A at x=0–500 and B at x=500–1000. All coordinates are px within the split area. The anchor rect of tab and window anchors is the same 1000×600 px as this split area.

| ID | Given | When | Then | Verifies | Mode | Oracle | Must-have |
|----|-------|------|------|----------|------|--------|-----------|
| AC-001 | F-1 (the anchor is the split pane of one tab; another tab has 2 split panes), 0 floats | `float.new` | The rectangles of all split panes are unchanged. The float's rectangle is (200,100)-(1000,600), grid 80×24, has focus, cwd is the same as the anchor's | REQ-001 | machine | assert in a noa-app unit test | yes |
| AC-002 | Right after AC-001 | `float.new` twice on the same anchor | The 2nd is (180,80)-(980,580), the 3rd is (160,60)-(960,560). Separately, after switching the 1st to a tab anchor and running `float.new`, the new float is (180,80)-(980,580) (not hidden behind the 1st). With an A/B split and a float only above B, `float.new` on A does not shift | REQ-001 | machine | Same | yes |
| AC-003 | Anchor rect 600×300 px | `float.new` | The float is 540×270 px, grid 54×12 | REQ-001 | machine | Same | yes |
| AC-004 | One float (normal) | Look at the screen | The 6 elements of REQ-002 are visible in the title band in that order | REQ-002 | human | The implementer checks the screenshot against the list in REQ-002, and FAILs if any is missing or out of order | yes |
| AC-005 | F-1, a 400×300 px float at (100,100) | Drag the title band by (+900,+900) px. Separately, from the same initial state, drag by (−900,−900) px | The former's top-left is (600,300), the latter's is (0,0) | REQ-003 | machine | assert in a unit test | yes |
| AC-006 | 2 tabs, one float of each of the 3 anchor kinds, the pane anchor's split pane is in tab 1 | Switch to tab 2 and deliver the 1 byte `x` to the pty of each of the 2 hidden floats | Only the window-anchored float is shown, at the same position as before switching. The character at the cursor position of both hidden floats' terminals is `x` | REQ-005 | machine | assert in unit tests of the display decision and the terminal state | yes |
| AC-007 | F-1, a pane-anchored float on A | Zoom B | The float is hidden. Releasing the zoom shows it at its original position | REQ-005 | machine | Same | yes |
| AC-008 | F-1, a 300×200 px float anchored to pane (A) at (100,100) | `float.cycle-anchor` → drag the title band by (+500,0) → `float.cycle-anchor` → click the anchor kind icon | The kinds are, in order, tab, window, pane (B). At each switch, the on-screen rectangle is unchanged. Right after each switch, the float is front-most in the new layer | REQ-006 | machine | assert in a unit test | yes |
| AC-009 | F-1, a float with origin (0.1,0.1) size (0.2,0.2), the title band being dragged | Make the anchor rect 500×300 px, then return it to 1000×600 px | When shrunk, 200×80 px from (50,30) (the lower limit is applied). Back again, 200×120 px from (100,60). The drag state continues | REQ-007 | machine | Same | yes |
| AC-010 | One float | Make the anchor rect 150×60 px, then return it to 1000×600 px | While shrunk it is hidden and not discarded. Returned, it is shown at its original ratio | REQ-007 | machine | Same | yes |
| AC-011 | Overlapping floats created in the order P1, P2 (pane), T1 (tab), W1 (window). Focus is on a split pane | Click P1. Then `float.focus-next` | After the click, from the front: W1, T1, P1, P2. After `float.focus-next`: W1, T1, P2, P1 | REQ-008 | machine | assert in a draw-order unit test | yes |
| AC-012 | F-1, a normal float with focus and a grid of 80×24 | Collapse and expand it. Do this with each of the collapse button, a double-click on the title band, and `float.toggle-collapse` | By every method: the collapsed rectangle has unchanged width, a height of 20 px and an unchanged top-left, and focus is on the return pane. After expanding, focus returns to the float. A round trip sends 0 pty size notifications and the grid stays 80×24. Separately, pressing a float's collapse button while focus is on a split pane leaves focus on the split pane | REQ-009, CFR-002 | machine | assert in a unit test | yes |
| AC-013 | F-1, a float at (200,100) 400×300 px | `float.toggle-maximize`, then the maximize button | The 1st gives (0,0) 1000×600 px, grid 100×29, title band present, 1 pty size notification. The 2nd gives (200,100) 400×300 px | REQ-010 | machine | Same | yes |
| AC-014 | A maximized float | (a) collapse and expand (b) drag the title band by (+50,+50) (c) drag the right edge by (−50,0) (d) collapse a normal-state float, press the maximize button, run `float.toggle-maximize`, then expand | (a) returns to maximized (b)(c) rectangle unchanged (d) stays collapsed, and on expanding is in the normal state | REQ-009, REQ-010 | machine | Same | yes |
| AC-015 | F-1, create pane-anchored floats F1, F2, F3 on B in order and collapse F3 ("click B" is at a position not covered by a float). Click F1, then click split pane A | In order: `float.toggle-focus`, `float.focus-next`, `float.focus-next`, `float.focus-previous`, `float.toggle-focus`, click B, `float.focus-next`, click B, `float.focus-previous`, click B, click F1's content area, wheel and mouse move over F1 | Focus is, in order, F1, F2, F1, F2, A, B, F1, B, F2, B, F1. The click, wheel and mouse move on F1 do not reach B, and the wheel scrolls F1 | REQ-011 | machine | Same | yes |
| AC-016 | The agent inside a collapsed float becomes `Permission`. Separately, `Input` and `Error` | Look at the screen | In every state the float stays collapsed. The title band is in attention display, and the state labels are `approval needed`, `reply needed` and `error` respectively | REQ-014 | human | For each state, the implementer checks 3 points on a screenshot of the collapsed state (stays collapsed, the band's background colour matches the sidebar card's attention colour, the label string), and FAILs if even one is missing | yes |
| AC-017 | A float with a running program, and its split pane with a running program | (a) the float's close button (b) focus the float and `tab.close` (c) close the split pane | (a)(b) a confirmation appears, and on approval only the float closes and focus goes to the return pane (c) a confirmation with 2 running programs appears, and on approval both close | REQ-012, REQ-013 | machine | assert in a unit test | yes |
| AC-018 | Auto-approve mode is enabled, and the same approval-wait screen is prepared in a split pane and a float | Auto-approve runs | The decision results are equal, and the approval input bytes are written to the float's pty | REQ-015 | machine | assert in a unit test of `auto_approve` | yes |
| AC-019 | Tab 1 has a collapsed float (in attention), tab 2 is displayed, the sidebar is shown | Select that float's card. Reset to the initial state and run `NextNotification` | The card carries the float mark. Either operation switches to tab 1, and the float is expanded and has focus | REQ-016 | human | The implementer checks each operation on screen (mark, tab, expanded, keystrokes go into the float), and FAILs if even one is missing | yes |
| AC-020 | Create one float of each of the 3 anchor kinds in the order pane (B), tab, window (the tab anchor is maximized from (300,200) 300×200 px and then collapsed; the window anchor is maximized from (200,100) 400×300 px), `window-save-state` enabled, `scrollback-persist = tail` | Save and read back | For all 3, origin, size, cwd and stacking order match those before saving. The pane anchor attaches to the split pane at the same position as B in the restored tree. The tab anchor is restored collapsed, and when expanded is in the normal state at (300,200) 300×200 px. The window anchor is in the normal state at (200,100) 400×300 px. Focus is on a split pane. `float.toggle-focus` right after restore moves to the window-anchored float (the most recently created target). There is no float file in the scrollback directory | REQ-017 | machine | assert in a session round-trip unit test | yes |
| AC-021 | (a) a file where one of 2 float records has origin `NaN` (b) a version-3 file (c) 1 float with `window-save-state` disabled | (a)(b) read it (c) run the save process | (a) only the 1 valid record is restored (b) reads normally with 0 floats (c) no float is written. The version of the file written is 4 | REQ-017 | machine | Same | yes |
| AC-022 | (a) 8 floats on the same split pane (b) a tab with 8 tab-anchored floats and 1 pane-anchored float (c) a tab with 8 floats and 8 split panes | (a) `float.new` (b) `float.cycle-anchor` on the pane-anchored float (c) add one split pane | (a) a 9th is not created and a toast is shown (b) the kind stays pane and a toast is shown (c) there are 9 split panes | REQ-019 | machine | assert in a unit test | yes |
| AC-023 | 0 floats | Run the 6 actions other than `float.new` in turn | Neither state nor focus changes | REQ-018 | machine | Same | yes |
| AC-024 | pty creation fails | `float.new` | The float count does not increase, focus does not change, and 1 error log line is written | REQ-001 | machine | Same | yes |
| AC-025 | A float is being dragged | (a) close the anchor's split pane (b) the float is discarded by its shell exiting | In both, the drag state is cleared and the float disappears | REQ-012, REQ-013 | machine | Same | yes |
| AC-026 | F-1, a float at (300,100) 400×300 px | (a) bottom-right corner by (−2000,−2000) (b) bottom-right corner by (+2000,+2000) (c) left edge by (−50,0) (d) each of the 8 handles outward by 10 px (e) with an A/B split, press 3 px outside the left edge (x=497) of a tab-anchored float at (500,100) 400×300 px and drag by (−50,0) (f) with floats F1 (back) and F2 (front) on the same split pane and focus on the split pane, press a point of F2's content area that overlaps F1's right-edge hit zone and drag by (+50,0) | (a) content area 200×60 px (b) the bottom-right stops at the anchor's bottom-right (1000,600) (c) width 450 px, columns +5, the last pty size notification is the new grid (d) only the matching edge moves, by 10 px (e) float width 450 px, the divider stays at x=500 (f) F1's rectangle is unchanged, and F2 gains focus | REQ-004 | machine | Same | yes |
| AC-027 | (a) default keybinds (b) `keybind =` that assigns an unused code to each of the 7 actions | Build the engine and, in (b), look up each code | (a) the defaults are only the 2 actions in L2.2, do not collide with existing defaults, and `cmd+shift+f` stays unassigned (b) all 7 resolve to the matching `AppCommand` | REQ-018 | machine | assert in a `keybind` unit test | yes |
| AC-028 | A float has focus | Run, in turn, every action whose name starts with `split.` | The split tree, zoom state and focus all stay unchanged | REQ-020 | machine | assert in a unit test (enumerated mechanically from the action list) | yes |
| AC-029 | (a) a Scratch Terminal (b) a Quick Terminal window has focus | `float.new` | In neither is a float added | REQ-021 | machine | Same | yes |
| AC-030 | `background-opacity = 0.5`, a float above a split pane filled with text | Look at the screen | The text of the split pane underneath cannot be seen inside the float's rectangle | REQ-022 | human | The implementer magnifies the screenshot, and FAILs if the text underneath is legible | yes |
| AC-031 | One float (not a worker) | Generate the AppleScript, IPC and AX snapshots and the Overview layout | None of them contains the float's `PaneId`. A worker's float appears only in IPC | REQ-023 | machine | assert in unit tests that generate each snapshot | yes |
| AC-032 | A build that includes this feature | at build | `cargo test --workspace` all pass, `cargo clippy --workspace` no warnings | CFR-001 | machine | Exit code of the commands | yes |
| AC-033 | (a) a float in another tab (b) a pane-anchored float whose pane has another split pane zoomed (c) a float whose anchor rect is below the lower limit | Build the draw plan | In every case the plan does not include that float | CFR-003 | machine | assert in a draw-plan unit test | yes |
| AC-034 | macOS and Linux builds | Run the acceptance test spec | The must-have TCs all PASS on both | CFR-004 | human | The 2 execution-record rows in §9 of the acceptance test spec | yes |
| AC-035 | A float is in the centre of the screen | Open each of the command palette, the close confirmation dialog and Overview | In all three, the part that overlaps the float is visible without being cut off | REQ-008 | human | On 3 screenshots, the implementer FAILs if even one pixel of the float lies on top of any overlay | yes |
| AC-036 | A float and a split pane, default settings | Run `exit` in each | The float's result (closing / display after exit) is the same as the split pane's result | REQ-012 | machine | assert in a unit test | yes |
| AC-037 | (a) a split pane that has a pane-anchored float (b) a tab that has a tab-anchored float (c) a window with 2 tabs. Tab 1 has 1 running split pane and 1 running tab-anchored float, the window has 1 running window-anchored float, and tab 2 has no running program | (a) move the split pane to another tab (b) move the tab to another window (c) three cases separately: close tab 1 / close the window / quit the app | (a) the float attaches to the same split pane at the destination, with the same ratio (b) the float moves with the tab (c) the running-program counts in the confirmation are 2, 3 and 3 respectively. After approval, the floats in the closed scope are discarded and their ptys are released | REQ-005, REQ-013 | machine | Same | yes |
| AC-038 | (a) the float of split pane A in tab 1 has focus, and tab 2 has split pane C (b) the float of split pane A has focus | (a) switch to tab 2 and return to tab 1 (b) make the anchor rect 150×60 px and return it to 1000×600 px | (a) in tab 2, C has focus. On returning, focus is on A, not on the float (b) when shrunk, focus moves to A, and after restoring, focus stays on A | REQ-024 | machine | Same | yes |
| AC-039 | F-1, collapse the float at (500,250) 400×300 px | (a) drag the title band by (+900,+900) (b) drag the right edge at that position (c) `float.cycle-anchor` (the target is this float, last focused) (d) expand | (a) the band's top-left is (600,580) (b) rectangle unchanged (c) the kind becomes tab (d) the rectangle after expanding is clamped into the anchor, top-left (600,300) | REQ-003, REQ-004, REQ-009 | machine | Same | yes |
| AC-040 | F-1, a window-anchored float that is maximized (normal-state (0,100) 800×500 px, centre (400,350) is over A) | `float.cycle-anchor` (→ pane A), then `float.toggle-maximize` | After switching it fills A at (0,0) 500×600 px and stays maximized (the anchor is decided by the centre of the normal-state rectangle). After unmaximizing, (0,100) 500×500 px | REQ-006 | machine | Same | yes |
| AC-041 | F-1, A's float has focus (the return pane is A) | `float.new` | The new float's anchor is A | REQ-001 | machine | Same | yes |
| AC-042 | Float F1 (back), F2 (front, collapsed), focus is on a split pane | Click F1's title band, then click F2's title band | The 1st brings F1 to the front and gives it focus. The 2nd brings F2 to the front, but focus stays on F1 | REQ-011 | machine | Same | yes |
| AC-043 | (a) a pane-anchored float of A while B is zoomed (b) a float in another window (c) a pane-anchored float whose anchor rect is below the lower limit | Select each one's sidebar card | (a) the zoom is released and the float gets focus (b) that window comes to the front and the float gets focus (c) the anchor's split pane gets focus | REQ-016 | machine | Same | yes |
| AC-044 | F-1, a float at (200,100) 400×300 px, grid 40×14 | Change the font size so cells are 20×40 px | The float's rectangle is unchanged. The title band becomes 40 px, the grid is 20×6, and the last pty size notification is 20×6 | REQ-007 | machine | Same | yes |

## Scope

- In scope
  - Create, close, move, resize, anchor switch, z-order, collapse, maximize (elicited T-3, T-5, T-6)
  - Focus movement and key bindings (ratified T-6)
  - Agent state and attention in the title band, making auto-approve target floats (elicited T-4 to T-6)
  - Listing in the sidebar (ratified T-6)
  - Session save and restore (ratified T-6)
  - macOS and Linux (ratified T-6)
  - Per-anchor count limit, no creation in Scratch / Quick Terminal, opaque background (ratified T-8)
- Out of scope
  - Splitting inside a float (ratified T-6)
  - Converting between floats and split panes (ratified T-6)
  - Detaching outside the window (ratified T-6)
  - Snapping and alignment (ratified T-6)
  - Move and resize from the keyboard (ratified T-6)
  - Persisting scrollback (ratified T-6)
  - Enumeration, creation and operation from AppleScript, IPC and AX (ratified T-6, T-8)
  - Tile display in Overview (ratified T-6)
  - Config keys that change the default size or limit (ratified T-8)

## Considered but rejected

Chosen direction: one kind of float has an anchor attribute (pane / tab / window) that can be switched after creation (elicited T-2, T-3).

- Build three separate features, one per anchor kind. The user wants all three, and one attribute can express them. Operation and implementation would be tripled.
- Fix the anchor to a single kind. The user asked for all three (T-2).
- Extend the Scratch Terminal. It is implemented as a separate OS window and can neither clip inside a pane nor follow a tab.
- Substitute Quick Terminal with `quick-terminal-autohide = false`. The pty stays alive when hidden, but there is only one, it is placed relative to the screen, it cannot be resized by dragging, and it is outside the sidebar and auto-approve (`crates/noa-app/src/app/sidebar/state.rs:399`).
- Make collapse an alignment to the anchor's bottom edge, or hide completely. The user chose folding into a band in place (T-5).
- Auto-expand on waiting for input. Rejected because the screen being worked on would move (T-6: A1).

## Assumption Ledger

For the defaults of ASSUME-1 to 20, the requirement named in the second column from the right of the table is the source of truth.

| ID | Assumption | Default chosen | Why | Status |
|----|-----------|----------------|-----|--------|
| ASSUME-1 to 9 | Initial size and position, relation to the pane limit, cwd, shell exit, scope of key operations, collapsed form, focus model, what is restored, minimum size | REQ-001, 004, 009, 011, 012, 017, 019 | Presented at T-5, T-6 | ratified (T-6) |
| ASSUME-10 | Limit value, how to tell on excess, no config key | REQ-019, Scope | 1 float = 1 pty, so it is not unlimited | ratified (T-8) |
| ASSUME-11 | Default codes | Replaced by ASSUME-32 | — | superseded (ASSUME-32) |
| ASSUME-12 to 20 | Order of anchor switching, hiding below the lower limit, details of maximize, jump from the sidebar, not exposed on external surfaces, scenes where unusable, creating a new one from a float, background, summing in the close confirmation | REQ-006, 007, 009, 010, 013, 016, 020 to 023 | Presented at T-7 | ratified (T-8) |
| ASSUME-21 | Meaning of "the most recent split pane" | The return pane (Glossary) | Matches the existing `focused_pane` and is unique even for window anchors | ratified (T-10) |
| ASSUME-22 | What can be done while collapsed, how the target is chosen | Move and anchor switch are allowed; resize and maximize are not. Clamp on expand if it does not fit. The collapse, maximize and anchor switch actions target the last focused visible float if there is no focused float | Limited to operations that mean something in the band-only state | ratified (T-10) |
| ASSUME-23 | Anchor switch while maximized | Keep maximized and fill the new anchor | Keeps the visible state | ratified (T-10) |
| ASSUME-24 | Focus details | On becoming hidden, go to the return pane; do not go back when shown again. Collapsing a float without focus does not move focus. An expanded float gains focus. Cycling is in creation order, and from a split pane goes to the first / last. With no history, go to the most recently created one | The destination of keystrokes does not change on its own | ratified (T-10) |
| ASSUME-25 | Stacking after anchor switch | Front-most of the new layer | The operated target is not hidden | ratified (T-10) |
| ASSUME-26 | Click and hit priority | A title band click is bring-to-front + focus (bring-to-front only while collapsed). A float's hit area takes priority over dividers and floats behind | What looks nearest responds | ratified (T-10) |
| ASSUME-27 | Ratios when the anchor changes | Recomputation does not rewrite the ratios | Restoring the anchor restores the original arrangement | ratified (T-10) |
| ASSUME-28 | Details of initial size | 90% applies to the whole including the title band. The shift is decided by the number of visible floats overlapping the anchor rect (changed at T-11 from "the number of existing floats of the same anchor") | A new float is not hidden behind a float in an upper layer | ratified (T-10) |
| ASSUME-29 | Definition of attention display, target states, label | The definition in the Glossary. The targets are `AgentState::needs_attention` (including `Error`). The label is `AgentState::label` | Matches the look of the existing sidebar | ratified (T-10) |
| ASSUME-30 | Details of jumping from the sidebar | Brings the window to the front, releases zoom and expands. If hidden below the lower limit, go to the anchor's split pane | It is an explicit operation, so it takes things to a visible state | ratified (T-10) |
| ASSUME-31 | Session details | The place stored represents the anchor. Save the creation-order serial number and stacking order; do not save focus. Maximized is saved with the pre-maximize rectangle. Raise `SESSION_VERSION` to 4 | `PaneId` does not span restarts. Old builds can treat a new file as no session | ratified (T-10) |
| ASSUME-32 | Default codes (replacing ASSUME-11) | `float.new` = macOS `cmd+shift+y` / Linux `ctrl+shift+y`, `float.toggle-focus` = `cmd+shift+i` / `ctrl+shift+i`. The remaining 5 have no default | An existing test verifies `cmd+shift+f` as reserved. The same characters on both OSes. Ghostty's GTK version uses `ctrl+shift+i` for the inspector, but noa has no inspector, so there is no collision | ratified (T-10) |
| ASSUME-33 | Hidden floats are not put in the draw plan | CFR-003 | No GPU time is spent on a hidden terminal | ratified (T-10) |

## Open Questions / Deferred Decisions

| ID | Question | Blocks | Owner | Resolve by | Impact if wrong |
|----|----------|--------|-------|-----------|-----------------|
| OQ-1 | Type names for the float and the anchor | nothing (naming only; does not affect behaviour or acceptance criteria) | Implementer | before-ship | Terminology drift |
| OQ-2 | File paths of the automated tests that implement the machine-mode TCs | nothing (what is judged is fixed by each AC and TC; only the place of execution is bound) | Implementer | before-ship | Tests cannot be traced from the execution record |
| OQ-3 | The concrete configuration of the Linux environment where acceptance is run | nothing (the required capabilities are fixed in §3 of the acceptance test spec) | simota | before-ship | Linux results cannot be reproduced |

Refutation record:
- The verdict on "is the problem real (basis: the repository's existing features)" is `survived`. None of Scratch Terminal, Quick Terminal, zoom or the sidebar meets the primary use. Two statements in L0 were made accurate, and Quick Terminal was added to the rejected options.
- The verdict on "do the ACs prove the requirements (basis: the spec only)" is `refuted-on-evidence`. More than 20 must-have requirement clauses were not touched by any AC, and it was sent back to SPECIFY. AC-005, 006, 008, 009, 011 to 022, 024 to 029 and 033 were strengthened, and AC-035 to 044 were added. Independent re-verification was done 3 times; the findings up to the 2nd (unreachable preconditions, how targets are chosen, numeric values, unverified clauses) were fixed, and the 3rd had nothing left (`GATE: PASS`).

## Build-path decision

Batch autonomous implementation (elicited T-8). The scope fits in one feature and there are 6 visual acceptance criteria, so design, implementation and verification proceed in continuous work that the user can attend. The L3 must-have ACs are the ship decision.

## Dialogue record

| ID | Gist of the user's statement |
|----|-------------------|
| T-1 | Wants floating terminal panes that stay resident above each pane, at any size and position |
| T-2 | Wants all three belonging scopes: pane, tab and window |
| T-3 | Approved the proposal to unify them into one anchor attribute (default pane, multiple floats with layered stacking, title band always shown, ratio retention, separate from Scratch Terminal). Added collapse |
| T-4 | Primary use is running agents side by side, secondary use is monitoring |
| T-5 | Chose collapse as "fold into only a band in place" (the scope proposal and ASSUME-1 to 5 were presented) |
| T-6 | A1 (do not auto-expand even when waiting for input), B1 (have a maximize toggle), C1 (approved the scope proposal and ASSUME-1 to 9) |
| T-7 | (Presented ASSUME-10 to 20 and options for the implementation method) |
| T-8 | D1 (approved ASSUME-10 to 20), E1 (batch autonomous implementation) |
| T-9 | (Presented the change of default keys and ASSUME-21 to 33. Additionally presented: including `Error` in attention, how an action chooses its target, saving creation order) |
| T-10 | F1 (approved everything in T-9), approved the lock |
| T-11 | Revised after checking on a real machine post-implementation: the initial position of a new float shifts for all visible floats |
