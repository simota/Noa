//! Pure Session Overview layout and hit-test math.
//!
//! This module deliberately stays independent of windows, terminals, ptys, and
//! GPU state so overview behavior can be tested without constructing app
//! runtime objects.

pub use crate::split_tree::{Direction, Point, Rect as TileRect};
use std::time::{Duration, Instant};
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Spec-locked maximum number of live thumbnail tiles in the overview grid.
pub const OVERVIEW_GRID_CAP: usize = 9;

/// Spec-locked 10Hz throttle for thumbnail regeneration.
pub const OVERVIEW_TILE_MIN_RENDER_INTERVAL: Duration = Duration::from_millis(100);

/// Per-frame cap for offscreen tile work. The render path is sequential, but
/// this keeps one overview frame from doing unbounded terminal locks.
pub const OVERVIEW_MAX_RENDER_TILES_PER_FRAME: usize = 2;

/// Spec-locked gap between adjacent tiles (REQ-OV-11, mockup parity v2) —
/// roughly 4% of a typical tile width. Compile-time constant, no config knob
/// (⚠G precedent: v1's throttle is likewise fixed rather than tunable).
pub const OVERVIEW_TILE_GUTTER: u32 = 18;

/// Spec-locked margin between the tile grid and the Overview window bounds
/// (REQ-OV-11).
pub const OVERVIEW_OUTER_MARGIN: u32 = 26;

/// Title-bar band height rendered at the top of every overview tile, live or
/// placeholder (REQ-OV-12/REQ-OV-13). Compile-time constant.
pub const OVERVIEW_TITLE_BAR_H: u32 = 30;

/// Height reserved at the *top* of the Overview window for the "Search sessions"
/// field (REQ-OV-16). v2/P2 only *reserves* this band in the grid-bounds math
/// so P3's search-field draw doesn't reflow the grid; P2 draws nothing here.
/// Compile-time constant (⚠G precedent: no config knob).
pub const OVERVIEW_SEARCH_BAND_H: u32 = 64;

/// Height reserved at the *bottom* of the Overview window for the hint bar
/// (REQ-OV-17). Compile-time constant.
pub const OVERVIEW_HINT_BAND_H: u32 = 54;

/// Mockup-parity chrome palette (REQ-OV-12/14, v2) — no config knob (⚠G
/// precedent), but the light/dark polarity follows the terminal theme via the
/// shared [`crate::chrome`] palette (selected once at startup), so the
/// overview and the session sidebar stay visually unified. Returned as
/// straight display-space RGBA because the Overview surface uses a
/// **non-sRGB** format (`Bgra8Unorm`, see `preferred_surface_format`), so
/// these are written to the target unchanged (no gamma re-encode).
///
/// Backdrop behind every card (mockup: "暗色の背景").
pub fn overview_bg_color() -> [f32; 4] {
    crate::chrome::rgba(crate::chrome::palette().bg)
}
/// Card face — one step lighter than [`overview_bg_color`] (mockup: "一段明るいカード面").
pub fn overview_card_color() -> [f32; 4] {
    crate::chrome::rgba(crate::chrome::palette().card)
}
/// Title-bar band — distinguishable from the card face (mockup: "区別可能な帯").
pub fn overview_title_bar_color() -> [f32; 4] {
    crate::chrome::rgba(crate::chrome::palette().band)
}
/// Thin resting card border.
pub fn overview_border_color() -> [f32; 4] {
    crate::chrome::rgba(crate::chrome::palette().border)
}
/// Blue accent focus ring for the selected tile (REQ-OV-14).
pub fn overview_focus_ring_color() -> [f32; 4] {
    crate::chrome::rgba(crate::chrome::palette().accent)
}
/// Search / hint pill face in the overview chrome.
pub fn overview_chrome_pill_color() -> [f32; 4] {
    crate::chrome::rgba(crate::chrome::palette().pill)
}
/// Thin border around search and hint pills.
pub fn overview_chrome_border_color() -> [f32; 4] {
    crate::chrome::rgba(crate::chrome::palette().pill_border)
}
/// Corner radius (px) of every card — the shared mid-size chrome radius.
pub const OVERVIEW_CARD_CORNER_RADIUS: f32 = crate::chrome::RADIUS_MD;
/// Resting border thickness (px).
pub const OVERVIEW_CARD_BORDER_WIDTH: f32 = 1.0;
/// Focus-ring thickness (px) — thicker than the resting border so the
/// selection reads as a single bright ring inside the separate outer glow.
pub const OVERVIEW_CARD_FOCUS_WIDTH: f32 = crate::chrome::RING_SELECTED;
/// Selected-card glow radius outside the card edge.
pub const OVERVIEW_CARD_FOCUS_GLOW_WIDTH: f32 = crate::chrome::GLOW_SELECTED;
/// Rounded search-field size within [`OverviewChrome::search_band`].
pub const OVERVIEW_SEARCH_FIELD_H: u32 = 34;
pub const OVERVIEW_SEARCH_FIELD_MIN_W: u32 = 180;
pub const OVERVIEW_SEARCH_FIELD_MAX_W: u32 = 320;
/// Rounded bottom hint-bar size within [`OverviewChrome::hint_band`].
pub const OVERVIEW_HINT_BAR_H: u32 = 32;
pub const OVERVIEW_HINT_BAR_MIN_W: u32 = 320;
pub const OVERVIEW_HINT_BAR_MAX_W: u32 = 460;

/// Width of the close (✕) button's clickable region at the title bar's right
/// edge (REQ-OV-13). Square with the title bar.
const OVERVIEW_CLOSE_BUTTON_W: u32 = OVERVIEW_TITLE_BAR_H;

/// Pure layout result for the Session Overview grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewLayout {
    pub cols: usize,
    pub rows: usize,
    pub placeholder_rows: usize,
    pub tiles: Vec<TileRect>,
    pub placeholders: Vec<TileRect>,
    pub overflow: bool,
}

/// Input row for pure thumbnail-regeneration selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverviewRenderCandidate<Id> {
    pub id: Id,
    pub dirty: bool,
    pub last_render_at: Option<Instant>,
}

/// Title label associated with a live or placeholder overview tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewTileLabel<Id> {
    pub id: Id,
    pub label: String,
}

/// Rendering mode selected for an overview tile under resource pressure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewTileMode {
    LiveThumbnail,
    Placeholder,
}

/// An Overview-focused keyboard action, resolved directly from the raw
/// keypress rather than through the general `AppCommand`/`KeybindEngine` ->
/// `overview_command_scope` path (REQ-OV-15). This lets Return/arrows/Esc and
/// Cmd+1..9 work while every other `AppCommand` still resolves to a
/// `CommandScope::Overview` no-op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewAction {
    MoveSelection(Direction),
    Activate,
    SwitchToLive(usize),
    Dismiss,
    /// Tab: toggle the quick-look zoom of the selected tile. Tab is the one
    /// comfortable non-printable key left — every printable key (including
    /// Space) types into the "Search sessions" field.
    ToggleZoom,
}

/// Resolve an Overview-focused keypress to its [`OverviewAction`], or `None`
/// if the key isn't part of the Overview keymap (REQ-OV-15) — printable text
/// for the "Search sessions" field (REQ-OV-16) is handled separately by the
/// caller, since it isn't an action but a query edit.
pub fn overview_key_action(logical_key: &Key, modifiers: ModifiersState) -> Option<OverviewAction> {
    match logical_key {
        Key::Named(NamedKey::ArrowLeft) => Some(OverviewAction::MoveSelection(Direction::Left)),
        Key::Named(NamedKey::ArrowRight) => Some(OverviewAction::MoveSelection(Direction::Right)),
        Key::Named(NamedKey::ArrowUp) => Some(OverviewAction::MoveSelection(Direction::Up)),
        Key::Named(NamedKey::ArrowDown) => Some(OverviewAction::MoveSelection(Direction::Down)),
        Key::Named(NamedKey::Enter) => Some(OverviewAction::Activate),
        Key::Named(NamedKey::Escape) => Some(OverviewAction::Dismiss),
        Key::Named(NamedKey::Tab) => Some(OverviewAction::ToggleZoom),
        // Plain Cmd+<digit> only (mirrors `cmd+1`..`cmd+9`'s keybind chords,
        // which likewise require no other modifier) — a shifted/alt'd combo
        // falls through to `None` rather than misfiring a tile switch.
        Key::Character(text)
            if modifiers.super_key()
                && !modifiers.shift_key()
                && !modifiers.control_key()
                && !modifiers.alt_key() =>
        {
            text.chars()
                .next()
                .and_then(|c| c.to_digit(10))
                .filter(|&n| (1..=9).contains(&n))
                .map(|n| OverviewAction::SwitchToLive(n as usize))
        }
        _ => None,
    }
}

