# Spec: Command Palette (command-palette)

> **Historical baseline:** Source paths and line numbers in FRAME/L2 record
> the pre-implementation layout. The current implementation is split across
> `command_palette.rs`, `app/commands.rs`, `app/input_ops/overlays.rs`, and
> `noa-render/src/renderer/overlay.rs`; prefer those symbols for navigation.

## Metadata

- slug: `command-palette`
- title: Command Palette (Command Palette)
- status: `locked` (Accord+Scribe spec authored 2026-07-04 / apex Phase 1-4 compressed)
- owner: simota
- parity: **REQ-MACOS-002 / IMPL-MACOS-002** (`docs/roadmaps/ghostty-parity-roadmap.md`) — "Add command palette backed by the action registry"
- recipe: apex (Discovery→Ideate→Verdict→Spec compressed). scope mode = **Standard** (17 requirements = 12 functional + 5 NFR, new enum variant + new modal session + renderer overlay + filter logic).
- build-path: TBD (selected via orbit loop / titan etc. after LOCK, on instruction). **This spec does not write code.**

## L0 — Vision

1. **Problem**: All of noa's app commands (`AppCommand`) can currently only be invoked via the macOS menu and keybindings. Users who don't remember the keybinding, or find navigating the menu too slow, can't quickly reach the operation they want to perform (split, font, scroll, etc.). Ghostty shipped a command palette in 1.1.0, and users switching over expect this search-and-execute surface.
2. **Target**: Users migrating from Ghostty and keyboard-driven power users.
3. **Job-to-be-done**: Open the palette with a single keystroke, narrow it down by typing a few characters of the command name, and execute it with one Enter press. See the list of executable commands and (if bound) their keybinding at a glance.
4. **Success criteria**: `cmd+shift+p` opens the palette, fuzzy (subsequence) matching against the title narrows the `AppCommand` list, Enter executes the highlighted command and closes the palette, and Esc closes it without executing. The palette's session state does not leak across tab/window closure (must not repeat the search_prompt's initial leak).
5. **Constraints**: Keyboard-driven (no mouse interaction in v1) / reuse search_prompt's overlay mechanism (grid-aligned modal, incremental, rendered via `FrameSnapshot`) / no new heavyweight dependencies · pure Rust (no new FFI) / macOS-first.
6. **Action registry**: What the palette exposes is the existing `AppCommand` enum (`crates/noa-app/src/commands.rs`) itself. This is what the roadmap calls the "action registry."

### Parity Fidelity Notes

- Ghostty's palette enumerates a large set of actions (config-reload, inspector, goto_tab:N, etc.). noa's palette enumerates **only the `AppCommand`s noa has implemented**. This is not a "deviation from Ghostty behavior" but simply that "noa's set of implemented actions is smaller than Ghostty's"; the observable behavior (open/filter/execute/close) is faithfully reproduced.
- **Explicit parity exceptions (deliberate scope differences)**: (a) Ghostty's `command_palette_entry` (user-defined custom entries) is **CUT** for v1. (b) `SelectTab(1..9)` (Go to Tab N) is **CUT** from the palette display as 9 redundant entries (kept in the title registry, directly reachable via `cmd+1..9`). Both are recorded here in L0.
- **Fidelity gap**: Ghostty's palette may have fuzzy-match scoring / most-recently-used ordering, whereas v1 uses only subsequence matching + registry declaration order (ranking and recency are CUT — see Scope below).

## FRAME — Reusable Assets and Constraints (Lens investigation 2026-07-04)

### Existing assets (search_prompt is the closest analog)

