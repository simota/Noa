# Spec: Pane Movement via the Overview Layout Minimap (pane-dnd)

## Metadata
- slug: `pane-dnd`
- title: Move panes by drag & drop in the Overview layout minimap
- status: `locked`
- owner: simota
- language: English

## L0 — Vision
- **Problem**: noa implements split panes (`SplitTree`), but without a movement
  mechanism, rearranging a layout requires closing panes and re-splitting.
- **Audience**: heavy split users (developers running sessions across multiple
  panes/tabs).
- **Job**: reposition panes — within a tab and across tabs — without disturbing
  the terminal (Pty/Surface), from a single bird's-eye surface.
- **Success**: a drag in the Overview moves the pane to the intended position,
  `SplitTree` is transformed correctly, and the shell/process continues
  uninterrupted.

## Terminology
- **Tab**: one `WindowState` (one NSWindow; winit reports each as a distinct
  `WindowId`). A tab owns `split_tree: SplitTree` and
  `surfaces: HashMap<PaneId, Surface>`.
- **Window group**: a `WindowGroupId` groups the NSWindows (tabs) of one macOS
  tab group (`app/state.rs`).
- **Overview**: the full-screen tab overview. It is the **sole pane-movement
  surface**: one tile per tab, reproducing that tab's internal split layout.

## UX Specification (Overview layout minimap)

### Tiles
- One tile per tab. The tile composites each pane's rendered texture at its
  scaled `SplitTree` sub-rect (`session_overview/tab_tiles.rs`:
  `tab_tile_content_rect` / `tab_tile_pane_rects`), so tile geometry matches
  the live tab exactly.
- Title band shows the tab title; the attention dot color aggregates the tab's
  panes (red > yellow > blue). Paging and search operate on tabs; search
  filters on the tab title.

### Click (below drag threshold)
- Press + release within 5px (DPR-scaled): select that tab AND focus the pane
  under the cursor. Press in a divider gap falls back to the tab's focused
  pane. The close button closes the tab's focused pane.

### Drag
- Left-press on a tile body, movement past the 5px threshold starts a pane
  drag (no modifier key). The pane under the press point is the drag source.
- **In-tab rearrange** (drop within the source tile): 60/40 zones on the
  target pane — center 60% = **swap** (`swap_pane_with_zoom`; tree shape
  unchanged, Leaf ids exchanged), outer 40% = **split insert** in that
  direction (`move_pane_with_zoom`).
- **Cross-tab move** (drop on a pane inside another tab's tile): the pane
  moves to that tab with position targeting via
  `move_pane_to_tab_at(source_window, pane, dest_window, Some((target, dir)))`
  — edge zones = insert at the target pane's edge in that direction, center =
  insert to the **right** of the target pane. Cross-tab swap does not exist
  (the engine is one-way).
- **Zone feedback**: a ring around the resolved zone rect — center = inner-box
  outline, edge = edge band; the shape difference (not color alone)
  distinguishes swap vs insert. Only resolutions that would actually commit
  light up: valid targets are same-window-group only, and the source pane's
  own zones never highlight.
- **Floating chip**: the dragged pane's sub-rect sampled from the tile texture
  (`src_uv`), roughly half size at 70% opacity, cursor-following. All drag
  visuals are cached; no per-frame allocation.
- **Cancel** (restores prior state, no tree change): release on the source
  pane / a same-window sibling zone that resolves to no-op / no tile /
  out-of-bounds; page or search change mid-drag; Overview close; host window
  focus loss; tab or pane close mid-drag. One shared teardown covers all
  paths. The Overview mouse path never forwards to a PTY.
- No page auto-flip during a drag: drop targets are the current page's tiles.

## Engine Specification

### SplitTree primitives (`split_tree/tree/reposition.rs`, `zoom.rs`)
- `swap_pane(tree, a, b)` — exchanges two Leaf `PaneId`s; no-op on self/absent.
- `extract_pane(tree, pane) -> RemoveOutcome { next_focus, tab_should_close }`
  — removes a pane, collapsing the parent split; never touches Surface/Pty.
- `move_pane(tree, moved, target, direction, tab_cap_ok)` — atomic
  extract+insert composition: runs against a clone and commits only on
  success, so every rejection leaves the tree unchanged. Enforces the axis cap
  (`MAX_PANES_PER_AXIS`, via `can_add_pane_in_direction`) itself; callers must
  compute `tab_cap_ok` from real geometry (`can_create_split`,
  `MAX_PANES_PER_TAB`).
- `*_with_zoom` wrappers: any successful swap/move force-unzooms the tab
  regardless of which pane is zoomed; rejected operations leave zoom intact.
- `PaneId::alloc()` is a process-global monotonic allocator — PaneIds are
  never reused, so cross-tab `surfaces` key transfer cannot collide. Session
  restore does not persist numeric PaneIds (fresh allocation on restore).

### Cross-tab move (`app/split_ops.rs::move_pane_to_tab_at`)
On a validated move (destination in the same window group; both caps checked
against real destination geometry; named target must exist):
1. Pre-detach teardown for the moved pane: copy mode, search prompt, confirm
   dialog / inline rename / send-selection picker / remote-ui Retry referencing
   it are dismissed; a live IME composition is committed and the OS IME session
   reset; hover-link state cleared; FocusOut (CSI O, mode-gated) is sent when
   the moved pane was the OS-focused window's focused pane.
2. The Surface moves between `surfaces` maps with its identity preserved (the
   Pty is carried alive — never respawned).
3. The io thread's shared `window_id` cell (`Arc<AtomicU64>`,
   Release/Acquire) is stored **before** the insert; every io-thread send site
   (Redraw, PtyExit, Clipboard read/write, Notify, both AutoApprove sites)
   rebuilds its target through one helper per send. The auto-approve flag and
   `RedrawFloor` are re-pointed to the destination tab through swappable
   holders (future toggles included).