/// Move `selected` one step within a row-major grid `cols` wide and
/// `tile_count` cells long (REQ-OV-15a). `selected` indexes directly into the
/// combined live-tile + placeholder-tile source order (both share the same
/// `cols`), so this covers both without translation.
///
/// Grid edges clamp — arrows never wrap. A `Down`/`Right` move that would
/// land on a cell past `tile_count` (the trailing row can be shorter than
/// `cols`, REQ-OV-3) is dropped instead of snapping sideways to the last
/// tile: jumping to an unrelated column would be more surprising than simply
/// not moving for that one keypress.
pub fn move_overview_selection(
    selected: usize,
    cols: usize,
    tile_count: usize,
    direction: Direction,
) -> usize {
    if tile_count == 0 || cols == 0 {
        return 0;
    }
    let selected = selected.min(tile_count - 1);
    let col = selected % cols;
    let row = selected / cols;

    let candidate = match direction {
        Direction::Left => (col > 0).then(|| selected - 1),
        Direction::Right => (col + 1 < cols).then(|| selected + 1),
        Direction::Up => (row > 0).then(|| selected - cols),
        Direction::Down => Some(selected + cols),
    };

    candidate
        .filter(|&index| index < tile_count)
        .unwrap_or(selected)
}

/// Initial Overview selection on open (REQ-OV-14): the focused tab's position
/// within `source_ids` when it is a *live* tile (index `< live_tile_count`),
/// else the first tile (`0`).
pub fn overview_initial_selection<Id: PartialEq>(
    source_ids: &[Id],
    live_tile_count: usize,
    focused_id: Option<&Id>,
) -> usize {
    focused_id
        .and_then(|focused| source_ids.iter().position(|id| id == focused))
        .filter(|&index| index < live_tile_count)
        .unwrap_or(0)
}

/// Case-insensitive **contiguous substring** filter over tab titles
/// (REQ-OV-16) — deliberately distinct from `command_palette::fuzzy_match`'s
/// non-contiguous subsequence semantics; "Search sessions" is a plain substring
/// search, not fuzzy matching. An empty query matches every title.
pub fn overview_tab_filter<Id: Copy>(query: &str, titles: &[(Id, String)]) -> Vec<Id> {
    let query = query.to_ascii_lowercase();
    titles
        .iter()
        .filter(|(_, title)| query.is_empty() || title.to_ascii_lowercase().contains(&query))
        .map(|(id, _)| *id)
        .collect()
}

/// The close (✕) button hit-rect for `tile`: a square at the title bar's
/// top-right corner (REQ-OV-13).
pub fn overview_close_button_rect(tile: TileRect) -> TileRect {
    let w = OVERVIEW_CLOSE_BUTTON_W.min(tile.w);
    let h = OVERVIEW_TITLE_BAR_H.min(tile.h);
    TileRect::new(tile.right().saturating_sub(w), tile.y, w, h)
}

/// Return the target id whose close button contains `point`, or `None` for a
/// point in the rest of the tile (or outside every tile). Deliberately a
/// separate hit-test surface from [`hit_test_overview_grid`] (REQ-OV-13):
/// callers check this one first and only fall back to the tile-body
/// hit-test on a miss, so a close-button click is never mistaken for a
/// tile-focus click even though both rects overlap at that corner.
pub fn overview_close_hit_test<T: Copy>(tiles: &[(T, TileRect)], point: Point) -> Option<T> {
    tiles
        .iter()
        .find(|(_, rect)| overview_close_button_rect(*rect).contains(point))
        .map(|(id, _)| *id)
}

/// Placeholder shown in the "Search sessions" field while the query is empty
/// (REQ-OV-16). Compile-time constant (⚠G precedent: no config knob).
pub const OVERVIEW_SEARCH_PLACEHOLDER: &str = "Search sessions";

/// The text to render in the top search field (REQ-OV-16): the live query, or
/// the [`OVERVIEW_SEARCH_PLACEHOLDER`] when it is empty. Kept pure so the
/// empty-vs-typed switch is unit-testable without a GPU.
pub fn overview_search_field_text(query: &str) -> String {
    if query.is_empty() {
        OVERVIEW_SEARCH_PLACEHOLDER.to_string()
    } else {
        query.to_string()
    }
}

/// Compose the single terminal row rendered into the rounded search field.
/// The leading search glyph is a visual affordance only; if the font cannot
/// render it, the row still degrades to readable placeholder/query text.
pub fn overview_search_field_row(query: &str, cols: u16) -> String {
    let cols = cols as usize;
    if cols == 0 {
        return String::new();
    }
    let text = overview_search_field_text(query);
    let row = format!("  ⌕  {text}");
    row.chars().take(cols).collect()
}

/// Two-stage Escape semantics for the Overview search field (REQ-OV-16). A
/// non-empty query swallows the first Escape to clear itself and keeps the
/// Overview open; an empty query dismisses the Overview. The command palette
/// has no two-stage-Escape precedent (its Escape always closes), so this
/// behavior is defined here per the P3 brief.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewEscapeAction {
    /// Clear the (non-empty) query; leave the Overview visible.
    ClearSearch,
    /// Dismiss the Overview (query already empty).
    Dismiss,
}

/// Resolve the Escape keypress against the current `query` (REQ-OV-16).
pub fn overview_escape_action(query: &str) -> OverviewEscapeAction {
    if query.is_empty() {
        OverviewEscapeAction::Dismiss
    } else {
        OverviewEscapeAction::ClearSearch
    }
}

/// The close glyph pinned to a title bar's final column (REQ-OV-13).
/// `'✕'` (U+2715) for mockup parity; falls back to font-fallback rendering —
/// if it tofus on some setup, swap back to ASCII `'x'` at this one site
/// (manual-verify caveat, same as [`overview_hint_bar_text`]).
pub const TITLE_BAR_CLOSE_GLYPH: char = '✕';

/// Compose one title-bar row: the centered tab `label` with the close glyph
/// pinned to the final column (REQ-OV-13). The label is centered within the
/// columns left of the glyph and clipped if it would overrun them, so the
/// close glyph is always visible.
pub fn title_bar_row_with_close(label: &str, cols: u16) -> String {
    let cols = cols as usize;
    if cols == 0 {
        return String::new();
    }
    if cols < 2 {
        // Too narrow for both a label and the glyph — show the glyph alone.
        return TITLE_BAR_CLOSE_GLYPH.to_string();
    }
    // Reserve the last column for the close glyph; center the label in the rest.
    let label_field = cols - 1;
    let centered = center_label(label, label_field as u16);
    let mut row: Vec<char> = centered.chars().take(label_field).collect();
    while row.len() < label_field {
        row.push(' ');
    }
    row.push(TITLE_BAR_CLOSE_GLYPH);
    row.into_iter().collect()
}

/// A truecolor SGR foreground prefix for the ANSI title-bar composer.
fn ansi_fg(color: noa_core::Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", color.r, color.g, color.b)
}

