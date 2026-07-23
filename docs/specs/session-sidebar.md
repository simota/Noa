# Session Sidebar — Specification

## Metadata
- slug: `session-sidebar`
- title: Session Sidebar (new tab-list UI)
- status: `locked` (2026-07-05)
- owner: simota
- build-path: **apex** (`/nexus apex` — design → risk gate → implementation loop → AC verification → ship. L3 AC is the verification contract. 7 [manual] ACs require human confirmation)
- source mockup: `~/Downloads/ChatGPT Image 2026年7月5日 08_43_15.png`

## L0 — Vision
noa's current tabs are native macOS tabs, and the only overview is the separate Tab Overview window. When working across multiple projects in parallel, there's no way to constantly see "what's happening in which session" (cwd, branch, last output, running state) and switch instantly. So we're adding a persistent left sidebar listing session cards (icon, name, cwd, branch, updated time, status dot, last-output 2-line preview).

- **audience**: developers working across multiple projects/sessions in parallel (i.e. the author)
- **job-to-be-done**: keep track of every session's state without leaving the terminal, and switch with one click
- **success**: every session's cwd, branch, last output, and status are always visible in the sidebar, and click-to-switch works

### FRAME decisions
1. **Session scope**: a sidebar spanning all windows. However, a **mode toggle** preserves the traditional native-tab mode too (sidebar mode / native-tab mode).
2. **Header bar**: in scope (a "✳ Claude Code"-style running-program label + centered title + session-name pill on the right).
3. **Git branch display**: in scope. New implementation — a throttled `git -C <cwd> branch --show-current` against cwd (cached, off the render path).

### Lens reuse/constraint findings (2026-07-05 scan)
**Reusable assets**
- `noa-render/src/blit.rs` — rounded-corner card pipeline (`overlay_texture_cards`, `CardStyle`, border/focus-glow)
- `noa-app/src/tab_overview.rs` — grid layout math, 10Hz throttling, filtering, hit-testing (pure logic, unit-tested)
- `Surface.overview_snapshot` (app.rs:298) — the slot where the io thread publishes a `FrameSnapshot`. Last-output preview can be built without a Terminal lock
- `Terminal.cwd` (OSC 7, noa-grid/src/terminal.rs:49) / `Terminal.title` (OSC 0/2)
- `relayout_and_resize_window` (app.rs) — grid-first resize path
- `noa-config`: `StartupConfig` + the parser.rs key-addition pattern

**Not yet implemented (new work)**
- Git branch detection / last-output timestamp (io thread can stamp with `Instant`) / structured status (busy detection partially exists via `group_running_program_count`)

**Hard constraints**
1. No text-label primitive exists — sidebar text must all be rendered as terminal cells (a dedicated small Renderer, overview-style) or a pre-rasterized texture
2. The renderer must not lock Terminal — read via a snapshot-publish slot (gated only while the sidebar is visible)
3. Changing the sidebar width requires a grid-first resize (grid → pty winsize order)
4. wgpu only in noa-app/noa-render, winit only in noa-app. Layout logic should be pure and testable, same as tab_overview.rs
5. Watch out for std140 / bind-group visibility pitfalls when extending blit.rs shaders

## EXPAND — Candidate directions (confirmed 2026-07-05)
Five directions considered: A=Window Aggregator (keep the current structure, switch by focus) / B=Session Host (multiple Terminals multiplexed in one window, swap the active one) / C=Shared Registry (a central SessionStore as source of truth, UI is a read-only view, switching policy is swappable) / D=Overlay Projection (minimal rework stacking Tab Overview vertically) / E=Attention Switcher (a summon overlay driven by attention signals).

**Surviving candidate: C (Shared Registry)** — adopted by the user.

### Additional EXPAND decisions
- Visibility: **persistent, toggle-based** (keybind + config. Persistent while shown; toggling triggers a grid-first resize). Auto-hide was not adopted.
- Scale: scrolling only for v1. Repo-root grouping/collapsing goes to Open Questions.

