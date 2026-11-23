# Split zoom animation and tab zoom indicator

Status: design, not built.

## Goal

Toggle Split Zoom (Cmd+Shift+Enter, `AppCommand::ToggleSplitZoom`,
`docs/specs/splits.md` REQ-SPL-10/11) gets a 400 ms "clip reveal" transition,
and a zoomed tab shows a zoom icon in its tab title.

Decided by the maintainer (not reopened here):

- Zoom-in resizes the zoomed pane's grid to the full content rect at once;
  the visible rect (scissor) animates from the pane's tile rect to the full
  rect. Text is never scaled.
- (Revision, approved mockup) The pane's content is anchored to the clip's
  top-left corner: it slides with the clip origin, so the grid's top-left is
  what shows first, never its middle.
- Unzoom shrinks the visible rect from full to tile. The grid keeps its zoomed
  size during the shrink and is resized to the tile once, when the animation
  ends (one SIGWINCH).
- Sibling panes fade 1→0 over the same tween on zoom-in (then are hidden as
  today), 0→1 on unzoom.
- 400 ms, a new constant in `crates/noa-app/src/anim.rs` (`DUR_BASE` stays),
  easing `anim::ease_out_cubic`.
- Toggling mid-animation reverses from the current rect/opacity; the reverse
  takes time proportional to the distance already travelled.
- No animation with macOS Reduce Motion, on close of a zoomed pane, or on any
  other implicit zoom change.
- An icon in the tab's title from the moment zoom is toggled on until unzoom.
  No badge over the pane.

## Code today (as read on this branch)

Zoom state and its decision:

- `WindowState::zoomed: Option<PaneId>` (`crates/noa-app/src/app/state.rs:291`)
  is the only zoom state; there is no tween.
- `toggle_split_zoom` (`crates/noa-app/src/app/split_ops.rs:983-995`) calls the
  pure `zoom_toggle` (`crates/noa-app/src/split_tree/tree/zoom.rs:23-41`),
  stores `decision.zoomed`, then `relayout_and_resize_window` and
  `request_redraw`.
- `relayout_and_resize_window` (`crates/noa-app/src/app/input_ops/layout.rs:71`)
  is where every reflow converges. It builds targets from
  `zoom_resize_targets(&state.split_tree, state.zoomed, bounds)` (layout.rs:134;
  zoom.rs:54-68: the zoomed pane gets `bounds`, every other pane its tree rect),
  sets `surface.rect` and pixel metrics at once (`apply_pane_layout_live`,
  `crates/noa-app/src/app/helpers/resize.rs:85-101`), and hands the grid
  resize + pty winsize to `resize_throttle` (layout.rs:167-169), whose apply
  skips panes whose grid size did not change (resize.rs:119-125). It also ends
  copy mode (layout.rs:74) and may show the resize toast (layout.rs:177-198).
- Implicit zoom changes, each already followed by a relayout:
  new split `split_ops.rs:279`; cross-tab move source `split_ops.rs:585-586`
  and destination `split_ops.rs:648`; focusing another pane while zoomed
  re-zooms to it `split_ops.rs:883-889`; D&D swap/move
  `crates/noa-app/src/app/pane_drag.rs:41-45,105-110`; close
  `crates/noa-app/src/app/lifecycle.rs:2151-2152` (`close_pane_with_zoom`,
  zoom.rs:71-84); revealing a float anchored to a hidden pane
  `crates/noa-app/src/app/floating.rs:1671-1675`. Constructors set
  `zoomed: None` at `lifecycle.rs:682`, `scratch_terminal.rs:417`,
  `quick_terminal.rs:1010`.

Drawing:

- `redraw` (`crates/noa-app/src/app/render.rs`) snapshots only
  `visible_pane_ids(&state.split_tree, state.zoomed)` (render.rs:559, 621;
  `crates/noa-app/src/app/helpers/geometry.rs:135-137`), so hidden siblings
  are not rebuilt while zoomed. Each snapshot carries `surface.rect`
  (`snapshots.push((pane_id, surface.rect, snapshot))`, render.rs:805).
- `Renderer::draw_rebuilt_panes(…, focused, zoomed, force_ring)`
  (`crates/noa-render/src/renderer/mod.rs:1083-1094`, called at render.rs:941-948)
  → `draw_panes` (mod.rs:1098) → pure `build_draw_plan_with_overlays`
  (`crates/noa-render/src/draw_plan.rs:111-185`): one `Clear`, one scissored
  `PaneCells` per pane, `Dividers`, optional `FocusIndicator`, then overlay
  groups (floats). With `zoomed` set only that pane draws, and dividers and
  ring are empty (draw_plan.rs:130-151).
- The scissor is the pane's layout rect; the per-pane uniform carries the pane
  origin (`populate_pane_uniform`, `crates/noa-render/src/instance.rs:117-158`).
  `Uniforms` is 160 bytes and ends in two unused pad floats: Rust
  `_pad: [f32; 2]` (instance.rs:72), WGSL `_pad1`, `_pad2`
  (`crates/noa-render/src/shaders/cell.wgsl:24-25`).