/// Compose one title-bar row with inline SGR styling, visually identical in
/// layout to [`title_bar_row_with_close`] but adding: an optional dim `⌘n`
/// switch badge before the label (REQ-OV-15c affordance), a colored status dot
/// when the label carries the `● ` needs-user prefix (red attention / yellow
/// bell / blue busy — the caller picks the color from card state), and an
/// accent-bold highlight of the first case-insensitive `query` match inside
/// the label (REQ-OV-16). The escapes occupy no cells, so the visible layout
/// (centering, clipping, trailing close glyph) matches the plain composer.
pub fn title_bar_row_ansi(
    label: &str,
    cols: u16,
    badge: Option<usize>,
    dot_color: Option<noa_core::Rgb>,
    query: &str,
) -> String {
    let cols = cols as usize;
    if cols == 0 {
        return String::new();
    }
    if cols < 2 {
        return TITLE_BAR_CLOSE_GLYPH.to_string();
    }
    let field = cols - 1;

    let badge_text = badge.map(|n| format!("{n} ")).unwrap_or_default();
    let badge_len = badge_text.chars().count();
    // Clip the label to the space left of the badge so the glyph never moves.
    let label: String = label.chars().take(field.saturating_sub(badge_len)).collect();
    let vis_len = badge_len + label.chars().count();
    let pad = (field.saturating_sub(vis_len)) / 2;

    const RESET_FG: &str = "\x1b[39m";
    let dim = ansi_fg(crate::chrome::palette().dim_fg);
    let accent = ansi_fg(crate::chrome::palette().accent);

    let mut out = String::new();
    out.extend(std::iter::repeat_n(' ', pad));
    if !badge_text.is_empty() {
        out.push_str(&dim);
        out.push_str(&badge_text);
        out.push_str(RESET_FG);
    }

    // Split the label into an optional colored dot prefix and the rest.
    let (dot_seg, rest) = match dot_color {
        Some(_) if label.starts_with("● ") => label.split_at("● ".len()),
        _ => ("", label.as_str()),
    };
    if !dot_seg.is_empty() {
        // `dot_color` is Some by construction of `dot_seg`.
        out.push_str(&ansi_fg(dot_color.unwrap_or(crate::chrome::palette().dot_red)));
        out.push_str(dot_seg);
        out.push_str(RESET_FG);
    }

    // First case-insensitive match of `query` within the rest of the label.
    // `to_ascii_lowercase` preserves byte offsets, so the byte range found in
    // the lowered copy slices the original safely.
    let match_range = if query.is_empty() {
        None
    } else {
        rest.to_ascii_lowercase()
            .find(&query.to_ascii_lowercase())
            .map(|start| (start, start + query.len()))
    };
    match match_range {
        Some((start, end)) if end <= rest.len() && rest.is_char_boundary(start) && rest.is_char_boundary(end) => {
            out.push_str(&rest[..start]);
            out.push_str("\x1b[1m");
            out.push_str(&accent);
            out.push_str(&rest[start..end]);
            out.push_str(RESET_FG);
            out.push_str("\x1b[22m");
            out.push_str(&rest[end..]);
        }
        _ => out.push_str(rest),
    }

    // Right-pad the label field, then pin the dim close glyph to the last col.
    let visible = pad + vis_len;
    out.extend(std::iter::repeat_n(' ', field.saturating_sub(visible)));
    out.push_str(&dim);
    out.push(TITLE_BAR_CLOSE_GLYPH);
    out.push_str(RESET_FG);
    out
}

/// The enlarged quick-look rect for a zoomed tile (Tab toggle): the tile's
/// size scaled up, clamped to `grid_bounds`, centered within it.
pub fn overview_zoom_rect(grid_bounds: TileRect, tile: TileRect) -> TileRect {
    const ZOOM_SCALE: f32 = 1.6;
    if grid_bounds.w == 0 || grid_bounds.h == 0 {
        return grid_bounds;
    }
    let w = ((tile.w as f32 * ZOOM_SCALE).round() as u32).min(grid_bounds.w).max(1);
    let h = ((tile.h as f32 * ZOOM_SCALE).round() as u32).min(grid_bounds.h).max(1);
    TileRect::new(
        grid_bounds.x + (grid_bounds.w - w) / 2,
        grid_bounds.y + (grid_bounds.h - h) / 2,
        w,
        h,
    )
}

/// Injected GPU lifecycle signal used by the resource-regeneration decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewResourceEvent {
    None,
    DeviceLost,
    SurfaceLost,
}

/// Compute equal-size row-major tile rectangles for the Session Overview.
///
/// `cap` is part of the pure seam so tests can exercise the degradation
/// boundary directly; production uses [`OVERVIEW_GRID_CAP`]. `gutter` is the
/// fixed gap between adjacent tiles and `margin` the gap between the grid and
/// `bounds`' edges (REQ-OV-11, mockup parity v2); production uses
/// [`OVERVIEW_TILE_GUTTER`]/[`OVERVIEW_OUTER_MARGIN`]. `gutter=0, margin=0`
/// reproduces v1's edge-to-edge tiling bit-for-bit (AC-OV-11).
pub fn compute_overview_grid(
    tab_count: usize,
    bounds: TileRect,
    cap: usize,
    gutter: u32,
    margin: u32,
) -> OverviewLayout {
    let live_cap = cap.min(tab_count);
    let overflow_count = tab_count.saturating_sub(live_cap);
    let overflow = overflow_count > 0;

    if live_cap == 0 {
        return OverviewLayout {
            cols: 0,
            rows: 0,
            placeholder_rows: 0,
            tiles: Vec::new(),
            placeholders: Vec::new(),
            overflow,
        };
    }

    let cols = ceil_sqrt(live_cap);
    let rows = live_cap.div_ceil(cols);
    let placeholder_rows = if overflow {
        overflow_count.div_ceil(cols)
    } else {
        0
    };
    let total_rows = rows + placeholder_rows;

    // Inner content area after subtracting the outer margin on both sides;
    // with margin=0 this is `bounds` itself.
    let inner_w = bounds.w.saturating_sub(2 * margin);
    let inner_h = bounds.h.saturating_sub(2 * margin);
    let col_gutters = gutter.saturating_mul(cols as u32 - 1);
    let row_gutters = gutter.saturating_mul(total_rows as u32 - 1);
    let tile_w = inner_w.saturating_sub(col_gutters) / cols as u32;
    let tile_h = inner_h.saturating_sub(row_gutters) / total_rows as u32;
    let origin_x = bounds.x + margin;
    let origin_y = bounds.y + margin;

    let tiles = (0..live_cap)
        .map(|index| rect_at(origin_x, origin_y, tile_w, tile_h, cols, index, gutter))
        .collect();
    let placeholders = (0..overflow_count)
        .map(|index| {
            rect_at(
                origin_x,
                origin_y,
                tile_w,
                tile_h,
                cols,
                live_cap + index,
                gutter,
            )
        })
        .collect();

    OverviewLayout {
        cols,
        rows,
        placeholder_rows,
        tiles,
        placeholders,
        overflow,
    }
}

/// Return the target id for `point`, or `None` outside live tiles.
///
/// Callers pass only live thumbnail tile pairs. Placeholder rows and empty grid
/// cells are therefore naturally non-interactive.
pub fn hit_test_overview_grid<T: Copy>(tiles: &[(T, TileRect)], point: Point) -> Option<T> {
    tiles
        .iter()
        .find(|(_, rect)| rect.contains(point))
        .map(|(id, _)| *id)
}

/// Decide whether a single tile is dirty and outside the compile-time
/// regeneration throttle.
pub fn should_render_tile(
    dirty: bool,
    last_render_at: Option<Instant>,
    now: Instant,
    min_interval: Duration,
) -> bool {
    if !dirty {
        return false;
    }
    let Some(last_render_at) = last_render_at else {
        return true;
    };
    now.saturating_duration_since(last_render_at) >= min_interval
}