### Flux warnings (to reflect in the design)
1. A fixed panel narrows every terminal → toggle is mandatory (adopted)
2. Freshness updates without a shared registry would cause a storm of git spawns → reason for adopting C (updates cost only N calls, N = number of sessions)
3. Text-is-cells tax: one card ≈ 6 text runs. More than 20 cards is unhandled (Open Question)

## CHALLENGE — Adopted / rejected (confirmed 2026-07-05)

### Adopted: C (Shared Registry) + 4 Magi arbitration items (all approved 3-0)
1. **Window model = A-flavor**: keep 1 tab = 1 WindowState. Switching = window focus. B-flavor (active-swap) is kept as a seam behind the switching policy (swappable in v2 without touching the store).
2. **SessionStore = channel-delta type**: the io thread sends deltas, the main thread owns the store. No cross-thread locking. Follows the existing UserEvent-poke pattern.
3. **Rendering = per-window**: each window reads the same store read-only. No privileged "main window" concept is created.
4. **Visibility = per-window toggle**: config sets the app-wide initial value; the keybind flips only the focused window.
- "Mode toggle (sidebar ↔ native)" is **reduced to a sidebar visibility toggle** (since native tabs remain intact under A-flavor, hiding the sidebar = the old mode).

### Ripple: GO-with-conditions (risk 6.5/10, affects ~6 areas, 2 new files, 700-1100 LOC)
Conditions adopted as constraints:
- (a) git spawn is strictly forbidden in the io read loop — a dedicated branch-poll thread, per-cwd caching (≥1s throttle), only on OSC-7 cwd change, negative cache for non-git directories, results posted via UserEvent
- (b) SessionStore is a publish-slot read model of the same shape as overview_snapshot. Never locks Terminal
- (c) GC is placed at all 5 teardown sites (close_pane / close_pane_after_pty_exit / close_tab / window remove / Quit) + a regression test that store size tracks overview_tiles
- (d) sidebar layout is pure and unit-testable, same as tab_overview.rs
- (e) **quick-terminal windows are excluded from the sidebar** (no inset applied either)
- (f) implementation split into 4+ PRs (config+store → layout → render → git)
- Resize order: on toggle, for the **focused WindowState**, apply the inset to pane_bounds_for_size → relayout_and_resize_window (grid → pty winsize) → request_redraw (visibility is per-window, so other windows are untouched)

### v1 scope re-arbitration (user ruling on Void's challenge)
- **Included in v1 (kept per FRAME, faithful to the full mockup)**: git branch / all 3 header-bar elements / updated-time / + button / project icon / … menu
- Void's KEEP items stand as-is: name, cwd, status dot, 2-line preview, click-to-switch
- CUT adopted: **user-facing configuration** of the pluggable switching policy (kept only as an internal seam)
- Scale handling (grouping/collapsing/badges): remains an Open Question

