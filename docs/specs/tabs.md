# Tabs

> **Historical baseline:** The Problem and FRAME sections describe the
> pre-implementation architecture at specification time. Native tabs,
> multi-window state, and split-aware sessions are implemented; use
> `docs/FEATURES.md` and current symbols rather than the historical line
> numbers below when navigating the code.

## Metadata

- slug: tabs
- feature title: Terminal Tabs
- status: locked
- owner: noa maintainers
- current phase: LOCKED
- build-path decision: orbit loop (engine: codex) — see Build-path
  decision section
- direction pick (CHALLENGE, user-confirmed): **A — native macOS tabs,
  lightweight; Surface abstraction deferred to splits.** Condition: one
  cohesive per-window struct; Ripple's 4 mitigations become ACs.
- scope change (CHALLENGE, user-confirmed): cwd inheritance deferred to
  Phase 5 (OSC 7 proper); v1 opens tabs in the login-shell default dir.
- build-path decision: (decided at LOCK)

## L0 - Vision

### Problem

`noa` is hard-wired to one window / one terminal session: `App`
(crates/noa-app/src/app.rs:51-69) holds exactly one Window, Renderer,
`Arc<Mutex<Terminal>>`, and io thread; `UserEvent` carries no surface
identity; `AppCommand::CloseWindow` quits the whole app. Users cannot run
multiple shell sessions in one app instance.

### Audience

- macOS users of `noa` expecting Ghostty's tab workflow.
- Contributors who need a Surface abstraction as the substrate for later
  Phase 4 work (splits, multi-window).

### Job To Be Done

Open, switch, and close multiple terminal sessions as tabs with Ghostty's
exact operational feel (Cmd+T / Cmd+W / Cmd+1..9 / Cmd+Shift+[ ]).

### Success Definition

Tab behavior is indistinguishable from Ghostty on macOS for the in-scope
operations — with one recorded, accepted v1 deviation: closing the last
tab quits the app (Ghostty defaults to staying resident,
`quit-after-last-window-closed = false`; see Open Questions). Existing
single-terminal behavior (grid-first resize, VT conformance, clippy/test
suite) regresses nowhere.

## Scope (confirmed at FRAME)

### In scope

- Core tab operations: new / close / select-by-index / next-prev cycling
  (Cmd+T, Cmd+W, Cmd+1..9, Cmd+Shift+[ ]) + tab bar UI.
- Per-tab title (OSC window title reflected per tab).
- Adding tab-related macOS menu items (net-new: grep confirms
  macos_menu.rs has no tab items today; the macos-app-menus spec merely
  *permits* placeholders — creation follows its AppCommand/menu_id
  pattern).
- Occlusion-based redraw suppression for non-visible tabs (added at
  CHALLENGE from the Flux cost-model finding; Magi risk register requires
  it in the same PR).
- Routing of existing per-terminal commands (copy/paste/search/selection/
  IME/font-size) to the focused tab's Terminal.
### Out of scope

- Splits (SplitTree), multi-window, session/window restoration
  (separate Phase 4+ items).
- cwd inheritance for new tabs — deferred to Phase 5 with OSC 7 proper
  (CHALLENGE decision; v1 opens tabs in the login-shell default dir).

## Reuse / constraint findings (Lens, FRAME)

Enablers:

- `Terminal::new` / `Pty::spawn` / `io_thread::spawn` are per-instance
  clean — N-plication is mechanical (app.rs:274, noa-pty/src/pty.rs:63,
  io_thread.rs:52). `EventLoopProxy` is `Clone`.
- `KeybindEngine` already parses `cmd+t`-style chords generically
  (commands.rs:172-266); `AppCommand` + `menu_id` round-trip pattern
  exists (commands.rs:7-153, macos_menu.rs).
- `FontGrid`/`Atlas` (noa-font) is shareable across tabs; wgpu
  Device/Queue already created once and shareable.

Constraints:

- Surface abstraction {Terminal, Pty, io thread, renderer state} is a
  structural rewrite of `App`'s five parallel fields, not additive
  (ghostty-parity-plan.md:105-106).
- `UserEvent` (events.rs:8-17) needs a surface/tab identity on Redraw /
  PtyExit / ClipboardWrite.
- `Renderer` owns one viewport (one atlas texture + instance buffer);
  decide per-tab renderers vs re-bindable single renderer.
- `AppCommand::CloseWindow` == `event_loop.exit()` must split into
  close-tab vs close-last-tab-quits semantics.
- `noa-config` `SUPPORTED_KEYS` allowlist hard-rejects unknown keys —
  new config keys need schema additions.
- Grid-first resize ordering must hold per surface.
- Tab close needs an orderly io-thread shutdown path (reader blocks on
  read).
- Parity plan sequencing note: Phase 4 nominally depends on Phases 1-3
  structural work; nothing in code enforces it (flagged as risk).

## Candidate directions (EXPAND)

Key evidence (Flux, verified): winit 0.30.13 (Cargo.lock) ships the full
macOS native-tab API — `WindowAttributesExtMacOS::with_tabbing_identifier`,
`WindowExtMacOS::{select_next_tab, select_previous_tab,
select_tab_at_index, num_tabs}` — a thin wrapper over AppKit
`NSWindow.tabbingIdentifier`/`tabGroup`, the same mechanism Ghostty itself
uses. Native tabs ARE separate NSWindows merged visually by the OS, so
N tabs → N windows → N wgpu surfaces is the standard multi-window pattern;
winit's `EventLoop` stays a process singleton dispatching per `WindowId`.