4. Session-state re-keying follows the card id: session store card, attention
   onset, auto-approve flash, foreground-process probe, and IPC registrations
   (registry + attach_panes + terminals, single-lock `resolve_terminal`).
5. Focus: the moved pane becomes the destination's focused pane; the source
   keeps its focus unless the moved pane held it (successor then chosen).
   The destination renderer invalidates its cache for the moved PaneId.
6. An emptied source tab closes through the normal close path. (A same-group
   destination always exists at commit time, so the close can never cascade to
   app exit.)

### Event delivery
Pane-scoped `UserEvent`s — including AppleScript WriteText / RaiseWindow /
ClosePane — re-resolve the owning window by process-global PaneId at receive
time, so events queued before a move still reach the pane. Stale session
deltas for panes that no longer resolve anywhere are dropped
(`SessionDelta::Remove` still applies).

## Requirements

| ID | Requirement |
|----|-------------|
| FR-1 | Overview tiles are per-tab and reproduce the tab's internal split layout at scaled sub-rects. |
| FR-2 | Click (< 5px) selects the tab and focuses the clicked pane; ≥ 5px starts a pane drag; no modifier key. |
| FR-3 | In-tab drop: center 60% = swap, outer 40% = directional split insert, with force-unzoom on success and both caps enforced. |
| FR-4 | Cross-tab drop: position-targeted insert via `move_pane_to_tab_at` (center = right of target), Pty carried alive, same-window-group only. |
| FR-5 | All cancel paths (source/self, no tile, page/search change, overview close, focus loss, tab/pane close) restore the prior state via one shared teardown. |
| FR-6 | All per-pane state follows a cross-tab move: session card, attention onset, auto-approve flash+flag, process probe, IPC registrations, redraw floor, io-thread event targets. |
| FR-7 | Zone feedback is shape-distinguished (not color-only); invalid targets never highlight. |
| NFR-1 | Drag visuals allocate nothing per frame; p95 per-frame CPU time stays within the 8ms redraw-floor budget. |
| NFR-2 | Any interruption at any drag/commit stage leaves `SplitTree`, `surfaces`, and Pty consistent (no panic, no orphan Surface). |
| NFR-3 | The Overview mouse path never emits PTY input or tracking reports. |

## Verification
- Tree primitives, caps, zoom, PaneId uniqueness, and the operation-sequence
  invariant (`surfaces` keys == live leaf ids, no panics) are unit-tested in
  `split_tree/tree/tests.rs`.
- Drop resolution (in-tab center/edge, cross-tab center/edge, self-drop,
  foreign-group, no-tile) is unit-tested via
  `session_overview::resolve_overview_drop`; tile pane hit-testing via
  `tab_tile_pane_at_point`.
- Cross-tab engine behavior (named-target insert, cap rejections, Surface
  identity via `Arc::ptr_eq`, focus preservation) is unit-tested in
  `app/split_ops.rs`; re-keying in `session_store.rs` / `ipc_bridge.rs` /
  io-thread tests.
- GPU composition (pane sub-rect targeting, clipped overlay draw) is verified
  by headless real-adapter tests in `noa-render/tests/pipeline/`.
- Visual appearance and real-device interaction (chip, rings, drag feel)
  require manual GUI verification.

## Out of Scope
- Pane movement in the main terminal view (drag gestures, keyboard commands,
  or menu items) — the Overview is the sole movement surface.
- Cross-window-group moves, moving to a new window (detach), and D&D on the
  macOS native tab bar.
- Cross-tab swap (the engine is one-way move).
- Page auto-flip while dragging.

## Deferred
- In-tile "Swap"/"Split" text labels on zone rings (shape cue shipped; label
  raster threshold designed at ≥ 120×40 px target sub-rect).
- Live-content floating chip enhancements beyond the sampled tile texture.