- The cell pipeline blends with `BlendState::ALPHA_BLENDING`
  (`crates/noa-render/src/pipeline.rs:139`); `fs_main` returns `in.color` or
  a coverage-scaled color (cell.wgsl:153-167).
- Default-background cells emit no background quad
  (`crates/noa-render/src/renderer/cell.rs:241-244`): what is under a pane's
  scissor shows through wherever its cells use the default background.
- `backdrop_pipeline` (mod.rs:150-189) fills the scissor with the pass's blend
  constant, *replacing* the destination; `draw_panes` uses it for the
  backdrop and draws the background image layer right after (mod.rs:1170-1203).
- Kitty image bands are drawn inside each pane's `PaneCells` op with their own
  pipeline (mod.rs:1238-1252).
- Scroll thumbs are collected per visible pane from `surface.rect`
  (render.rs:676-684) and drawn after the pane pass without a scissor
  (render.rs:952-988).

Floats: `FloatLayout { tab, window, panes, zoomed }`
(`crates/noa-app/src/floating.rs:127-156`); a pane-anchored float is shown
only when its anchor is not hidden by zoom (`anchor_shown`, floating.rs:150-155;
`pane_under`, floating.rs:1095-1096). The app builds it in `float_geometry`
(`crates/noa-app/src/app/floating.rs:473-494`) from split panes'
`surface.rect`; floats hidden by it get no layout target, so their pty is not
resized (floating.rs app side, comment at 497-499).

Pointer: `pane_cell_at_position` hit-tests `visible_pane_ids(…, state.zoomed)`
with `surface.rect` (`crates/noa-app/src/app/input_ops/pointer.rs:192-204`,
first match wins, `crates/noa-app/src/split_tree/tree/hit_test.rs:11-20`);
divider drags are refused while zoomed (pointer.rs:137-139).

Frame driving: the Session Overview's quick-look zoom (`OverviewZoomAnim`,
state.rs:692-697) starts a `Tween` (`crates/noa-app/src/app/overview/interaction.rs:298-308`)
and, after presenting, requests another redraw until `tween.done`, then drops
it (`crates/noa-app/src/app/overview/render.rs:1174-1182`). Timed work joins
`about_to_wait`'s single `WaitUntil` (`crates/noa-app/src/app/event_loop.rs:575-622`),
e.g. `tick_quick_terminal` (`crates/noa-app/src/app/quick_terminal.rs:809-838`).
`redraw` returns before drawing while the window is occluded (render.rs:548-550).

Reduce Motion: `crate::idle_ember::reduce_motion()`
(`crates/noa-app/src/idle_ember.rs:476-497`, `pub(crate)`, macOS reads
`NSWorkspace.accessibilityDisplayShouldReduceMotion`, `false` elsewhere);
idle ember samples it once per gesture start (idle_ember.rs:211, 272).

Tabs: on macOS every tab is its own `NSWindow` in an AppKit tab group, and
AppKit draws the tab bar (`crates/noa-app/src/app/linux_tabs.rs:1-13`). The
tab label is pushed by `refresh_window_title` (render.rs:306-353) through
`set_title` and `set_native_tab_title` (`crates/noa-app/src/macos_window.rs:175-214`),
only when the resolved title differs from the mirror `state.title`
(`tab_title_update`, `crates/noa-app/src/app/helpers/geometry.rs:178-180`), and
is re-asserted from `state.title` by `tick_native_tab_titles`
(`crates/noa-app/src/app/timers.rs:619-633`). **Noa draws its own tab bar only
off macOS**: `tab_bar_model` (linux_tabs.rs:234-252) → `sidebar::draw_tab_bar`
(`crates/noa-app/src/app/sidebar/palette.rs:692`, called at render.rs:1349-1362).
`state.title` is also read as plain text by the Set Tab Title prompt prefill
(`crates/noa-app/src/app/input_ops/tab_title.rs:22-25`), AppleScript
(`crates/noa-app/src/app/applescript.rs:311,353`), the overview label
(`crates/noa-app/src/app/overview/layout.rs:148-150`) and send-selection
labels (`crates/noa-app/src/app/input_ops/overlays.rs:209-213`), so it must
stay undecorated.

## Model

One value drives everything: the *reveal* `r ∈ [0, 1]` of the animating pane
P (0 = tile rect, siblings opaque; 1 = full rect, siblings gone).

- `clip(r) = lerp_rect(tile(P), bounds, r)`, where `tile(P)` comes from
  `compute_layout(tree, bounds)` and `bounds` from `window_pane_bounds`, both
  re-read every frame, so a window resize mid-tween stays correct. Left/top
  and right/bottom edges are interpolated and rounded separately
  (`w = round(right) - round(x)`), so edges move monotonically without
  1 px jitter.
- `sibling_alpha(r) = 1 - r`.
- A tween segment goes `from → to` (`to` is 1 for zoom-in, 0 for unzoom),
  lasting `DUR_SPLIT_ZOOM × |to − from|`, with
  `r(t) = lerp(from, to, ease_out_cubic(linear_progress(t, dur)))`.
  A fresh zoom-in is `0 → 1` over 400 ms. A toggle mid-tween starts a new
  segment `r(now) → (1 − to)` lasting `400 ms × r(now)` (reversing a zoom-in)
  or `400 ms × (1 − r(now))` (reversing an unzoom): exactly the distance
  already travelled.