/// Select the dirty-and-due tile ids for one overview frame.
///
/// Source-window occlusion must NOT gate this selection: tabs mirrored in the
/// overview are almost always occluded (they sit behind the overview window
/// itself and/or in a macOS native tab group), so filtering them out would
/// leave every live tile permanently blank and defeat REQ-OV-4's live mirror.
/// REQ-NF-7's occlusion-aware redraw suppression is honored at the tab-window
/// redraw layer (`TargetedRedrawDecision`) instead, which the overview tile
/// path does not bypass.
pub fn select_due_overview_tile_ids<Id: Copy>(
    candidates: &[OverviewRenderCandidate<Id>],
    now: Instant,
    min_interval: Duration,
    max_tiles: usize,
) -> Vec<Id> {
    candidates
        .iter()
        .filter(|candidate| {
            should_render_tile(candidate.dirty, candidate.last_render_at, now, min_interval)
        })
        .take(max_tiles)
        .map(|candidate| candidate.id)
        .collect()
}

/// Outcome of the post-frame dirty-backlog check `redraw_overview` runs
/// after each Session Overview frame (Fix A): either an immediate redraw is
/// warranted right now, or — if every remaining dirty tile is merely
/// throttle-blocked — the single instant at which the earliest one becomes
/// due, so the caller can schedule one delayed wake-up instead of spinning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverviewBacklogDecision {
    pub request_immediate_redraw: bool,
    pub wake_at: Option<Instant>,
}

/// Decide the post-frame backlog action from each source tile's dirty +
/// last-render state.
///
/// A tile only warrants `request_immediate_redraw` when it is dirty *and*
/// already due (i.e. [`should_render_tile`] would render it right now) —
/// that only happens when [`OVERVIEW_MAX_RENDER_TILES_PER_FRAME`] left it
/// un-rendered this frame. A tile that is merely dirty-but-throttled
/// contributes its throttle deadline (`last_render_at + min_interval`, or
/// `now` if it has never been rendered) to `wake_at`, and the earliest one
/// wins: one delayed wake-up covers every throttled tile, since a tile that
/// becomes due re-triggers this same check when it fires.
pub fn overview_backlog_decision<Id: Copy>(
    candidates: &[OverviewRenderCandidate<Id>],
    now: Instant,
    min_interval: Duration,
) -> OverviewBacklogDecision {
    let mut wake_at: Option<Instant> = None;
    for candidate in candidates {
        if !candidate.dirty {
            continue;
        }
        if should_render_tile(candidate.dirty, candidate.last_render_at, now, min_interval) {
            return OverviewBacklogDecision {
                request_immediate_redraw: true,
                wake_at: None,
            };
        }
        let due_at = candidate
            .last_render_at
            .map(|last_render_at| last_render_at + min_interval)
            .unwrap_or(now);
        wake_at = Some(wake_at.map_or(due_at, |current| current.min(due_at)));
    }
    OverviewBacklogDecision {
        request_immediate_redraw: false,
        wake_at,
    }
}

/// Decide the tile mode from an injected VRAM budget flag.
pub fn overview_tile_mode_for_budget(budget_exceeded: bool) -> OverviewTileMode {
    if budget_exceeded {
        OverviewTileMode::Placeholder
    } else {
        OverviewTileMode::LiveThumbnail
    }
}

/// Decide whether overview GPU resources must be regenerated.
pub fn overview_regen_required(event: OverviewResourceEvent) -> bool {
    matches!(
        event,
        OverviewResourceEvent::DeviceLost | OverviewResourceEvent::SurfaceLost
    )
}

/// Map source tabs to display labels using already-known tab titles.
pub fn overview_tile_labels<Id: Copy>(
    source_ids: &[Id],
    mut title_for_id: impl FnMut(Id) -> Option<String>,
) -> Vec<OverviewTileLabel<Id>> {
    source_ids
        .iter()
        .copied()
        .map(|id| OverviewTileLabel {
            id,
            label: title_for_id(id).unwrap_or_else(|| "Noa".to_string()),
        })
        .collect()
}

/// Overflow window ids relegated to title-only placeholder rows (REQ-OV-10):
/// the tail of `source_ids` beyond the live tile cap. Index-parallel with
/// `OverviewLayout::placeholders` (both walk the same overflow ids in order).
pub fn overview_placeholder_source_ids<Id: Copy>(
    source_ids: &[Id],
    live_tile_count: usize,
) -> &[Id] {
    source_ids.get(live_tile_count..).unwrap_or(&[])
}

/// Sanitize a tab title for display in a single-row placeholder tile: tab
/// titles arrive via OSC 0/2 with no control-character filtering, and a
/// placeholder tile has no live mirror to clip an overlong string visually,
/// so this strips control characters and clamps to `max_cols` characters.
pub fn sanitize_placeholder_label(label: &str, max_cols: u16) -> String {
    label
        .chars()
        .filter(|c| !c.is_control())
        .take(max_cols as usize)
        .collect()
}

/// The three horizontal bands the Overview window is split into (REQ-OV-11/16/17):
/// a reserved top search band, the middle tile-grid area, and a bottom hint
/// band. `grid_bounds` is what feeds [`compute_overview_grid`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverviewChrome {
    pub search_band: TileRect,
    pub grid_bounds: TileRect,
    pub hint_band: TileRect,
}

/// Carve `bounds` into the search / grid / hint bands (REQ-OV-11, v2 mockup
/// parity). Reserving both bands here — rather than only around the grid —
/// keeps the grid origin stable when P3 starts drawing the search field, and
/// routes hit-testing + selection nav through the same `grid_bounds` the tiles
/// are laid out in. Both band heights clamp so a very short window degrades to
/// an empty grid instead of underflowing.
pub fn overview_chrome_bands(bounds: TileRect) -> OverviewChrome {
    let search_h = OVERVIEW_SEARCH_BAND_H.min(bounds.h);
    let after_search = bounds.h - search_h;
    let hint_h = OVERVIEW_HINT_BAND_H.min(after_search);
    let grid_h = after_search - hint_h;

    OverviewChrome {
        search_band: TileRect::new(bounds.x, bounds.y, bounds.w, search_h),
        grid_bounds: TileRect::new(bounds.x, bounds.y + search_h, bounds.w, grid_h),
        hint_band: TileRect::new(bounds.x, bounds.y + search_h + grid_h, bounds.w, hint_h),
    }
}

/// Centered rounded search-field rect inside the top chrome band.
pub fn overview_search_field_rect(search_band: TileRect) -> TileRect {
    centered_pill_rect(
        search_band,
        0.36,
        OVERVIEW_SEARCH_FIELD_MIN_W,
        OVERVIEW_SEARCH_FIELD_MAX_W,
        OVERVIEW_SEARCH_FIELD_H,
    )
}

/// Centered rounded hint-bar rect inside the bottom chrome band.
pub fn overview_hint_bar_rect(hint_band: TileRect) -> TileRect {
    centered_pill_rect(
        hint_band,
        0.48,
        OVERVIEW_HINT_BAR_MIN_W,
        OVERVIEW_HINT_BAR_MAX_W,
        OVERVIEW_HINT_BAR_H,
    )
}

/// Build the bottom hint-bar text (REQ-OV-17). `live_tile_count` is the number
/// of live thumbnail tiles (`min(tab_count, cap)`); the `⌘1-N` range tracks it
/// dynamically rather than hard-coding the mockup's "1-6".
///
/// NOTE (manual-verify): the `⌘`, arrow, and `・` glyphs depend on font
/// fallback. If they render as tofu, swap to the ASCII form returned by
/// [`overview_hint_bar_text_ascii`] (a compile-time swap at the one call site)
/// and record the deviation.
pub fn overview_hint_bar_text(live_tile_count: usize) -> String {
    let n = live_tile_count.max(1);
    format!("⌘1-{n} to switch・↑↓←→ to navigate・Return to open・Tab to zoom・esc to close")
}