### Considered but rejected (from EXPAND)
- A: Window Aggregator alone — without a store, git/snapshot updates explode to N × window count (Flux warning #2)
- B: Session Host — a major rework of app.rs's window=tab assumption, highest effort (rejected by Magi, kept reachable later via a seam)
- D: Overlay Projection — hits a ceiling quickly; adding the full feature set means doing the work twice
- E: Attention Switcher — doesn't fit the "always-on dashboard" JTBD

## SHAPE — Proposal (approved 2026-07-05)

### Solution
A persistent, toggle-based left sidebar with a central `SessionStore` (channel-delta type, owned by the main thread) as the source of truth, read read-only by each window. The io thread sends session state through a publish slot of the same shape as `overview_snapshot`, and a dedicated branch-poll thread supplies the git branch. Switching = window focus (A-flavor).

Card layout: `[icon] name … ●dot` / `cwd … branch` / 2 lines of ANSI last-output / relative updated-time.
Header bar: busy label (reduced form) + centered title + session-name pill. + button and … menu at the top of the sidebar.

### Open Question resolutions (approved by user)
- **Icon**: rendered as a cell-drawn Nerd Font glyph. Determined by first-match on cwd markers (`Cargo.toml`→rust, `package.json`→node, `*.tf`→terraform, `go.mod`→go, `pyproject.toml`→python, `.git` only→git, none→folder). Re-evaluated only on cwd change (co-located with the branch-poll thread).
- **… menu**: v1 has 2 actions, close / rename (rename is a name override stored in SessionStore).
- **+ button**: opens a new tab in the focused window, inheriting cwd from the active session (reuses the existing new-tab path).
- **Status dot**: busy (OSC 133 `has_running_program`) = blue / idle = green / bell-attention (unread bell) = yellow.
- **updated-time**: relative display ("3 minutes ago"; beyond 24h shown as "Yesterday 23:47"). The io thread stamps Instant + wall-clock time whenever there's new output.
- **Sidebar width / preview line count**: config `sidebar-width` (points, default 360pt). Converted via grid-first resize. `sidebar-preview-lines` sets the card's last-output preview line count (default 3, `0` hides it).
- **Header label**: the foreground-process-name detection originally parked was implemented at user request on 2026-07-05 (`Pty::foreground_probe` = dup the master fd → tcgetpgrp → libproc `proc_name`, polled at 1s by the session-metadata worker, only while the sidebar is visible). The label shows the actual process name (`✳ <proc>`); falls back to Running/Idle when undetected.

### Assumptions
- A1: Nerd Font/emoji can be cell-rendered via the font fallback cascade (face.rs). `noa-font` embeds a vendored Symbols Nerd Font Mono face as a permanent fallback, so Nerd Font PUA icons render even with no Nerd Font installed; emoji still degrades to tofu if Apple Color Emoji is unavailable
- A2: concurrent session count ≤ ~20
- A3: macOS only
- A4: busy/bell assume OSC 133 shell integration. Shells without it degrade to busy=false

### MoSCoW (PR order)
- **Must** (PR1-2): SessionStore + GC + regression tests / pure sidebar layout / toggle + grid-first resize / basic card rendering (name, cwd, dot, click-switch)
- **Should** (PR3-4): 2-line preview / git branch / updated-time / icon
- **Could** (PR4+): header bar (reduced form) / … menu / + button

## L1 — Requirements

### Functional
- **FR-1 SessionStore**: a central `SessionStore` holding session card state across all windows is the source of truth; deltas from the io thread are applied and owned by the main thread (no cross-thread locking).
- **FR-2 Card rendering**: each card is rendered as 3 lines — `●dot [icon] name … relative updated-time` / `cwd … branch` / a running-process line (`✳ proc` = busy / `❯ shell` = idle) (2026-07-05 user ruling: the last-output 2-line preview was replaced by the process line. The preview plumbing is kept in the store/io_thread, only rendering was stopped).
- **FR-3 Click-to-switch**: clicking a card moves window focus to that session's `{window_id, pane_id}` (A-flavor, no active-swap).
- **FR-4 Toggle + resize**: sidebar visibility is toggled **per focused window** via hotkey/config, and toggling applies a grid-first resize (grid → pty winsize) to **every pane of that window** (quick-terminal windows excluded). Other windows' visibility/grid are unaffected.
- **FR-5 Header bar**: ~~renders an execution-status label at the top of the sidebar (the focused session's actual process name `✳ <proc>`, or Running/Idle when undetected) + centered title + session-name pill on the right.~~ (2026-07-11 update: **removed** from the implementation. Collapsed to `SIDEBAR_HEADER_H = 0` due to information duplication with the terminal title bar — see the constant comment in `sidebar.rs`. AC-7 is obsolete)
- **FR-6 + button**: opens a new tab in the focused window, inheriting cwd from the active session (reuses the existing new-tab path).
- **FR-7 … menu**: each card provides a close action; rename is kept as a name override in SessionStore. Close delegates to the existing close_pane/close_tab teardown path (including the confirm dialog, pty termination, and GC choke-point) — since cards are per-pane (SessionCardId holds a pane_id), close_pane is correct, cascading to close_tab for the last pane (Judge ruling, 2026-07-05). Inline text-input UI for rename is **implemented** (2026-07-11 update: inline on-card editing via `SidebarRenameSession`. The deferral in Open Question 5 is resolved).
- **FR-8 Git branch**: a throttled `git -C <cwd> branch --show-current` supplies results into SessionStore.
- **FR-9 Icon detection**: determines the project icon by first-match on cwd markers (`Cargo.toml`→rust, `package.json`→node, `*.tf`→terraform, `go.mod`→go, `pyproject.toml`→python, `.git` only→git, none→folder).
- **FR-10 Updated-time**: shows the last-output timestamp relatively ("3 minutes ago"; beyond 24h shown as "Yesterday 23:47").
- **FR-11 Status indicators**: busy (OSC 133 `has_running_program`) = blue play icon + segmented rail / idle = hollow green circle + no rail / unread bell = yellow bell + short rail notch. The unread bell is drained by the io thread from `Terminal::take_pending_bell` (terminal.rs:305, sourced from BEL) and sent as a SessionDelta, cleared once that session's window is focused. Rail shapes are categorical, not completion percentages.
- **FR-12 GC/teardown**: removes the corresponding entry from SessionStore at all 5 teardown sites when a session ends.
- **FR-13 Config keys**: adds `sidebar-enabled` (bool initial value), `sidebar-width` (points, default 360), `sidebar-hotkey` (toggle chord, following the existing parse/dispatch pattern of `quick-terminal-hotkey`), and `sidebar-preview-lines` (card last-output preview line count, default 3) to noa-config. No generic keybind→action system is introduced.
- **FR-14 Quick-terminal exclusion**: quick-terminal windows are excluded from the sidebar, and no inset is applied either.
- **FR-15 Scroll**: when the card count exceeds the sidebar's visible area, vertical scrolling (with clamped scroll offset) reaches every card. No grouping/collapsing.
- **FR-16 Attention (notification indicator)**: when a pane in a non-focused window issues an OSC 9/777 desktop notification, an `attention` flag is set on that session card. This shows as (a) red exclamation icon + solid status rail (priority: attention > bell > busy > idle), (b) `· 通知あり` appended to the process line, and (c) a persistent red marker/ring in the tab overview. The initial transition gets a one-shot 150 ms emphasis and then stays still. Cleared together with unread bell when the window gains focus. Notifications on the currently focused window don't raise attention because the user is already looking at it.

### Non-Functional
- **NFR-1 No render-path lock**: the render path never locks Terminal, reading session state only via a publish slot of the same shape as `overview_snapshot`.
- **NFR-2 No git on io loop**: git spawn never runs on the io read loop (dedicated branch-poll thread).
- **NFR-3 Throttles**: the last-output preview is reused at a minimum render interval (~10Hz); branch-poll throttles per-cwd (≥1s) with a negative cache for non-git.
- **NFR-4 Pure layout**: sidebar layout/hit-testing/scroll math live in a pure, unit-testable module, same as `tab_overview.rs`.
- **NFR-5 Graceful degradation**: degrades gracefully — no Nerd Font installed → icons still render via `noa-font`'s embedded Symbols Nerd Font Mono fallback (no tofu); shell without OSC 133 → busy=false, non-git cwd → branch hidden.
- **NFR-6 Crate boundaries**: wgpu only in noa-app/noa-render, winit only in noa-app. SessionStore/layout stay GUI-independent.

## L2 — Detail

- **SessionStore** (`crates/noa-app/src/session_store.rs`, new): keyed by `SessionCardId{window_id, pane_id}` (following the existing `OverviewTileId`), holding `SessionCard` (name/cwd/branch/icon/dot/updated/preview-slot reference). Updates go through `enum SessionDelta` (**closed over 6 variants: Upsert / Remove / Branch / Rename / Bell / Process** — Process was added when process-name detection was promoted on 2026-07-05), posted via the existing `UserEvent` channel (`events.rs`) and applied by the main thread.
- **io-thread publishing** (`crates/noa-app/src/io_thread.rs`): near `publish_overview_snapshot`, stamps `Instant` + wall-clock time on new output and sends SessionDelta::Upsert. In the same lock section, drains `Terminal::take_pending_bell`, and if true, sends SessionDelta::Bell (cleared by the main thread on focus). The preview is **bundled into SessionDelta::Upsert** rather than a second snapshot slot (implementation-time Judge ruling, 2026-07-05: bundling into the delta wins on coherence, memory, and lock time. A dedicated `SidebarPublish` gate + `decide_sidebar_publish` throttle is implemented by **reusing the pattern, in a separate instance,** from `OverviewPublish` — Omen T1).
- **branch-poll thread** (new): triggered by cwd-change events sourced from OSC-7. Caches `(branch, Instant)` per cwd (≥1s throttle), negative-cache for non-git. Results posted via `UserEvent` (SessionDelta::Branch). Icon detection (FR-9) is co-located and re-evaluated only on cwd change.
- **sidebar layout** (`crates/noa-app/src/sidebar.rs`, new): mirrors `tab_overview.rs`, computing card-rectangle vertical stacking geometry, scroll offset, `hit_test` (→ SessionCardId), and close/… button rectangles as pure functions. Independent of winit/wgpu.
- **rendering** (reuses `crates/noa-render/src/blit.rs`): card frames use `CardStyle` + `overlay_texture_cards`. Text (name/cwd/branch/preview) uses a small dedicated `Renderer`, overview-style (one instance reused across all cards, not a per-card renderer). Status dots are small filled quads.
- **resize path** (`crates/noa-app/src/app.rs`): on toggle, applies the sidebar-width inset to `pane_bounds_for_size` for the target `WindowState` group → `relayout_and_resize_window` (grid → pty winsize) → `request_redraw`, in that order (quick-terminal excluded).
- **config** (`crates/noa-config/src/lib.rs` `StartupConfig`, `parser.rs`): adds `sidebar-enabled`/`sidebar-width`/`sidebar-hotkey`/`sidebar-preview-lines`, following the `quick_terminal_hotkey` pattern. No generic keybind→action system is introduced (parser.rs's keybind handling is currently diagnostic-only).
- **header bar** (`sidebar.rs` + rendering): execution status is reduced to a boolean via `group_running_program_count`. Centered title is `WindowState.title`, pill is SessionCard.name.
- **teardown GC sites** (`crates/noa-app/src/app.rs`): sends SessionDelta::Remove at the 5 sites `close_tab`, `close_pane_after_pty_exit`, `close_pane`, window remove, and `request_quit`.

## L3 — Acceptance Criteria

- **AC-1 (FR-1)**: applying SessionDelta::Upsert then Remove in sequence increases/decreases the store size, and after Remove the corresponding `SessionCardId` is gone — verified by a unit test.
- **AC-2 (FR-1, NFR-6)**: a source-scan `#[test]` that reads the module source asserts that `use winit`/`use wgpu`/`winit::`/`wgpu::` never appear in `session_store.rs` or `sidebar.rs`.
- **AC-3 (FR-2)**: text lines generated from a given SessionCard include `[icon] name`, `cwd … branch`, and updated-time (asserted on the layout's line strings via a unit test).
- **AC-4 (FR-3)**: `hit_test(point)` returns the correct `SessionCardId` for a point inside a card's area, and `None` outside it (unit test).
- **AC-4b (FR-3) [manual]**: clicking a card focuses the target window, and other windows' Terminal contents don't change.
- **AC-5 (FR-4)**: on toggle, every pane grid of the focused window is resized before the pty winsize send — same shape as the existing `pane_resize_batch_plan` test (`multi_pane_resize_batching_resizes_all_grids_before_pty_winsize_sends`, app/helpers/tests.rs:773), asserting the order when the sidebar inset is applied.
- **AC-6 (FR-4) [manual]**: on toggle, the terminal drawing area narrows by the sidebar width, and the shell reflows to the reduced column count (confirm the decrease via `tput cols`).
- **AC-7 (FR-5) [obsolete]**: ~~the header shows `● Running`/`Idle`, a centered title, and a session-name pill on the right.~~ (invalidated by FR-5's removal. 2026-07-11)
- **AC-8 (FR-6) [manual]**: the + button opens a new tab in the focused window, with cwd inherited from the active session.
- **AC-9 (FR-7)**: after applying a rename action, SessionCard.name becomes the override value and is not overwritten by subsequent Upserts (unit test).
- **AC-9b (FR-7) [manual]**: closing via the … menu ends the corresponding session and the card disappears.
- **AC-10 (FR-8, NFR-3)**: the pure function `decide_branch_poll(now, last_poll, cache)` returns Skip for <1s, Spawn for ≥1s, and Hit for an already negative-cached non-git cwd — asserted with explicit `now: Instant` values (following the now-as-param pattern of `decide_overview_publish`, no wall-clock sleep).
- **AC-11 (FR-9)**: given a set of marker files, the icon-determination function returns results matching the first-match table (table-driven unit test).
- **AC-12 (FR-10)**: the updated-time formatter returns the correct string at each boundary — "3 minutes ago" / "Yesterday 23:47" / same-day time (unit test).
- **AC-13 (FR-11)**: the dot-color mapping is verified by unit test — `has_running_program`=true→blue, false→green, unread bell→yellow.
- **AC-14 (FR-12)**: the pure function `reconcile_sessions(&mut store, live_ids)` removes entries not in live_ids, and store size == live_ids count is verified by unit test. That all 5 teardown sites actually call this is confirmed via implementation review + [manual] integration check (since `App` can't be constructed from a unit test).
- **AC-15a (FR-13)**: `sidebar-enabled`/`sidebar-width`/`sidebar-preview-lines` parse correctly, and the defaults (width=360, preview-lines=3) apply (parser unit test, following the `parse_bool`/`quick-terminal-size` pattern).
- **AC-15b (FR-13)**: the `sidebar-hotkey` chord is accepted via the same parse path as `quick-terminal-hotkey` (parser unit test). Diagnostics for an invalid chord occur at app-layer registration time via `parse_hotkey`, not at parse time — since noa-config cannot depend on noa-app (same precedent as quick-terminal-hotkey; Judge ruling, 2026-07-05).
- **AC-16a (FR-14)**: the sidebar inset is not applied to `pane_bounds_for_size` for quick-terminal windows (pure-function unit test).
- **AC-16b (FR-14)**: the sidebar-eligibility predicate returns false for quick-terminal windows, and they're never registered in the store (pure-function unit test).
- **AC-17 (NFR-1)**: a source-scan `#[test]` asserts `terminal.lock()` never appears in `sidebar.rs` or the sidebar rendering path's source. Preview is read-only via the slot.
- **AC-18 (NFR-2)**: a source-scan `#[test]` asserts that `io_thread.rs`'s read loop (`feed_terminal`) source contains no git spawn such as `Command::new("git")` (that branch-poll runs on a separate thread is confirmed via code review).
- **AC-19 (NFR-3)**: the preview slot is published only while the sidebar is visible, and is throttled at the minimum render interval (extends the existing throttle test pattern to the sidebar gate).
- **AC-20 (NFR-4, FR-15)**: each of `sidebar.rs`'s `card_layout`, `hit_test`, and scroll-clamp functions has window/GPU-independent unit tests, and scrolling asserts the offset is clamped to an upper/lower bound when card count exceeds the viewport.
- **AC-21 (NFR-5) [manual]**: no Nerd Font installed → icons still render via the embedded Symbols Nerd Font Mono fallback (no tofu); shell without OSC 133 → all dots green (idle); non-git cwd → branch field is empty.
- **AC-22 (NFR-6)**: satisfied by AC-2's source-scan test covering SessionStore and sidebar layout (`cargo tree` isn't used since it can't see module boundaries; noa-config's wgpu/winit independence can be confirmed via `cargo tree` as a crate dependency).
- **AC-23 (FR-15)**: increasing/decreasing the scroll offset with a card count exceeding the viewport height allows reaching the first/last card respectively, with the offset clamped to [0, max] (unit test).

## Scope

### In-scope (v1)
- Central SessionStore (channel-delta type, owned by the main thread) and GC at all 5 teardown sites
- Persistent, toggle-based left sidebar + grid-first resize (per-window visibility)
- All card elements: icon, name, cwd, status dot, branch, 2-line preview, relative updated-time
- Click-to-switch (window focus, A-flavor)
- Header bar (`Running`/`Idle` reduced label + title + session-name pill)
- + button (cwd-inheriting new-tab) / … menu (close/rename)
- Git branch detection (dedicated branch-poll thread, cache/negative-cache) / project icon detection
- Config keys (`sidebar-enabled`/`sidebar-width`/`sidebar-hotkey`/`sidebar-preview-lines`)
- Vertical scroll (offset clamped, FR-15)
- Excluding quick-terminal windows

### Out-of-scope (deferred / rejected)
- Active-swap window model (B-flavor) — kept as a seam behind the switching policy, v2
- User-facing configuration of the pluggable switching policy — internal seam only
- Repo-root grouping / collapsing / badges / handling scale beyond 20 cards — Open Question
- (Resolved 2026-07-05: foreground process-name detection is implemented → see FR-2/FR-5)
- Auto-hide, a separate mode toggle vs. native tabs (reduced to the visibility toggle)
- Soft-wrap reflow (existing scope of inc≥3)

## Considered but rejected
(not yet started)

## Open Questions / Deferred Decisions
1. ~~Foreground process-name detection~~ — **implemented 2026-07-05** (tcgetpgrp + libproc, session-metadata worker. Used in both card and header). Remaining: an option to re-enable the last-output preview (plumbing kept, only rendering stopped).
2. **Handling scale beyond 20 cards** — repo-root/cwd grouping, collapsing, unread badges (a Slack-style UI's survival condition, Flux warning #3). v1 is scroll-only.
3. **B-flavor active-swap** — switching via multiple Terminals multiplexed in one window. Kept as an internal seam behind the switching policy.
4. **A new-window variant for the + button** — v1 opens only a new tab in the current window.
5. **Additional … menu actions** (duplicate, copy cwd, etc.) — v1 has close/rename only. ~~Inline text-input UI for rename was also deferred~~ (**resolved 2026-07-11**: implemented as inline input UI via `SidebarRenameSession`. Additional actions remain unimplemented).
6. **An absolute-time display option for updated-time** — v1 is fixed to relative display.
7. **Turning AC-14 into an integration test** — since `App` can't be unit-tested, mechanical verification of teardown-site calls awaits a harness (currently implementation review + manual).

## Quality Gate Results (2026-07-05)
- First pass: Judge = REQUEST CHANGES (Ambiguity/Completeness/Consistency/Scope FAIL, 3 HIGH items) / Attest = 3 NOT-VERIFIABLE, 4 requiring setup, 2 requiring split.
- Fixes: HIGH-1 unified the resize target to the focused window (FR-4/L2/CHALLENGE). HIGH-2 added scrolling per FR-15 + AC-23. HIGH-3 defined the bell supply path in L2 (`take_pending_bell` drain → SessionDelta::Bell, cleared on focus). Closed SessionDelta over 6 variants. Defined close semantics in FR-7. Changed FR-13 to `sidebar-hotkey` (the quick-terminal pattern) and made explicit that no generic keybind→action system is introduced. Rewrote AC-2/5/6/10/14/15/16/17/18/20/22 per Attest's findings (source-scan tests, now-as-param, pure-functioning, splitting).
- All FAIL findings were resolved by the above fixes; the remaining item (AC-14's mechanical-verification limit) was downgraded and recorded in Open Question 7.