Content is anchored to the clip's top-left corner (decided by the approved
mockup). During a tween P's grid already has its zoomed size
(`surface.rect = bounds`, `w × h` of the full rect), but it is drawn at
`content(r) = Rect(clip(r).x, clip(r).y, bounds.w, bounds.h)`: the full-size
grid translated so that its origin is the clip origin. The content therefore
slides from the tile origin to the full origin on zoom-in, and back on unzoom.
The first thing visible in the clip is always the grid's top-left, never its
middle. Text never scales; it only translates, by whole pixels (the clip
origin is rounded). The clip cuts the grid's right and bottom. At `r = 1`
`content = bounds` (the steady zoomed state). At `r = 0` the origin is the tile
origin, which is where the tile-sized grid is drawn after the end-of-unzoom
reflow, so that reflow changes the grid's size but not its origin.

State: `WindowState` gains `split_zoom_anim: Option<SplitZoomAnim>` next to
`zoomed`. `zoomed` stays the *logical* zoom and changes at toggle time, as
today. Three derived values come from pure functions:

| Derived value | Definition | Used by |
|---|---|---|
| `grid_zoomed(zoomed, anim)` | `zoomed.or(anim.pane)` | resize targets (layout.rs:134) |
| `draw_zoomed(zoomed, anim)` | `None` while `anim` is live, else `zoomed` | snapshot set (render.rs:559, 621) and `draw_rebuilt_panes`' `zoomed` |
| `zoom_frame(anim, tile, bounds, now)` | `{ pane, clip, content, sibling_alpha }`, `content` as above | renderer (scissor = `clip`, uniform/image origin = `content`), hit-test and cell mapping, float and thumb suppression |

## Approach per acceptance criterion

### AC1: clip reveal on zoom-in

`toggle_split_zoom` keeps calling `zoom_toggle`, then
`next_split_zoom_anim(prev_zoomed, decision.zoomed, state.split_zoom_anim,
reduce_motion, now)` (pure, below) and stores the result before the existing
`relayout_and_resize_window` call. Because `grid_zoomed` is `Some(P)`, the
relayout resizes P's grid to `bounds` immediately (leading edge of the
throttle), exactly as today, and `surface.rect = bounds`. On the first frame
`r = 0`: the clip is P's tile and the grid's top-left sits at the tile origin,
so P looks as before the toggle, but now holds a full-size grid cut by its
tile. As `r` grows, the clip grows and the content slides with the clip's
top-left corner to the full origin.

Renderer (noa-render):

- New public `SplitZoomFx { pane: PaneId, clip: PaneRect, content_origin:
  (u32, u32), sibling_alpha: f32 }`
  and `Renderer::set_split_zoom_fx(Option<SplitZoomFx>)`, a setter stored on
  the renderer like `overlays`/`backdrop_rect`, so `draw_rebuilt_panes` keeps
  its signature. The app calls it once per frame before `draw_rebuilt_panes`:
  `Some` while a tween is live, else `None`.
- New pure `build_draw_plan_with_zoom_fx(layout, overlays, focused, zoomed,
  force_ring, fx: Option<(PaneId, PaneRect)>)`. The existing
  `build_draw_plan_with_overlays` delegates with `None`, so its output and all
  existing callers and tests stay unchanged. With `fx = Some((P, clip))` and P
  in the base layout: `zoomed` filtering is ignored; every base pane except P
  emits `PaneCells` in layout order; then a new `DrawOp::ZoomUnderlay { rect }`
  with `rect = clip ∩ layout_rect(P)`; then P's `PaneCells` with
  `scissor = rect`. `Dividers { rects: [] }` and no `FocusIndicator` (as when
  zoomed). Overlay groups are unchanged and still drawn after.