/// ASCII fallback for [`overview_hint_bar_text`] when the Unicode glyphs tofu.
pub fn overview_hint_bar_text_ascii(live_tile_count: usize) -> String {
    let n = live_tile_count.max(1);
    format!(
        "cmd+1-{n} to switch / arrows to navigate / return to open / tab to zoom / esc to close"
    )
}

/// Horizontally center `text` within `cols` columns by left-padding with
/// spaces (used for title-bar and hint-bar labels rendered through a synthetic
/// single-row `Terminal`). Longer-than-`cols` text is returned unpadded; the
/// renderer clips it to the tile.
pub fn center_label(text: &str, cols: u16) -> String {
    let width = text.chars().count();
    let cols = cols as usize;
    if width >= cols {
        return text.to_string();
    }
    let pad = (cols - width) / 2;
    let mut out = String::with_capacity(cols);
    out.extend(std::iter::repeat_n(' ', pad));
    out.push_str(text);
    out
}

fn ceil_sqrt(n: usize) -> usize {
    let mut cols = 1;
    while cols * cols < n {
        cols += 1;
    }
    cols
}

fn rect_at(
    origin_x: u32,
    origin_y: u32,
    tile_w: u32,
    tile_h: u32,
    cols: usize,
    index: usize,
    gutter: u32,
) -> TileRect {
    let col = index % cols;
    let row = index / cols;
    TileRect::new(
        origin_x + (tile_w + gutter).saturating_mul(col as u32),
        origin_y + (tile_h + gutter).saturating_mul(row as u32),
        tile_w,
        tile_h,
    )
}

