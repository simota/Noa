# Splits

## Metadata

- slug: splits
- feature title: Split Panes (Ghostty-style splits)
- status: locked
- owner: noa maintainers
- current phase: LOCKED
- build-path decision: orbit loop (engine: codex) — see Build-path
  decision section
- quality gate: round 1 FAIL (Judge: 11 must-fix; Attest: 9 AC
  rewrites) → all fixes applied in the SPECIFY fix round; round 2
  **PASS**. Lock preconditions met: testable L3 ACs (Attest) + Spec
  Quality Gate (Judge).
- LOCK sign-off (user-confirmed): all four provisional decisions
  approved — SHAPE proposal adoption; per-pane transform = N uniform
  buffers + N bind groups; window-wide font-size change (recorded
  Ghostty deviation, REQ-SPL-19); split-at-floor no-op (REQ-SPL-1).
- SHAPE checkpoint: proposal adopted provisionally (user away),
  **confirmed by user at LOCK**.
- L2 mechanism decision (**confirmed at LOCK**): per-pane transform =
  **N uniform buffers + N bind groups** (Flux/Magi path; CellInstance
  vertex layout untouched; std140 drift contained by one shared
  populate function per Ripple mit. 7). Omen's CPU-side origin-baking
  alternative recorded in Considered but rejected.
- FRAME confirmation: **confirmed by user at EXPAND checkpoint**
  (problem statement, v1 scope incl. resize/equalize/zoom/divider,
  mouse = click-to-focus only).
- EXPAND checkpoint (user-confirmed): carry **A (SplitTree + scissor)**
  as front-runner and **B (offscreen-texture compositing)** as
  challenger into CHALLENGE; C/D/E rejected (see Considered but
  rejected).
- direction pick (CHALLENGE, user-confirmed): **A — SplitTree +
  single-surface scissor rendering, with Magi conditions + Ripple
  mitigations + Omen countermeasures folded into ACs.**
- scope decision (CHALLENGE, user-confirmed): keyboard resize +
  equalize **stay in v1** (Void's deferral considered and rejected —
  Ghostty-parity value outweighs the ratio-infrastructure cost).