- **`AppCommand` registry** (`commands.rs:8-31`) = the exposure target. `action_name()` (commands.rs:217) is the stable machine name, and `menu_id()`/`from_menu_id()` already round-trip. → `action_name` can be reused as the palette's stable ID.
- **`KeybindEngine`** (`commands.rs:344-455`) = the single source of truth for `AppCommand → chord`. The reverse mapping `AppCommand → Option<chord>` can be derived from `KeybindEngine::default()`'s spec table (commands.rs:350-437).
- **`SearchPromptSession`** (`app.rs:193-197`) = the precedent for a single app-wide modal session. `window_id`/`pane_id`/buffer. The open-guard (`app.rs:2415`), the `KeyboardInput` preemptive routing (`app.rs:1890-1897`), and the modal handling of Esc/Enter/Backspace/text (`handle_search_prompt_key`, `app.rs:2453-2509`).
- **Session cleanup precedent (leak already fixed)**: `close_tab` (`app.rs:848-854`) and `close_pane` (`app.rs:928-935`) set the search_prompt of the affected window/pane to `None`. **These two spots are the no-leak contract the palette must also honor.**
- **`FrameSnapshot::search_prompt`** (`app.rs:399-400`) = the state→GPU seam. The precedent for adding an overlay payload while keeping locking minimal and the snapshot self-contained.
- **`CommandScope`** (`app.rs:3331-3388`) + `handle_app_command` (`app.rs:526`) + the `overview_command_scope` guard (`app.rs:527`) = scope resolution for command dispatch. `ToggleTabOverview`/`ToggleSplitZoom` are the isomorphic pattern for adding a toggle command.
- **`macos_menu.rs`** (View menu; `ToggleTabOverview` is at `app.rs`/`macos_menu.rs:198`) = the pattern for adding a native menu item.

### Constraints

- search_prompt is a **single-line** buffer (`search_prompt.rs`). The palette needs a **multi-line list + highlight + filtering** → the overlay rendering should follow search_prompt's "grid-aligned modal" pattern while the row-list rendering itself is new (noa-render).
- `cmd+shift+p` is free (verified: `cmd+shift+o`=overview, `cmd+shift+d`=split-down, `cmd+shift+g`=find-prev, `cmd+shift+enter`=zoom, `cmd+shift+[`/`]`=tab, `cmd+shift+plus`=font). Matches Ghostty's macOS default.
- Crate dependency rule: keep the palette's title table, filter, and session state as GUI-agnostic as possible, and leave `noa-grid` and below unmodified. Confine winit/wgpu to `noa-app`/`noa-render`.

## L1 — Requirements

Priority tags: **[MH]** = must-have (v1 blocker), **[NH]** = nice-to-have.

### Functional Requirements

**Command definition & wiring**

- **R-1** [MH]: Add a new `AppCommand::ToggleCommandPalette` variant. Assign `action_name()` = `"command-palette.toggle"`, `menu_id()` = `"noa.view.toggle-command-palette"`, and update the exhaustive matches in `from_action_name`/`from_menu_id`/`menu_id` so round-tripping works. Add `("cmd+shift+p", AppCommand::ToggleCommandPalette)` to `KeybindEngine::default()`, and add a menu item to the View menu in `macos_menu.rs`. `command_scope` is **`CommandScope::App`** (can be opened from any tab).

**Title registry**

- **R-2** [MH]: Define a pure function `command_palette_title(AppCommand) -> &'static str` that gives a human-readable title to **every `AppCommand` variant** (every row in the "Command Title Registry" table below, including `SelectTab(1..9)` and `ToggleCommandPalette`). Use an exhaustive match (no `_` wildcard arm allowed) so that adding a new variant produces a compile error for the missing title (pairs with NFR-4).