- Content origin: `draw_panes` derives a *uniform layout* from `layout` in
  which P's rect `R` becomes `PaneRect::new(content_origin.0,
  content_origin.1, R.w, R.h)`. That rect is translated, not resized. The
  uniform layout feeds `upload_uniforms` (mod.rs:1434-1460) and
  `build_frame_image_draws` (mod.rs:1295-1316, which resolves kitty images
  against the draw-time pane rect). The plan keeps the real `layout`, so the
  scissor is `clip ∩ R`. The uniform field that moves is
  `Uniforms::grid_padding`: `populate_pane_uniform` writes `pane_y + top` into
  `.x` and `pane_x + left` into `.w` (instance.rs:131-136). The vertex shader
  places cells at `(grid_padding.w, grid_padding.x)` (cell.wgsl:105) and reads
  no other `grid_padding` component. `grid_size` and the balanced padding
  (`pane_grid_padding`, mod.rs:636-644) depend only on `R.w`/`R.h`, so they
  are unchanged. The right/bottom components are written clamped with
  `.max(0.0)` (instance.rs:133-134) and are unused, so a translated rect that
  runs past the window edge is harmless. No struct or layout change comes
  from this: it is the same field with a different origin.
- `ZoomUnderlay` is needed because default-bg cells draw no quad
  (cell.rs:241-244): without it, siblings drawn earlier would show through
  P's revealed area. It runs `backdrop_pipeline` with blend constant =
  the frame's `background` color (the same constant the backdrop uses) under
  `set_scissor_rect(rect)`, replacing the destination. It then redraws the
  background image layer within the same scissor, with the viewport rules
  that `draw_panes` already applies to it (mod.rs:1170-1203). That restores
  exactly the pixels the frame's base produced there, including transparent
  `background-opacity` and glass windows. `backdrop_pipeline` is created
  lazily when the plan contains this op (today it is created only for
  `backdrop_rect`, mod.rs:1142-1144).

### AC2: sibling fade both ways

- `Uniforms`: replace the Rust tail `_pad: [f32; 2]` with
  `pane_alpha: f32, _pad: f32`, and WGSL `_pad1: f32` with `pane_alpha: f32`.
  Same position, same size (still 160 bytes; scalar group still
  `min_contrast, blend_mode, pane_alpha, _pad`), so the std140 lockstep holds.
  Update the field-order comments in both files.
- `PaneUniformParams` gains `pane_alpha: f32`. Its only constructor is
  `upload_uniforms` (mod.rs:1434-1460). That function sets it to
  `fx.sibling_alpha` for every non-overlay pane other than `fx.pane`, and to
  `1.0` otherwise. The existing byte-compare in `upload_uniforms` re-uploads
  only when the alpha changed.
- `fs_main` multiplies the returned alpha by `uniforms.pane_alpha` in all three
  returns (color glyph, mask glyph, quad). With straight `ALPHA_BLENDING`
  (pipeline.rs:139) a pane at alpha `a` blends toward the frame base, which is
  the fade.
- Kitty images use their own pipeline and do not read `pane_alpha`. While a
  pane's alpha is below 1, `draw_panes` skips its image bands. Images vanish
  at the start of a zoom-in fade and reappear at the end of an unzoom (listed
  under Risks).
- App side: while a tween is live, `draw_zoomed` is `None`, so every split
  pane is snapshotted and rebuilt (render.rs:559, 621). Hidden siblings keep
  their own tile-sized grids (REQ-SPL-10), so they draw at their tile rects.
  On zoom-in completion the tween is dropped, `draw_zoomed` is `Some(P)` again,
  and siblings leave the draw list (hidden, as today).

### AC3: unzoom shrink with one deferred resize

- Toggle off: `zoom_toggle` gives `zoomed = None`; the anim becomes
  `(P, 1 → 0)` (or a reversal). `grid_zoomed` stays `Some(P)`, so the toggle's
  relayout, and any other relayout during the shrink (window resize, font
  size, sidebar), keeps P at `bounds`. P's grid size is unchanged, so
  `apply_pane_grid_resize` sends no winsize (resize.rs:119-125). No SIGWINCH.
- During the shrink the clip goes from `bounds` to the tile, and the
  full-size content slides with the clip's top-left corner from the full
  origin to the tile origin. Its right and bottom are cut by the shrinking
  clip; the top-left cells stay visible throughout.
- Finish: a new `tick_split_zoom_anims()` in `about_to_wait`'s deadline list
  (event_loop.rs:575-617) finds every window whose tween is `done(now)`. It
  sets `split_zoom_anim = None`, calls `relayout_and_resize_window` once, and
  requests a redraw. For an unzoom that relayout is the single grid resize to
  the tile rect (one SIGWINCH, through the same throttle). The content
  origin is already the tile origin at `r = 0`, so the finish frame changes
  the grid's size (reflow) but not where its top-left is drawn. For a zoom-in it
  changes no split pane's grid, so no winsize is sent. It still re-places
  pane-anchored floats (see "Surrounding behaviour during the tween"). The tick returns the earliest
  live tween's end instant (`Tween::end`, new) as its deadline. With no tween
  it returns `None`, so it adds no wake-ups when idle.
- The finish runs from the tick, not from `redraw`, so it completes even when
  the window is occluded (native tab switched away mid-tween; `redraw`
  returns early then, render.rs:548-550).

### AC4: mid-animation reversal

`next_split_zoom_anim(prev, next, in_flight, reduce_motion, now)`:

| Case | Result |
|---|---|
| `reduce_motion` | `None` (instant) |
| `prev == next` (toggle was a no-op, e.g. a float has focus) | `in_flight` unchanged |
| `None → Some(P)`, `in_flight` is P unzooming | `in_flight.reversed(now)` |
| `None → Some(P)`, otherwise | `SplitZoomAnim::zoom_in(P, now)` (0 → 1) |
| `Some(P) → None`, `in_flight` is P zooming in | `in_flight.reversed(now)` |
| `Some(P) → None`, otherwise | `SplitZoomAnim::unzoom(P, now)` (1 → 0) |
| `Some(P) → Some(Q)`, P ≠ Q | `None` (instant; not reachable from the toggle today, since focusing another pane while zoomed re-zooms it, split_ops.rs:883-889) |

`reversed` starts at the current `r(now)`, so the clip and opacity continue
from where they are. A tween for a pane other than the toggled one is
replaced. The replaced tween is not finished first: the relayout the toggle
already runs resizes by the new `grid_zoomed`.

### AC5: instant paths

- Reduce Motion: `toggle_split_zoom` passes `crate::idle_ember::reduce_motion()`,
  sampled once per toggle (as idle ember does per gesture start). True →
  `None` → today's behaviour exactly.
- Every implicit zoom change listed under "Code today" (close, new split,
  cross-tab move source and destination, re-zoom on focus, D&D swap/move,
  float reveal) goes through one new `WindowState::set_zoom_instant(zoomed)`.
  It sets `zoomed` and clears `split_zoom_anim`. Each site already runs a
  relayout afterwards, which then resizes by the logical zoom (a cancelled
  shrink gets its tile size there). Done check:
  `grep -rn "state.zoomed = \|source_state.zoomed = \|dest_state.zoomed = " crates/noa-app/src/app`
  shows only `set_zoom_instant` and the toggle. The overview's unrelated
  `overview.zoomed` (overview/interaction.rs:300) is not part of this.
- Liveness: `live_split_zoom_anim(tree, anim)` drops a tween whose pane left
  the tree (same rule as `live_zoomed`, zoom.rs:165-167), applied wherever the
  anim is read. This case is reachable: moving P to another tab during its
  shrink leaves the source's zoom alone, because logical `zoomed` is already
  `None` there (split_ops.rs:585-586).

### AC6: zoom icon in the tab title

- Pure `zoom_tab_title(title: &str, zoomed: bool) -> Cow<str>` prefixes
  `ZOOM_TAB_MARK` (`"⤢ "`, U+2922 + space) when zoomed. It lives in
  `helpers/geometry.rs` next to `tab_title_update`.
- `state.title` stays the raw resolved title. A new mirror
  `state.title_zoom_mark: bool` records whether the pushed title carried the
  mark. `refresh_window_title` pushes when the raw title changed **or**
  `state.zoomed.is_some() != title_zoom_mark`. It sends
  `zoom_tab_title(&title, zoomed)` to `set_title` and `set_native_tab_title`,
  then updates both mirrors. `tick_native_tab_titles` re-asserts
  `zoom_tab_title(&state.title, state.title_zoom_mark)`. A background tab
  (`background_tab`) keeps its early return; its label is painted by the
  front tab.
- Off macOS, `tab_bar_model` maps each sibling's title through
  `zoom_tab_title(title, sibling.zoomed.is_some())`.
- It reads logical `zoomed`, so the mark appears on the zoom-in toggle and
  disappears on the unzoom toggle (not at the end of the shrink). Each toggle
  path already calls `request_redraw`, and `redraw` calls
  `refresh_window_title` first (render.rs:502). Implicit paths do the same.
- On macOS the mark also appears in the window title (the NSWindow title is
  what AppKit shows on the tab). That is the existing single title channel.

### AC7: constant and pure logic, GUI-free and unit-tested

- `crates/noa-app/src/anim.rs`: `pub const DUR_SPLIT_ZOOM: Duration =
  Duration::from_millis(400);` and `Tween::end(&self) -> Instant`, with a test.
  `DUR_BASE` is untouched.
- New `crates/noa-app/src/split_tree/tree/zoom_anim.rs`, re-exported from
  `split_tree/tree.rs`. It uses only `split_tree` types and `crate::anim`,
  with no winit, wgpu, terminal or pty dependency. Contents: `SplitZoomAnim`
  (`zoom_in`, `unzoom`, `pane`, `reveal(now)`, `reversed(now)`, `done(now)`,
  `end()`), `lerp_rect`, `ZoomFrame` + `zoom_frame`, `next_split_zoom_anim`,
  `grid_zoomed`, `draw_zoomed`, `live_split_zoom_anim`, and
  `transition_hit_layout(layout, frame)` and
  `pane_mapping_rect(pane, surface_rect, frame)`. `zoom_frame` returns
  `content = Rect(clip.x, clip.y, bounds.w, bounds.h)` next to `clip`.
  `transition_hit_layout` returns the hit layout with P's rect replaced by
  `frame.clip` (what is visible) and P moved first.
  `pane_mapping_rect` returns `frame.content` for `frame.pane` and
  `surface_rect` for every other pane; it is the rect pointer code maps
  window pixels to grid cells with.
- Unit tests (inline `#[cfg(test)]`): rect lerp endpoints and monotone,
  separately rounded edges; `content` origin equals the clip origin at every
  `r`, its size is `bounds`' size, and it equals the tile origin at `r = 0`
  and `bounds` at `r = 1`; `reveal` at 0 / mid / end and `done`; sibling
  alpha = 1 − r; reversal starts at the current `r` and lasts
  `400 ms × distance`, also tested at r = 0 and r = 1; every row of the AC4
  table, including `reduce_motion`; `grid_zoomed` keeps P through the shrink
  and releases it after; `draw_zoomed` is `None` while live;
  `live_split_zoom_anim` drops a closed pane; `transition_hit_layout` puts P
  first with the clip; `pane_mapping_rect` gives P the content rect and leaves
  siblings on `surface_rect`. A worked mapping case: a point at
  `clip.origin + padding` maps to cell (0, 0) of P during the tween, while
  `surface.rect` would map it to a cell well inside the grid.