- staging decision (CHALLENGE, user-confirmed): **single atomic PR**
  (Void's 2-PR staging rejected; large review accepted).

## L0 — Vision

### Problem

`noa` is hard-wired to one Terminal per WindowState (native tab): `App`
keys everything by `WindowId` (app.rs:56-97), `UserEvent` carries only a
`WindowId` (events.rs:8-18), and the renderer always draws one full-surface
pass (renderer.rs:117-162). Users cannot split a tab into multiple panes,
each running its own shell session — Ghostty parity plan Phase 4's 分割
item (ghostty-parity-plan.md:109-110) and the item tabs explicitly
deferred (tabs.md:66-68, 347-348: "Surface" reserved for splits).

### Audience

- macOS users of `noa` expecting Ghostty's split workflow.
- Contributors: splits is the first feature that structurally requires
  the Surface abstraction tabs deferred.

### Job To Be Done

Split a tab into panes and operate them with Ghostty's exact feel:
create (Cmd+D right / Cmd+Shift+D down), close (Cmd+W closes the focused
split first), directional focus (Cmd+Opt+arrows), resize
(Cmd+Ctrl+arrows), equalize (Cmd+Ctrl+=), zoom (Cmd+Shift+Enter).

### Success Definition

Split behavior is indistinguishable from Ghostty on macOS for the
in-scope operations. Existing tab and single-terminal behavior
(grid-first resize, VT/grid conformance, clippy/test suite, tabs
AC-1..19 behavior) regresses nowhere.

## Scope (provisional, FRAME)

### In scope (v1)

- Core: new_split right/down (Cmd+D / Cmd+Shift+D), close pane (Cmd+W
  split-first precedence), directional focus movement (Cmd+Opt+arrows).
- Resize (Cmd+Ctrl+arrows) + equalize_splits (Cmd+Ctrl+=).
- toggle_split_zoom (Cmd+Shift+Enter).
- Split divider rendering (visual necessity).
- Mouse click-to-focus on a pane.

### Out of scope (v1)

- Divider drag-resize (keyboard resize covers it; deferred).
- unfocused-split-opacity / unfocused-split-fill config (deferred).
- new_split left/up/auto variants (Ghostty has them; start right/down —
  revisit at SHAPE).
- Split topology session restore (Phase 6), cwd inheritance for new
  panes (Phase 5, same deferral as tabs), multi-window.

## Reuse / constraint findings (Lens, FRAME)

Enablers:

- {Terminal, Pty, io thread} N-plication proven by tabs; per-instance
  clean (app.rs:367-374).
- `IoThreadHandle::shutdown_and_join` (io_thread.rs:29-58) reusable
  per-pane.
- `AppCommand`/`menu_id`/`action_name` round-trip + `KeybindEngine`
  chord parser (commands.rs:270-350) generic, reusable for split
  commands.
- `FrameSnapshot` state↔GPU seam (snapshot.rs:11-40); shared wgpu
  Device/Queue + FontGrid/Atlas with generation counter already landed
  (tabs REQ-NF-1).

Constraints:

- Renderer has NO sub-viewport/scissor/region concept — one full-surface
  render pass, one `GridPadding` rect (renderer.rs:117-162, 216-246).
  Per-pane viewport rendering is net-new capability, not a refactor.
- `UserEvent` is keyed by `WindowId` only; splits live INSIDE one
  WindowId — a pane-id concept is net-new (events.rs:8-18).
- `FrameSnapshot::from_terminal` is single-terminal, no pane offset.
- `Pty` is Send-not-Sync → exactly one owning io thread per pane.
- Dependency rule: wgpu only in noa-app/noa-render; winit only in
  noa-app. Pane layout math must live in noa-app or a new
  windowing-agnostic module — not in noa-grid/noa-vt.
- Grid-first resize ordering (lock Terminal, then signal pty) must hold
  per pane (app.rs:660-697).
- `noa-config` has no `keybind` key; `SUPPORTED_KEYS` allowlist rejects
  unknown keys; KeybindEngine defaults are hardcoded — split keybinds
  land as hardcoded defaults like tabs did (config exposure is Phase 3
  work).

## Candidate directions (EXPAND)

Key evidence (Flux, verified):

- wgpu 27.0.1: `RenderPass::set_scissor_rect`/`set_viewport` are stable
  core API, unused today; today's draw is one unscissored full-window
  call (renderer.rs:131-158). N scissored draws in one pass is viable.
- `Uniforms.cursor_color`/`cursor_pos` are DEAD fields — cursor renders
  per-instance via `CellInstance::FLAG_CURSOR` (renderer.rs:316-321), so
  multi-pane cursors work for free; only the coordinate-transform half
  of the uniform must go per-pane (N uniform buffers + N bind groups,
  matching the existing 1-bind-group pattern; dynamic offsets not wired).
- winit 0.30.13 has NO child-window/subview API; raw-AppKit child views
  would get zero WindowEvent dispatch (hand-rolled hit-testing + event
  forwarding, fragile against winit upgrades).
- FrameSnapshot dirty-row diffing is NOT landed (snapshot.rs:9-10;
  parity plan P2 backlog) — N panes = N full-row-clone snapshots per
  redraw; a real per-frame cost the spec must budget.
- IME/mouse state (`ime_state`, `mouse_selection`, `last_mouse_cell`,
  `pressed_mouse_button`) is 1:1 with the OS window in WindowState
  (app.rs:56-72) — must be lifted per-pane.
- No within-window focus concept exists (`focused: Option<WindowId>`
  only); mode 1004 focus reporting is unimplemented (parity plan
  Phase 1). Split-focus is a new dimension.
- Grid-first resize survives layout-driven resize: `resize_grid`
  (app.rs:683) takes a bare GridSize, doesn't read WindowEvent —
  callable per-pane directly. Odd-pixel remainder policy is unscoped
  (Ghostty: remainder to first/left/top pane + divider).
- Cross-domain: kitty and WezTerm both use single-GPU-context +
  per-pane scissor — validates (A)'s mechanism.
- MAJOR REFRAME: shader/uniform work is the easy ~20%; the unscoped
  ~80% is decomposing WindowState into Window{Renderer, layout, focus}
  + N×Surface{Terminal, io thread, ime/mouse state, rect} — the IOU the
  tabs spec explicitly deferred to splits (tabs.md:12-15).

Candidates (Riff):

- **A. SplitTree + single-surface scissor rendering** — WindowState
  holds a recursive SplitTree of Surface{Terminal, Pty, io thread};
  renderer does N per-pane scissored draws (per-pane snapshot + uniform)
  in one pass; Device/Queue/FontGrid/Atlas stay singletons. Closest
  fidelity (one seamless window); riskiest single change is the
  renderer's single-pass/single-uniform assumption. Kitty/WezTerm-
  validated shape.
- **B. Offscreen-texture compositing** — each pane keeps an unmodified
  full Renderer rendering to its own texture (`draw()` already accepts
  any TextureView); new composite pass blits N textures + dividers.
  Smallest diff to render code; N full renders + N× render-target VRAM
  per frame — most GPU-wasteful.
- **C. Child NSView per pane, own wgpu surface (Ghostty's literal
  shape)** — zero renderer changes, but winit has no child-view API:
  hand-rolled AppKit event forwarding/hit-testing, macOS-only windowing
  path parallel to winit, least testable.
- **D. Flat Vec<Pane> + BSP rect layout** — same scissor mechanism as
  A with a flat list instead of a tree; simplest for 2-4 panes; cannot
  express Ghostty's arbitrarily nested split-of-a-split layouts —
  likely rewritten into A later (fidelity gap).
- **E. Tiled borderless child NSWindows** — abutting undecorated
  windows faking one surface; zero renderer changes; Mission Control/
  fullscreen/screenshots reveal separate windows — weakest fidelity,
  stopgap only.

## Proposal (SHAPE)

### Proposed solution

Windows decompose into two tiers: `WindowState` keeps only window-level
concerns — the winit `Window`, its wgpu surface, one `Renderer`, the
current split layout, per-window zoom state (`Option<PaneId>`), and
which pane holds focus — while everything terminal-specific moves into
a `Surface` held inside a recursive `SplitTree`: `Terminal`, pty
input/resize senders, an io-thread shutdown handle, `ime_state`,
mouse/selection state, and a computed screen `rect`. Internal tree
nodes carry a split ratio and orientation; leaves are `Surface`s.
Rendering stays single-pass: one `LoadOp::Clear` followed by N
scissored draws, one per visible leaf, each transformed into its pane's
`rect`. The per-pane coordinate-transform mechanism is an open L2
decision carried to SPECIFY, not resolved here: N per-pane uniform
buffers + N bind groups (Flux/Magi's path, matching today's
one-bind-group pattern) versus baking pane origin into `CellInstance`
CPU-side (Omen's counter-proposal); both are recorded and must be
arbitrated before implementation. `UserEvent`'s per-surface variants
(Redraw/PtyExit/ClipboardWrite) gain a `(WindowId, PaneId)` pair
atomically across every send/match site, closing the pane-identity gap
Omen ranked the top pre-mortem risk. Layout, directional focus lookup,
and resize distribution are extracted as pure functions over the
`SplitTree` — mirroring the `resolve_command_target`/
`close_tab_outcome` precedent — carrying ratio state (resize and
equalize stay in v1) and a minimum-pane-size floor so degenerate splits
clamp instead of collapsing. `CloseTab`/Cmd+W interposes a pane-count
check ahead of tab-close: with 2+ panes in the focused tab it removes
and re-lays-out only the focused pane; only the last pane's exit closes
the tab. Zoom is a per-window `Option<PaneId>` that hides sibling panes
from the draw list without dropping their `Surface`s — hidden panes
keep receiving `resize()` so un-zooming needs no rebuild, and closing a
zoomed pane force-unzooms first. A focus switch between panes commits
the losing pane's IME preedit before `set_ime_cursor_area` retargets to
the new pane. Any layout-driven resize batches ALL
affected panes' grid resizes before sending ANY pane's new winsize to
its pty, preserving the grid-first invariant per pane rather than per
window. The divider renders in the same pass as an instanced draw, not
a separate compositor pass, and its click hit-zone width is one named
shared constant used by both hit-testing and rendering. The whole
surface — SplitTree, per-pane renderer plumbing, command wiring, and
pane-identity event routing — lands as a single atomic PR, per the
CHALLENGE staging decision.

### Sub-features (MoSCoW)

| Sub-feature | MoSCoW | Note |
|---|---|---|
| Split create right/down (Cmd+D / Cmd+Shift+D) | Must | Core v1 scope, confirmed at FRAME |
| Close-pane precedence + PtyExit-per-pane | Must | Ripple mit. 1 + Omen #3 RPN210; tabs AC-2/AC-10 regression risk |
| SplitTree + pure layout math (ratio-bearing) | Must | Ripple mit. 3; resize/equalize confirmed IN v1 needs ratio infra |
| Directional focus + focus-dimension introduction | Must | Core v1 scope; no within-window focus concept exists today (Flux) |
| Per-pane render (scissor + transform) | Must | Candidate A's core mechanism; Magi 3-0 verdict |
| Divider render + click-to-focus hit-test | Must | Visual necessity (in-scope) + Omen #5 RPN175 |
| Keyboard resize (Cmd+Ctrl+arrows) | Must | CHALLENGE scope decision: stays in v1, Void's deferral rejected |
| Equalize (Cmd+Ctrl+=) | Must | Same CHALLENGE scope decision as resize |
| Zoom toggle (Cmd+Shift+Enter) | Must | Core v1 scope; Omen #6 RPN175 mandates per-window Option<PaneId> |
| IME preedit handoff | Must | Omen #2 RPN216, 2nd-highest pre-mortem risk |
| pipeline.rs scissor/multi-buffer headless tests | Must | Magi condition 1: required BEFORE merging split rendering |
| Multi-pane resize batching | Must | Omen #8 RPN150 + Ripple mit. 6; grid-first invariant per pane |
| io-thread N-way shutdown join | Must | Omen #11 RPN100; tabs AC-9 regression risk |
| Pane-level command routing (Copy/Paste/Search/IME/font-size) | Must | tabs REQ-TAB-14 analog at pane granularity (Judge completeness finding) |
| Atlas-reallocation bind-group rebuild (N-way) | Must | Judge consistency finding; CLAUDE.md GPU bug class |
| Noisy-pane perf limitation documentation | Should | Omen #12 RPN96 (lowest); dirty-row diff fix explicitly deferred, not this PR |

### Assumptions

- Pane counts stay single-digit in realistic usage (2-6 typical); no
  pane-count pooling or hard cap in v1 (Void: do not scaffold
  MAX_PANES).
- N-full-row-clone `FrameSnapshot` cost per redraw is accepted for v1
  pane counts; dirty-row diffing remains the explicit P2 backlog
  follow-up, not pulled forward here.
- wgpu 27.0.1's `set_scissor_rect`/`set_viewport` behave as documented
  for N sequential scissored draws within one render pass (stable core
  API, verified unused-but-available today).
- Device/Queue and FontGrid/Atlas — with its already-landed generation
  counter (tabs REQ-NF-1) — remain app-level singletons shared across
  every pane of every window; no per-pane atlas duplication.
- Existing per-Terminal invariants (grid-first resize, VT/grid
  conformance suite, screen.rs cell-clamping) hold unchanged per-pane;
  splits multiply the Terminal instance count, not its internal
  contract.

## L1 — Requirements

### Functional

- **REQ-SPL-1**: Cmd+D splits the focused pane right, adding a new `Surface` as a sibling leaf in the `SplitTree` and focusing it. Splitting a pane too small to host two panes plus a divider above the minimum-size floor (REQ-NF-4) is a no-op (provisional rule; see Open Questions).
- **REQ-SPL-2**: Cmd+Shift+D splits the focused pane down — same mechanics as REQ-SPL-1 with vertical orientation.
- **REQ-SPL-3**: A new split's ratio defaults to equal (50/50) between the split pane and its new sibling.
- **REQ-SPL-4**: Cmd+W with 2+ panes in the focused tab removes and re-lays-out only the focused pane; the tab itself stays open. **Deliberate behavior change from tabs**: Cmd+W now closes the focused split before it closes the tab.
- **REQ-SPL-5**: A pane's pty exit removes that pane and re-lays-out its tab; only the last pane's exit closes the tab (today's unconditional PtyExit→close_tab, app.rs:555, becomes per-pane).
- **REQ-SPL-6**: Cmd+Opt+arrow moves focus to the pane adjacent across the focused pane's edge in that direction; among multiple candidates, the pane with the greatest perpendicular overlap with the focused pane wins, remaining ties resolved to the top-most (horizontal moves) / left-most (vertical moves) candidate; focus is unchanged if no pane exists in that direction.
- **REQ-SPL-7**: Clicking inside a pane's rect focuses that pane.
- **REQ-SPL-8**: Cmd+Ctrl+arrow moves the boundary of the nearest ancestor split node whose orientation matches the arrow's axis and whose divider lies in the arrow's direction from the focused pane, by a fixed step `SPLIT_RESIZE_STEP_PX = 10` per keypress (Ghostty's default `resize_split` amount; hardcoded in v1, config exposure is Phase 3 work), clamped by the minimum-pane-size floor (REQ-NF-4). No matching ancestor → no-op.
- **REQ-SPL-9**: Cmd+Ctrl+= resets every ratio in the focused tab's `SplitTree` to equal.
- **REQ-SPL-10**: Cmd+Shift+Enter toggles zoom of the focused pane: while zoomed, the zoomed pane's Terminal grid is resized to the full window content rect and it is the only pane drawn; sibling `Surface`s are hidden from the draw list without being dropped and keep receiving `resize()` to their unzoomed tree-proportional rects (a window resize while zoomed updates both the zoomed pane's full rect and the hidden panes' tree rects). Toggling again restores the prior layout with no rebuild.
- **REQ-SPL-11**: Closing a zoomed pane force-unzooms its tab before the pane is removed.
- **REQ-SPL-12**: A divider between two panes renders as an instanced draw in the same render pass as the panes, not a separate compositor pass.
- **REQ-SPL-13**: Divider geometry is two named shared constants: `DIVIDER_WIDTH_PX` — the layout footprint `compute_layout` reserves between sibling rects and rendering fills — and `DIVIDER_HIT_ZONE_PX` (≥ `DIVIDER_WIDTH_PX`) — the click hit-zone used by hit-testing. Layout, rendering, and hit-testing all read these constants; no other divider dimension exists.
- **REQ-SPL-14**: Split layout first reserves the divider's `DIVIDER_WIDTH_PX`, then divides the remaining pixels between the two children by ratio; any odd leftover pixel is allocated to the first (left/top) pane, never the second (right/bottom) pane.
- **REQ-SPL-15**: A focus switch between panes **commits** the losing pane's IME preedit (deterministically commit, not cancel — matching macOS `unmarkText` convention) before `set_ime_cursor_area` retargets to the newly focused pane.
- **REQ-SPL-16**: Every per-surface `UserEvent` (Redraw/PtyExit/ClipboardWrite) carries `(WindowId, PaneId)` and resolves to the exact pane, not the whole window.
- **REQ-SPL-17**: Existing tab behavior (tabs.md AC-1..19) is unchanged by splits except the deliberate REQ-SPL-4 Cmd+W precedence change.
- **REQ-SPL-18**: Existing per-terminal commands (Copy, Paste, Search, selection, IME input, font-size targeting) operate on the focused pane's Terminal only — tabs REQ-TAB-14 extended to pane granularity.
- **REQ-SPL-19**: Font-size change (Cmd+±/0) applies window-wide: the shared cell size changes, `compute_layout` re-runs once, and every pane's grid resizes batched per REQ-NF-3. **Deliberate v1 deviation from Ghostty**, whose font size is per-surface — forced by the shared FontGrid/Atlas singleton; recorded in Open Questions.

### Non-Functional

- **REQ-NF-1**: Per-pane rendering is one shared `LoadOp::Clear` followed by N scissored draws, one per visible (non-zoom-hidden) leaf pane, each transformed by its own uniform buffer + bind group.
- **REQ-NF-2**: All N per-pane `Uniforms` buffers are populated through exactly one shared function.
- **REQ-NF-3**: Any layout-driven resize batches every affected pane's grid resize before sending any pane's new winsize to its pty (grid-first invariant, applied per pane).
- **REQ-NF-4**: Layout clamps every pane to a minimum size floor instead of collapsing it to zero/negative dimensions.
- **REQ-NF-5**: Within one frame, shared-`Atlas` sync order relative to per-pane cell rebuilding is deterministic and identical across all panes (one ordering rule, not a per-pane race).
- **REQ-NF-6**: `noa-render/tests/pipeline.rs` gains headless coverage for per-pane scissored drawing (draw-plan execution without validation errors, incl. post-atlas-growth), landed **in the same atomic PR** as the per-pane `draw()` rewrite and passing within it (satisfies the Magi precondition inside the single-PR staging decision; no separate earlier landing).
- **REQ-NF-7**: Closing a tab or window drives the shutdown primitive for every one of its panes' io threads and joins all of them before the tab's/window's state is dropped.
- **REQ-NF-8**: Layout, directional-focus lookup, resize distribution, equalize, zoom-toggle decisions, close-pane focus reassignment, divider hit-testing, **draw-plan construction** (`build_draw_plan`), **focus-switch IME ordering** (`focus_switch_plan`), and **pane command-target resolution** are pure functions, unit-testable without constructing a `Window`/GPU context.
- **REQ-NF-9**: The `(WindowId, PaneId)` `UserEvent` payload change and all other split-surface plumbing land as one atomic PR — not incrementally shippable.
- **REQ-NF-10**: `cargo test --workspace` and `cargo clippy --workspace` stay green with splits landed.
- **REQ-NF-11**: A pane producing continuous output while siblings are static still triggers a full-window redraw every frame (no per-pane dirty tracking); documented as an accepted v1 performance limitation, dirty-row diffing left as explicit future work.
- **REQ-NF-12**: When the shared `Atlas` texture is resized/recreated (generation bump with reallocation), every per-pane bind group of the affected window is rebuilt before its next draw — extending today's single-bind-group rebuild (renderer.rs:191) to N; a stale pane bind group referencing a dropped atlas texture must be impossible.

## L2 — Detail

### noa-app

- `WindowState` decomposes into two tiers: window-level fields stay on `WindowState` (winit `Window`, wgpu surface, one `Renderer`, `split_tree: SplitTree`, `zoomed: Option<PaneId>`, `focused_pane: PaneId`); everything terminal-specific moves into a new `Surface` held at tree leaves: `Arc<Mutex<Terminal>>`, pty input/resize senders, io-thread shutdown handle, `ime_state`, mouse/selection state (`mouse_selection`, `last_mouse_cell`, `pressed_mouse_button`), and a computed `rect: PaneRect`.
- `PaneId` is a newtype (Copy+Eq+Hash) minted on split-create, stable across resize/equalize/zoom.
- `SplitTree`: recursive `enum { Leaf(Surface), Split { orientation: Orientation, ratio: f32, first: Box<SplitTree>, second: Box<SplitTree> } }` — internal nodes carry ratio + orientation, leaves are `Surface`s, expressing arbitrarily nested split-of-a-split layouts per the direction-A decision.
- New `AppCommand` variants — `NewSplitRight`, `NewSplitDown`, `FocusDirection(Direction)`, `ResizeSplit(Direction)`, `EqualizeSplits`, `ToggleSplitZoom` — route through the existing `KeybindEngine`/`menu_id` plumbing (commands.rs:270-350).
- `CloseTab`/Cmd+W interposes a pane-count check ahead of tab-close (REQ-SPL-4): 2+ panes → remove+re-layout the focused pane only; 1 pane → falls through to today's `close_tab_outcome` path.
- Pure-function seam (unit-testable, no `Window`/GPU): `compute_layout(&SplitTree, bounds) -> Vec<(PaneId, Rect)>` (reserves `DIVIDER_WIDTH_PX`, ratio division, odd-pixel remainder to first pane, REQ-SPL-13/14); `focus_in_direction(&SplitTree, PaneId, Direction) -> Option<PaneId>` (overlap-then-top/left tie-break, REQ-SPL-6); `resize_split(&mut SplitTree, PaneId, Direction, step)` (nearest matching ancestor + `SPLIT_RESIZE_STEP_PX`, REQ-NF-4 floor); `equalize(&mut SplitTree)`; `zoom_toggle(...) -> ZoomDecision`; `close_pane(&mut SplitTree, PaneId) -> CloseOutcome { next_focus, tab_should_close }`; `hit_test(&[(PaneId, Rect)], point) -> HitTarget::{Pane(PaneId), Divider(..)}` (`DIVIDER_HIT_ZONE_PX`); `focus_switch_plan(losing, winning) -> Vec<ImeOp>` (ordered: `CommitPreedit(losing)` then `RetargetIme(winning)`, REQ-SPL-15 — the winit calls merely execute this plan); pane command-target resolution (REQ-SPL-18, mirroring tabs' `resolve_command_target`).
- `UserEvent`'s per-surface variants gain `PaneId` alongside `WindowId`; `user_event()` resolves `(WindowId, PaneId)` to the exact `Surface` and no-ops on stale ids (mirrors tabs' stale-`WindowId` no-op; REQ-SPL-16/REQ-NF-9).
- Any layout-driven resize (window resize, split-resize, equalize) calls `compute_layout` once, resizes every affected pane's `Terminal` grid, THEN sends every affected pane's new winsize to its pty (REQ-NF-3).
- Focus switch between panes: commit the losing pane's `ime_state` preedit, then `set_ime_cursor_area` on the newly focused pane (REQ-SPL-15).
- Tab/window close drives the already-landed (tabs REQ-TAB-9) `Pty` shutdown primitive for every pane's io thread and joins all of them (REQ-NF-7) — no new `noa-pty` API.
- No `noa-font` API change: the atlas generation counter (tabs REQ-NF-1) is reused as-is; splits add an intra-frame ordering rule on top (REQ-NF-5) — `sync_atlas()` runs once per frame before any pane's `rebuild_cells`.

### noa-render

- Renderer stays one-per-`WindowState` (from tabs); gains N per-pane uniform buffers + N bind groups, replacing the single 1-buffer/1-bind-group pattern. Per-pane coordinate-transform fields populate through one shared `populate_pane_uniform(pane_rect, window_size) -> Uniforms` function (REQ-NF-2), keeping std140 layout drift (CLAUDE.md GPU gotchas: vec4/mat4 first, scalar padding last) to one call site.
- Rendering splits into a **pure draw-plan builder** and a thin executor: `build_draw_plan(&[(PaneId, Rect)], zoomed) -> Vec<DrawOp>` (windowing/GPU-free; ops: `Clear`, `PaneCells { pane, scissor, bind_group_index }`, `Dividers`) is the unit-testable seam that owns structure and order — exactly one `Clear` first, then one scissored `PaneCells` per visible (zoom-filtered) leaf, then `Dividers` in the same plan (REQ-NF-1, REQ-SPL-12). `draw()` executes the plan verbatim inside one render pass: `set_scissor_rect` + `set_bind_group` + instanced draw per op; wgpu-API structure claims are asserted on the plan, not introspected from the GPU (no such API exists — Attest).
- On an atlas generation bump that reallocated the texture, all N per-pane bind groups are rebuilt before the next draw (REQ-NF-12), extending the existing single rebuild at renderer.rs:191.
- `FrameSnapshot::from_terminal` is called once per visible pane per frame (N snapshots/redraw); the resulting N full-row-clone cost is the accepted v1 limitation (REQ-NF-11), not addressed by this PR.
- `noa-render/tests/pipeline.rs` gains headless cases (same atomic PR, REQ-NF-6): executing a multi-pane draw plan (incl. divider ops and a post-atlas-growth redraw) against a real adapter completes without validation errors — structural assertions (clear count, draw order, bind-group identity, scissor rects) live in the draw-plan unit tests, since wgpu exposes no pass-introspection API.
- `Uniforms` fields become private with one constructor (`populate_pane_uniform`), making the single-population-path a compile-time property (REQ-NF-2) rather than a runtime assertion.

### noa-font / noa-pty

- Not touched. `Atlas`'s generation counter (tabs REQ-NF-1) and `Pty`'s shutdown primitive (tabs REQ-TAB-9) are reused unmodified, each invoked once per pane instead of once per window.

## L3 — Acceptance Criteria

- **AC-1** (REQ-SPL-1) [manual-visual] — Given one pane focused, When the user presses Cmd+D, Then a new pane appears to its right running a fresh login shell and gains focus.
- **AC-2** (REQ-SPL-2) [manual-visual] — Given one pane focused, When the user presses Cmd+Shift+D, Then a new pane appears below it and gains focus.
- **AC-3** (REQ-SPL-3) [unit] — Given `compute_layout` on a tree with one freshly created split node, When evaluated, Then both children's rects are equal within the odd-pixel remainder rule.
- **AC-4a** (REQ-SPL-4) [unit] — Given `close_pane` + `compute_layout` on a 2-leaf tree for the focused pane, When evaluated, Then the outcome is remove-and-relayout with `tab_should_close=false` and the survivor's rect fills the freed space (pure, no `ActiveEventLoop` — tabs AC-2/AC-10 precedent).
- **AC-4b** (REQ-SPL-4) [manual-visual] — Given a tab with 2 panes, When the user presses Cmd+W, Then only the focused pane disappears, the tab stays open, and the sibling expands into the freed rect.
- **AC-5** (REQ-SPL-5) [unit] — Given `close_pane` on a 3-leaf tree for a pty-exited pane, When evaluated, Then it returns `next_focus` to a sibling with `tab_should_close=false`; given the tree's last leaf, Then `tab_should_close=true`.
- **AC-6** (REQ-SPL-6) [unit] — Given `focus_in_direction` on (a) a 2x2 grid with top-left focused and (b) a nested unequal layout where two panes both touch the focused pane's right edge, When Direction::Right, Then (a) returns the top-right `PaneId` and (b) returns the candidate with the greatest perpendicular overlap (tie → top-most); When Direction::Up at the layout's top edge, Then it returns `None`.
- **AC-7** (REQ-SPL-7) [manual-visual] — Given 2 unfocused panes, When the user clicks inside one, Then that pane gains focus.
- **AC-8** (REQ-SPL-8) [unit] — Given `resize_split` on (a) a 2-leaf tree and (b) a nested tree where the matching boundary is an ancestor above the focused pane, When driven by repeated `SPLIT_RESIZE_STEP_PX` steps beyond the minimum-pane-size floor, Then (a/b) the correct (nearest matching ancestor) boundary moves exactly one step per call and clamps at the floor instead of collapsing a pane; with no matching ancestor, Then the tree is unchanged.
- **AC-9** (REQ-SPL-9) [unit] — Given a tree with skewed nested ratios (e.g. 0.2/0.8), When `equalize` runs, Then every ratio in the tree becomes 0.5.
- **AC-10** (REQ-SPL-10) [unit] — Given `zoom_toggle` on a 3-pane tree with pane B focused, When toggled on, Then the draw list contains only B while A/C's `Surface`s persist and still receive `resize()`; When toggled off, Then the draw list returns to all 3 panes at their prior rects.
- **AC-11a** (REQ-SPL-11) [unit] — Given the composed `zoom_toggle`+`close_pane` decision path for a zoomed pane, When close fires, Then the returned decisions set `zoomed=None` before pane removal and re-layout (pure composition, no live Cmd+W).
- **AC-11b** (REQ-SPL-11) [manual-visual] — Given a zoomed pane, When the user presses Cmd+W, Then the tab unzooms and the remaining panes reappear at their prior layout.
- **AC-12** (REQ-SPL-12) [unit] — Given `build_draw_plan` for a 2-pane layout, When evaluated, Then the plan contains the `Dividers` op in the same single plan (one pass by construction) after the `PaneCells` ops — no separate compositor plan exists.
- **AC-13** (REQ-SPL-13) [unit] — Given `hit_test` with a point within `DIVIDER_HIT_ZONE_PX` of a divider vs. one pixel further, When evaluated, Then the former returns `HitTarget::Divider` and the latter returns `HitTarget::Pane`.
- **AC-14** (REQ-SPL-14) [unit] — Given `compute_layout` on odd-width bounds split into two panes, When evaluated, Then `DIVIDER_WIDTH_PX` is reserved between the rects and the one leftover pixel lands in the first (left/top) pane's rect — the two children plus divider exactly tile the bounds with no gap or overlap.
- **AC-15** (REQ-SPL-15) [unit] — Given `focus_switch_plan(losing, winning)` with the losing pane mid-composition, When evaluated, Then the returned ops are exactly `[CommitPreedit(losing), RetargetIme(winning)]` in that order (the winit calls execute this plan; ordering is asserted on the pure seam).
- **AC-16** (REQ-SPL-16) [unit] — Given a `UserEvent::Redraw(WindowId, PaneId)` for a since-closed pane, When `user_event()` processes it, Then it no-ops without panicking; given a live pane, Then it resolves to that pane's `Surface` only.
- **AC-17a** (REQ-SPL-17) [integration] — Given tabs.md's unit/integration ACs (AC-6/7a/9/11a/14/15/16/19 there), When `cargo test --workspace` runs against the splits codebase, Then all pass unchanged.
- **AC-17b** (REQ-SPL-17) [manual-visual] — Given tabs.md's manual-visual ACs (AC-1..5/7b/8/10/11b/13/18 there) re-checked by hand with single-pane tabs, When exercised, Then each behaves unchanged — except Cmd+W, re-verified against REQ-SPL-4's documented precedence change.
- **AC-18** (REQ-NF-1) [unit] — Given `build_draw_plan` for a 3-pane layout, When evaluated, Then the plan is exactly one `Clear` followed by 3 `PaneCells` ops with pairwise-distinct scissor rects and bind-group indices (structure asserted on the plan; GPU execution covered by AC-23).
- **AC-19** (REQ-NF-2) [unit] — Given two panes with different rects, When `populate_pane_uniform` runs for each, Then each output's transform fields match its pane rect (value correctness). Single-population-path is a compile-time property (private `Uniforms` fields + one constructor) — recorded as a Process note, not a runtime assertion.
- **AC-20** (REQ-NF-3) [unit] — Given a table-driven multi-pane resize (3 panes affected by one equalize), When the resize distribution runs, Then all 3 grids resize before any pty winsize send is recorded.
- **AC-21** (REQ-NF-4) [unit] — Given `resize_split` driven to an extreme delta, When evaluated, Then no pane's rect drops below the minimum-size floor.
- **AC-22** (REQ-NF-5) [integration] — Given 2 panes sharing one `Atlas`, When a new glyph lands mid-frame and both panes' `rebuild_cells` run (either order), Then both observe the mutation per the one chosen ordering rule, verified for the 2-pane new-glyph-same-frame case.
- **AC-23** (REQ-NF-6) [integration] — Given pipeline.rs executing a 3-pane draw plan (incl. divider ops) headlessly against a real adapter, When one frame renders, Then it completes with `pop_error_scope() == None` (no validation error); structural order/bounds claims are covered by the AC-12/AC-18 plan unit tests, and scissor rects are the verbatim `compute_layout` outputs already covered by AC-3/AC-14.
- **AC-24** (REQ-NF-7) [unit] — Given a tab with 3 panes' io threads blocked on a receiver (real or fake, per io_thread.rs's existing `io_thread_handle_shutdown_joins_within_timeout` precedent — no real `Pty` needed), When the tab closes, Then all 3 shutdown primitives fire and all 3 joins return within the bounded timeout.
- **AC-25** (REQ-NF-8) [unit] — Given each pure seam (layout, focus-nav, resize, equalize, zoom, close-pane, hit-test) with table-driven inputs (single pane, nested tree, stale ids), When exercised directly, Then each returns the expected value without constructing a `Window`/GPU context.
- **Process note** (REQ-NF-9): atomic landing is enforced by PR review scope (single PR spanning SplitTree/renderer/commands/events), not a runtime-testable AC — mirrors tabs REQ-NF-3.
- **AC-26** (REQ-NF-10) [integration] — Given the full new split-feature test set (AC-3/4a/5/6/8/9/10/11a/12/13/14/15/16/17a/18/19/20/21/22/23/24/25/28/29/30/31), When `cargo test --workspace` and `cargo clippy --workspace` run, Then all pass in the standard gate with no `#[ignore]` beyond the documented pty sandbox constraint.
- **AC-27** (REQ-NF-11) [manual-visual] — Given one pane running a noisy busy-loop shell alongside static sibling panes, When observed over multiple frames (debug frame counter), Then every frame re-renders all panes as a documented limitation — no crash, no visual corruption.
- **AC-28** (REQ-SPL-10) [unit] — Given the zoom resize-target decision on a zoomed 3-pane tree, When the window resizes, Then the zoomed pane's resize target is the full content rect while each hidden pane's target is its recomputed tree-proportional rect; When unzoom fires, Then no further resize is needed (targets already current).
- **AC-29** (REQ-SPL-18) [unit] — Given the pane command-target resolution helper with a known `focused_pane`, When each per-terminal `AppCommand` (Copy/Paste/Search/font-size targeting) is dispatched, Then the resolved target is the focused pane's Terminal only (table-driven; mirrors tabs AC-19).
- **AC-30** (REQ-NF-12) [integration] — Given a headless 2-pane draw where a glyph pack forces atlas texture reallocation between frames, When the next frame renders, Then every pane's bind group was rebuilt and the frame completes without a validation error.
- **AC-31** (REQ-NF-3) [unit] — Given `compute_layout` on a nested tree with non-default ratios at two different window sizes, When evaluated at each size, Then every pane's rect scales proportionally to its ratios (ratios preserved across window resize) and the REQ-SPL-14 tiling invariant holds at both sizes.

## Stress-test findings (CHALLENGE)

- **Magi verdict: 3-0 for A** (Logos 82 / Pathos 74 / Sophia 78), with
  conditions: (1) extend `noa-render/tests/pipeline.rs` to exercise
  scissor + multi-uniform-buffer/bind-group draws BEFORE merging split
  rendering (CLAUDE.md-flagged silent-at-build bug class); (2) IME/
  mouse/focus per-pane lift is identical work under A or B — not a
  differential cost; (3) divider renders as a same-pass instanced draw,
  not a separate compositor. Strongest counter (recorded): on Apple
  Silicon unified memory B's VRAM cost is near-moot at 2-6 panes and B
  adds zero risk to the proven single-Renderer path — legitimate
  risk-minimization, outweighed by evolution/TCO (B→A is a rewrite, not
  a refactor).
- **Void subtraction:** keyboard resize (Cmd+Ctrl+arrows) + equalize
  (Cmd+Ctrl+=) recommended DEFER — no ratio state exists anywhere;
  ratio + ancestor-lookup-by-direction is LOC-comparable to split
  creation itself; v1 ships correct with fixed 50/50 splits. All other
  v1 items KEEP. Recursive SplitTree is NOT speculative (nested
  split-of-a-split is reachable from the confirmed scope; Ghostty's own
  model is a binary tree). Staging: land as TWO PRs — PR1 mechanical
  Window+Vec<Surface> decomposition with exactly one Surface (behavior
  no-op, suite green), PR2 SplitTree + scissor + commands on the clean
  seam. Do not scaffold MAX_PANES or SplitDirection::Auto.
- **Ripple impact (A): risk 7.5/10 HIGH, Conditional-Go**, mandatory
  mitigations (→ ACs): (1) spec PtyExit-for-one-pane explicitly —
  remove pane + re-layout; close window only when last pane exits
  (today PtyExit→close_tab unconditionally, app.rs:555); (2)
  UserEvent WindowId params become (WindowId, PaneId) atomically across
  all send/match sites; (3) extract split-tree layout / focus-nav /
  resize-distribution as pure functions (mirror
  resolve_command_target/close_tab_outcome precedent); (4) ~~atlas
  generation counter~~ — verified ALREADY LANDED (atlas.rs:16-127,
  renderer.rs:28-96); superseded by the intra-frame ordering rule from
  Omen #10; (5) pipeline.rs headless-GPU cases for scissor bounds +
  bind-group swap ordering before draw() rewrite; (6) grid-first
  resize preserved per pane, layout computed once per window resize;
  (7) all N Uniforms buffers populated through ONE shared function
  (std140 layout-drift would multiply N-fold).
- **Omen pre-mortem (top by RPN):** #1 RPN 256 pane-identity gap in
  UserEvent (= Ripple mit. 2); #2 RPN 216 IME preedit desync on focus
  switch mid-composition → AC: focus change commits/cancels the losing
  pane's preedit before `set_ime_cursor_area` retargets; #3 RPN 210
  Cmd+W collision with `close_tab_outcome`/tabs AC-2/AC-10 → CloseTab
  interposes pane-count check; #4 RPN 189 single LoadOp::Clear vs N
  panes → one clear + N scissored draws, headless-tested; #5 RPN 175
  pointer hit-test precedes cell translation, divider hit zone a named
  shared constant; #6 RPN 175 zoom corruption → zoom is per-window
  Option<PaneId>, closing zoomed pane force-unzooms, hidden panes still
  receive resize(); #7 RPN 168 minimum pane size floor in layout (grid
  clamps to 1×1 silently, screen.rs:496); #8 RPN 150 multi-pane resize
  batching: ALL grids resize before ANY pty winsize send, table-driven
  test; #9 RPN 144 recommends baking pane origin into CellInstance
  CPU-side instead of N uniform buffers — CONFLICTS with Flux/Magi's
  N-bind-group path; recorded as an open L2 mechanism decision; #10
  RPN 120 intra-frame atlas ordering: sync_atlas once before any
  pane's rebuild_cells (or all conversions before first sync) — pick
  one, test 2-pane new-glyph-same-frame; #11 RPN 100 window shutdown
  joins ALL pane io-threads (tabs AC-9 regression risk); #12 RPN 96
  noisy-pane full re-render documented as known v1 limitation, dirty-
  row diff the explicit follow-up.

## Considered but rejected

(user-confirmed at EXPAND checkpoint)

- **C. Child NSView per pane (Ghostty's literal shape)** — winit 0.30
  has no child-view API; requires hand-rolled AppKit event forwarding /
  hit-testing parallel to winit's backend; least testable; fragile
  against winit upgrades.
- **D. Flat Vec<Pane> + BSP layout** — cannot express Ghostty's nested
  split-of-a-split layouts; fidelity gap that forces a later rewrite
  into a tree.
- **E. Tiled borderless child NSWindows** — Mission Control /
  fullscreen / screenshots reveal separate windows; weakest fidelity;
  stopgap only.

## Open Questions / Deferred Decisions

- **Font-size scope deviation (REQ-SPL-19) — CONFIRMED at LOCK**: v1
  applies font-size changes window-wide; Ghostty's is per-surface.
  Forced by the shared FontGrid/Atlas singleton; adopting per-surface
  font size later needs per-size atlas keying (recorded parity debt).
- **Split-at-floor rule (REQ-SPL-1) — CONFIRMED at LOCK**: splitting a
  pane too small for two panes + divider is a no-op. Alternative
  (clamp/refuse with bell) deferred.
- **`SPLIT_RESIZE_STEP_PX = 10` hardcoded** (Ghostty's default resize
  amount); config exposure belongs to the Phase 3 config-system work,
  as do all split keybind customizations.
- Sub-features table cross-ref tags for Omen #7/#10 omitted (cosmetic;
  covered by REQ-NF-4/AC-21 and REQ-NF-5/AC-22 — Judge LOW, parked).
- L2 transform mechanism (N uniform buffers + bind groups): firm REQs
  (REQ-NF-1/2, AC-18/19) — **confirmed at LOCK**.
- Interplay with unimplemented mode 1004 (focus reporting): Ghostty
  sends per-surface focus events on split focus change; mode 1004 is
  Phase 1 work — when it lands, split focus switches must emit CSI I/O
  per pane. Out of scope here; noted for the 1004 implementer.

## Build-path decision

Recorded at LOCK (user-selected): **orbit loop, executor engine =
codex** — turn this spec into a `nexus-autoloop` runner where the L3
acceptance criteria form the machine-checkable completion contract;
unattended/resumable execution, each iteration run by Codex CLI. Same
configuration as the tabs spec's successful precedent.

- Handoff target: `orbit` agent (loop generation) —
  `~/.claude/skills/orbit/SKILL.md`; pass engine=codex so the runner's
  `EXEC_CMD` targets Codex CLI.
- Engine prereqs to verify before generation: `codex features list`
  shows `multi_agent = true`, and `~/.codex/config.toml` has
  `[agents] max_depth >= 2`.
- The loop's DONE gate covers the unit/integration ACs
  (AC-3/4a/5/6/8/9/10/11a/12/13/14/15/16/17a/18/19/20/21/22/23/24/25/
  26/28/29/30/31); the manual-visual set (AC-1/2/4b/7/11b/17b/27) is
  the human acceptance pass after the loop completes.
- Fallbacks if orbit/codex prereqs fail: orbit + claude engine,
  `/nexus apex` (single bounded run), or `/nexus feature` (supervised).
- N-snapshot per-frame cost: RESOLVED — accepted for v1 (REQ-NF-11,
  AC-27); dirty-row diffing is the explicit follow-up.
- Odd-pixel remainder: RESOLVED — REQ-SPL-14 (divider width reserved
  first, leftover pixel to the first/left/top pane).
