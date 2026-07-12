# Spec: Session Overview (all-tab tiled monitoring view)

> **Historical baseline:** Inline `file:line` references preserve the
> 2026-07-03/04 design worktrees and are not current navigation targets. The
> implementation now lives primarily in `session_overview/`, `app/overview/`,
> and `app/input_ops/`; use the named symbols as stable anchors.

## Metadata
- slug: `tab-overview`
- title: Monitoring dashboard view that lays out all tabs as tiles for at-a-glance overview
- status: `locked` (2026-07-03 user sign-off: SHAPE reaffirmed + ⚠A-⚠G all recommended options approved)
- owner: simota
- build-path: **orbit loop (engine: codex, gpt-5.5)** — specified by the user on 2026-07-03. Runner: `.nexus/loops/tab-overview/` (25 L3 ACs mapped to verify.sh's completion contract. manual/visual ACs live in done.md's manual-verified slots)

## Amendment v2 — Display host change (2026-07-06, per user instruction)

Instead of a dedicated NSWindow (outside the tab group, maximized), the view now appears **as an overlay inside the terminal window (host) that had focus when toggled**. The implementation changes `OverviewWindowState` to a `host: WindowId`-based model, presents to the host's surface, and routes the host window's input (redraw / key / mouse / scroll / IME) to the Overview keymap only while Overview is displayed (structural events — resize / focus / occlusion / close — fall through to normal window handling).

- **Decisions overridden**: Magi decision 4 ("dedicated window outside the tab group"), the "dedicated window" wording in REQ-OV-1/OV-2, and the SHAPE section's window lifecycle paragraph. The intent of REQ-OV-2 (Overview must not appear in tab cycling/counts) is now structurally trivial under the host model (no dedicated WindowId exists).
- **Accompanying behavior change**: Tile activation (click / Return / Cmd+n) now **dismisses** the overlay, just like Exposé (previously: the dedicated window stayed open = a permanent dashboard). Leave-it-open monitoring is still possible by leaving any one window in Overview mode while working in other windows. Closing the host's tab also tears down the overlay. The host itself is now included among the monitored tiles (self-exclusion is no longer needed).
- **Unchanged**: grid layout / 9-tile cap + placeholder degradation / 10Hz throttle / shared scratch + blit / search & hint bar / Overview keymap / no PTY pass-through (REQ-OV-7).

## L0 — Vision
- **Problem**: noa has native macOS tabs (each tab is a separate NSWindow within one tab group), but as tabs multiply, the only way to check the status of work running in each tab (builds, tests, logs, etc.) is to cycle through them one by one.
- **Audience**: noa users who are heavy terminal users running long processes concurrently across many tabs.
- **Job-to-be-done**: Show every tab's on-screen content as a grid of tiles, and **leave it open to monitor multiple tabs' output live at once**. Also let the user quickly switch to the target tab by recognizing its content.
- **Success criteria**: One keystroke shows the overview; while shown, all tabs' tiles update live; clicking or selecting with a key focuses that tab.
- **Primary purpose**: monitoring dashboard (live updates are a hard requirement). Switch navigation is a secondary purpose.
- **Tile granularity**: per-tab (one tab = one tile). Tabs with splits are represented by either the focused pane or the split layout as a whole (to be finalized in SPECIFY).
- **Parity exception (⚠E, finalized)**: Session Overview has no counterpart in Ghostty; it is this repository's first deliberate departure from the faithful-clone philosophy, and is recorded in L0 as out of scope for Ghostty parity checking.

## FRAME — Reusable assets and constraints (Lens survey, 2026-07-03)