fn centered_pill_rect(
    band: TileRect,
    width_fraction: f32,
    min_width: u32,
    max_width: u32,
    preferred_height: u32,
) -> TileRect {
    if band.w == 0 || band.h == 0 {
        return TileRect::new(band.x, band.y, 0, 0);
    }
    let desired_w = (band.w as f32 * width_fraction).round() as u32;
    let w = desired_w
        .clamp(min_width.min(max_width), max_width)
        .min(band.w);
    let h = preferred_height.min(band.h);
    TileRect::new(band.x + (band.w - w) / 2, band.y + (band.h - h) / 2, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: TileRect = TileRect::new(10, 20, 90, 120);

    #[test]
    fn overview_grid_handles_zero_tabs_and_all_closed_as_empty() {
        let layout = compute_overview_grid(0, BOUNDS, OVERVIEW_GRID_CAP, 0, 0);

        assert_eq!(layout.cols, 0);
        assert_eq!(layout.rows, 0);
        assert_eq!(layout.placeholder_rows, 0);
        assert!(layout.tiles.is_empty());
        assert!(layout.placeholders.is_empty());
        assert!(!layout.overflow);
    }

    #[test]
    fn overview_grid_uses_equal_size_row_major_tiles_up_to_cap() {
        let cases = [
            (1, 1, 1),
            (2, 2, 1),
            (5, 3, 2),
            (7, 3, 3),
            (8, 3, 3),
            (9, 3, 3),
        ];

        for (tab_count, expected_cols, expected_rows) in cases {
            let layout = compute_overview_grid(tab_count, BOUNDS, OVERVIEW_GRID_CAP, 0, 0);

            assert_eq!(layout.cols, expected_cols, "cols for {tab_count}");
            assert_eq!(layout.rows, expected_rows, "rows for {tab_count}");
            assert_eq!(layout.tiles.len(), tab_count, "tile count for {tab_count}");
            assert!(
                layout.placeholders.is_empty(),
                "placeholders for {tab_count}"
            );
            assert!(!layout.overflow, "overflow for {tab_count}");
            assert_equal_tile_size(&layout.tiles);
            assert_row_major(&layout.tiles, expected_cols);
            assert_no_overlap(&layout.tiles);
        }
    }

    #[test]
    fn overview_grid_places_overflow_in_title_only_placeholder_rows() {
        let ten = compute_overview_grid(10, BOUNDS, OVERVIEW_GRID_CAP, 0, 0);
        assert_eq!(ten.cols, 3);
        assert_eq!(ten.rows, 3);
        assert_eq!(ten.placeholder_rows, 1);
        assert_eq!(ten.tiles.len(), 9);
        assert_eq!(ten.placeholders.len(), 1);
        assert!(ten.overflow);
        assert_equal_tile_size(&ten.tiles);
        assert_eq!(ten.placeholders[0], TileRect::new(10, 110, 30, 30));

        let twelve = compute_overview_grid(12, BOUNDS, OVERVIEW_GRID_CAP, 0, 0);
        assert_eq!(twelve.cols, 3);
        assert_eq!(twelve.rows, 3);
        assert_eq!(twelve.placeholder_rows, 1);
        assert_eq!(twelve.tiles.len(), 9);
        assert_eq!(twelve.placeholders.len(), 3);
        assert!(twelve.overflow);
        assert_eq!(
            twelve.placeholders,
            vec![
                TileRect::new(10, 110, 30, 30),
                TileRect::new(40, 110, 30, 30),
                TileRect::new(70, 110, 30, 30),
            ]
        );
    }

    #[test]
    fn overview_grid_leaves_trailing_row_empty_cells_for_non_square_counts() {
        let layout = compute_overview_grid(5, BOUNDS, OVERVIEW_GRID_CAP, 0, 0);

        assert_eq!(layout.cols, 3);
        assert_eq!(layout.rows, 2);
        assert_eq!(layout.tiles[4], TileRect::new(40, 80, 30, 60));

        let empty_cell_point = Point::new(75, 90);
        let hit_tiles: Vec<_> = layout
            .tiles
            .iter()
            .enumerate()
            .map(|(index, rect)| (index, *rect))
            .collect();
        assert_eq!(hit_test_overview_grid(&hit_tiles, empty_cell_point), None);
    }

    #[test]
    fn overview_hit_test_maps_each_tile_interior_back_to_its_id() {
        let layout = compute_overview_grid(8, BOUNDS, OVERVIEW_GRID_CAP, 0, 0);
        let tiles: Vec<_> = layout
            .tiles
            .iter()
            .enumerate()
            .map(|(index, rect)| (index as u64 + 100, *rect))
            .collect();

        for (id, rect) in &tiles {
            let point = Point::new(rect.x + rect.w / 2, rect.y + rect.h / 2);
            assert_eq!(hit_test_overview_grid(&tiles, point), Some(*id));
        }
    }

    #[test]
    fn overview_hit_test_ignores_outside_gaps_and_placeholder_row() {
        let layout = compute_overview_grid(10, BOUNDS, OVERVIEW_GRID_CAP, 0, 0);
        let tiles: Vec<_> = layout
            .tiles
            .iter()
            .enumerate()
            .map(|(index, rect)| (index, *rect))
            .collect();

        assert_eq!(hit_test_overview_grid(&tiles, Point::new(0, 0)), None);
        assert_eq!(hit_test_overview_grid(&tiles, Point::new(101, 20)), None);
        assert_eq!(hit_test_overview_grid(&tiles, Point::new(15, 115)), None);
    }

    #[test]
    fn should_render_tile_uses_dirty_gate_and_min_interval() {
        let now = Instant::now();
        let last = now - OVERVIEW_TILE_MIN_RENDER_INTERVAL / 2;
        let due = now - OVERVIEW_TILE_MIN_RENDER_INTERVAL;

        assert!(!should_render_tile(
            false,
            Some(due),
            now,
            OVERVIEW_TILE_MIN_RENDER_INTERVAL
        ));
        assert!(should_render_tile(
            true,
            None,
            now,
            OVERVIEW_TILE_MIN_RENDER_INTERVAL
        ));
        assert!(!should_render_tile(
            true,
            Some(last),
            now,
            OVERVIEW_TILE_MIN_RENDER_INTERVAL
        ));
        assert!(should_render_tile(
            true,
            Some(due),
            now,
            OVERVIEW_TILE_MIN_RENDER_INTERVAL
        ));
    }

    #[test]
    fn overview_lock_count_selects_only_dirty_due_tiles_up_to_cap() {
        let now = Instant::now();
        let due = now - OVERVIEW_TILE_MIN_RENDER_INTERVAL;
        let too_recent = now - OVERVIEW_TILE_MIN_RENDER_INTERVAL / 2;
        let candidates = [
            OverviewRenderCandidate {
                id: 1,
                dirty: false,
                last_render_at: Some(due),
            },
            OverviewRenderCandidate {
                id: 2,
                dirty: true,
                last_render_at: Some(too_recent),
            },
            OverviewRenderCandidate {
                id: 3,
                dirty: true,
                last_render_at: Some(due),
            },
            OverviewRenderCandidate {
                id: 4,
                dirty: true,
                last_render_at: None,
            },
            OverviewRenderCandidate {
                id: 5,
                dirty: true,
                last_render_at: Some(due),
            },
        ];

        let locked_tabs =
            select_due_overview_tile_ids(&candidates, now, OVERVIEW_TILE_MIN_RENDER_INTERVAL, 2);

        assert_eq!(locked_tabs, vec![3, 4]);
        assert_eq!(locked_tabs.len(), 2, "lock_count");
    }

    /// Tabs mirrored by the overview are almost always occluded (behind the
    /// overview window itself or in a native tab group); their tiles must
    /// still be selected for rendering (REQ-OV-4). Candidates carry no
    /// occlusion input at all, so a dirty+due tile from a fully hidden source
    /// window is selected like any other.
    #[test]
    fn tiles_from_occluded_source_windows_are_still_selected_when_dirty_and_due() {
        let now = Instant::now();
        let hidden_source = OverviewRenderCandidate {
            id: 7,
            dirty: true,
            last_render_at: None,
        };

        let selected = select_due_overview_tile_ids(
            &[hidden_source],
            now,
            OVERVIEW_TILE_MIN_RENDER_INTERVAL,
            2,
        );

        assert_eq!(selected, vec![7]);
    }

    #[test]
    fn backlog_decision_schedules_a_delayed_wake_when_only_throttled_tiles_remain_dirty() {
        let now = Instant::now();
        let last_render_at = now - OVERVIEW_TILE_MIN_RENDER_INTERVAL / 2;
        let candidates = [OverviewRenderCandidate {
            id: 1,
            dirty: true,
            last_render_at: Some(last_render_at),
        }];

        let decision =
            overview_backlog_decision(&candidates, now, OVERVIEW_TILE_MIN_RENDER_INTERVAL);

        assert!(!decision.request_immediate_redraw);
        assert_eq!(
            decision.wake_at,
            Some(last_render_at + OVERVIEW_TILE_MIN_RENDER_INTERVAL)
        );
    }

    #[test]
    fn backlog_decision_requests_immediate_redraw_when_a_due_dirty_tile_survives_the_frame_cap() {
        let now = Instant::now();
        let due = now - OVERVIEW_TILE_MIN_RENDER_INTERVAL;
        let candidates = [
            OverviewRenderCandidate {
                id: 1,
                dirty: true,
                last_render_at: Some(now - OVERVIEW_TILE_MIN_RENDER_INTERVAL / 2),
            },
            OverviewRenderCandidate {
                id: 2,
                dirty: true,
                last_render_at: Some(due),
            },
        ];

        let decision =
            overview_backlog_decision(&candidates, now, OVERVIEW_TILE_MIN_RENDER_INTERVAL);

        assert!(decision.request_immediate_redraw);
        assert_eq!(decision.wake_at, None);
    }

    #[test]
    fn backlog_decision_requests_nothing_when_every_tile_is_clean() {
        let now = Instant::now();
        let candidates = [
            OverviewRenderCandidate {
                id: 1,
                dirty: false,
                last_render_at: Some(now),
            },
            OverviewRenderCandidate {
                id: 2,
                dirty: false,
                last_render_at: None,
            },
        ];

        let decision =
            overview_backlog_decision(&candidates, now, OVERVIEW_TILE_MIN_RENDER_INTERVAL);

        assert!(!decision.request_immediate_redraw);
        assert_eq!(decision.wake_at, None);
    }

    #[test]
    fn budget_exceeded_degrades_live_tiles_to_placeholders() {
        assert_eq!(
            overview_tile_mode_for_budget(false),
            OverviewTileMode::LiveThumbnail
        );
        assert_eq!(
            overview_tile_mode_for_budget(true),
            OverviewTileMode::Placeholder
        );
    }

    #[test]
    fn device_lost_and_surface_lost_require_resource_regeneration() {
        assert!(!overview_regen_required(OverviewResourceEvent::None));
        assert!(overview_regen_required(OverviewResourceEvent::DeviceLost));
        assert!(overview_regen_required(OverviewResourceEvent::SurfaceLost));
    }

    #[test]
    fn overview_tile_labels_follow_source_tab_titles() {
        let labels = overview_tile_labels(&[1_u8, 2, 3], |id| match id {
            1 => Some("build".to_string()),
            2 => Some("tests".to_string()),
            _ => None,
        });

        assert_eq!(
            labels,
            vec![
                OverviewTileLabel {
                    id: 1,
                    label: "build".to_string()
                },
                OverviewTileLabel {
                    id: 2,
                    label: "tests".to_string()
                },
                OverviewTileLabel {
                    id: 3,
                    label: "Noa".to_string()
                }
            ]
        );
    }

    #[test]
    fn overview_placeholder_source_ids_is_the_tail_beyond_the_live_cap() {
        let source_ids = [1_u8, 2, 3, 4, 5];

        assert_eq!(overview_placeholder_source_ids(&source_ids, 3), &[4_u8, 5]);
        assert_eq!(
            overview_placeholder_source_ids(&source_ids, 5),
            &[] as &[u8]
        );
        assert_eq!(
            overview_placeholder_source_ids(&source_ids, 8),
            &[] as &[u8]
        );
    }

    #[test]
    fn sanitize_placeholder_label_strips_control_chars_and_clamps_to_max_cols() {
        assert_eq!(sanitize_placeholder_label("build", 10), "build");
        assert_eq!(sanitize_placeholder_label("build", 3), "bui");
        assert_eq!(
            sanitize_placeholder_label("build\x07\x1b[31m", 20),
            "build[31m"
        );
        assert_eq!(sanitize_placeholder_label("", 10), "");
        assert_eq!(sanitize_placeholder_label("build", 0), "");
    }

    #[test]
    fn overview_grid_applies_gutter_and_margin_offsets() {
        let layout = compute_overview_grid(4, BOUNDS, OVERVIEW_GRID_CAP, 6, 4);

        assert_eq!(
            layout.tiles,
            vec![
                TileRect::new(14, 24, 38, 53),
                TileRect::new(58, 24, 38, 53),
                TileRect::new(14, 83, 38, 53),
                TileRect::new(58, 83, 38, 53),
            ]
        );
        assert_equal_tile_size(&layout.tiles);
        assert_no_overlap(&layout.tiles);
    }

    #[test]
    fn overview_grid_with_production_gutter_margin_keeps_equal_size_and_no_overlap() {
        let layout = compute_overview_grid(
            5,
            BOUNDS,
            OVERVIEW_GRID_CAP,
            OVERVIEW_TILE_GUTTER,
            OVERVIEW_OUTER_MARGIN,
        );

        assert_equal_tile_size(&layout.tiles);
        assert_no_overlap(&layout.tiles);
        for tile in &layout.tiles {
            assert!(tile.x >= BOUNDS.x + OVERVIEW_OUTER_MARGIN, "{tile:?}");
            assert!(tile.y >= BOUNDS.y + OVERVIEW_OUTER_MARGIN, "{tile:?}");
        }
    }

    #[test]
    fn move_overview_selection_moves_within_a_row_major_grid() {
        // 3x3 grid (cols=3), 9 tiles, starting at the center (index 4).
        assert_eq!(move_overview_selection(4, 3, 9, Direction::Left), 3);
        assert_eq!(move_overview_selection(4, 3, 9, Direction::Right), 5);
        assert_eq!(move_overview_selection(4, 3, 9, Direction::Up), 1);
        assert_eq!(move_overview_selection(4, 3, 9, Direction::Down), 7);
    }

    #[test]
    fn move_overview_selection_clamps_at_grid_edges_without_wrapping() {
        // Top-left corner: Left/Up are no-ops.
        assert_eq!(move_overview_selection(0, 3, 9, Direction::Left), 0);
        assert_eq!(move_overview_selection(0, 3, 9, Direction::Up), 0);
        // Bottom-right corner: Right/Down are no-ops.
        assert_eq!(move_overview_selection(8, 3, 9, Direction::Right), 8);
        assert_eq!(move_overview_selection(8, 3, 9, Direction::Down), 8);
    }

    /// Chosen policy for a trailing row shorter than `cols` (REQ-OV-3): a
    /// move that would land past `tile_count` simply doesn't move, rather
    /// than snapping sideways to the last tile.
    #[test]
    fn move_overview_selection_does_not_move_into_a_missing_trailing_row_cell() {
        // 5 tiles, cols=3: row 0 = [0,1,2], row 1 = [3,4] (index 5 is missing).
        assert_eq!(move_overview_selection(2, 3, 5, Direction::Down), 2);
        assert_eq!(move_overview_selection(4, 3, 5, Direction::Right), 4);
        // Moves that stay within the short row still work.
        assert_eq!(move_overview_selection(3, 3, 5, Direction::Right), 4);
        assert_eq!(move_overview_selection(4, 3, 5, Direction::Left), 3);
    }

    #[test]
    fn move_overview_selection_handles_an_empty_grid_without_panicking() {
        assert_eq!(move_overview_selection(0, 0, 0, Direction::Right), 0);
    }

    #[test]
    fn overview_initial_selection_prefers_the_focused_live_tile() {
        let source_ids = [10_u8, 11, 12, 13, 14];
        assert_eq!(overview_initial_selection(&source_ids, 3, Some(&12)), 2);
    }

    #[test]
    fn overview_initial_selection_falls_back_to_zero_when_focused_is_overflow_or_absent() {
        let source_ids = [10_u8, 11, 12, 13, 14];
        // Focused tab exists but sits past the live tile cap (overflow row).
        assert_eq!(overview_initial_selection(&source_ids, 3, Some(&14)), 0);
        // No focused tab at all.
        assert_eq!(overview_initial_selection::<u8>(&source_ids, 3, None), 0);
        // Focused tab isn't a source tab at all.
        assert_eq!(overview_initial_selection(&source_ids, 3, Some(&99)), 0);
    }

    #[test]
    fn overview_key_action_resolves_arrows_return_and_escape() {
        let no_mods = ModifiersState::empty();
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::ArrowLeft), no_mods),
            Some(OverviewAction::MoveSelection(Direction::Left))
        );
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::ArrowRight), no_mods),
            Some(OverviewAction::MoveSelection(Direction::Right))
        );
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::ArrowUp), no_mods),
            Some(OverviewAction::MoveSelection(Direction::Up))
        );
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::ArrowDown), no_mods),
            Some(OverviewAction::MoveSelection(Direction::Down))
        );
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::Enter), no_mods),
            Some(OverviewAction::Activate)
        );
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::Escape), no_mods),
            Some(OverviewAction::Dismiss)
        );
    }

    #[test]
    fn overview_key_action_resolves_plain_cmd_digit_to_switch_to_live() {
        let cmd = ModifiersState::SUPER;
        assert_eq!(
            overview_key_action(&Key::Character("1".into()), cmd),
            Some(OverviewAction::SwitchToLive(1))
        );
        assert_eq!(
            overview_key_action(&Key::Character("9".into()), cmd),
            Some(OverviewAction::SwitchToLive(9))
        );
        // Outside the 1..=9 keybind range.
        assert_eq!(overview_key_action(&Key::Character("0".into()), cmd), None);
        // A shifted combo does not misfire (mirrors the `cmd+1`..`cmd+9`
        // keybind chords, which likewise require no other modifier).
        assert_eq!(
            overview_key_action(&Key::Character("1".into()), cmd | ModifiersState::SHIFT),
            None
        );
        // No Cmd held: not part of the Overview keymap.
        assert_eq!(
            overview_key_action(&Key::Character("1".into()), ModifiersState::empty()),
            None
        );
    }

    #[test]
    fn overview_key_action_ignores_unbound_keys() {
        assert_eq!(
            overview_key_action(&Key::Character("a".into()), ModifiersState::empty()),
            None
        );
    }

    #[test]
    fn overview_tab_filter_matches_case_insensitive_contiguous_substrings() {
        let titles = [
            (1_u32, "Build Log".to_string()),
            (2, "logs-worker".to_string()),
            (3, "README".to_string()),
        ];

        assert_eq!(overview_tab_filter("log", &titles), vec![1, 2]);
        assert_eq!(overview_tab_filter("LOG", &titles), vec![1, 2]);
        // Non-contiguous query does not match (distinct from subsequence
        // search, e.g. `command_palette::fuzzy_match`).
        assert!(overview_tab_filter("lg", &titles).is_empty());
        // Empty query matches everything, source order preserved.
        assert_eq!(overview_tab_filter("", &titles), vec![1, 2, 3]);
    }

    #[test]
    fn overview_close_hit_test_only_matches_the_title_bar_corner() {
        let tile = TileRect::new(0, 0, 100, 80);
        let tiles = [(1_u8, tile)];
        let close_rect = overview_close_button_rect(tile);

        let inside_close = Point::new(close_rect.x + 1, close_rect.y + 1);
        let inside_body = Point::new(10, 50);

        assert_eq!(overview_close_hit_test(&tiles, inside_close), Some(1));
        assert_eq!(overview_close_hit_test(&tiles, inside_body), None);
        assert_eq!(
            overview_close_hit_test(&tiles, Point::new(1000, 1000)),
            None
        );
    }

    #[test]
    fn overview_search_field_text_shows_placeholder_only_when_empty() {
        assert_eq!(overview_search_field_text(""), OVERVIEW_SEARCH_PLACEHOLDER);
        assert_eq!(overview_search_field_text("log"), "log");
    }

    #[test]
    fn overview_search_field_row_adds_search_affordance_and_clips() {
        assert_eq!(overview_search_field_row("", 20), "  ⌕  Search sessions");
        assert_eq!(overview_search_field_row("build", 20), "  ⌕  build");
        assert_eq!(overview_search_field_row("abcdef", 6), "  ⌕  a");
        assert_eq!(overview_search_field_row("build", 0), "");
    }

    #[test]
    fn overview_escape_action_clears_a_query_before_dismissing() {
        // Two-stage: a non-empty query is cleared first, an empty one dismisses.
        assert_eq!(
            overview_escape_action("log"),
            OverviewEscapeAction::ClearSearch
        );
        assert_eq!(overview_escape_action(""), OverviewEscapeAction::Dismiss);
    }

    #[test]
    fn title_bar_row_pins_close_glyph_to_the_last_column() {
        // 10 cols: 9-wide centered label field + the trailing close glyph.
        let row = title_bar_row_with_close("build", 10);
        assert_eq!(row.chars().count(), 10);
        assert_eq!(row.chars().next_back(), Some(TITLE_BAR_CLOSE_GLYPH));
        assert!(row.contains("build"));

        // A label wider than the field is clipped, but the glyph still shows.
        let clipped = title_bar_row_with_close("a-very-long-tab-title", 6);
        assert_eq!(clipped.chars().count(), 6);
        assert_eq!(clipped.chars().next_back(), Some(TITLE_BAR_CLOSE_GLYPH));

        // Degenerate widths never panic.
        assert_eq!(title_bar_row_with_close("build", 0), "");
        assert_eq!(
            title_bar_row_with_close("build", 1),
            TITLE_BAR_CLOSE_GLYPH.to_string()
        );
    }

    /// Strip SGR escapes so the ANSI composer's visible layout can be compared
    /// against the plain composer's.
    fn strip_sgr(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for e in chars.by_ref() {
                    if e == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    // The ANSI composer's visible cells match the plain composer (centered
    // label + pinned glyph), and the styling segments land where expected.
    #[test]
    fn title_bar_row_ansi_matches_plain_layout_and_styles_segments() {
        // No badge/dot/query: visible layout identical to the plain composer.
        let plain = title_bar_row_with_close("build", 12);
        let ansi = title_bar_row_ansi("build", 12, None, None, "");
        // Both are 12 visible cells with the same centered label.
        assert_eq!(strip_sgr(&ansi).trim_end(), plain.trim_end());

        // Badge: a leading `n ` inside the visible field, dim-colored.
        let badged = title_bar_row_ansi("build", 14, Some(3), None, "");
        let visible = strip_sgr(&badged);
        assert!(visible.contains("3 build"), "{visible:?}");
        assert_eq!(visible.chars().count(), 14);

        // Dot: the `● ` needs-user prefix picks up the caller's color.
        let red = noa_core::Rgb::new(0xe8, 0x5d, 0x5d);
        let dotted = title_bar_row_ansi("● build", 14, None, Some(red), "");
        assert!(dotted.contains("\x1b[38;2;232;93;93m●"), "{dotted:?}");

        // Query: the first case-insensitive match is bold+accented.
        let hit = title_bar_row_ansi("Build Log", 20, None, None, "log");
        assert!(hit.contains("\x1b[1m"), "{hit:?}");
        assert!(strip_sgr(&hit).contains("Build Log"));
        // A non-matching query changes nothing visible.
        let miss = title_bar_row_ansi("Build Log", 20, None, None, "zzz");
        assert!(!miss.contains("\x1b[1m"));

        // Degenerate widths never panic.
        assert_eq!(title_bar_row_ansi("build", 0, Some(1), None, "b"), "");
        assert_eq!(
            title_bar_row_ansi("build", 1, Some(1), None, "b"),
            TITLE_BAR_CLOSE_GLYPH.to_string()
        );
    }

    // Tab-zoom rect: scaled up from the tile, clamped to the grid bounds, and
    // centered within them.
    #[test]
    fn overview_zoom_rect_scales_clamps_and_centers() {
        let grid = TileRect::new(10, 20, 400, 300);
        let tile = TileRect::new(10, 20, 100, 80);
        let zoom = overview_zoom_rect(grid, tile);
        assert_eq!((zoom.w, zoom.h), (160, 128));
        assert_eq!(zoom.x, 10 + (400 - 160) / 2);
        assert_eq!(zoom.y, 20 + (300 - 128) / 2);

        // A tile whose zoom would overflow clamps to the grid bounds.
        let big = TileRect::new(0, 0, 390, 290);
        let clamped = overview_zoom_rect(grid, big);
        assert_eq!((clamped.w, clamped.h), (400, 300));
        assert_eq!((clamped.x, clamped.y), (10, 20));

        // Degenerate bounds pass through without division by zero.
        let empty = TileRect::new(0, 0, 0, 0);
        assert_eq!(overview_zoom_rect(empty, tile), empty);
    }

    #[test]
    fn chrome_bands_reserve_search_and_hint_and_keep_grid_in_between() {
        let bounds = TileRect::new(0, 0, 800, 600);
        let chrome = overview_chrome_bands(bounds);

        // Search band pinned to the top, hint band to the bottom.
        assert_eq!(
            chrome.search_band,
            TileRect::new(0, 0, 800, OVERVIEW_SEARCH_BAND_H)
        );
        assert_eq!(
            chrome.hint_band,
            TileRect::new(0, 600 - OVERVIEW_HINT_BAND_H, 800, OVERVIEW_HINT_BAND_H)
        );
        // Grid sits between them, full width, no overlap, no gap.
        assert_eq!(chrome.grid_bounds.x, 0);
        assert_eq!(chrome.grid_bounds.y, OVERVIEW_SEARCH_BAND_H);
        assert_eq!(chrome.grid_bounds.w, 800);
        assert_eq!(
            chrome.grid_bounds.h,
            600 - OVERVIEW_SEARCH_BAND_H - OVERVIEW_HINT_BAND_H
        );
        // The three bands exactly tile the bounds vertically.
        assert_eq!(
            chrome.search_band.h + chrome.grid_bounds.h + chrome.hint_band.h,
            600
        );
    }

    #[test]
    fn chrome_pill_rects_center_inside_reserved_bands() {
        let bounds = TileRect::new(0, 0, 800, 600);
        let chrome = overview_chrome_bands(bounds);

        assert_eq!(
            overview_search_field_rect(chrome.search_band),
            TileRect::new(256, 15, 288, 34)
        );
        assert_eq!(
            overview_hint_bar_rect(chrome.hint_band),
            TileRect::new(208, 557, 384, 32)
        );
    }

    #[test]
    fn chrome_bands_clamp_without_underflow_in_a_short_window() {
        // Window shorter than the search band alone: grid + hint collapse to
        // zero height, nothing underflows.
        let chrome = overview_chrome_bands(TileRect::new(0, 0, 100, 20));
        assert_eq!(chrome.search_band.h, 20);
        assert_eq!(chrome.grid_bounds.h, 0);
        assert_eq!(chrome.hint_band.h, 0);
    }

    #[test]
    fn hint_bar_text_substitutes_the_live_tile_count() {
        assert_eq!(
            overview_hint_bar_text(6),
            "⌘1-6 to switch・↑↓←→ to navigate・Return to open・Tab to zoom・esc to close"
        );
        assert_eq!(
            overview_hint_bar_text(9),
            "⌘1-9 to switch・↑↓←→ to navigate・Return to open・Tab to zoom・esc to close"
        );
        // Never renders "1-0": a zero-tile overview still shows "1-1".
        assert_eq!(
            overview_hint_bar_text(0),
            "⌘1-1 to switch・↑↓←→ to navigate・Return to open・Tab to zoom・esc to close"
        );
    }

    #[test]
    fn hint_bar_ascii_fallback_mirrors_the_unicode_range() {
        assert_eq!(
            overview_hint_bar_text_ascii(6),
            "cmd+1-6 to switch / arrows to navigate / return to open / tab to zoom / esc to close"
        );
    }

    #[test]
    fn overview_key_action_resolves_tab_to_zoom() {
        assert_eq!(
            overview_key_action(&Key::Named(NamedKey::Tab), ModifiersState::empty()),
            Some(OverviewAction::ToggleZoom)
        );
    }

    #[test]
    fn center_label_pads_to_center_and_passes_overflow_through() {
        assert_eq!(center_label("ab", 6), "  ab");
        assert_eq!(center_label("abc", 3), "abc");
        // Wider than the field: returned unpadded (renderer clips it).
        assert_eq!(center_label("abcdef", 3), "abcdef");
    }

    fn assert_equal_tile_size(tiles: &[TileRect]) {
        let Some(first) = tiles.first() else {
            return;
        };

        for tile in tiles {
            assert_eq!((tile.w, tile.h), (first.w, first.h), "{tile:?}");
        }
    }

    fn assert_row_major(tiles: &[TileRect], cols: usize) {
        for (index, tile) in tiles.iter().enumerate() {
            let col = index % cols;
            let row = index / cols;
            let first = tiles[0];
            assert_eq!(tile.x, first.x + first.w * col as u32, "{tile:?}");
            assert_eq!(tile.y, first.y + first.h * row as u32, "{tile:?}");
        }
    }

    fn assert_no_overlap(tiles: &[TileRect]) {
        for (index, a) in tiles.iter().enumerate() {
            for b in tiles.iter().skip(index + 1) {
                assert!(!rects_overlap(*a, *b), "{a:?} overlaps {b:?}");
            }
        }
    }

    fn rects_overlap(a: TileRect, b: TileRect) -> bool {
        a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
    }
}