### AC8: checks and docs

- `scripts/verify.sh --full` green as before (macOS; clippy `-D warnings` runs
  on Linux CI, so no new dead code: every new item is reachable from a `pub`
  path or used).
- `crates/noa-render/tests/pipeline.rs` gains `mod split_zoom;`, a new
  `tests/pipeline/split_zoom.rs` (skips without an adapter, like its
  siblings). It draws a 2-pane layout with `set_split_zoom_fx(Some(..))` at
  `sibling_alpha = 0.5` and a clip half-way between tile and full, with a
  visible glyph in the sibling. It asserts no validation error and reads back
  pixels: inside the clip but outside P's tile there is no sibling glyph
  (underlay works), and a sibling pixel outside the clip lies strictly
  between the background color and the opaque sibling color (alpha works).
  P has a distinct glyph in cell (0, 0) and the clip's origin differs from
  the full rect's origin. The glyph's pixels must appear at
  `content_origin + padding` and not at `R`'s origin + padding (the content
  origin reaches the uniform). A kitty image placed at P's (0, 0) moves the
  same way (the uniform layout reaches the image draws).
  A second draw of the same layout with `fx = None` must read back the
  same pixels as the existing unzoomed split draw (`pane_alpha` = 1 is a
  no-op). A `size_of::<Uniforms>() == 160` assertion
  guards the lockstep.