- **R-3** [MH]: Define a function `command_palette_entries() -> &'static [AppCommand]` that produces the set of commands shown in the palette, in deterministic order. **Exclusions**: (a) `ToggleCommandPalette` itself (self-exclusion, isomorphic to overview's self-exclusion), (b) `SelectTab(1..9)` (v1 CUT). The order matches the registry table's declaration order (also used as the tie-break for equal fuzzy matches).

- **R-4** [NH]: Define a pure function `command_palette_keybind(AppCommand) -> Option<String>` that looks up the current binding from `KeybindEngine` and shows a right-aligned keybinding hint on each entry row (hidden when unbound). macOS glyph formatting (⌘⇧ etc.) is an inner-NH within the NH (plain-text chord notation is acceptable).

**Session & modal behavior**

- **R-5** [MH]: `AppCommand::ToggleCommandPalette` is a toggle. Firing while closed → create a single app-wide `CommandPaletteSession` **bound to the focused window** (empty query, all of `command_palette_entries()`, selected=0). Firing again while open → close it (isomorphic to Ghostty's `toggle_command_palette`). No more than one session exists at a time.

- **R-6** [MH]: While the palette targets the focused window, `KeyboardInput` **preemptively routes** key input to the palette handler (before the normal keybind-resolve→pty-encode path, isomorphic to search_prompt's `app.rs:1890-1897`). While the palette is open, no keystroke reaches the pty (modal).

- **R-7** [MH]: Incremental fuzzy filtering. Printable text input appends to the end of the query; Backspace pops one character. The filter is a **case-insensitive subsequence match against the title**. The displayed list is recomputed on each keystroke, and selected is reset to the front (0). The order of the match set preserves `command_palette_entries()`'s declaration order (no scoring in v1).

- **R-8** [MH]: Navigation and execution. Up/Down arrows move selected within the filtered list (clamped at both ends, no wraparound). Enter executes the highlighted command via `handle_app_command` and closes the palette. Esc closes without executing.

- **R-9** [MH]: Empty-result handling. When the filtered result set is 0 entries, Enter is a no-op (palette stays open) and must not panic. selected must never index into an empty list.

- **R-10** [MH]: Execution semantics. Execution goes through the existing `handle_app_command`/`command_scope`, and the command re-resolves its own target within its own scope (FocusedTab-family commands act on the focused tab). **When executing a command that opens another modal (`Search(Find)` → search_prompt), the palette closes before the side effect** (to prevent two modals being open at once). The palette does not open while overview is focused (v1 — see Scope below).

**Cleanup & rendering**

- **R-11** [MH]: **No-leak session contract (search_prompt parity)**. The palette session is cleared to `None` when its target window is closed (`close_tab` path) (isomorphic to `app.rs:848-854`). A closed window can't deliver keys — not even Esc — so without clearing, an open-guard-equivalent would block every future `cmd+shift+p`, or a dead window reference would linger. Since the palette is window-bound only (pane-agnostic), a mere pane close that doesn't close the whole tab does not require clearing.

- **R-12** [MH]: Rendering seam. Add `command_palette: Option<CommandPaletteSnapshot>` (query string, titles + keybinding hints of the filtered entries, selected index) to `FrameSnapshot`, populated at the construction point equivalent to `FrameSnapshot::from_terminal` with minimal locking. noa-render draws the row list using the same grid-aligned modal pattern as the search_prompt overlay.

### Non-Functional Requirements (NFR)

- **NFR-1** [MH] (pure Rust / dependency hygiene): Implement fuzzy matching as a hand-written subsequence check; do not add new crates such as `fuzzy-matcher`/`nucleo`. Introduce no new FFI.
- **NFR-2** [NH] (cost): Filtering + snapshot construction is O(N) against the `AppCommand` registry (~40 entries) and must not add perceptible latency per keystroke (reference: < 1ms per key, as a sanity check).
- **NFR-3** [MH] (dependency rule): Keep the palette's title table, filter, and session-state logic GUI-agnostic wherever possible. `noa-grid`/`noa-vt`/`noa-core` remain unmodified and do not depend on `wgpu`/`winit` (verifiable via `cargo tree`). winit/wgpu are confined to `noa-app`/`noa-render`.
- **NFR-4** [MH] (registry completeness): `command_palette_title` is an exhaustive `match` (no `_` wildcard arm), so adding a variant to `AppCommand` fails to compile until a title is added (compile-time gate).
- **NFR-5** [MH] (quality gate): `cargo test --workspace` and `cargo clippy --workspace` remain clean after this change. No papering over issues with new `#[allow(...)]`.

> Must-ratio note: 14 of 17 requirements are [MH] (82%). Modal correctness, no-leak guarantees, and execution semantics are the core of v1, so a high ratio is warranted. The 2 [NH] items are R-4 (keybinding hint display) and NFR-2 (perf sanity check).

## L2 — Detail

Defines only the seams per crate (no code is written here).

### noa-app / commands.rs

- Add `ToggleCommandPalette` to `AppCommand` (R-1). Add one line each to the exhaustive matches in `action_name`/`from_action_name`/`menu_id`/`from_menu_id`. Add `TOGGLE_COMMAND_PALETTE_MENU_ID: &str = "noa.view.toggle-command-palette"` alongside the `ABOUT_MENU_ID` group.
- Add `("cmd+shift+p", AppCommand::ToggleCommandPalette)` to the spec array in `KeybindEngine::default()`.
- The pure functions for title/entries/keybind lookup (R-2/R-3/R-4) live in `commands.rs` or a new `command_palette.rs` (GUI-agnostic, unit-testable). Since `command_palette_keybind` needs to consult `KeybindEngine`, either add a reverse-lookup API equivalent to `KeybindEngine::binding_for(AppCommand) -> Option<&KeyTrigger>`, or re-scan the default spec table.

### noa-app / app.rs

- New `struct CommandPaletteSession { window_id: WindowId, query: String, filtered: Vec<AppCommand>, selected: usize }` (isomorphic to SearchPromptSession, but without a `pane_id` since it's window-bound, R-11). Add a `command_palette: Option<CommandPaletteSession>` field to `App` (next to `search_prompt`, initial value `None`).
- **Extract the filter into pure logic**: `command_palette_filter(query: &str) -> Vec<AppCommand>` (subsequence match, scans `command_palette_entries()`, preserves declaration order, R-7). `is_subsequence_ci(needle, haystack) -> bool` (case-insensitive, NFR-1, hand-written). Both are unit-testable without GUI/Window.
- `command_scope(ToggleCommandPalette) = CommandScope::App` (R-1). `overview_command_scope(ToggleCommandPalette) = CommandScope::Overview` (no-op while overview is focused, R-10's overview guard). Call `toggle_command_palette()` in the `ToggleCommandPalette` arm of `handle_app_command`.
- `toggle_command_palette()`: if open, set `self.command_palette = None`; if closed, take `self.focused` as the window_id and create `CommandPaletteSession { query: "", filtered: command_palette_entries().to_vec(), selected: 0 }`, then redraw (R-5).
- In the `KeyboardInput` handler (near `app.rs:1882`), insert, right after the IME/search_prompt branch and right before keybind-resolve: "if `command_palette` targets `window_id`, delegate to `handle_command_palette_key` and return" (R-6, isomorphic to search_prompt's 1890-1897).
- `handle_command_palette_key(event_loop, window_id, event)` (isomorphic to `handle_search_prompt_key`, R-7/R-8/R-9):
  - Esc → `self.command_palette = None` (no execution, R-8).
  - Enter → if the filtered list is empty, no-op (R-9); if non-empty, take `filtered[selected]`, set `self.command_palette = None` (**close before the side effect**, R-10) → `handle_app_command(event_loop, command)`.
  - ArrowUp/ArrowDown → clamp-move `selected`, redraw (R-8).
  - Backspace → `query.pop()` → `filtered = command_palette_filter(&query)`, `selected = 0`, redraw (R-7).
  - `cmd`-held combos → swallow (same convention as search).  A repeated `cmd+shift+p` closes via toggle.
  - printable text → `query.push_str(filtered_text)` → refilter, `selected = 0`, redraw (R-7).
- **Cleanup (R-11)**: Add to `close_tab` (`app.rs:848-854`): "if `command_palette` targets the closing window, set it to `None`." `close_pane` needs no addition since the palette is not pane-bound, but the whole-tab-close path already goes through `close_tab`, so it's covered.
- **Snapshot (R-12)**: At the `FrameSnapshot` construction point (near `app.rs:399`), build `CommandPaletteSnapshot { query, rows: Vec<(String /*title*/, Option<String> /*keybind*/)>, selected }` from the palette session. No lock is held (the palette is independent of terminal state).

### noa-render

- Add `command_palette: Option<CommandPaletteSnapshot>` to `FrameSnapshot` (adjacent to search_prompt, R-12).
- The renderer extends search_prompt overlay's grid-aligned modal drawing to render a **row list** (left-aligned title + right-aligned keybinding hint + inverted/highlighted background for the selected row + a query input row at the top). Rectangles and text are composited via the existing per-cell instruction pipeline (no new GPU pipeline needed).
- Rendering stays within the existing overlay mechanism, adding nothing new to what `noa-render/tests/pipeline.rs` validates (bind-group visibility / std140) — the existing pipeline is reused.

### noa-font / noa-pty / noa-grid / noa-vt / noa-core

- Unmodified. The palette is independent of terminal state (NFR-3). No winit/wgpu is introduced into `noa-grid` and below.

## L3 — Acceptance Criteria

Each AC states its corresponding `R-*`/`NFR-*` in Given/When/Then form. [unit] = `cargo test -p noa-app` (pure logic, no GPU/Window needed), [manual] = real GUI visual check, [inspection] = static type/structure inspection / compile-time boundary, [headless] = `noa-render/tests/pipeline.rs`.

- **AC-1 → R-1** [MH] [unit]: Given `AppCommand::ToggleCommandPalette`. When evaluating `menu_id`/`from_menu_id`, `action_name`/`from_action_name`, `from_key(Key::Character("p"), SUPER|SHIFT)`. Then all paths round-trip consistently, and `from_key` returns `ToggleCommandPalette`. Additionally, `command_scope(ToggleCommandPalette) == CommandScope::App`.
- **AC-2 → R-1** [MH] [unit]+[inspection]: Given the existing `cmd+shift+*` bindings other than `cmd+shift+p`. When evaluating `KeybindEngine::default()`. Then `cmd+shift+p` resolves to `ToggleCommandPalette`, and none of the existing bindings (o/d/g/enter/[/]/plus) are overwritten.
- **AC-3 → R-2, NFR-4** [MH] [unit]+[inspection]: Given `command_palette_title`. When called for every `AppCommand` variant (including `SelectTab(1..9)` and `ToggleCommandPalette`). Then every variant returns a non-empty title (test enumerates all variants reached). Additionally, code inspection confirms the `match` has no `_` wildcard arm (compile error on variant addition).
- **AC-4 → R-3** [MH] [unit]: Given `command_palette_entries()`. When inspecting its contents. Then it **excludes** `ToggleCommandPalette` and all `SelectTab(n)`, **includes** every remaining `AppCommand` variant, and the order matches the registry declaration order.
- **AC-5 → R-4** [NH] [unit]: Given `command_palette_keybind`. When called for `Copy`/`NewTab`/`Search(Find)`/`Quit`. Then it returns the chord strings equivalent to `cmd+c`/`cmd+t`/`cmd+f`/`cmd+q` respectively. When called for `ClearScrollback` (unbound). Then it returns `None`.
- **AC-6 → R-5** [MH] [unit]: Given the palette is closed and a window is focused. When `ToggleCommandPalette` is dispatched. Then `command_palette` becomes `Some` (empty query, filtered = all entries, selected=0, window_id = focused). When dispatched again. Then `command_palette` returns to `None` (toggle).
- **AC-7 → R-6** [MH] [unit]+[manual]: Given the palette targets window_id. When a character-key `KeyboardInput` is processed for that window. Then the palette handler consumes it and it never reaches the keybind-resolve/pty-encode path (unit: routing branch); When typing a character (manual). Then no bytes reach any pty.
- **AC-8 → R-7** [MH] [unit]: Given `command_palette_filter`. When filtering (subsequence) with `"add"`. Then only entries whose titles contain `a..d..d` as a subsequence — e.g. "Add Pane Left"/"Add Pane Right"/"Add Pane Up"/"Add Pane Down" — are returned, in declaration order. When filtering with the uppercase query `"QUIT"`. Then "Quit noa" matches (case-insensitive). When filtering, selected is 0 after each filter.
- **AC-9 → R-7** [MH] [unit]: Given a state with query `"new"`. When Backspace is processed. Then query becomes `"ne"`, filtered is recomputed, and selected=0.
- **AC-10 → R-8** [MH] [unit]: Given 3 filtered entries, selected=0. When ArrowDown×2→ArrowDown. Then selected goes 1→2→2 (clamped at the tail, no wraparound). When ArrowUp is pressed at the front. Then selected stays at 0.
- **AC-11 → R-8** [MH] [unit]+[manual]: Given filtered is non-empty, selected=k. When Enter is processed. Then `filtered[k]` is passed to `handle_app_command`, and `command_palette` becomes `None` (unit: dispatch is recorded); Given a real GUI with "New Tab" highlighted and Enter pressed. Then a new tab opens and the palette closes (manual).
- **AC-12 → R-8** [MH] [unit]: Given the palette is open. When Esc is processed. Then `command_palette` becomes `None` and no `AppCommand` is passed to `handle_app_command` (closes without executing).
- **AC-13 → R-9** [MH] [unit]: Given a query that matches no title, so filtered is empty. When Enter is processed. Then it's a no-op (palette stays open, no panic). No out-of-bounds access of the empty list via selected occurs.
- **AC-14 → R-10** [MH] [unit]+[manual]: Given `Search(Find)` is highlighted in the palette. When Enter is processed. Then the palette closes **first** (`command_palette == None`) and only then is `Search(Find)` dispatched (order verified by test); Given a real GUI. Then the palette closes and search_prompt opens afterward, with the two modals never open simultaneously (manual).
- **AC-15 → R-10** [MH] [unit]: Given overview is displayed (overview focused). When `ToggleCommandPalette` is dispatched. Then it resolves to a no-op via `overview_command_scope`, and `command_palette` stays `None` (v1: palette hidden while overview is active).
- **AC-16 → R-11** [MH] [unit]: Given the palette targets window A. When window A is closed via `close_tab`. Then `command_palette` is cleared to `None`, and `ToggleCommandPalette` can subsequently reopen it normally (no dangling window reference / no blocked open).
- **AC-17 → R-11** [MH] [unit]: Given the palette targets window A, and window A has multiple panes. When only one pane within window A is closed (the tab remains). Then `command_palette` is not cleared (window-bound = pane-agnostic).
- **AC-18 → R-12** [MH] [unit]: Given a palette session (query, filtered, selected). When `FrameSnapshot` is constructed. Then `snapshot.command_palette` reflects the query, the filtered title rows, and selected, and construction requires no terminal lock.
- **AC-19 → R-12** [MH] [headless]+[manual]: Given a `FrameSnapshot` containing the palette payload. When one frame is rendered on a real adapter. Then it completes with no wgpu validation errors (headless, unsandboxed); Given a real GUI. Then the query row, entry list, keybinding hints, and highlighted row are all displayed (manual).
- **AC-20 → NFR-1** [MH] [inspection]: Given `crates/noa-app/Cargo.toml` and the implementation. When inspecting the dependencies and the fuzzy-match implementation. Then no new fuzzy-matching crate (e.g. `fuzzy-matcher`/`nucleo`) and no new FFI have been added, and the subsequence check is hand-written.
- **AC-21 → NFR-2** [NH] [unit]: Given `command_palette_filter`. When running a single filter pass against the full registry. Then the complexity is O(N)-equivalent (a single scan of the registry), confirmed by code review, and as a reference the per-keystroke time is sanity-checked against < 1ms via microbenchmark (not a strict pass/fail criterion).
- **AC-22 → NFR-3** [MH] [unit]: Given the workspace after the change. When running `cargo tree -p noa-grid --offline` and `cargo tree -p noa-vt --offline`. Then neither includes `wgpu`/`winit` (the palette addition hasn't leaked GUI dependencies downward).
- **AC-23 → NFR-4** [MH] [inspection]: Given the `match` in `command_palette_title`. When inspecting the source. Then there is no `_` wildcard arm, and adding a variant to `AppCommand` fails to compile that function (compile-time completeness gate).
- **AC-24 → NFR-5** [MH] [unit]+[headless]: Given the workspace with this change applied. When running `cargo test --workspace --offline` and `cargo clippy --workspace --offline`. Then both exit 0, and no new `#[allow(...)]` was added by this change.

### Traceability — R/NFR ↔ AC (bidirectional)

| Requirement | AC | Priority |
|---|---|---|
| R-1 | AC-1, AC-2 | MH |
| R-2 | AC-3 | MH |
| R-3 | AC-4 | MH |
| R-4 | AC-5 | NH |
| R-5 | AC-6 | MH |
| R-6 | AC-7 | MH |
| R-7 | AC-8, AC-9 | MH |
| R-8 | AC-10, AC-11, AC-12 | MH |
| R-9 | AC-13 | MH |
| R-10 | AC-14, AC-15 | MH |
| R-11 | AC-16, AC-17 | MH |
| R-12 | AC-18, AC-19 | MH |
| NFR-1 | AC-20 | MH |
| NFR-2 | AC-21 | NH |
| NFR-3 | AC-22 | MH |
| NFR-4 | AC-3, AC-23 | MH |
| NFR-5 | AC-24 | MH |

**Coverage: 17/17 requirements trace to ≥1 AC = 100%** (Standard-scope minimum ≥85%). Total AC count: **24** (reverse direction: every AC states its originating R/NFR).

## Command Title Registry

Every row of `command_palette_title(AppCommand)` (R-2, covering every `AppCommand` variant). **"Shown" column** = whether it appears in the v1 palette (`command_palette_entries()`). **"Keybind" column** = the current binding from `KeybindEngine::default()` (blank = unbound).

| # | AppCommand variant | Title | Keybind | Shown |
|---|---|---|---|---|
| 1 | `About` | About noa | | ✓ |
| 2 | `Preferences` | Open Preferences | | ✓ |
| 3 | `Copy` | Copy to Clipboard | cmd+c | ✓ |
| 4 | `Paste` | Paste from Clipboard | cmd+v | ✓ |
| 5 | `Terminal(Clear)` | Clear Screen | cmd+k | ✓ |
| 6 | `Terminal(ClearScrollback)` | Clear Scrollback | | ✓ |
| 7 | `Terminal(SelectAll)` | Select All | cmd+a | ✓ |
| 8 | `FontSize(Increase)` | Increase Font Size | cmd+= | ✓ |
| 9 | `FontSize(Decrease)` | Decrease Font Size | cmd+- | ✓ |
| 10 | `FontSize(Reset)` | Reset Font Size | cmd+0 | ✓ |
| 11 | `Search(Find)` | Find… | cmd+f | ✓ |
| 12 | `Search(FindNext)` | Find Next | cmd+g | ✓ |
| 13 | `Search(FindPrevious)` | Find Previous | cmd+shift+g | ✓ |
| 14 | `Search(Clear)` | Clear Search | | ✓ |
| 15 | `ScrollViewport(LineUp)` | Scroll Up One Line | shift+↑ | ✓ |
| 16 | `ScrollViewport(LineDown)` | Scroll Down One Line | shift+↓ | ✓ |
| 17 | `ScrollViewport(PageUp)` | Scroll Up One Page | shift+PageUp | ✓ |
| 18 | `ScrollViewport(PageDown)` | Scroll Down One Page | shift+PageDown | ✓ |
| 19 | `ScrollViewport(Top)` | Scroll to Top | shift+Home | ✓ |
| 20 | `ScrollViewport(Bottom)` | Scroll to Bottom | shift+End | ✓ |
| 21 | `NewTab` | New Tab | cmd+t | ✓ |
| 22 | `NewWindow` | New Window | cmd+n | ✓ |
| 23 | `NewSplitLeft` | Add Pane Left | | ✓ |
| 24 | `NewSplitRight` | Add Pane Right | cmd+d | ✓ |
| 25 | `NewSplitUp` | Add Pane Up | | ✓ |
| 26 | `NewSplitDown` | Add Pane Down | cmd+shift+d | ✓ |
| 27 | `FocusDirection(Left)` | Focus Split Left | cmd+alt+← | ✓ |
| 28 | `FocusDirection(Right)` | Focus Split Right | cmd+alt+→ | ✓ |
| 29 | `FocusDirection(Up)` | Focus Split Up | cmd+alt+↑ | ✓ |
| 30 | `FocusDirection(Down)` | Focus Split Down | cmd+alt+↓ | ✓ |
| 31 | `ResizeSplit(Left)` | Resize Split Left | cmd+ctrl+← | ✓ |
| 32 | `ResizeSplit(Right)` | Resize Split Right | cmd+ctrl+→ | ✓ |
| 33 | `ResizeSplit(Up)` | Resize Split Up | cmd+ctrl+↑ | ✓ |
| 34 | `ResizeSplit(Down)` | Resize Split Down | cmd+ctrl+↓ | ✓ |
| 35 | `EqualizeSplits` | Equalize Splits | cmd+ctrl+= | ✓ |
| 36 | `ToggleSplitZoom` | Toggle Split Zoom | cmd+shift+enter | ✓ |
| 37 | `ToggleTabOverview` | Toggle Session Overview | cmd+shift+o | ✓ |
| 38 | `CloseTab` | Close Tab | cmd+w | ✓ |
| 39 | `SelectTab(1)` | Go to Tab 1 | cmd+1 | — (CUT) |
| 40 | `SelectTab(2)` | Go to Tab 2 | cmd+2 | — (CUT) |
| 41 | `SelectTab(3)` | Go to Tab 3 | cmd+3 | — (CUT) |
| 42 | `SelectTab(4)` | Go to Tab 4 | cmd+4 | — (CUT) |
| 43 | `SelectTab(5)` | Go to Tab 5 | cmd+5 | — (CUT) |
| 44 | `SelectTab(6)` | Go to Tab 6 | cmd+6 | — (CUT) |
| 45 | `SelectTab(7)` | Go to Tab 7 | cmd+7 | — (CUT) |
| 46 | `SelectTab(8)` | Go to Tab 8 | cmd+8 | — (CUT) |
| 47 | `SelectTab(9)` | Go to Tab 9 | cmd+9 | — (CUT) |
| 48 | `NextTab` | Next Tab | cmd+shift+] | ✓ |
| 49 | `PrevTab` | Previous Tab | cmd+shift+[ | ✓ |
| 50 | `CloseWindow` | Close Window | | ✓ |
| 51 | `Quit` | Quit noa | cmd+q | ✓ |
| 52 | `ToggleCommandPalette` *(new, R-1)* | Toggle Command Palette | cmd+shift+p | — (self) |

> Since `SelectTab(n)`'s `action_name` is `tab.select-{n}` (commands.rs:254-265) and carries an index, the title registry keeps 1..9 as individual rows (exhaustive-match completeness, NFR-4). Exclusion from the display set is handled by R-3's `command_palette_entries()`.

## Scope

### In-scope (v1)

- New `AppCommand::ToggleCommandPalette` variant + `cmd+shift+p` binding + View menu item + round-trip wiring (R-1).
- Title registry covering every variant (R-2/NFR-4) + display entry set (R-3) + keybinding-hint reverse lookup (R-4).
- Single app-wide toggle modal session (R-5), modal keyboard routing (R-6).
- Incremental subsequence fuzzy filter (R-7), arrow navigation + Enter execution + Esc cancel (R-8), empty-result no-op (R-9).
- Execution via the existing `handle_app_command`/`command_scope`, closing before the side effect, hidden while overview is active (R-10).
- Session clearing on tab/window close (no-leak parity, R-11).
- `FrameSnapshot` overlay payload + row-list rendering reusing the search_prompt pattern (R-12).

### Out-of-scope (YAGNI / void-style scope cut)

- **Mouse interaction** (click to select/execute, hover highlight) — since search_prompt is keyboard-only, v1 is keyboard-only too. **CUT**.
- **Command history / most-recently-used ordering / frequency ranking** — v1 uses declaration order only. **CUT**.
- **Fuzzy scoring / highlighting matched characters** — v1 uses only a boolean subsequence check. **CUT**.
- **Displaying `SelectTab(1..9)`** — 9 redundant entries, directly reachable via `cmd+1..9`. Kept in the title registry. **CUT**.
- **User-defined custom entries** (Ghostty's `command_palette_entry`) — **CUT** (parity exception, recorded in L0).
- **Argument input from the palette** (actions taking arguments, prompt chaining) — unnecessary since noa's `AppCommand` takes no arguments. **CUT**.
- **Palette display while overview is focused** — overview has a dedicated keymap, so it's hidden (no-op) in v1. **DEFER**.
- **macOS glyph formatting for keybinding hints (⌘⇧ etc.)** — plain-text chord notation suffices; glyph rendering is a future increment. **DEFER**.

### Non-goals

- Passing key input through to the pty (fully modal while the palette is open).
- Command undo / confirmation dialogs.
- Customizing palette behavior via config file.

## Considered but rejected

- **Sharing a keybinding: reuse `Search(Find)`'s cmd+f to also launch the palette** — Rejected: Ghostty has an independent `cmd+shift+p`, and search and command execution are separate JTBDs. Don't conflate them.
- **Render the palette with an entirely new noa-render pipeline** — Rejected: extending search_prompt's existing overlay mechanism (grid-aligned modal) avoids new exposure to the CLAUDE.md GPU gotchas (bind-group visibility / std140) and is safer.
- **Consolidate `SelectTab(n)` into a single "Go to Tab…" entry plus numeric input** — Rejected (for v1): argument-input UI is out of scope (CUT). Candidate for a future increment.
- **Adopt a `nucleo`/`fuzzy-matcher` crate for fuzzy matching** — Rejected: violates NFR-1 (no new heavyweight dependencies). A hand-written subsequence check is sufficient for ~40 entries.
- **Bind the palette session to `pane_id` the same way as search_prompt** — Rejected: since palette commands re-resolve their own target within their own scope (R-10), the palette is pane-agnostic. Binding to the window only simplifies leak cleanup (R-11, no `close_pane` addition needed).

## Open Questions / Deferred Decisions

- Arrow-navigation wraparound behavior (clamping at both ends adopted; wraparound is a future consideration) — v1 is settled on clamping.
- Display format for keybinding hints (text chord vs. ⌘ glyphs) — v1 uses text; glyph rendering is DEFERRED.
- Palette availability while overview is focused (v1: no-op) — extending overview's keymap is a future increment.
- Future increments (deferred): mouse interaction / recency ordering / matched-character highlighting / consolidated `SelectTab` entry / custom entries / glyph formatting.

## Next Actions

- Handoff to: **atlas** (design + risk gate). This spec's R-1..R-12 / NFR-1..5 and 24 ACs form the population for the machine-checkable DONE gate.
- Key risks: (1) the insertion point for KeyboardInput routing (ordering of IME/search_prompt/keybind, `app.rs:1882-1901`), (2) missed leak-cleanup addition to `close_tab` (R-11, must follow search_prompt's proven path), (3) the extent to which the renderer's row-list drawing can reuse the existing overlay.
- **This spec does not write code — generating/launching the loop happens separately, on instruction.**