### Existing assets
- Tabs are native NSWindows (one tab group). All tabs are enumerable via `App.windows: HashMap<WindowId, WindowState>` + `window_order: Vec<WindowId>` (`crates/noa-app/src/app.rs:190-205`).
- `Renderer::draw_panes` (`renderer.rs:345`) + `build_draw_plan`/`PaneRect` (`draw_plan.rs`) already implement "scissor-draw N panes onto one surface" for splits — the closest existing analog to tile drawing.
- `FrameSnapshot::from_terminal` (`snapshot.rs:43`) is a self-contained snapshot. It can be collected across tabs (briefly locking each tab's Mutex).
- `KeybindEngine` + `AppCommand` + `CommandScope` (`commands.rs`, `app.rs:376+`, `app.rs:2116`) — an established pattern for adding toggle commands (isomorphic to `ToggleSplitZoom`).
- `hit_test` in `split_tree.rs` — a precedent for resolving click → tile → tab.
- `macos_menu.rs` — the pattern for adding native menu items.

### Constraints
- **No downscaled rendering path exists**: the glyph atlas uses fixed cell sizes. Shrinking a rectangle merely clips it; glyphs don't shrink. Producing thumbnails is new engineering work: (a) introduce scale into the shader, (b) render offscreen then downscale-blit, (c) re-rasterize at a small font size, etc.
- No precedent for overlay/modal UI (command palette etc., Inc 6, hasn't started) — interaction design is greenfield.
- Because this is a monitoring dashboard, **live updates while displayed are mandatory** → continuous cost of locking + drawing every tab's Terminal. Scaling with tab count needs care.
- New scope outside the roadmap (Inc 4-6). Positioning is a product decision.
- New command needs a `CommandScope` classification (affects the whole tab group → closer to `NativeTabGroup`).

## EXPAND — Candidates (finalized 2026-07-03)

Consolidated from Riff (8 options) + Flux (5 reframings) into 5 directions, with the user selecting.

- **B. Offscreen downscaled thumbnail approach (survives → to CHALLENGE)**: render each tab full-size to an offscreen texture → downscale-blit into the tile layout. A pixel-faithful, Exposé-style live mirror. Requires a new blit pipeline.
- **Display location: dedicated window/tab (finalized)**: assumes a monitoring use case shown alongside the working tab.

### Considered but rejected (dropped by the user in EXPAND)
- **A. Virtual-split clip approach** (reusing draw_panes, 1:1 clip): least effort, but doesn't produce a miniature overview — rejected in favor of pixel-faithful overview.
- **C. Small-font second-atlas approach**: highest effort due to duplicated shaping + atlas.
- **D. Badge + activity ranking approach** (Flux badges-first): furthest from the JTBD of "leave-it-open monitoring via tiles."
- **E. Adaptive hybrid approach**: maintenance cost of three code paths.

### Points carried to CHALLENGE (Flux's counterarguments)
- Glyphs are unreadable below ~8pt — does fidelity at small tile sizes become wasted cost (can this be absorbed by tile-size/tab-count-cap design)?
- Live monitoring means N offscreen passes/frame in GPU cost. Can dirty-row diffing (WP4, 5524a1a) be used to limit tile redraws to active tabs?

## CHALLENGE — Ruling (2026-07-03, Magi+Void+Ripple+Omen)

### Magi ruling (4 design decisions)
1. **Fidelity**: full resolution is overkill (unreadable below ~8pt). Render at downscaled resolution (~2x tile size) + **grid cap of ~9-12 tabs**, with anything beyond that degrading to paging/activity icons. (3-0, confidence 78) *(the cap was later finalized as ⚠B=9, and the degradation was finalized as ⚠F=placeholder row.)*
2. **Update strategy**: dirty-gate (reusing WP4 dirty-row diffing) + **rate cap of ~10-15Hz**. Neither continuous redraw nor fixed-cycle-only was adopted. (3-0, 82)
3. **Texture strategy**: render offscreen at **downscaled resolution (~2x tile size)** rather than full window resolution (decoupling VRAM from tab count and main resolution). (converged, 75) *(partially overridden later by ⚠A: reusing a single shared full-resolution scratch texture decouples VRAM from tab count, but decoupling from main resolution is deferred to future optimization option (ii). See L2 ⚠A rationale.)*
4. **Window model**: the dedicated window sits **outside the native tab group**. When focused, only the overview-specific keymap (move/select/switch/close) applies, with no PTY input pass-through. (3-0, 80)

### Void scope (v1)
- **KEEP**: click-to-focus / tile labels (tab title) / launch keybinding + native menu item / defined degenerate cases for 1-tab and all-tabs-closed / self-exclusion of the overview itself (structurally free).
- **CUT**: keyboard navigation between tiles / activity badges / sort-by-activity / resize / config knobs for update interval / in-tile scrollback / multiple overviews.
- **DEFER**: remembering window position / reproducing split panes inside a tile (v1 shows the whole tab as one image).
- **FLAG**: **no precedent in Ghostty** — this repository's first deliberate exception to the faithful-clone philosophy. Must be documented in L0.

### Ripple impact analysis — risk 6/10 (upper MEDIUM), Conditional-Go
- Favorable: `Renderer::draw`/`draw_panes` are already surface-less (they take a `&wgpu::TextureView`) — **render-to-texture is possible with zero core changes**. New work is a quad-blit pipeline (~150-250 LOC, `CellPipeline` as a template).
- Required mitigations: (1) thumbnails must **reuse each tab's existing Renderer** (N new Renderers would duplicate the atlas N times and cold-start the dirty-cache). (2) The offscreen texture must use the **same** `TextureFormat` as each tab's surface (the non-sRGB/gamma math from 4e2fd7f is fixed at construction time). (3) exclude the overview WindowId from `window_order`/`tab_group_identifier`. (4) explicit `CommandScope` handling while overview is focused (terminal commands must cleanly no-op). (5) add a headless test for the blit path to `noa-render/tests/pipeline.rs`. (6) respect/extend occlusion-aware redraw suppression rather than bypassing it (it directly conflicts with saving GPU on background tabs).
- Unresolved plumbing: there is no fan-out path for the overview to learn "pane X was updated" (`pane_render_cache` is private to noa-render) — new plumbing is required.
- Files with the largest blast radius: `crates/noa-app/src/app.rs` (WindowState/window_order/CommandScope/redraw path), `noa-render/src/{renderer,pipeline}.rs`, `noa-render/tests/pipeline.rs`, `noa-app/src/macos_menu.rs`.

### Omen premortem (16 modes, none S≥9, top ones are GPU + concurrency)
Top 5 mitigations baked into the spec:
1. **Gate** the new pipeline's bind-group visibility / uniform layout with a **headless real-GPU test** (must run un-sandboxed — the sandbox skips GPU tests).
2. **Input-latency NFR**: while overview is displayed, keystroke-to-echo for the focused tab must not gain more than +Xms of extra latency. No unconditional per-frame locking of all N terminals (dirty-gate + a cap on concurrent renders).
3. **Threading discipline**: overview must not reach across into another window's Renderer/device state (hand off via texture copies of each tab's own render output).
4. **Cap + degrade**: define a max tile count and VRAM budget, degrading to placeholder/low-res when exceeded. Document the device-lost/surface-lost recovery path.
5. **Lifecycle ACs**: ACs + tests for frames during tab closure / app-quit ordering / Spaces & fullscreen. Document the parity exception in L0.

### Design tension (carried to SPECIFY)
- An implementation choice remains between Magi's "draw directly at ~2x tile resolution" and the current "fixed cell size (no scale uniform)": (i) full-resolution offscreen + GPU downscale-blit (simple, expensive) vs. (ii) introduce a scale into the projection matrix/uniform to draw directly at downscaled resolution (cheap, needs a new uniform). To be finalized in L2.

### User decisions (2026-07-03)
- **Finalized as B′**: downscaled thumbnails with mitigations (downscaled-resolution render + dirty-gate + 10-15Hz cap + dedicated window outside the tab group + all 6 Ripple mitigations + all 5 Omen mitigations, all turned into spec requirements).
- **No scope revival**: Void's minimal v1 stands (CUT/DEFER items remain future increments).

## SHAPE — Proposal (provisionally adopted → reaffirmed by 2026-07-03 LOCK sign-off)

### Proposed solution (B′)
- **Window lifecycle**: a dedicated NSWindow launched via keybinding + native menu (outside the tab group, excluded from `window_order`/`tab_group_identifier`). Degenerate display is defined for 1-tab and all-tabs-closed cases. The overview itself is structurally excluded from what it monitors.
- **Tile grid + cap/degradation**: 1 tab = 1 tile, grid cap of **⚠B=9 (3×3)**. Layout is `cols=ceil(sqrt(N))`, `rows=ceil(N/cols)` (N≤9), where the last row can have fewer tiles but every tile is the same size (N=5,7,8 leave empty cells at the end — the invariant is this equal-size grid, not "gapless tiling"). Anything beyond 9 degrades to **⚠F=no paging, title-only placeholder row** (full tiles go to the most-recently-focused top 9 tabs). VRAM uses **⚠A=a single shared full-resolution scratch texture (one) + N tile-sized textures**, independent of tab count (only one full-resolution texture exists). It is not independent of main resolution (the shared scratch scales with the main window resolution).
- **Update pipeline**: dirty-gate (reusing WP4) → **⚠G=10Hz throttle (min_interval=100ms, 10-15Hz is the acceptable tuning band, a compile-time constant with no config knob)** → only dirty tabs are drawn to the shared scratch using each tab's existing Renderer → quad-blit downscales into that tile's texture → tiles are composited. No new Renderer is created per tab.
- **Interaction**: click-to-focus. While overview is focused, only its dedicated keymap (move/select/switch/close) applies, with no PTY input pass-through.

### In scope (v1)
click-to-focus / tile labels (tab title) / keybinding + menu item / degenerate cases / self-exclusion + **turning 11 mitigations into requirements** (Ripple's 6 + Omen's 5, see CHALLENGE section).

### Out of scope
- CUT: keyboard navigation between tiles, activity badges, sort-by-activity, config knobs, in-tile scrollback, multiple overviews.
- DEFER: remembering window position, reproducing split panes inside a tile (v1 shows the whole tab as one image).
- Non-goals: PTY input pass-through, operating the terminal via the overview.

### Assumptions
WP4 dirty-row diffing can be reused as the activity signal source / the renderer's surface-less property (taking a `&wgpu::TextureView`) is preserved / macOS only / **no Ghostty precedent = a deliberate exception to the faithful-clone philosophy (must be documented in L0)**.

### Questions carried to SPECIFY (all resolved in SPECIFY → see Open Questions ⚠ section)
1. Render strategy: (i) vs (ii) → finalized as **⚠A=option (i′) single shared scratch** (pending LOCK).
2. The exact grid cap number (somewhere in 9-12) → finalized as **⚠B=9 (3×3)** (pending LOCK).
3. Update notification fan-out approach → finalized as **⚠C=reuse the existing `UserEvent::Redraw`** (pending LOCK).
4. The ms value for the input-latency NFR → finalized as **⚠D=≤2ms (non-gating smoke) + an observable gate as a hermetic unit test** (addresses F3) (pending LOCK).
5. L0 wording for the Ghostty parity exception → finalized as **⚠E**, reflected in L0.

## SPECIFY — L1/L2/L3

- scope mode: **Full** (21 requirements = 10 functional + 11 non-functional; the 11 CHALLENGE mitigations become requirements; new GPU pipeline + new window model = high complexity).
- Priority tags: **[MH]** = must-have (v1 blocker), **[NH]** = nice-to-have (can follow up within a WP).
- Verification tags: **[unit]** `cargo test -p <crate>` / **[headless]** `noa-render/tests/pipeline.rs` (real GPU, skips if no adapter, **must run un-sandboxed**) / **[inspection]** static inspection of types/structure (compile boundary via private field / non-pub type — a violation is a compile error, or `cargo tree` / code review) / **[visual]** manual visual check / **[manual]** manual operational check (non-gating smoke). The CLAUDE.md pty (openpty=un-sandboxed) / GPU (sandbox skips headless) constraints apply.

### L1 — Requirements

#### Functional (REQ-OV-*)

- **REQ-OV-1** [MH]: Toggle the Session Overview's dedicated window shown/hidden via a dedicated keybinding + native menu item (a toggle command isomorphic to `ToggleSplitZoom`, following the established `KeybindEngine`/`AppCommand`/`macos_menu.rs` pattern).
- **REQ-OV-2** [MH] (Ripple mitigation 3): Create Overview as a dedicated NSWindow outside the native tab group, excluded from `window_order`/`tab_group_identifier` (app.rs:196,201,220). The overview itself must never appear in tab cycling, tab-count, or tab-group tabbing.
- **REQ-OV-3** [MH]: Lay out monitored tabs as a grid of 1 tab = 1 tile. Grid cap is **⚠B=9 (3×3)**. The layout invariant (N≤9) is an **equal-size grid** with `cols=ceil(sqrt(N))`, `rows=ceil(N/cols)` — all tiles are the same size, and only the last row may have fewer than `cols` tiles (N=5,7,8 leave empty cells at the end). "Gapless tiling" isn't required, since under the equal-size constraint it only holds exactly for N=1,2,3,4,6,9. The requirement is: "all tiles are the same size, row-major order, only the last row may be short, tiles never overlap."
- **REQ-OV-4** [MH]: Each tile is a downscaled live mirror of the target tab's screen content. While Overview is displayed, any tile whose target tab's output changes is updated live (the core requirement of a monitoring dashboard).
- **REQ-OV-5** [MH]: Each tile displays that tab's title as a label (so a tab can be identified by content + title together).
- **REQ-OV-6** [MH]: Clicking a tile moves focus to that tab (click-to-focus). Following the `hit_test` precedent in `split_tree.rs`, resolve click point → tile → WindowId.
- **REQ-OV-7** [MH] (Ripple mitigation 4 / Magi decision 4): Treat two scopes as distinct. (a) The launch command `ToggleTabOverview` fires under **`CommandScope::NativeTabGroup`** (works from any tab, affects the whole tab group, isomorphic to `ToggleSplitZoom`). (b) Dispatch while Overview is focused resolves under a new **`CommandScope::Overview`**, which only handles the Overview-specific keymap (dismiss) with **no PTY input pass-through**. Terminal-related `AppCommand`s cleanly no-op under `CommandScope::Overview`. (a) and (b) are distinct concerns and don't conflict.
- **REQ-OV-8** [MH]: Overview itself is structurally excluded from what it monitors (Overview's own WindowId is never in the tab set fed into the grid). This follows automatically from the exclusion in REQ-OV-2.
- **REQ-OV-9** [MH] (Omen mitigation 5): Define degenerate cases — 0/1 monitored tabs, all tabs closed, a target tab closed while Overview is displayed (tile removal + relayout, no panic), app-quit ordering, and closing the last tab. This includes display under Spaces / fullscreen.
- **REQ-OV-10** [MH] (Omen mitigation 4 / ⚠F): Define degradation when the grid cap is exceeded (>9 tabs) — **v1 has no paging**. Full tiles go to the **most-recently-focused top 9 tabs**; the rest degrade to a **title-only placeholder tile row** (no live mirror, title only) (⚠F, pending LOCK sign-off). When the VRAM budget is exceeded, degrade to low-resolution/placeholder per the injected budget-flag (paired with REQ-NF-8). Paging = tile navigation was CUT, so it isn't adopted in v1.

#### Non-Functional (REQ-NF-*)

- **REQ-NF-1** [MH] (Ripple mitigation 1): Each tile's rendering **reuses the `Renderer` the target tab already owns**. It must not create a new `Renderer` per tab (N new Renderers would duplicate the atlas N times and cold-start the dirty-cache).
- **REQ-NF-2** [MH] (Ripple mitigation 2): The tile's offscreen texture must be created with the **same** `TextureFormat` as the target tab's surface (because the non-sRGB / gamma math from 4e2fd7f is fixed at `CellPipeline` construction time — renderer.rs:120,214 `target_format_is_srgb`).
- **REQ-NF-3** [MH] (⚠A): The downscale composite uses a **new quad-blit pipeline**. Each tab draws unmodified via its existing `Renderer` into a **single shared full-resolution scratch texture** (draw tab k → downscale-blit into its small tile texture → reuse the same scratch for tab k+1), sampled with a linear filter and downscaled into the tile rectangle (`CellPipeline` as a template, ~150-250 LOC). Only **one** full-resolution texture exists at a time (independent of tab count). The std140 layout of `Uniforms` (instance.rs:54) / `populate_pane_uniform` is left untouched.
- **REQ-NF-4** [MH] (Magi decision 2 / Omen mitigation 2 / ⚠G): Tile updates pass through **dirty-gate (reusing WP4 dirty-row diffing) → rate throttle**. The throttle defaults to **10Hz (min_interval=100ms)**, with 10-15Hz as the acceptable tuning band, as a **compile-time constant** (no config knob, ⚠G). Unconditional per-frame Mutex-lock-and-redraw of all N terminals is prohibited. Clean tabs are not redrawn. The number of terminals locked in one Overview frame is limited to tabs that are both dirty and past their interval (capped at K).
- **REQ-NF-5** [MH] (⚠D / Omen mitigation 2): While Overview is displayed, the focused tab's input responsiveness must not degrade. The **observable gate** is that an Overview frame (a) does not lock clean tabs, and (b) never exceeds a per-frame cap K on concurrent offscreen renders, verified via a hermetic unit test with an injected clock (AC-NF-05). The **≤ 2ms** added keystroke-to-echo latency is downgraded to a **non-gating manual smoke note** measured on real GPU (since it can't be observed rigorously by hand).
- **REQ-NF-6** [MH] (Omen mitigation 3): Threading discipline — Overview must not reach across into another window's `Renderer`/`Device` state. Handoff occurs only via texture copies of each tab's own render output.
- **REQ-NF-7** [MH] (Ripple mitigation 6): Respect/extend the existing occlusion-aware redraw suppression (app.rs:2092-2110 `TargetedRedrawDecision`) rather than bypassing it (it directly conflicts with saving GPU on background tabs).
- **REQ-NF-8** [MH] (Omen mitigation 4 / ⚠A): Define the VRAM budget model as **1 shared full-resolution scratch texture (proportional to the main window resolution) + N tile-sized textures** (full resolution does not scale with tab count; tile textures are small). Define the max tile count (⚠B=9) and a VRAM budget; when an injected budget-flag exceeds it, degrade to placeholder/low-resolution (paired with REQ-OV-10). Document the `device-lost`/`surface-lost` recovery path (regenerating the shared scratch + tile textures) and ensure it doesn't crash.
- **REQ-NF-9** [MH] (Ripple mitigation 5 / Omen mitigation 1): Add a headless real-GPU test for the blit path to `noa-render/tests/pipeline.rs`. Verify the new pipeline's bind-group `visibility` and uniform layout (CLAUDE.md GPU gotcha: vertex-stage sampling needs `VERTEX_FRAGMENT`, std140 alignment), and that one frame draws with no wgpu validation errors. **Must run un-sandboxed** (the sandbox skips GPU tests).
- **REQ-NF-10** [MH] (⚠C): The "tab X was updated" fan-out **reuses the existing `UserEvent::Redraw(WindowId, PaneId)` path** (io_thread.rs:128 → app.rs:1027). Overview maintains a per-tab dirty-set from the same redraw signal. No new event variant, and no exposure of renderer-private state. Respect the crate dependency rules — everything at `noa-grid` and below stays GUI-agnostic (winit stays confined to `noa-app`).
- **REQ-NF-11** [MH]: `cargo test --workspace` and `cargo clippy --workspace` stay green after adding Overview.

> Must-ratio note: all 21 requirements are [MH]. This feature centers on mitigation requirements directly tied to the monitoring dashboard's safety, concurrency, and GPU correctness (11 of them user-confirmed during CHALLENGE); demoting any to [NH] in v1 would invite a degraded UX, so all are deliberately [MH] (matching the precedent set by locked splits/rendering-improvements specs).

### L2 — Detail

#### noa-app (window model / CommandScope / event wiring)

- **Window model**: Create Overview as an independent `WindowState` (or a lightweight dedicated `OverviewWindow`), excluded from `window_order`/`tab_group_identifier` (REQ-OV-2). Don't attach `with_tabbing_identifier` (app.rs:600) at creation. Tab cycling (app.rs:948-959) and the CloseTab path pass through the Overview WindowId transparently.
- **CommandScope (distinguish two, REQ-OV-7)**: add a new **`CommandScope::Overview`** to the existing `CommandScope { FocusedTab, NativeTabGroup, App }` (app.rs:2116).
  - **Launch scope**: `AppCommand::ToggleTabOverview` (tentative) fires under `CommandScope::NativeTabGroup` and is wired into `KeybindEngine`/`menu_id`/`macos_menu.rs` (isomorphic to `ToggleSplitZoom`, toggles from any tab).
  - **Overview-focused scope**: dispatch while Overview is focused resolves under `CommandScope::Overview`. Terminal commands cleanly no-op at the `resolve_command_target` stage; only dismiss (toggle key/Esc) is handled. PTY input, IME, selection, copy, etc. are not passed through.
- **Fan-out (⚠C)**: Overview keeps a per-tab dirty-set (equivalent to `HashMap<WindowId, bool>`). When the app processes `UserEvent::Redraw(window_id, pane_id)` (app.rs:1027) and Overview is open, mark that tab's tile dirty and request a throttled Overview redraw. Extend the existing Redraw wiring rather than adding a new event variant. `noa-grid` stays GUI-agnostic (REQ-NF-10).
- **click-to-focus**: Overview's `WindowEvent` mouse coordinates → the pure function `hit_test_overview_grid(&[(WindowId, TileRect)], point) -> Option<WindowId>` (precedent: `split_tree.rs` hit_test) → `focus`/`order_front` on that tab.
- **Degradation/lifecycle (REQ-OV-9)**: on target-tab closure, remove the entry from the dirty-set and grid and relayout (stale-WindowId no-ops follow the tab precedent). Define behavior for 0/1 tabs, all tabs closed, app-quit ordering, and Spaces/fullscreen.
- **Pure-function seams (unit-testable without GPU/Window)**:
  - `compute_overview_grid(tab_count, bounds, cap=9) -> OverviewLayout { tiles: Vec<TileRect>, placeholders: Vec<TileRect>, overflow: bool }` — an equal-size grid with `cols=ceil(sqrt(n))`, `rows=ceil(n/cols)` (n=min(tab_count,cap)). All `TileRect`s are the same size, row-major, only the last row may be short, no overlap (REQ-OV-3). When tab_count>cap, the top 9 become `tiles` and the rest become a title-only `placeholders` row with `overflow=true` (REQ-OV-10, ⚠F).
  - `overview_command_scope(AppCommand) -> CommandScope` (REQ-OV-7, terminal commands → no-op under `CommandScope::Overview`).
  - `should_render_tile(dirty, last_render_at, now, min_interval) -> bool` — decides dirty-gate + throttling with an **injected clock**. Clean→false, dirty but `now - last_render_at < min_interval`→false, otherwise→true (REQ-NF-4/5, ⚠G min_interval=100ms).
  - `overview_redraw_decision(...)` (REQ-NF-7, extends `TargetedRedrawDecision`).

#### noa-render (offscreen + blit pipeline)

- **Offscreen rendering (⚠A decision = option (i′) = single shared scratch)**: each tab's existing `Renderer` draws **unmodified** in sequence into **one full-resolution scratch `TextureView` shared by all tabs** (`draw`/`draw_panes` already take a `&wgpu::TextureView` and are surface-less. renderer.rs:326,345). Right after drawing, downscale-blit into that tab's small tile texture, then reuse the same scratch for the next dirty tab. The scratch matches the target tab's surface `TextureFormat` (REQ-NF-2). → Only one full-resolution texture exists at a time (independent of tab count).
- **Blit pipeline (new)**: a new `BlitPipeline` (`CellPipeline` as a template) samples the shared scratch with a linear-filtered `Sampler` and draws a downscaled quad into that tab's tile texture (sized to the grid tile rectangle). Bind group = color texture + sampler (fragment-only sampling → `visibility = FRAGMENT`; only use `VERTEX_FRAGMENT` if vertex-stage `textureDimensions` is introduced. CLAUDE.md gotcha). `Uniforms`/std140 are left unchanged (REQ-NF-3).
- **⚠A rationale (an explicit partial override of Magi decision 3)**: Magi decision 3 ruled to "draw directly at ~2x tile downscaled resolution, decoupling VRAM from both tab count **and main resolution**," but `Uniforms` (instance.rs:54-66) has no scale term and is std140-locked (CLAUDE.md GPU gotcha), and `cell_size` drives the glyph atlas sampling, so introducing a scale uniform wouldn't actually lower re-rasterization cost — it would only introduce a risk of silent std140 drift. We therefore **partially override** Magi #3: option (i′) reuses each tab's Renderer unmodified with zero core changes, adding only the blit (matching Ripple's finding that "render-to-texture is possible with zero core changes"). With the **single shared scratch**, VRAM is "1 full-resolution texture + N tile-sized textures," **independent of tab count** (this part of Magi #3's goal is achieved). However, since the shared scratch scales with main window resolution, it is **not** independent of main resolution (this is the delta from Magi #3 = the scope of the override). One full-resolution texture is acceptable on Apple Silicon's unified memory (precedent: splits). **Fallback = option (ii)** (introducing a scale uniform to draw directly at downscaled resolution, independent of main resolution too) is held in reserve as a future optimization if VRAM/perf prove unacceptable.
- **Update pipeline**: dirty-gate (reusing WP4) → 10-15Hz throttle → only dirty tabs get an offscreen redraw (via existing Renderer) → blit composites the tile (REQ-NF-4). A per-frame cap on concurrent offscreen draws protects input latency (REQ-NF-5).
- **Threading discipline (REQ-NF-6)**: Overview must not reach across into another window's `Renderer`/`Device` fields; handoff is only via texture copies (offscreen output). Respect macOS's constraint that presenting happens on the window-owning thread.
- **Recovery (REQ-NF-8)**: on `device-lost`/`surface-lost`, regenerate the offscreen textures. On VRAM budget overrun, degrade to low-resolution/placeholder (REQ-OV-10).
- **Testing (REQ-NF-9)**: add a headless case for the blit path to `noa-render/tests/pipeline.rs` — on a real adapter, a one-frame downscale-blit completes with `pop_error_scope() == None`, verifying bind-group visibility / uniform layout. Must run un-sandboxed.

#### noa-font / noa-pty / noa-grid

- **noa-font / noa-pty**: unchanged. Reuse the atlas (shared singleton) and Pty as-is.
- **noa-grid**: stays GUI-agnostic (REQ-NF-10). The dirty signal stays limited to reusing the existing WP4 dirty-row mechanism, without introducing winit/wgpu.

### L3 — Acceptance Criteria

- **AC-OV-01** (REQ-OV-1) [MH] [unit]+[manual] — Given Overview is not displayed, When `ToggleTabOverview` is dispatched, Then the overview-visible state flag flips to true, and re-dispatching flips it back to false (unit, a state transition that needs no Window); Given multiple tabs are open, When the launch keybinding or menu item fires, Then the dedicated Overview window actually shows/hides (manual).
- **AC-OV-02** (REQ-OV-2) [MH] [unit] — Given a window set that includes the Overview WindowId, When evaluating `window_order` cycling, tab count, and tab-group enumeration, Then Overview's WindowId appears in none of them, and no tabbing identifier is attached to it.
- **AC-OV-03** (REQ-OV-3) [MH] [unit] — Given `compute_overview_grid(tab_count, bounds, cap=9)`, When 1 ≤ tab_count ≤ 9, Then it returns tab_count `TileRect`s in an equal-size grid with `cols=ceil(sqrt(tab_count))`, `rows=ceil(tab_count/cols)` — all tiles the same size, row-major, only the last row possibly short, no overlap (N=5,7,8 may leave trailing empty cells; strict gaplessness is not required); When tab_count > 9, Then it returns 9 tiles + `overflow=true` + a set of placeholders.
- **AC-OV-04** (REQ-OV-4) [MH] [headless]+[visual] — Given a real adapter renders one tile and captures its pixel hash, When the target tab's content changes and it redraws, Then the tile's pixel hash changes (unchanged content → unchanged hash) (headless, un-sandboxed lane); Given a noisy tab and a static tab shown in Overview, When observed over multiple frames, Then the active tab's tile updates live and the static tile does not change (visual).
- **AC-OV-05** (REQ-OV-5) [MH] [unit]+[visual] — Given tabs with known titles, When Overview is built, Then each tile's label maps to the corresponding tab's title (unit) / titles are visible on inspection (visual).
- **AC-OV-06** (REQ-OV-6) [MH] [unit]+[manual] — Given `TileRect`s returned by `compute_overview_grid`, When `hit_test_overview_grid(&[(WindowId, TileRect)], point)` is evaluated at an interior point of tile k, Then it returns tab k's `WindowId`, and returns `None` outside tile boundaries, in placeholder rows, and in empty cells (unit, directly verifying the inverse mapping); Given Overview is displayed with multiple tiles, When a tile is clicked, Then the corresponding tab is focused and brought forward (manual).
- **AC-OV-07** (REQ-OV-7) [MH] [unit]+[manual] — Given Overview is focused, When each terminal-related `AppCommand` is dispatched, Then it resolves to a no-op via `overview_command_scope` (unit); When text is typed, Then it never reaches any pty (manual).
- **AC-OV-08** (REQ-OV-8) [MH] [unit] — Given the enumeration of the tab set fed to the grid, When evaluated, Then Overview's own WindowId is not included.
- **AC-OV-09a** (REQ-OV-9) [MH] [unit] — Given a set with 0 monitored tabs, 1 monitored tab, or all tabs already closed, When `compute_overview_grid` + the supplied tab set are evaluated, Then it returns an empty or single-tile layout, or no-ops, without panicking (0 tabs = empty grid, all closed = empty).
- **AC-OV-09b** (REQ-OV-9) [MH] [unit] — Given a target tab is closed while Overview is displayed, When that WindowId is removed from the dirty-set and grid, Then tile removal + relayout completes and stale WindowId references become no-ops (following the tab precedent).
- **AC-OV-09c** (REQ-OV-9) [MH] [headless]+[inspection] — Given Overview's shared scratch + tile textures + blit pipeline have been created on a real adapter, When dropped in the specified order (Overview resources → tab Renderers → Device), Then teardown completes with no wgpu validation error or panic (headless, un-sandboxed lane); additionally, code review confirms the app-quit path's drop ordering follows the specified order (inspection). No hermetic unit oracle exists (a real Device is required), so [unit] is not claimed.
- **AC-OV-09d** (REQ-OV-9) [MH] [manual] — Given Spaces / fullscreen, When Overview is displayed, Then it displays correctly in each environment (visual check).
- **AC-OV-10** (REQ-OV-10) [MH] [unit] — Given tab_count > cap=9, When `compute_overview_grid` is evaluated, Then full tiles go to the top 9 tabs and the rest degrade to title-only `placeholders` with `overflow=true` (no paging in v1, ⚠F); Given an injected `budget_exceeded` flag, When the degradation decision is evaluated, Then it degrades to low-resolution/placeholder (with the flag injection made explicit).
- **AC-NF-01** (REQ-NF-1) [MH] [unit]+[inspection] — Given an injected `Renderer::new` call counter on Overview's offscreen render path, When N tabs' tiles are drawn, Then the counter's delta is 0 (no new `Renderer` is created; existing ones are reused) (unit); additionally, code review confirms `Renderer::new` never appears in Overview's type seam (inspection).
- **AC-NF-02** (REQ-NF-2) [MH] [unit] — Given a tab's surface `TextureFormat`, When that tab's offscreen texture is created, Then its format matches the target surface's.
- **AC-NF-03** (REQ-NF-3) [MH] [headless] — Given the blit pipeline and an offscreen texture, When a downscale-blit into one tile is executed, Then one frame completes with no wgpu validation error (`pop_error_scope() == None`). **Un-sandboxed run**.
- **AC-NF-04** (REQ-NF-4) [MH] [unit] — Given `should_render_tile(dirty, last_render_at, now, min_interval)` (default min_interval=100ms=10Hz, acceptable band 1/15..1/10 s, compile-time constant, ⚠G), When the tab is clean, Then false; When dirty but under the interval, Then false (deferred); When dirty and past the interval, Then true. No unconditional per-frame locking of all N terminals may occur.
- **AC-NF-05** (REQ-NF-5) [MH] [unit]+[manual] — Given `should_render_tile(dirty, last_render_at, now, min_interval)` with an **injected clock**, When the tab is clean, Then false; When dirty and `now - last_render_at < min_interval`, Then false; When dirty and past the interval, Then true (unit). Given a gate evaluation for one Overview frame across N tabs (some clean), When the frame is constructed, Then the number of terminals locked matches tabs that are both dirty and past their interval, clean tabs are never locked, and concurrent offscreen renders never exceed cap K (lock-count assertion, unit). The ≤2ms added keystroke-to-echo latency is a **non-gating manual smoke** measurement (real GPU, un-sandboxed, informational).
- **AC-NF-06** (REQ-NF-6) [MH] [inspection] — Given other windows' `Renderer`/`Device` state is encapsulated as private fields / non-pub types, When Overview's compositing path is designed to accept only other windows' texture copies as input, Then attempting to write cross-window access is a compile error (oracle = the compiler). Visibility is enforced via private fields / non-pub types, making any handoff other than texture copies impossible by type.
- **AC-NF-07** (REQ-NF-7) [MH] [unit] — Given an occluded/background tab, When `overview_redraw_decision` (extending `TargetedRedrawDecision`) is evaluated, Then the existing occlusion suppression is respected and Overview's path does not bypass it.
- **AC-NF-08a** (REQ-NF-8) [MH] [unit] — Given an injected `budget_exceeded` flag, When the VRAM degradation decision is evaluated, Then it degrades to low-resolution/placeholder (VRAM model = 1 full-resolution texture + N tile textures, ⚠A).
- **AC-NF-08b** (REQ-NF-8) [MH] [unit]+[headless]+[manual] — Given an **injected `device-lost`/`surface-lost` event**, When the regeneration-needed decision function is evaluated, Then it transitions to `regen_required=true` without panicking (the decision itself needs no GPU, so it's a unit test); Given a real adapter, When the recovery routine runs, Then the shared scratch + tile textures are regenerated via `device.create_texture` and state returns to valid (headless, un-sandboxed lane); confirm no crash under real GPU manual triggering (non-gating manual smoke).
- **AC-NF-09** (REQ-NF-9) [MH] [headless] — Given the blit case in `noa-render/tests/pipeline.rs`, When drawn on a real adapter, Then bind-group visibility and uniform layout are validated and one frame completes with no validation error. Skipped in sandbox, **gated on un-sandboxed runs**.
- **AC-NF-10** (REQ-NF-10) [MH] [unit] — Given `UserEvent::Redraw(window_id, pane_id)` is processed while Overview is displayed, When that tab updates, Then its tile is marked dirty in the dirty-set; When `cargo tree` is inspected, Then `noa-grid`/`noa-vt` have no dependency on `wgpu`/`winit`.
- **AC-NF-11** (REQ-NF-11) [MH] [unit]+[headless] — Given Overview's full test suite, When `cargo test --workspace` and `cargo clippy --workspace` run, Then both are green with no `#[ignore]` beyond the documented pty sandbox constraint, and the headless pipeline tests are also green.

### Traceability — REQ ↔ AC (bidirectional)

| REQ | AC | Priority |
|---|---|---|
| REQ-OV-1 | AC-OV-01 | MH |
| REQ-OV-2 | AC-OV-02 | MH |
| REQ-OV-3 | AC-OV-03 | MH |
| REQ-OV-4 | AC-OV-04 | MH |
| REQ-OV-5 | AC-OV-05 | MH |
| REQ-OV-6 | AC-OV-06 | MH |
| REQ-OV-7 | AC-OV-07 | MH |
| REQ-OV-8 | AC-OV-08 | MH |
| REQ-OV-9 | AC-OV-09a, AC-OV-09b, AC-OV-09c, AC-OV-09d | MH |
| REQ-OV-10 | AC-OV-10 (+AC-NF-08a) | MH |
| REQ-NF-1 | AC-NF-01 | MH |
| REQ-NF-2 | AC-NF-02 | MH |
| REQ-NF-3 | AC-NF-03 | MH |
| REQ-NF-4 | AC-NF-04 | MH |
| REQ-NF-5 | AC-NF-05 | MH |
| REQ-NF-6 | AC-NF-06 | MH |
| REQ-NF-7 | AC-NF-07 | MH |
| REQ-NF-8 | AC-NF-08a, AC-NF-08b | MH |
| REQ-NF-9 | AC-NF-09 | MH |
| REQ-NF-10 | AC-NF-10 | MH |
| REQ-NF-11 | AC-NF-11 | MH |

**Coverage: 21/21 requirements trace to ≥1 AC = 100%** (Full-scope minimum ≥95%). Total ACs: **25** (all [MH]; reverse direction: every AC states its originating REQ explicitly). Breakdown: AC-OV-01..08 (8) + AC-OV-09a/b/c/d (4) + AC-OV-10 (1) + AC-NF-01..07 (7) + AC-NF-08a/b (2) + AC-NF-09/10/11 (3). Mapping of the 11 mitigations: Ripple mitigations 1-6 → REQ-NF-1/REQ-NF-2/REQ-OV-2/REQ-OV-7/REQ-NF-9/REQ-NF-7; Omen mitigations 1-5 → REQ-NF-9/REQ-NF-5(+REQ-NF-4)/REQ-NF-6/REQ-NF-8(+REQ-OV-10)/REQ-OV-9(+L0 parity exception).

### Quality Gate Run 1 (FAIL) — fix log

Responses to every finding from Run 1, where Judge + Attest returned FAIL (one line each).

- **F1 (BLOCKER)**: Resolved the dual authority over render strategy. Changed ⚠A to **reusing a single shared full-resolution scratch** (option i′), recorded as an **explicit partial override** of Magi decision 3 (independent of tab count, not independent of main resolution). Updated SHAPE / REQ-NF-3 / REQ-NF-8 (VRAM model) / L2 noa-render consistently.
- **F2 (MAJOR)**: Defined `compute_overview_grid`'s row/column allocation as an equal-size grid with `cols=ceil(sqrt(N))`,`rows=ceil(N/cols)`, and rewrote REQ-OV-3 / AC-OV-03 as the invariant "all tiles the same size, only the last row possibly short, no overlap" (withdrawing the blanket gapless claim).
- **F3 (MAJOR)**: Downgraded the ≤2ms [manual] in REQ-NF-5 / AC-NF-05 to non-gating smoke, replaced with an observable hermetic unit test (gate logic + lock-count).
- **F4 (MINOR)**: Unified overflow handling as ⚠F=no paging, title-only placeholder row (full tiles for the top 9 tabs). Reflected in REQ-OV-10 / AC-OV-10.
- **F5 (MINOR)**: Clearly separated the two scopes: launch `ToggleTabOverview`=`CommandScope::NativeTabGroup` and Overview-focused=new `CommandScope::Overview` (REQ-OV-7 / L2).
- **F6 (MINOR)**: Finalized the throttle as ⚠G=10Hz (min_interval=100ms) compile-time constant, 10-15Hz acceptable band, no config knob (REQ-NF-4 / AC-NF-04 / AC-NF-05).
- **F7 (MINOR)**: Retagged AC-NF-06 as a compile boundary [inspection] (compile error via private/non-pub), and AC-NF-01 as a `Renderer::new` counter [unit]+[inspection] (defined the [inspection] tag in the legend).
- **F8 (MINOR)**: Updated SHAPE's "~9-12" to ⚠B=9, and added override notes for ⚠B/⚠F/⚠A to CHALLENGE Magi decisions #1/#3.
- **Attest AC-NF-05**: rewrote entirely as a hermetic [unit] (injected clock + lock-count, clean tabs never locked), with 2ms as manual smoke.
- **Attest AC-OV-01**: added [unit] verifying that overview-visible state flips on `ToggleTabOverview` (the window itself remains [manual]).
- **Attest AC-OV-04**: added a real-adapter pixel-hash [headless] oracle (hash differs before/after a content change).
- **Attest AC-OV-09**: split into 09a (0/1/all-closed) / 09b (relayout on closure while displayed) / 09c (quit-teardown ordering) / 09d (Spaces/fullscreen manual).
- **Attest AC-NF-08**: split into 08a (VRAM degradation, injected budget-flag) / 08b (device-lost recovery, regen via injected lost-event).
- **Attest AC-OV-10**: made the injected budget-flag explicit.
- **Bookkeeping**: total AC count 21→**25**, updated traceability table/coverage row (100% maintained), added ⚠F/⚠G to Open Questions.

### Quality Gate Run 2 (PASS) — 2026-07-03

- **Judge**: PASS on all 5 dimensions (0 remaining BLOCKER/MAJOR). Verified in-text that F1-F8 were resolved; grounding of new code citations confirmed (`Uniforms` has no scale term = the ⚠A rationale is backed by actual code).
- **Attest**: OK 22 / RISK 3 / FAIL 0 (CONDITIONAL). The remaining 3 RISK items were resolved directly by Nexus after this run:
  - AC-OV-06: attached [unit] directly to the `hit_test_overview_grid` inverse mapping ([manual] only → [unit]+[manual]).
  - AC-OV-09c: no hermetic unit oracle exists (requires a real Device), so retagged [unit]→[headless]+[inspection].
  - AC-NF-08b: split into 3: decision [unit] / regeneration [headless] / manual trigger [non-gating manual smoke].
- **Verification lane tally (Attest)**: sandboxed cargo-test lane ~18 ACs / un-sandboxed real-GPU lane 4-6 ACs (NF-03/NF-09/OV-04/OV-09c/NF-08b/NF-11 half-headless) / pure manual 2 (OV-09d, half of OV-06's manual) — **OV-09d and each manual half are excluded from the automated loop's DONE gate and explicitly documented at LOCK as a "manual-verified sign-off slot"** (to avoid stalling the loop).

**Gate verdict: PASS (LOCK precondition satisfied)** — satisfies both testable L3 ACs (Attest) and the 5-dimension quality gate (Judge).

## Open Questions / Deferred Decisions — ⚠A–⚠G (all recommended options approved at 2026-07-03 LOCK)

- **⚠A Render strategy = option (i′) single shared full-resolution scratch + GPU downscale blit (provisional, a partial override of Magi decision 3)**: `Uniforms` (instance.rs:54) has no scale term and is std140-locked, and `cell_size` drives atlas sampling, so a scale uniform alone wouldn't lower re-rasterization cost — it would only leave a silent-drift risk. Option (i′) reuses each tab's Renderer unmodified with zero core changes; only the blit is added. **All tabs reuse a single full-resolution scratch in sequence** (draw tab k → blit into its tile texture → reuse for tab k+1), so VRAM = "1 full-resolution texture + N tile-sized textures." **Partial override of Magi decision 3**: "VRAM independent of tab count" is achieved (only one full-resolution texture); "also independent of main resolution" is not (the shared scratch scales with main resolution) = the scope of the override. One full-resolution texture is acceptable on Apple Silicon's unified memory. Fallback = option (ii), a scale-uniform direct-draw approach (also independent of main resolution). *Approved at LOCK (2026-07-03).*
- **⚠B Grid cap = 9 (3×3) (provisional)**: 3×3 is the only cap candidate that tiles strictly gaplessly, is VRAM-conservative, and gives the simplest degradation boundary (⚠F). Overflow degrades per REQ-OV-10. *Approved at LOCK (2026-07-03).*
- **⚠C Update fan-out = reuse the existing `UserEvent::Redraw(WindowId, PaneId)` path (provisional)**: Overview maintains a per-tab dirty-set from the existing signal at io_thread.rs:128 → app.rs:1027. No new event variant, no exposure of renderer-private state. `noa-grid` and below stay GUI-agnostic (winit stays confined to `noa-app`), respecting the crate dependency rules. *Approved at LOCK (2026-07-03).*
- **⚠D Input-latency NFR = ≤ 2ms (provisional)**: the cap on added keystroke-to-echo latency for the focused tab. Imperceptible to humans and measurable. Guaranteed by dirty-gate + 10-15Hz throttle + a cap on concurrent offscreen renders, measured on real GPU un-sandboxed. *Approved at LOCK (2026-07-03).*
- **⚠E Ghostty parity exception wording (finalized, reflected in L0)**: "Session Overview has no counterpart in Ghostty; it is this repository's first deliberate departure from the faithful-clone philosophy, and is recorded in L0 as out of scope for Ghostty parity checking." *Approved at LOCK (2026-07-03).*
- **⚠F Overflow policy (>9 tabs) = no paging, title-only placeholder row (provisional)**: v1 does not adopt paging (tile navigation = already CUT). Full tiles go to the most-recently-focused top 9 tabs; the rest degrade to a title-only placeholder row (no live mirror). Reflected in REQ-OV-10/AC-OV-10. *Approved at LOCK (2026-07-03).*
- **⚠G Update throttle = 10Hz (min_interval=100ms), 10-15Hz acceptable band, compile-time constant (provisional)**: adopts a 10Hz default, treats 10-15Hz as an acceptable tuning band, with no config knob (compile-time constant). Reflected in REQ-NF-4/AC-NF-04/AC-NF-05. *Approved at LOCK (2026-07-03).*
- **(ongoing) Representation of split tabs**: focused pane only vs. downscaled reproduction of the split layout — DEFER (v1 shows the whole tab as one image, consistent with Void's DEFER).

## v2 — Mockup Parity

### Metadata (v2)
- trigger: a target UI mockup image the user provided (2026-07-04). The requirements in this section are not based on the agent directly viewing the image, but are specced from the requester's verbalized observations, taken as authoritative.
- scope mode: **Standard addendum** (REQ-OV-11..17, 7 functional + REQ-NF-12..13, 2 non-functional = 9 requirements, 16 ACs).
- Continuation policy: v1's invariants (equal size, row-major, non-overlapping, REQ-OV-3) and scope boundaries (Void CUT/DEFER, L0 parity exception) are preserved and not overridden. This section **supplements** v1's requirements (including resolving REQ-OV-5's shortfall) — it does not reuse v1's REQ/AC numbers.
- Grounding: all current-state descriptions in this section are based on code investigation of the `feat/tab-overview-v2` worktree (2026-07-04). The primary references now are `crates/noa-app/src/session_overview/` (the pure-function layer), `crates/noa-app/src/app/overview/` and `app/input_ops/` (rendering/input wiring), and `crates/noa-app/src/command_palette.rs` (a reference implementation of filtering). The paths and line numbers below are kept as evidence from that point in time.

### L0 — Vision delta (v2)
- v1 made "leave-it-open monitoring dashboard" its primary purpose, keeping only click operation (Void scope, lines 90-102) and cutting inter-tile keyboard nav, activity badges, reordering, etc. The mockup the user provided newly requests part of that cut keyboard nav (arrow-key movement, Cmd+1-9 direct switching), plus title bars, close buttons, a search filter, and a hint bar, none of which v1 implemented. This does not retract v1's Void CUT — it is treated as a **user-directed, explicit scope revival** as a v2 addition.
- Inventory of current gaps (backed by code, individually specced below):
  - Title bars are only drawn on placeholder rows (`render_overview_placeholder_labels`, `app.rs:1563-1623`, invoked at `app.rs:1573`'s `overview_tile_labels`); live tiles (`render_due_overview_tiles`, `app.rs:1468-1526`) have no title-drawing path at all — v1's REQ-OV-5/AC-OV-05 is **effectively unmet** (resolved by REQ-OV-12).
  - `overview_command_scope` (`app.rs:3561-3587`) classifies almost all `AppCommand`s (including `SelectTab`/`CloseTab`/`NextTab`/`PrevTab`/`CloseWindow`) as `CommandScope::Overview`, and `handle_app_command` (`app.rs:551-552`) uniformly no-ops that scope. There is no dedicated key-handling function for arrow/Enter/Esc (equivalent to `handle_search_prompt_key`/`handle_command_palette_key`) — effectively only "dismiss via re-toggle" works (newly established by REQ-OV-14/15).
  - `compute_overview_grid` (`tab_overview.rs:65-107`) and `rect_at` (`tab_overview.rs:224-233`) add no gutter or margin at all, so tiles are packed edge-to-edge with zero gap (extended by REQ-OV-11).
  - There is no UI or hit-test target corresponding to a close (✕) button (newly established by REQ-OV-13).
  - There is no tab search filter, but there's already a hand-rolled non-contiguous subsequence match in `command_palette.rs:139` (`command_palette_filter`) / `command_palette.rs:151` (`is_subsequence_ci`). However the mockup's "Search sessions" is a title **substring** match, a different semantic — so while the pattern is a useful reference, this is specced as a new function (REQ-OV-16).
  - `redraw_overview` (`app.rs:1710-1745`) calls `present_overview_frame` (`app.rs:1727`) unconditionally every time regardless of whether the frame content changed, and if `backlog_remains` (`app.rs:1737-1741`, dirty tiles remain whose throttle hasn't yet elapsed), it immediately requests another frame (`app.rs:1742-1744`). During the throttle wait window (100ms default), it repeats composite+present every frame with nothing actually changed — this is the known bug referenced in the request (resolved by REQ-NF-12).
  - `render_due_overview_tiles` (`app.rs:1493-1496`) locks the target tab's `surface.terminal` directly and calls `FrameSnapshot::from_terminal`. Because this goes through `Screen::take_visible_rows_with_damage` (`noa-render/src/snapshot.rs:114`, a consuming "take"), Overview can potentially steal damage that the normal tab's own redraw path should have consumed (resolved by REQ-NF-13).

### Non-Goals (v2)
- Background blur wallpaper (decorative background around tile perimeters).
- Paging / tile navigation (v1's Void CUT continues; REQ-OV-10's title-only placeholder-row degradation is preserved).
- Drag-to-reorder tiles.
- Animated transitions (open/close, selection movement, and filter-triggered relayout are all instant, no transition).

### SPECIFY — v2 L1/L2/L3

The verification-tag and priority-tag legend is inherited from this file's SPECIFY section at line 118 ([MH]/[NH], [unit]/[headless]/[inspection]/[visual]/[manual]).

#### L1 — Requirements (v2 additions)

##### Functional (REQ-OV-11..17)

- **REQ-OV-11** [MH]: Extend `compute_overview_grid` to accept a gutter (fixed spacing between tiles) and an outer margin (spacing between the grid and the Overview window boundary) — e.g. `gutter: u32, margin: u32` arguments, or an `OverviewLayoutParams` struct. v1's invariant (REQ-OV-3: all tiles the same size, row-major, only the last row possibly short, no overlap) is preserved, and `gutter=0, margin=0` must **bit-for-bit match** v1's edge-to-edge layout (regression safety).
- **REQ-OV-12** [MH]: Show a title bar on every tile (both live mirrors and placeholder rows). The title bar is a band at the top of the tile, centered on that tab's title. The placeholder side can directly reuse the existing `overview_tile_labels` (`tab_overview.rs:180-192`), but the live-tile side needs new work since no path currently calls this function there (add a compositing call inside `render_due_overview_tiles`). This requirement **satisfies v1 REQ-OV-5 / AC-OV-05's effectively-unmet status, in title-bar form**.
- **REQ-OV-13** [MH]: Show a ✕ close button at the right end of the title bar. Clicking it closes that tab, removing its tile and relaying out the grid (this reuses REQ-OV-9's degenerate path for "target tab closed while Overview is displayed" as-is — only the trigger source changes, from outside the pty to the user's ✕ click; the subsequent removal/relayout/stale-reference-no-op contract is identical). The ✕'s hit test is resolved as a region separate from the tile's focus hit test (`hit_test_overview_grid`, `tab_overview.rs:113-118`), returning a distinct close target from a click on the tile body.
- **REQ-OV-14** [MH]: Introduce a selection model. Exactly one tile in Overview's live grid (row-major, REQ-OV-3/11) holds "selected" state at a time. Selection is visualized with a blue focus ring (glow). The initial selection when Overview opens is the focused tab's tile if it's in the live tile set, otherwise the first tile (index 0).
- **REQ-OV-15** [MH]: Keyboard navigation. The items below are the concrete form of "the Overview-specific keymap (move/select/switch/close)" referenced at lines 94 and 158 of this file, and require a new Overview-specific key-handling path (inserted into `KeyboardInput` pre-emptive routing, isomorphic to `handle_search_prompt_key`/`handle_command_palette_key`). The current blanket no-op via `overview_command_scope` (`app.rs:3561-3587`) coexists with this new path, with no-op now limited to terminal `AppCommand`s only.
  - (a) Arrow keys (↑↓←→) move the selected tile within the row-major grid. Clamped at grid edges, no wrap. Placeholder-row tiles are also selectable (see (b)).
  - (b) Return confirms focus on the selected tile's tab. If the selection is a live tile, it reuses the existing `focus_tab_from_overview` (`app.rs:1765-1775`). If the selection is on a **placeholder row, it must also be selectable, and Return focuses that tab too** (placeholders have only a title with no content mirror, but the target tab is real and must remain reachable).
  - (c) Cmd+1..9 switches directly to the currently live tile at position N (1-indexed, Nth in row-major order). Reuse the existing `AppCommand::SelectTab(n)` (`commands.rs:26`) and its keybinding (`commands.rs:363-371`), but while Overview is focused, resolve it via **REQ-OV-15's dedicated path**, excluded from `overview_command_scope`'s no-op set (in v1, `SelectTab` is a no-op under `CommandScope::Overview`, app.rs:3582).
  - (d) Esc closes Overview (equivalent to `hide_tab_overview`, `app.rs:1331`). Focus does not move to any tab.
- **REQ-OV-16** [MH]: A "Search sessions" field at top center filters the live tile set by **case-insensitive substring** match against tab titles, immediately relaying out the grid against the filtered results (passing only the filtered set of source ids into `compute_overview_grid` after the REQ-OV-11 extension). Implement containment as a new case-insensitive substring-match function — `command_palette.rs:151`'s `is_subsequence_ci` is a **non-contiguous subsequence** match with different semantics, so it is not directly reused (only its implementation pattern, e.g. case-folding and scanning, serves as reference, not a direct call). Each time the filter string changes, reset the selection index to 0 (aligning with command-palette.md R-7's `selected = 0` reset pattern). This concretizes REQ-OV-7's "printable input while Overview is focused doesn't go to PTY" by specifying that printable input goes to this search field.
- **REQ-OV-17** [MH]: Show a static hint bar at bottom center. Content is "⌘1-N to switch・↑↓←→ to navigate・Return to open・esc to close", with **N replaced by the current live tile count (min(tab count, 9))** (the mockup's "⌘1-6" was a concrete example matching the tab count=6 at the time the user provided it; to stay consistent with the real system's Cmd+1..9 range (`OVERVIEW_GRID_CAP=9`, `tab_overview.rs:11`), use a dynamic N rather than the fixed literal "1-6" — noted here as an ambiguous+reversible design call). The hint bar itself has no input response (display only).

##### Non-Functional (REQ-NF-12..13)

- **REQ-NF-12** [MH]: Overview's compositing/present must not run at full speed on frames where no tile update is due. Currently, `redraw_overview` (`app.rs:1710-1745`) calls `present_overview_frame` (`app.rs:1727`) unconditionally regardless of whether the frame content changed, and as long as any tile is dirty but not yet past its throttle (`backlog_remains`, `app.rs:1737-1741`), it immediately requests another frame (`app.rs:1742-1744`). The result is a known bug where, during the throttle wait window (100ms default, ⚠G), it repeats composite+present every frame (at display refresh rate) even though nothing changed. Fix contract: if zero tiles were actually drawn this frame (`due_window_ids` is empty) and there's no layout/selection/search-filter change either, do not call `present_overview_frame`. Even when a dirty backlog remains, schedule the next redraw request for the throttle deadline (when `min_interval` elapses) rather than requesting immediately every unchanged frame.
- **REQ-NF-13** [MH] (a strengthening of REQ-NF-6): Overview's render path must never lock a normal tab's `Terminal` Mutex. Currently, `render_due_overview_tiles` (`app.rs:1493-1496`) directly takes `surface.terminal.lock()` and calls `FrameSnapshot::from_terminal`, which goes through `Screen::take_visible_rows_with_damage` (`noa-render/src/snapshot.rs:114`, a consuming "take"), and can steal damage that the tab's own redraw path should have consumed. Fix contract: the io thread publishes a read-only snapshot per tab (an acquisition path that **does not consume** damage), and Overview's render path reads only from that. Overview must not directly lock any tab's `Terminal` Mutex.

> Must-ratio note: all 9 v2 additions are [MH]. The mockup is a target UI the user explicitly provided, and UI fidelity (title bar/close button/focus ring/search/hint bar) plus correct keyboard nav are the core purpose of this section, so no [NH] demotion is made (consistent with v1's all-[MH] precedent).

#### L2 — Detail (v2)

##### noa-app / tab_overview.rs (extending the pure layer)

- Extend `compute_overview_grid`'s signature to `compute_overview_grid(tab_count: usize, bounds: TileRect, cap: usize, gutter: u32, margin: u32) -> OverviewLayout` (or a bundled `OverviewLayoutParams`). Add gutter/margin offset math to `rect_at` (`tab_overview.rs:224-233`), subtracting `(cols-1)*gutter + 2*margin` etc. from the `tile_w`/`tile_h` computation (`tab_overview.rs:89-90`) (REQ-OV-11). Add a regression test confirming existing call sites, which pass `gutter=0, margin=0`, preserve v1 behavior.
- New pure function `move_overview_selection(selected: usize, cols: usize, tile_count: usize, direction: Direction) -> usize` (REQ-OV-15a, edge-clamped, non-wrapping movement on the row-major grid, no Window/GPU needed). Either reuse `Direction` (the existing `FocusDirection`, see `commands.rs`) or introduce a dedicated enum.
- New pure function `overview_tab_filter(query: &str, titles: &[(Id, String)]) -> Vec<Id>` (REQ-OV-16, case-insensitive substring match, implemented independently while referencing `command_palette.rs`'s case-folding pattern).
- `overview_tile_labels` (`tab_overview.rs:180-192`) requires no change itself — only the call site (app.rs) needs to change so both live tiles and placeholder rows call it (REQ-OV-12).
- New pure function `overview_close_hit_test(&[(Id, TileRect)], point) -> Option<Id>` (REQ-OV-13, dedicated to the close-button rectangle at the title bar's right end. Kept separate from `hit_test_overview_grid` (`tab_overview.rs:113-118`), which takes a different set of input rectangles, so the existing function is left unchanged).

##### noa-app / app.rs (window events, command wiring, rendering)

- **Overview-specific key handling (REQ-OV-15)**: introduce `handle_overview_key(event_loop, window_id, event)`, isomorphic to `handle_search_prompt_key`/`handle_command_palette_key`, delegated to from `overview_window_event`'s (`app.rs:1777-1815` area) `KeyboardInput` branch. Arrow → update selection via `move_overview_selection` + redraw; Return → resolve the selected tile's Id and call `focus_tab_from_overview` (live) or the equivalent focus path (placeholder); Esc → call the equivalent of `hide_tab_overview()`; printable character → append to the search query + recompute `overview_tab_filter` + reset selection to 0 + redraw.
- **Direct resolution of Cmd+1..9 (REQ-OV-15c)**: ahead of `overview_command_scope`'s (`app.rs:3561-3587`) `AppCommand::SelectTab(_) => CommandScope::Overview` arm, which currently blanket no-ops it, add a branch (via the `handle_overview_key`-equivalent path) that intercepts `SelectTab(n)` and directly focuses the Nth live tile (the no-op classification in `overview_command_scope` remains unchanged for other terminal commands).
- **✕ close button (REQ-OV-13)**: in `overview_window_event`'s `MouseInput` handling (`app.rs:1797` area, the same event consumed by `focus_overview_tile_at_last_cursor`), first check the close-button region via `overview_close_hit_test`; on a hit, invoke REQ-OV-9's closure path (equivalent to `close_tab`) for that tab; on a miss, fall through to the existing `focus_overview_tile_at_last_cursor`.
- **Title-bar drawing (REQ-OV-12)**: inside `render_due_overview_tiles`'s (`app.rs:1468-1526`) loop, overlay-composite the tab title from `overview_tile_labels` onto the top band of each tile (share the `label_renderer` used by `render_overview_placeholder_labels`, repurposed for simple label-row compositing on live tiles — no new GPU pipeline needed).
- **Focus ring (REQ-OV-14)**: in `present_overview_frame`'s (`app.rs:1628-1692`) tile-compositing loop (`app.rs:1683-1688`), add a blue ring (glow) overlay drawn only over the selected tile's rectangle.
- **Due-gating of present (REQ-NF-12)**: modify `redraw_overview` (`app.rs:1710-1745`) to skip the `present_overview_frame` call (`app.rs:1727`) when `due_window_ids.is_empty()` and there's no layout/selection/search change. Change the next-redraw request driven by `backlog_remains` (`app.rs:1742-1744`) from an immediate request to one scheduled against the earliest tile's throttle deadline.
- **Damage-non-consuming snapshot (REQ-NF-13)**: replace `render_due_overview_tiles`'s (`app.rs:1468-1526`) `surface.terminal.lock()` + `FrameSnapshot::from_terminal` (`app.rs:1493-1496`) with a read of a read-only snapshot published by the io thread. This requires adding a non-consuming peek path to `Terminal`, distinct from the damage-consuming path (`take_visible_rows_with_damage`, `noa-render/src/snapshot.rs:114`) (concrete design left to the Builder during noa-grid/noa-render implementation).

##### noa-render / noa-grid

- Draw the title bar, focus ring, and hint bar entirely within the existing cell/overlay compositing pipeline (an extension of the same grid-aligned modal drawing pattern used for search_prompt/command palette), adding no new bind-group/uniform layout (does not expand what `noa-render/tests/pipeline.rs` needs to verify).
- Add the damage-non-consuming peek path (REQ-NF-13) to `noa-grid` (`Screen`). `noa-grid` stays GUI-agnostic (no winit/wgpu introduced, continuing v1's REQ-NF-10 constraint).

#### L3 — Acceptance Criteria (v2 additions)

- **AC-OV-11** (REQ-OV-11) [MH] [unit] — Given the extended `compute_overview_grid(tab_count, bounds, cap, gutter, margin)`, When `gutter=0, margin=0`, Then the returned `OverviewLayout` matches every tile rectangle from v1's `compute_overview_grid(tab_count, bounds, cap)` exactly (regression); When `gutter>0` or `margin>0`, Then all live tiles stay the same size, the spacing between adjacent tiles matches `gutter`, the spacing between the grid and the `bounds` boundary matches `margin`, and the row-major/non-overlap invariant (REQ-OV-3) is preserved.
- **AC-OV-12** (REQ-OV-12) [MH] [unit]+[visual] — Given live tiles and known tab titles, When the compositing call equivalent to `render_due_overview_tiles` is evaluated, Then the title-bar row's compositing input receives that tab's title (unit); Given multiple live tabs shown in Overview on a real GUI, Then each live tile visibly shows a title bar with its own title at the top (visual) — confirming v1 REQ-OV-5/AC-OV-05's shortfall (placeholder-only drawing) is resolved.
- **AC-OV-13** (REQ-OV-13) [MH] [unit]+[manual] — Given a tile rectangle and the close-button region at the title bar's right end, When `overview_close_hit_test` is evaluated at a point inside the tile body vs. inside the close-button region, Then the tile-body point returns `None` (handled by the normal `hit_test_overview_grid`), and the close-button point returns that tab's close target (unit); Given a real GUI, When ✕ is clicked, Then that tab closes, its tile is removed, and the grid relays out (manual, reusing REQ-OV-9's degenerate path).
- **AC-OV-14** (REQ-OV-14) [MH] [unit] — Given the focused tab is in the live tile set when Overview opens, When the initial selection is evaluated, Then the selection index points to the focused tab's tile; Given the focused tab is not in the live tile set (e.g., it's on the overflow side), When the initial selection is evaluated, Then the selection index is 0; in either case exactly one tile is selected.
- **AC-OV-15a** (REQ-OV-15a) [MH] [unit] — Given `cols`/`tile_count` and a selection index, When `move_overview_selection` is evaluated in each direction, Then it clamps at grid edges (no wrap), and movement across the short trailing row's missing cells never returns an out-of-range index.
- **AC-OV-15b** (REQ-OV-15b) [MH] [unit]+[manual] — Given the selected tile is a live tile, When Return is processed, Then the equivalent of `focus_tab_from_overview` is invoked with the selected tab's WindowId (unit); Given the selected tile is a **placeholder row**, When Return is processed, Then focus is likewise resolved onto that tab (unit, verifying placeholders are also selectable); Given a real GUI, Then Return actually moves focus to the highlighted tab (manual).
- **AC-OV-15c** (REQ-OV-15c) [MH] [unit] — Given Overview is focused with N live tiles, When `Cmd+k` is dispatched (1≤k≤min(N,9)), Then it bypasses `overview_command_scope`'s no-op and directly focuses the row-major kth live tile's tab; When `k>N`, Then it's a no-op (no switch to a nonexistent tile, no panic).
- **AC-OV-15d** (REQ-OV-15d) [MH] [unit]+[manual] — Given Overview is displayed, When Esc is processed, Then Overview becomes hidden with no focus change to any tab (unit: dispatch recording); Given a real GUI, Then Esc closes Overview and returns to the original tab view (manual).
- **AC-OV-16a** (REQ-OV-16) [MH] [unit] — Given `overview_tab_filter(query, titles)`, When query `"log"` is applied to mixed-case titles (e.g. `"Build Log"`, `"logs-worker"`, `"README"`), Then only titles containing `query` as a case-insensitive **contiguous substring** (`"Build Log"`, `"logs-worker"`) are returned in order, and `"README"` is excluded (regression that clarifies the distinction from non-contiguous subsequence matching: a non-contiguous query like `"lg"` does not hit).
- **AC-OV-16b** (REQ-OV-16) [MH] [unit] — Given a search query change, When grid relayout is evaluated, Then only the filtered set of source ids is passed to `compute_overview_grid`, and the selection index is reset to 0.
- **AC-OV-16c** (REQ-OV-16) [MH] [unit]+[manual] — Given Overview is focused with the search field active, When a printable character is typed, Then it's appended to the query and never reaches any pty (unit: routing branch; manual: confirm no bytes reach the pty).
- **AC-OV-17** (REQ-OV-17) [MH] [unit]+[visual] — Given a live tile count N (=min(tab count,9)), When the hint-bar string is built, Then `"⌘1-N to switch・↑↓←→ to navigate・Return to open・esc to close"`'s N matches the actual live tile count; Given a real GUI, Then the hint bar displays at bottom center (visual).
- **AC-NF-12** (REQ-NF-12) [MH] [unit] — Given exactly one tile is dirty but not yet past its throttle (`should_render_tile=false`), with nothing else changed, When that frame's `redraw_overview`-equivalent decision logic is evaluated, Then `present_overview_frame` is not called (frame content unchanged), and the next redraw request is scheduled against the throttle deadline rather than requested immediately (this locks in the fix as a regression test against the current unconditional-present + immediate-re-request bug at `app.rs:1710-1745`).
- **AC-NF-13** (REQ-NF-13) [MH] [unit]+[inspection] — Given Overview's tile render path, When fetching a target tab's current content, Then it reads only from the io thread's published read-only, damage-non-consuming snapshot, never directly locking that tab's `Terminal` Mutex (unit: verifying publish/read are independent paths); code review confirms no direct `surface.terminal.lock()` / `FrameSnapshot::from_terminal` call remains in the `render_due_overview_tiles`-equivalent code (inspection).

### Traceability — v2 additions (REQ ↔ AC)

| REQ | AC | Priority |
|---|---|---|
| REQ-OV-11 | AC-OV-11 | MH |
| REQ-OV-12 | AC-OV-12 | MH |
| REQ-OV-13 | AC-OV-13 | MH |
| REQ-OV-14 | AC-OV-14 | MH |
| REQ-OV-15 | AC-OV-15a, AC-OV-15b, AC-OV-15c, AC-OV-15d | MH |
| REQ-OV-16 | AC-OV-16a, AC-OV-16b, AC-OV-16c | MH |
| REQ-OV-17 | AC-OV-17 | MH |
| REQ-NF-12 | AC-NF-12 | MH |
| REQ-NF-13 | AC-NF-13 | MH |

**Coverage: v2's 9/9 added requirements trace to ≥1 AC = 100%** (v2 adds **16** ACs total, all [MH]). Combined v1+v2 coverage for this file: 30/30 requirements = 100%, 41 ACs total.

## v3 — Overflow Reachability (Paging)

### Metadata (v3)
- trigger: user instruction (2026-07-11). Once the 9-tab cap is exceeded, v1/v2's title-only placeholder row (REQ-OV-10, ⚠F) makes full tiles (live mirrors) unreachable — this improves reachability so that every tab can be equally used as part of the monitoring dashboard.
- scope mode: **Standard addendum** (REQ-OV-18..20, 3 functional + REQ-NF-14, 1 non-functional = 4 requirements).
- Continuation policy: v1's invariants (equal size, row-major, non-overlapping, no overlap, REQ-OV-3) and `compute_overview_grid`'s signature (including REQ-OV-11's gutter/margin extension) are left unmodified. This section only adds a paging layer one level above deciding "which set of tabs gets passed to `compute_overview_grid`" when the 9-tab count is exceeded — the grid computation itself still works exactly as before as long as it's given one page (≤9 tabs).
- **Superseded decisions**: REQ-OV-10 (⚠F, degradation to a title-only placeholder row) is **retired** in v3 — tabs beyond 9 are no longer stored in a placeholder row; they become reachable as full tiles (live mirrors) via additional pages. The judgment in v1's Void CUT (lines 90-102) and v2's Non-Goals (lines 313-317) that "paging / tile navigation is CUT/a Non-Goal" is likewise explicitly revived and overridden by the user in v3. REQ-NF-8 (degradation on VRAM budget overrun via `budget_exceeded`, AC-NF-08a) is **kept** — paging is a question of "which set of tabs to show," an independent concern from the degradation path for insufficient rendering resources within one page (≤9).
- Grounding: implementation research is based on `crates/noa-app/src/session_overview/{input.rs, text.rs}` and `crates/noa-app/src/app/{state.rs, overview/{layout.rs, interaction.rs, render.rs, lifecycle.rs}}` (2026-07-11).

### L0 — Vision delta (v3)
- v1/v2 crammed anything beyond 9 tabs into a "title visible, content invisible" placeholder row. Monitoring the 10th tab and beyond still required manually cycling through tabs, breaking L0's JTBD (lines 24-28) — "show all tabs' screen content as tiles and monitor them live, left open, at once" — once you crossed 9 tabs. v3 resolves this: **no matter how many pages it spans, every tab is reachable as a full tile**.
- Design: discrete paging. Split the filtered tab set (`App::overview_source_tile_ids()`) into pages of `OVERVIEW_GRID_CAP` (9) each, and pass only the current page's slice into the existing `compute_overview_grid`. **Every page consists solely of full tiles — no page ever shows a placeholder row** (REQ-OV-18).

### Non-Goals (v3)
- Continuous scroll / animated page transitions (page switches are instant, continuing v1/v2's "no transitions" policy).
- Jumping directly to a page number (e.g., a "go to page 3" command) — only relative movement via PageUp/PageDown, wheel, or Cmd+[/Cmd+].
- Changes to the io thread / `decide_overview_publish` — paging is purely an app-side layer choosing which tab set to show, and doesn't touch REQ-NF-13's (damage-non-consuming snapshot publishing) wiring.

### SPECIFY — v3 L1/L2/L3

The verification-tag and priority-tag legend is inherited from this file's SPECIFY section at line 118 ([MH]/[NH], [unit]/[headless]/[inspection]/[visual]/[manual]).

#### L1 — Requirements (v3 additions)

##### Functional (REQ-OV-18..20)

- **REQ-OV-18** [MH] (supersedes REQ-OV-10/⚠F): Split the filtered tab set into pages of `OVERVIEW_GRID_CAP` each (page count = `ceil(filtered_len / OVERVIEW_GRID_CAP)`, minimum 1 — even 0 items yields "page 1/1"). Pass only the current page's slice (`source_tile_ids[page*9 .. min(len, page*9+9)]`) into the unmodified `compute_overview_grid`. As a result, **no page ever has a placeholder row** (each slice's length is always ≤9, so `compute_overview_grid`'s overflow judgment is never reached). Page navigation happens via PageUp/PageDown keys, the Cmd+[ / Cmd+] chord, and a mouse-wheel/trackpad accumulated threshold (part of REQ-OV-15's keymap). Like grid edges, page edges also clamp and never wrap. Page navigation resets the selection to the first tile (index 0, page-local) (paired with REQ-OV-20, consistent with the search-reset precedent). ⌘1-9 (REQ-OV-15c) resolves **page-locally** (the Nth live tile on the current page).
- **REQ-OV-19** [MH]: Append a "Page p/N" segment to the bottom hint bar (REQ-OV-17) only when there are 2 or more pages (`p` is the 1-indexed current page, `N` is the total page count). When there is only 1 page, the hint bar's wording is unchanged from v1/v2 (no regression).
- **REQ-OV-20** [MH] (extends REQ-OV-16): A search-query change resets the page to 0 (paired with the existing "reset selection to 0" — REQ-OV-16 — since the page structure itself changes when the filtered tab count changes).

##### Non-Functional (REQ-NF-14)

- **REQ-NF-14** [MH] (a consequence of REQ-NF-1/REQ-NF-4/REQ-NF-5): Both dirty-gate render-candidate selection (`due_overview_tile_ids`) and backlog decisions (`overview_backlog_decision`) target **only the current page's slice** — tabs on hidden pages never become GPU render candidates until that page is actually switched to (preserving REQ-NF-1's goal of independence from tab count, even after paging). Page navigation itself is handled via the existing `mark_all_overview_tiles_dirty` + a single `request_overview_redraw()` call (this simply reaffirms REQ-NF-12's contract of "never unconditionally present on a non-due frame" — no new present path is added). The io thread's publish path / `decide_overview_publish` are left unmodified (see Non-Goals).

> Must-ratio note: all 4 v3 additions are [MH]. Reachability beyond the 9-tab cap is directly tied to the dashboard's practical usability, and demoting any to [NH] would only ratify a degraded UX (v1/v2's known unreachable region), so this is kept consistent with v1/v2's all-[MH] precedent.

#### L2 — Detail (v3)

- **paging seam**: `App::overview_page_view(&self) -> OverviewPageView { slice, page, page_count, selected_in_page }` (`app/overview/layout.rs`). Calls the existing memoized `App::overview_source_tile_ids()` (keyed on `(unfiltered order, query)` as before — **page is not part of the memo key**), clamps `OverviewWindowState.page` via `clamp_overview_page`, then slices. All existing interaction/rendering consumers (`focus_overview_tile_at_last_cursor`, `overview_close_target_at_last_cursor`, `step_overview_selection`, `activate_overview_selection`, `switch_to_live_overview_tile`, `update_overview_hover`, `redraw_overview`, `present_overview_frame`) are repointed to read this page slice.
- **Pure functions** (`session_overview/input.rs`): `overview_page_count(len, page_size)` / `overview_page_slice_range(len, page_size, page)` / `clamp_overview_page(page, len, page_size)` / `page_step(page, direction, len, page_size)` / `page_after_wheel(page, wheel_accum, delta_y, len, page_size) -> (new_page, new_wheel_accum)` (accumulated threshold `WHEEL_PAGE_THRESHOLD`, a compile-time constant with no config knob, consistent with the ⚠G-style precedent).
- **State** (`app/state.rs`): add `page: usize` and `wheel_accum: f32` to `OverviewWindowState`. Add `page: usize` to `OverviewPillKey` (the shared cache key for the search/hint pills) — since the hint bar's "Page p/N" segment changes on page navigation alone, this correctly invalidates the cache.
- **Input** (`session_overview/input.rs`): add `OverviewAction::{PageForward, PageBack}`, resolved in `overview_key_action` for PageUp/PageDown (no Cmd) and Cmd+[ / Cmd+] (following the same "no other modifiers" discipline as the existing Cmd+digit).
- **Hint bar** (`session_overview/text.rs`): `overview_hint_bar_text`/`_ascii`/`_compact`/`overview_hint_bar_row` take `page`/`page_count` as additional arguments, appending "Page p/N" only when `page_count > 1`.
- **Spec-level supersession**: REQ-OV-10/⚠F (degradation to a title-only placeholder row) becomes an unreachable path under v3's paging layer — `compute_overview_grid` itself is unmodified, so the overflow/placeholder logic still exists in code (and v1's direct-call regression test continues to verify it), but as long as it's routed through v3's paging layer, `live_cap = min(len, 9) = len` (slice length is always ≤9) always holds, so `overflow` never occurs.

#### L3 — Acceptance Criteria (v3 additions)

- **AC-OV-18a** (REQ-OV-18) [MH] [unit] — Given `overview_page_count(len, page_size)`, When `len=0,9,10,25` (`page_size=9`), Then it returns `1,1,2,3` respectively (minimum 1 page even for 0 items).
- **AC-OV-18b** (REQ-OV-18) [MH] [unit] — Given `overview_page_slice_range(len, page_size, page)` called across every page spanning `len`, When the resulting ranges are concatenated, Then they cover `0..len` exactly once each with no overlap or gap (non-overlapping, non-missing slice partition).
- **AC-OV-18c** (REQ-OV-18) [MH] [unit] — Given `clamp_overview_page(page, len, page_size)`, When `page` exceeds the total page count, Then it clamps to the last page.
- **AC-OV-18d** (REQ-OV-18) [MH] [unit] — Given `page_step(page, direction, len, page_size)`, When stepping back on the first page / forward on the last page, Then it clamps without wrapping.
- **AC-OV-18e** (REQ-OV-18) [MH] [unit] — Given `page_after_wheel`, When the accumulation is below `WHEEL_PAGE_THRESHOLD`, Then the page stays unchanged and only the accumulator increases; When it crosses the threshold, Then it advances exactly 1 page, carrying the excess to the next call (a single call never advances more than one page, even with a huge delta — so a single trackpad gesture can't skip multiple pages); When crossing at an edge, Then the page doesn't move and the accumulator resets to 0 (so a swipe pinned at the edge doesn't misfire on direction reversal).
- **AC-OV-18f** (REQ-OV-18) [MH] [unit] — Given each page's slice returned by `App::overview_page_view()`, When passed into `compute_overview_grid`, Then the returned `OverviewLayout.placeholders` is always empty and `overflow` is always `false` (no page ever shows a placeholder row).
- **AC-OV-19** (REQ-OV-19) [MH] [unit] — Given `overview_hint_bar_text(n, page, page_count)`, When `page_count<=1`, Then the wording matches v1/v2 exactly (regression); When `page_count>1`, Then a 1-indexed "Page p/N" is appended at the end.
- **AC-OV-20** (REQ-OV-20) [MH] [unit] — Given a search-query change, When `set_overview_search_query` is evaluated, Then both the page and the selection reset to 0.
- **AC-NF-14** (REQ-NF-14) [MH] [unit] — Given a tab set spanning multiple pages, When a tab on a non-current page becomes dirty and `due_overview_tile_ids`/`overview_backlog_decision` are evaluated, Then tabs on hidden pages never appear as candidates (only the current page's slice is considered).

### Traceability — v3 additions (REQ ↔ AC)

| REQ | AC | Priority |
|---|---|---|
| REQ-OV-18 | AC-OV-18a, AC-OV-18b, AC-OV-18c, AC-OV-18d, AC-OV-18e, AC-OV-18f | MH |
| REQ-OV-19 | AC-OV-19 | MH |
| REQ-OV-20 | AC-OV-20 | MH |
| REQ-NF-14 | AC-NF-14 | MH |

**Coverage: v3's 4/4 added requirements trace to ≥1 AC = 100%** (v3 adds **9** ACs total, all [MH]). Combined v1+v2+v3 coverage for this file: 34/34 requirements = 100%, 50 ACs total.