- **A. Native tabs, lightweight (no upfront Surface abstraction)** —
  each tab = a winit Window created with a shared tabbing identifier; N
  near-verbatim instances of today's {Terminal, Pty, io thread, Renderer}
  wiring keyed by `WindowId`, sharing wgpu Device/Queue + FontGrid/Atlas.
  Full Surface/viewport abstraction deferred to splits (the feature that
  structurally needs it). Tradeoff: Phase-4 splits later re-touch this
  code; native tabs confuse tiling WMs (Ghostty's own acknowledged regret,
  ghostty #10711) — accepted, since fidelity to Ghostty is the repo goal.
- **B. Surface-refactor-first, then native tabs** — PR1 mechanically
  extracts a `Surface` struct (behavioral no-op, suite stays green), PR2
  adds tab grouping + keybinds on the clean seam. Tradeoff: no visible
  feature until PR2; pulls the splits-grade abstraction forward for a
  feature that doesn't structurally need it.
- **C. Single-window custom tab bar** — one Window/one swapchain;
  `Vec<Surface>` state-side only; tab strip drawn as instanced quads from
  the shared atlas; background tabs stream pty output unrendered.
  Tradeoff: every native affordance (drag-to-detach, tab overview, OS tab
  prefs) hand-built or a permanent gap — in tension with "indistinguishable
  from Ghostty on macOS".
- **D. Tabs-first vertical spike** — `Vec<TabState>` inline in `App`,
  Cmd+1..9 switching only, no chrome; proves event-routing + io-shutdown
  unknowns. Tradeoff: throwaway-shaped; Flux's verification already
  de-risked the native path, so spike value is low.

Cost model gap found (Flux): background tabs firing `UserEvent::Redraw`
from noisy shells waste presents — `WindowEvent::Occluded` is supported on
macOS but unconsumed today; tab spec must handle redraw suppression for
non-visible tabs.

## Proposal (SHAPE)

### Proposed solution

Introduce `WindowState`, one cohesive struct holding everything today's
singleton `App` fields hold, per instance: `Arc<Mutex<Terminal>>`, pty
input/resize senders, io-thread join + shutdown handle, and per-window
renderer state (wgpu surface, instance buffer, viewport). `App` replaces
its five parallel fields with `windows: HashMap<WindowId, WindowState>` +
`focused: WindowId`. `spawn_tab()` creates a new winit `Window` via
`WindowAttributesExtMacOS::with_tabbing_identifier` (one shared group
identifier, so AppKit merges tabs visually), builds a fresh
Terminal/Pty/io-thread trio exactly as today's `resumed()` does, and
inserts the `WindowState`; `resumed()` becomes `spawn_tab()`'s first
caller. wgpu `Device`/`Queue` and `FontGrid`/`Atlas` stay app-level
singletons shared into every renderer; each renderer tracks a
per-consumer atlas **generation counter** compared against the shared
atlas generation (replacing the read-and-clear `take_dirty()` bool) so
every tab's `sync_atlas()` sees pending uploads regardless of paint
order. `UserEvent` gains a `WindowId` on every per-surface variant
(Redraw/PtyExit/ClipboardWrite); `user_event()` looks up the target
`WindowState` and no-ops on stale IDs (closed-tab races) — landed as one
atomic PR. New `AppCommand` variants (NewTab, CloseTab, SelectTab(n),
NextTab, PrevTab) route through the existing keybind/menu_id plumbing.
`CloseTab` removes one `WindowState` and drives an explicit io-thread
shutdown primitive (close pty master / kill child, then join — channel
drop does NOT unblock the reader). `WindowEvent::Occluded(bool)` is
tracked per `WindowState` and gates redraw/present for hidden tabs.

### Sub-features (MoSCoW)

| Sub-feature | MoSCoW | Note |
|---|---|---|
| Tab create/close/select/cycle (Cmd+T/W/1..9/Shift+[/]) | Must | Core ACs |
| io-thread shutdown primitive | Must | Reader blocks forever otherwise |
| Atlas generation-counter fix | Must | Silent glyph corruption otherwise |
| Focused-tab tracking (`focused: WindowId`) | Must | Keybind/menu/title routing |
| Native tab bar (AppKit) | Should | Free via tabbing identifier; verify |
| Per-tab title (OSC → tab label) | Should | Existing title path, routed per window |
| Tab menu items (net-new) | Should | AppCommand/menu_id pattern |
| Occlusion redraw suppression | Must | Magi risk register: same-PR requirement (background present waste) |
| Drag-to-detach / tab overview | Must (verify-only) | Free from AppKit — "don't regress", not "build" |
| New noa-config keys | Won't (v1) | No confirmed need; allowlist untouched |

### Assumptions

- winit 0.30.13's macOS tab API covers all in-scope ops without raw
  AppKit/objc calls (API surface verified; multi-tab runtime behavior
  unverified).
- N WindowState resource cost acceptable at single-digit..low-tens tabs;
  no pooling in v1.
- Per-WindowId io-thread + EventLoopProxy::clone() adds no meaningful
  latency vs today.
- Existing single-terminal invariants (grid-first resize, VT
  conformance) are per-Terminal already; only the atlas fix touches
  shared state.

## L1 — Requirements

### Functional

- **REQ-TAB-1**: Cmd+T creates a new tab (fresh `WindowState`: Terminal+Pty+io-thread+renderer) and focuses it.
- **REQ-TAB-2**: Cmd+W on a non-last tab tears down that tab's `WindowState` without affecting other open tabs.
- **REQ-TAB-3**: Cmd+1..9 focuses the tab at that 1-indexed position in the AppKit tab group's current visual order (post-drag order, delegated to `WindowExtMacOS::select_tab_at_index`) if it exists; focus is unchanged otherwise.
- **REQ-TAB-4**: Cmd+Shift+] / Cmd+Shift+[ cycles focus to the next/previous tab, wrapping at the ends.
- **REQ-TAB-5**: All tabs of a window group render as one native macOS tab bar via a shared `tabbingIdentifier`.
- **REQ-TAB-6**: A single `focused: WindowId` is the source of truth for keybind, menu, and title routing.
- **REQ-TAB-14**: Existing per-terminal commands (Copy, Paste, Search, selection, IME input, font-size changes) operate on the focused tab's Terminal only.
- **REQ-TAB-7**: Each tab's OSC window-title update changes only that tab's own label.
- **REQ-TAB-8**: The macOS app menu exposes New Tab / Close Tab / Next Tab / Previous Tab, wired through the existing `AppCommand`/`menu_id` pattern.
- **REQ-TAB-9**: Closing a tab shuts down its io thread deterministically (explicit primitive, not channel-drop) before its `WindowState` is dropped.
- **REQ-TAB-10**: Closing the last remaining tab quits the application. **Accepted v1 deviation from Ghostty's default resident behavior** (recorded in Success Definition + Open Questions; re-confirm at LOCK).
- **REQ-TAB-11**: A tab that receives `WindowEvent::Occluded(true)` suppresses its own redraw/present until occlusion clears.
- **REQ-TAB-12**: Existing single-terminal behavior — grid-first resize ordering, VT/grid conformance suite, pre-existing keybinds — is unchanged by multi-tab.
- **REQ-TAB-13**: Native drag-to-detach and the AppKit tab overview continue to work unmodified (verify-only; not built by noa).

### Non-Functional

- **REQ-NF-1**: Every tab's renderer observes a shared `Atlas` mutation exactly once, regardless of frame paint order (generation counter, not a consumed dirty bool).
- **REQ-NF-2**: Per-surface `UserEvent`s (Redraw/PtyExit/ClipboardWrite) referencing a since-closed `WindowId` are dropped safely (no panic, no crash).
- **REQ-NF-3**: The `UserEvent` WindowId-payload change (events.rs, io_thread.rs, app.rs) lands as one atomic PR — not incrementally shippable.
- **REQ-NF-4**: Tab-routing decisions (index lookup, next/prev cycling, close-last-tab-quits) are pure functions, unit-testable without constructing a `Window`/`ApplicationHandler`.
- **REQ-NF-5**: `cargo test --workspace` and `cargo clippy --workspace` stay green with the tab feature landed.
- **REQ-NF-6**: Resource cost of N `WindowState`s (Terminal+Pty+io-thread+renderer each) is acceptable at single-digit..low-tens tab counts; no pooling required in v1.

## L2 — Detail

### noa-app

- `App` replaces its five singleton fields with `windows: HashMap<WindowId, WindowState>` + `focused: WindowId`.
- `WindowState`: one cohesive struct — `Arc<Mutex<Terminal>>`, pty input/resize senders, io-thread join+shutdown handle, per-window renderer state (surface, instance buffer, viewport), `occluded: bool`.
- `spawn_tab()`: builds a `Window` via `WindowAttributesExtMacOS::with_tabbing_identifier` (one shared group id) plus a fresh Terminal/Pty/io-thread trio, exactly as today's `resumed()`; `resumed()` becomes its first caller.
- New `AppCommand` variants (`NewTab`, `CloseTab`, `SelectTab(usize)`, `NextTab`, `PrevTab`) route through the existing `KeybindEngine`/`menu_id` plumbing.
- **Keybind remap**: the default `cmd+w` binding (commands.rs:180, currently `AppCommand::CloseWindow` → `event_loop.exit()` at app.rs:167) is remapped to `CloseTab`. `CloseWindow` remains for the whole-window path — triggered by `WindowEvent::CloseRequested` (AppKit traffic-light / titlebar close), which routes to `close_tab(window_id)` for that tab-window, same path as Cmd+W. Quitting happens only via `Quit` (Cmd+Q) or `close_tab` on the final tab.
- **Tab ordering**: Cmd+1..9 and next/prev cycling delegate to `WindowExtMacOS::{select_tab_at_index, select_next_tab, select_previous_tab}`, i.e. the AppKit tab group's live visual order (post-drag). App code maintains no separate order Vec for selection; `windows` map + `focused` cover lifecycle/routing only.
- Per-terminal `AppCommand`s (Copy/Paste/Search/font-size/…) resolve their target Terminal via `focused` (REQ-TAB-14); IME and selection state live per `WindowState`.
- `close_tab(id)`: removes the `WindowState`, drives the io-thread shutdown primitive, reassigns `focused` (or quits if the map is now empty).
- `UserEvent` gains `WindowId` on every per-surface variant; `user_event()` looks up the target `WindowState` and no-ops on a missing id.
- `macos_menu.rs`: add tab menu items — needs a focused-tab concept threaded from `App` (net-new; none exists today).
- Extract pure, winit-free seams for tab-index lookup, cycling math, and close-last-tab-quits so they are directly unit-testable.

### noa-render

- Each `WindowState` owns its own renderer instance (surface + instance buffer); wgpu `Device`/`Queue` stay app-level singletons injected into every renderer.
- Renderer tracks a per-consumer `atlas_seen_generation`, compared against the shared `Atlas`'s generation counter on each `sync_atlas()` call.
- Redraw/present is suppressed at the `App` level for occluded tabs before it reaches the renderer, not inside the renderer itself.

### noa-font

- `Atlas::dirty`/`take_dirty()` (single read-and-clear bool) is replaced by a monotonically-bumped `generation: u64` plus a `generation()` getter; `FontGrid`/`Atlas` remain app-level singletons shared by every tab's renderer.

### noa-pty

- `Pty` gains an explicit shutdown primitive (close the pty master / terminate the child) so the io thread's blocking `select!` on `pty.event_rx()` unblocks — channel drop alone does not.
- `io_thread.rs` exits its loop on the shutdown signal; `close_tab()` joins the thread (bounded wait) before dropping the `WindowState`.

## L3 — Acceptance Criteria

- **AC-1** (REQ-TAB-1) [manual-visual] — Given noa open with one tab, When the user presses Cmd+T, Then a new tab appears in the native tab bar, spawns a fresh login shell, and becomes focused.
- **AC-2** (REQ-TAB-2) [manual-visual] — Given 2+ open tabs, When the user presses Cmd+W on a non-last tab, Then that tab's Terminal/Pty/io-thread tear down, the tab disappears, and other tabs' state is untouched.
- **AC-3** (REQ-TAB-3) [manual-visual] — Given N tabs open, When the user presses Cmd+k (1≤k≤9), Then tab k gains focus if it exists, else focus is unchanged.
- **AC-4** (REQ-TAB-4) [manual-visual] — Given 3+ tabs with tab 2 focused, When the user presses Cmd+Shift+] then Cmd+Shift+[, Then focus moves to tab 3 then back to tab 2, wrapping at the ends.
- **AC-5** (REQ-TAB-5) [manual-visual] — Given 2+ tabs open, When the window is inspected, Then all tabs appear in one native macOS tab group (one title bar, OS tab strip), not separate windows.
- **AC-6** (REQ-TAB-6) [unit] — Given a `windows` map and a sequence of select/close events, When the pure focused-tab helper runs, Then it returns exactly one unambiguous `focused: WindowId` after each event.
- **AC-7a** (REQ-TAB-7) [unit] — Given two independent Terminal+Stream instances, When each is fed a distinct OSC window-title sequence, Then each Terminal's title state reflects only its own sequence (mirrors the existing feed_terminal test pattern).
- **AC-7b** (REQ-TAB-7) [manual-visual] — Given two tabs with distinct shell titles, When the tab bar is inspected, Then each native tab label shows its own title.
- **AC-8** (REQ-TAB-8) [manual-visual] — Given the app menu is open, When the user selects New Tab / Close Tab / Next Tab / Previous Tab, Then each performs the same action as its keybind equivalent.
- **AC-9** (REQ-TAB-9) [unit] — Given an io thread blocked on `pty.event_rx()`, When the shutdown primitive closes the pty master / kills the child, Then `join()` returns within a bounded timeout instead of hanging.
- **AC-10** (REQ-TAB-10) [manual-visual] — Given exactly one tab open, When the user closes it (Cmd+W), Then the application quits.
- **AC-11a** (REQ-TAB-11) [unit] — Given the pure occlusion-gate decision function, When evaluated for `occluded=true` with pending pty output, Then it returns suppress; and returns redraw once `occluded=false`.
- **AC-11b** (REQ-TAB-11) [manual-visual] — Given a tab fully covered (`Occluded(true)`), When its shell produces continuous output, Then no present occurs for it (debug frame counter) until it is revealed.
- **AC-12** (REQ-TAB-12) [integration] — Given the existing noa-vt/noa-grid suites and the grid-first-resize test, When run against the multi-tab codebase, Then all pass unchanged and `cargo clippy --workspace` stays clean.
- **AC-13** (REQ-TAB-13) [manual-visual] — Given 2+ tabs open, When the user drags a tab out of the tab bar, Then AppKit detaches it into its own window, unmodified by noa code.
- **AC-14** (REQ-NF-1) [integration] — Given two renderer instances built against one headless Device/Queue and a shared `FontGrid`/`Atlas` (pipeline.rs precedent), When a glyph pack bumps the atlas generation and both call `sync_atlas()` in the same frame (either order), Then both observe the mutation via their tracked generation.
- **AC-15** (REQ-NF-2) [unit] — Given a `UserEvent::Redraw(WindowId)` whose id is no longer in `windows` (closed-tab race), When `user_event()` processes it, Then it no-ops without panicking.
- **AC-16** (REQ-NF-4) [unit] — Given the pure tab-lifecycle helpers (focus reassignment on close, close-last-tab-quits decision, command-target resolution) with table-driven inputs (empty/one/many tabs, stale ids), When exercised directly, Then each returns the expected `WindowId`/quit decision without constructing a `Window`. (Index selection/cycling is delegated to AppKit via winit and excluded here.)
- **AC-17** (REQ-NF-5) [integration] — Given the new tab-feature tests (AC-6/7a/9/11a/14/15/16/19), When `cargo test --workspace` and `cargo clippy --workspace` run, Then the new tests execute in the standard gate (no `#[ignore]` beyond the documented pty sandbox constraint) and clippy stays clean. (AC-12 gates regression of *existing* suites; AC-17 gates integration of the *new* tests.)
- **AC-18** (REQ-NF-6) [manual-visual] — Given 10 tabs opened in sequence with shells running, When the demonstration protocol runs — type a burst in the focused tab, switch through all tabs with Cmd+1..9, and check the AC-11b debug frame counter — Then typing echo in the focused tab is hand-verified as immediate, tab switches render on first frame, and occluded tabs record zero presents.
- **AC-19** (REQ-TAB-14) [unit] — Given the command-target resolution helper and a `windows` map with a known `focused` id, When each per-terminal `AppCommand` (Copy/Paste/Search/font-size) is dispatched, Then the resolved target is the focused tab's Terminal only (table-driven, no `Window` constructed).
- **Process note** (REQ-NF-3): atomic landing is enforced by PR review scope (single PR spanning events.rs/io_thread.rs/app.rs), not a runtime-testable AC.

## Stress-test findings (CHALLENGE)

- **Magi verdict: 3-0 for A** (Logos 78 / Pathos 68 / Sophia 80).
  Condition: keep per-window {Terminal, Pty, io-thread handle, renderer
  state} in ONE cohesive struct keyed by `WindowId` — never scattered
  `App` fields — so the eventual Surface extraction for splits stays
  mechanical. Strongest counter-argument (recorded): if splits arrive
  soon, B's upfront investment could have been cheaper in total.
- **Void subtraction:** cwd inheritance is NOT a small pull-forward —
  zero OSC 7 code exists anywhere (parser/handler/grid); it is net-new
  VT-core surface (URI parse, percent-decode, host match, cwd state,
  Pty::spawn wiring) and inverts Phase 4→5 ordering. Recommend DEFER to
  Phase 5; v1 tabs open via the login-shell default dir (strict subset of
  eventual behavior, zero rework).
- **Ripple impact (candidate A): risk ~7 HIGH, Conditional-Go** with
  four mandatory mitigations:
  (a) **Atlas dirty-flag bug**: `Atlas::dirty` is a single read-and-clear
  bool (`take_dirty()`, noa-font atlas.rs:122-125); N renderers sharing
  one `FontGrid` means only the first `sync_atlas()` per frame sees
  dirty=true → stale/missing glyphs in other tabs. Needs a per-consumer
  generation counter.
  (b) **io-thread shutdown**: the `select!` arm on `pty.event_rx()`
  (io_thread.rs:63-76) blocks until the child exits; dropping the other
  channels does not unblock it. Tab close needs an explicit shutdown
  primitive (kill child / close pty master, then join).
  (c) **Atomic landing**: `UserEvent` gaining a `WindowId` payload fans
  out across events.rs, io_thread.rs send sites, and app.rs routing —
  not separately shippable; one PR.
  (d) **Test seams**: zero existing tests exercise `App`/
  `ApplicationHandler` (app.rs unit tests hit only pure helpers) — new
  pure-function seams (tab lookup/routing, close-last-tab-quits) must be
  extracted and unit-tested.
  Blast radius: app.rs L/CRITICAL (5 singular fields → WindowId-keyed
  collection; `resumed()` → callable `spawn_tab()`; all handlers become
  per-tab lookups), io_thread.rs S + shutdown gap, events.rs S but
  breaking, commands.rs S/M (idiom fits), macos_menu.rs S/M (needs a
  focused-tab concept — none exists today), renderer.rs S + atlas fix.

## Considered but rejected

(user-confirmed at CHALLENGE)

- **B. Surface-refactor-first** — speculative generality: pulls a
  splits-grade abstraction forward for a feature that doesn't need it;
  no visible value until PR2; splits' requirements are not yet known, so
  the abstraction risks being shaped wrong.
- **C. Single-window custom tab bar** — hand-rolls OS chrome
  (drag-to-detach, tab overview, OS tab prefs) that A gets free from
  AppKit; contradicts "indistinguishable from Ghostty on macOS";
  permanent maintenance surface.
- **D. Vertical spike** — Flux's winit API verification already
  de-risked the unknowns a spike would probe; throwaway-shaped code.

## Open Questions / Deferred Decisions

- Sequencing: pulling tabs ahead of remaining Phase 1-3 items — accepted
  risk or gate?
- **Close-last-tab semantics (CONFIRMED at LOCK):** v1 quits the app
  when the last tab closes — an accepted deviation from Ghostty's default
  `quit-after-last-window-closed = false` (resident app, macOS
  convention). Adopting resident behavior later needs a zero-window
  NewTab path from the menu; deferred.
- Struct naming defaulted to `WindowState` ("Surface" reserved for the
  future splits abstraction).

## Build-path decision

Recorded at LOCK (user-selected): **orbit loop, executor engine =
codex** — turn this spec into a `nexus-autoloop` runner where the L3
acceptance criteria (AC-1..19) form the machine-checkable completion
contract; unattended/resumable execution, each iteration run by Codex
CLI.

- Handoff target: `orbit` agent (loop generation) —
  `~/.claude/skills/orbit/SKILL.md`; pass engine=codex so the runner's
  `EXEC_CMD` targets Codex CLI.
- Engine prereqs to verify before generation: `codex features list`
  shows `multi_agent = true`, and `~/.codex/config.toml` has
  `[agents] max_depth >= 2`.
- Manual-visual ACs (AC-1..5, 7b, 8, 10, 11b, 13, 18) cannot be
  machine-gated by the loop; the loop's DONE gate covers the
  unit/integration ACs, and the manual-visual set is the human
  acceptance pass after the loop completes.
- Fallbacks if orbit/codex prereqs fail: `/nexus apex` (single bounded
  run) or `/nexus feature` (supervised).