- `docs/specs/splits.md`: REQ-SPL-10/11 gain the transition (new
  REQ-SPL-20..24 for clip reveal, fade, deferred resize, reversal/instant
  paths, plus the tab mark), with ACs mapped to the unit tests above and a
  manual-visual AC for the look. `docs/FEATURES.md` line 41 ("zoom toggle"):
  add "animated (clip reveal, 400 ms; instant under Reduce Motion), zoom icon
  in the tab title".

## Surrounding behaviour during the tween

- **Floats anchored to a pane**: hidden for the whole tween, for P and for
  siblings. `FloatLayout` gains `pane_anchors_hidden: bool`; `anchor_shown`
  and `pane_under` return false/skip for `FloatAnchor::Pane` when set.
  `float_geometry` sets it from a live tween. Reason: floats are overlay
  groups drawn above everything with no scissor and no alpha, so they would
  float outside the clip or over fading panes. They are re-placed (and their
  grids resized once) by the finish relayout (AC3). Tab- and window-anchored
  floats are unaffected.
- **Scroll thumbs**: skipped for split panes while a tween is live. They are
  drawn without a scissor (render.rs:952-988), so P's thumb would sit at the
  full rect's edge outside the clip. They return on the next frame after the
  finish.
- **Dividers and focus ring**: suppressed during the tween (AC1 plan). After an
  unzoom they appear on the finish frame.
- **Pointer**: during a tween the hit layout is
  `transition_hit_layout(visible layout, frame)` (P's visible clip first).
  Cell mapping for P must use the content origin, not `surface.rect`:
  `pane_cell_in` and `pane_pixel_in` (pointer.rs:214-245) subtract
  `surface.rect.x/y` today. During a tween they take their rect from
  `pane_mapping_rect(pane, surface.rect, frame)`, which is P's content rect
  (clip origin, full size). Padding is unchanged, because it is computed from
  the rect's size. A click on the visible part of P then hits the cell drawn
  under it, and SGR mouse reports and selection get the same cell. Siblings
  keep `surface.rect`. Divider drags are refused while a tween is live
  (extend pointer.rs:137 with the anim check).
- **Frames**: after presenting, `redraw` requests another redraw while the
  window's tween is live (the overview pattern, overview/render.rs:1174-1182).
  After the finish nothing re-requests, so an idle window stops drawing. The
  tick's deadline is the tween end, not a frame interval.

## Files involved

- noa-render: `src/instance.rs`, `src/shaders/cell.wgsl`, `src/draw_plan.rs`,
  `src/renderer/mod.rs`, `src/lib.rs`, `tests/pipeline.rs`,
  `tests/pipeline/split_zoom.rs` (new).
- noa-app pure: `src/anim.rs`, `src/split_tree/tree.rs`,
  `src/split_tree/tree/zoom_anim.rs` (new), `src/floating.rs`.
- noa-app title: `src/app/helpers/geometry.rs`, `src/app/helpers/tests.rs`,
  `src/app/render.rs` (`refresh_window_title` only), `src/app/timers.rs`
  (`tick_native_tab_titles` only), `src/app/linux_tabs.rs`, `src/app/state.rs`,
  and the three `WindowState` constructors (`src/app/lifecycle.rs`,
  `src/app/scratch_terminal.rs`, `src/app/quick_terminal.rs`).
- noa-app wiring: `src/app/state.rs`, `src/app/split_ops.rs`,
  `src/app/render.rs`, `src/app/input_ops/layout.rs`,
  `src/app/input_ops/pointer.rs`, `src/app/floating.rs`, `src/app/pane_drag.rs`,
  `src/app/lifecycle.rs`, `src/app/scratch_terminal.rs`,
  `src/app/quick_terminal.rs`, `src/app/event_loop.rs`, `src/app/timers.rs`.
- docs: `docs/specs/splits.md`, `docs/FEATURES.md`.

No new dependency, no `Cargo.lock` change. wgpu stays in noa-render/noa-app,
winit in noa-app; the pure module is neither.

## Risks

- **GPU validation (silent at build time).** The uniform change is a rename
  inside the existing tail, but Rust and WGSL must change together.
  `ZoomUnderlay` adds a `backdrop_pipeline` use and a second background-image
  draw inside the cell pass. The new pipeline test draws both. Bind group
  visibility is unchanged: `pane_alpha` is read in the fragment stage, and
  binding 0 is already `VERTEX_FRAGMENT` (pipeline.rs:26).
- **Underlay vs. background image placement**: with a `backdrop_rect` the
  image is drawn in backdrop coordinates via `set_viewport` (mod.rs:1177-1184,
  1194-1202). The underlay must reproduce the same viewport, or the image
  will be misplaced inside the clip. The pipeline test covers the no-backdrop
  case; the backdrop case is a manual check (quick terminal overlay / glass).
- **Kitty images** on fading siblings pop instead of fading (AC2). Fading
  them would need alpha in the image pipeline; out of scope.
- **Cost**: during a tween every sibling is snapshotted and its rows rebuilt
  for ~24 frames at 60 Hz. The first frame of a zoom-in rebuilds the hidden
  siblings in full. That is bounded by 9 panes per tab
  (`MAX_PANES_PER_TAB`) and lasts 400 ms.
- **Resize throttle**: a zoom-in immediately followed by an unzoom can land
  the final resize on the throttle's trailing edge rather than at the finish
  instant. It is still exactly one apply of the final size.
- **Finish relayout side effects**: `relayout_and_resize_window` ends copy
  mode (layout.rs:74). Copy mode entered during the 400 ms of a zoom-in is
  ended at the finish. The resize toast for an unzoom moves from the toggle
  to the finish.
- **Glyph coverage of `⤢`**: AppKit falls back to a system symbol font. Off
  macOS, the tab bar uses Noa's font stack with fallback discovery. If it is
  missing there, it shows as tofu (manual check on Linux).
- **IME and native overlay anchors** still use `surface.rect` during a tween.
  `update_focused_ime_cursor_area` (`crates/noa-app/src/app/input_ops/ime.rs:198`)
  runs every redraw (render.rs:505), and the macOS overlay cards take
  `focused_rect` from the snapshot rect (render.rs:1033-1039). So for
  at most 400 ms an IME candidate window or an open palette card is placed
  for the content's final origin, not its sliding one. A palette or IME
  composition rarely overlaps a zoom toggle, and moving them per frame would
  put AppKit calls in the frame loop. Left as is; not in any unit.
- **Overview tiles** render whatever the pane renderer last drew. A tile
  captured mid-tween shows the intermediate frame until its next refresh.

## Implementation units

Four units. U1, U2 and U3 touch disjoint files and can be built in parallel
worktrees from this commit. U4 depends on all three and runs after they merge.

### U1: renderer support (noa-render) [parallel]

Owns: `crates/noa-render/src/instance.rs`, `src/shaders/cell.wgsl`,
`src/draw_plan.rs`, `src/renderer/mod.rs`, `src/lib.rs`,
`tests/pipeline.rs`, `tests/pipeline/split_zoom.rs`.

Builds `pane_alpha` (AC2 renderer half), `SplitZoomFx` (with
`content_origin`) + `set_split_zoom_fx`, `build_draw_plan_with_zoom_fx` +
`DrawOp::ZoomUnderlay`, the uniform layout that moves P's
`Uniforms::grid_padding` `.x`/`.w` origin and its image quads to
`content_origin` (AC1), image-band skip below alpha 1, and the pipeline test
(AC8).

Done when: new `draw_plan.rs` unit tests pass. They cover the fx plan order
(siblings, underlay, P last), the clip ∩ rect scissor, no dividers or ring,
`fx = None` identical to `build_draw_plan_with_overlays` across the existing
exhaustive combinations (draw_plan.rs:570-603 style), and fx naming an absent
pane falling back to the plain plan. A `populate_pane_uniform` unit test
checks that a rect translated by `(dx, dy)` changes only `grid_padding.x`
(by `dy`) and `.w` (by `dx`) among the fields the shader reads, and leaves
`grid_size` unchanged. `tests/pipeline/split_zoom.rs` (including the
content-origin glyph and image checks) passes on a GPU machine. `scripts/verify.sh` is green for noa-render and its dependents
(noa-app must still build; its behaviour is unchanged because it never sets
the fx).

### U2: pure transition logic (noa-app, GUI-free) [parallel]

Owns: `crates/noa-app/src/anim.rs`, `src/split_tree/tree.rs`,
`src/split_tree/tree/zoom_anim.rs`, `src/floating.rs`, and the one
`FloatLayout { .. }` literal in `src/app/floating.rs` (`FloatGeometry::layout`,
lines 195-200).

Builds `DUR_SPLIT_ZOOM`, `Tween::end`, everything listed under AC7, and the
`FloatLayout::pane_anchors_hidden` field with its `anchor_shown`/`pane_under`
rule and tests. Adding a pub field breaks the app-side literal, so U2 adds
`pane_anchors_hidden: false` there. Behaviour is unchanged until U4 wires it.
Neither U1 nor U3 touches `src/app/floating.rs`. U4 edits it later
(sequential).

Done when: all AC7 tests and the float tests pass in
`cargo test -p noa-app --offline` (sandbox), `cargo clippy -p noa-app` adds no
warnings, and `scripts/verify.sh` is green.

### U3: tab zoom mark (noa-app) [parallel]

Owns: `crates/noa-app/src/app/helpers/geometry.rs`,
`src/app/helpers/tests.rs`, `src/app/linux_tabs.rs`, plus, *for U3 only, the
hunks named here*: `src/app/render.rs` (`refresh_window_title`),
`src/app/timers.rs` (`tick_native_tab_titles`), `src/app/state.rs` (the
`title_zoom_mark` field) and its three constructors (`lifecycle.rs:682`
region, `scratch_terminal.rs:417` region, `quick_terminal.rs:1010` region).

Builds AC6. Reads only the existing `state.zoomed`, so it needs nothing from
U1/U2.

Done when: `zoom_tab_title` tests pass (mark on/off, empty title, no double
mark), together with a `refresh_window_title` decision test on the
pushed-vs-mirror predicate (factor it as a pure `title_push_needed(raw_prev,
raw_new, mark_prev, mark_new)` in geometry.rs). `scripts/verify.sh` is green.
Manual: Cmd+Shift+Enter shows and removes `⤢` on the native tab (macOS) and
on the in-window tab bar (Linux).

### U4: app wiring (noa-app) [sequential: after U1, U2, U3 merge]

Owns: `crates/noa-app/src/app/state.rs`, `src/app/split_ops.rs`,
`src/app/render.rs`, `src/app/input_ops/layout.rs`,
`src/app/input_ops/pointer.rs`, `src/app/floating.rs`, `src/app/pane_drag.rs`,
`src/app/lifecycle.rs`, `src/app/scratch_terminal.rs`,
`src/app/quick_terminal.rs`, `src/app/event_loop.rs`, `src/app/timers.rs`,
`docs/specs/splits.md`, `docs/FEATURES.md`.

U4 shares `render.rs`, `timers.rs`, `state.rs` and the three constructor files
with U3 and `src/app/floating.rs` with U2, and depends on U1's and U2's APIs,
so it cannot run in parallel. It
builds: the `split_zoom_anim` field and `set_zoom_instant`; the toggle with
reduce motion; `grid_zoomed` at layout.rs:134; `draw_zoomed` at render.rs:559,
621 and 941-948; `set_split_zoom_fx` per frame; thumb suppression; the
redraw-while-live request; `tick_split_zoom_anims` in `about_to_wait`;
`transition_hit_layout`, `pane_mapping_rect` in `pane_cell_in`/`pane_pixel_in`
and the divider-drag refusal in pointer.rs; `content_origin` from
`zoom_frame` into `SplitZoomFx`; float
hiding through `float_geometry`; replacing every implicit `zoomed =` with
`set_zoom_instant`; and the docs (AC8).

Done when: `scripts/verify.sh --full` is as green as before this work (same
sandbox-only failures, if any). The grep check in AC5 holds. With the window
idle after a toggle, no further redraws are requested and `about_to_wait`
returns `ControlFlow::Wait` once the tween ends (check with
`NOA_LATENCY_TRACE` or a debug log). A manual-visual pass covers: zoom-in
reveal with the content sliding from the tile origin (the grid's top-left
visible first), clicking and selecting in P mid-tween, unzoom shrink with
exactly one SIGWINCH (`trap 'echo WINCH' WINCH`
in the pane prints once, after the shrink), reversal mid-tween, Reduce Motion
on (instant), Cmd+W on a zoomed pane (instant), new split while zoomed
(instant), a pane-anchored float, and a scrolled pane's thumb. The PR states
these were checked by hand.

## Open questions

1. Glyph for the tab mark: `⤢` (U+2922) is the default here. On macOS an SF
   Symbol via `NSWindowTab.attributedTitle` would look native but is a
   second, AppKit-only path whose interaction with the `setTitle:` re-assert
   (timers.rs:619-633) is unverified.
2. ~~Content anchoring~~ **Decided (approved mockup):** P's content is
   anchored to the clip's top-left corner and slides with it; the grid's
   top-left is always what shows first. See Model, AC1, AC3, Pointer.
3. Should pane-anchored floats fade with their anchor instead of hiding for
   400 ms? That would need alpha on overlay groups (a second renderer change).
