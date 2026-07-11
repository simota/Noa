use std::ops::Range;

use super::Direction;
use winit::keyboard::{Key, ModifiersState, NamedKey};

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
    /// PageDown / Cmd+] : advance to the next page of tiles (v3 paging).
    PageForward,
    /// PageUp / Cmd+[ : go back to the previous page of tiles (v3 paging).
    PageBack,
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
        // v3 paging: PageUp/PageDown step a page each way (no wrap, see
        // `page_step`). Plain keys — no modifier requirement, mirroring the
        // arrow keys above.
        Key::Named(NamedKey::PageUp) => Some(OverviewAction::PageBack),
        Key::Named(NamedKey::PageDown) => Some(OverviewAction::PageForward),
        // v3 paging: Cmd+[ / Cmd+] as an alternate chord (same modifier
        // discipline as the Cmd+<digit> arm below — a shifted/ctrl'd/alt'd
        // combo falls through instead of misfiring a page flip).
        Key::Character(text)
            if modifiers.super_key()
                && !modifiers.shift_key()
                && !modifiers.control_key()
                && !modifiers.alt_key()
                && text.as_str() == "[" =>
        {
            Some(OverviewAction::PageBack)
        }
        Key::Character(text)
            if modifiers.super_key()
                && !modifiers.shift_key()
                && !modifiers.control_key()
                && !modifiers.alt_key()
                && text.as_str() == "]" =>
        {
            Some(OverviewAction::PageForward)
        }
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

// --- v3 paging (REQ-OV-18/19/20, REQ-NF-14) ---------------------------------
//
// Discrete paging over the filtered source-tile order: the visible page's
// tiles are always live (no placeholders on any page), each page holds at
// most `page_size` tiles (production: `OVERVIEW_GRID_CAP`), and the page
// count is `ceil(len / page_size)` with a floor of 1 so an empty overview
// still has "page 1 of 1". `page_size` is a parameter (not the `OVERVIEW_GRID_CAP`
// constant) purely so these stay unit-testable without importing it, mirroring
// `compute_overview_grid`'s `cap` parameter.

/// Number of pages `len` source tiles span at `page_size` tiles per page.
/// Always at least 1, even for `len == 0` — an empty Overview still has one
/// (empty) page rather than zero pages.
pub fn overview_page_count(len: usize, page_size: usize) -> usize {
    if page_size == 0 {
        return 1;
    }
    len.div_ceil(page_size).max(1)
}

/// Clamp `page` to the last valid page for `len` source tiles at `page_size`
/// tiles per page (never past the end — an over-range page, e.g. after a
/// search filter shrinks the source set, lands on the new last page).
pub fn clamp_overview_page(page: usize, len: usize, page_size: usize) -> usize {
    let last_page = overview_page_count(len, page_size) - 1;
    page.min(last_page)
}

/// The `[start, end)` slice range for `page` within `len` source tiles at
/// `page_size` tiles per page. `page` is clamped first (see
/// `clamp_overview_page`), so this never panics or returns a range past
/// `len`. Pages partition `0..len` with no overlap and no gap.
pub fn overview_page_slice_range(len: usize, page_size: usize, page: usize) -> Range<usize> {
    if page_size == 0 {
        return 0..0;
    }
    let page = clamp_overview_page(page, len, page_size);
    let start = (page * page_size).min(len);
    let end = (start + page_size).min(len);
    start..end
}

/// Step `page` by one page in `direction`'s sign (positive = forward,
/// negative = back), clamped to `[0, last_page]` for `len` source tiles at
/// `page_size` tiles per page. Never wraps: stepping back from page 0 (or
/// forward from the last page) is a no-op.
pub fn page_step(page: usize, direction: isize, len: usize, page_size: usize) -> usize {
    let last_page = overview_page_count(len, page_size) - 1;
    let stepped = (page as isize + direction.signum()).clamp(0, last_page as isize);
    stepped as usize
}

/// Wheel/trackpad delta magnitude (accumulated across calls) needed to flip
/// one Overview page. Deliberately coarse: a trackpad swipe delivers many
/// small `PixelDelta` events, and a low threshold would flip several pages in
/// one gesture. Compile-time constant — no config knob, matching the ⚠G
/// scroll-throttle precedent elsewhere in this module.
pub const WHEEL_PAGE_THRESHOLD: f32 = 120.0;

/// Accumulate one wheel/trackpad `delta_y` into `wheel_accum` and step `page`
/// by at most one page when the accumulated magnitude crosses
/// [`WHEEL_PAGE_THRESHOLD`]. Sign convention mirrors
/// `mouse_wheel_viewport_scroll`'s `delta_y > 0.0 => Up`: a positive
/// (scroll-up) delta steps back a page, negative steps forward.
///
/// Returns `(new_page, new_wheel_accum)`. Below threshold, the page is
/// unchanged and the delta is simply added to the accumulator (sub-threshold
/// accumulation). At or past threshold, the page steps by exactly one — never
/// more, regardless of how large `delta_y` is, so one oversized trackpad
/// sample can't skip pages — and the crossed threshold amount is subtracted
/// off via `%` (Rust's float remainder always has magnitude strictly less
/// than its divisor), carrying only that bounded remainder into the next
/// call. Bounding it is what makes "at most one flip per call" hold across
/// calls too: an unbounded remainder from a single oversized sample (e.g.
/// 10x the threshold) would itself still exceed the threshold, so the very
/// next call — even with a near-zero `delta_y` and no further user input —
/// would cascade another flip. A genuine sustained swipe (many separate
/// above-threshold deltas) still flips across calls as before, since each
/// call's own `delta_y` keeps adding to the bounded carry. If the step was
/// clamped at a grid boundary (`new_page == page`), the whole accumulator
/// resets to 0 instead of carrying a remainder at all: holding a swipe
/// against the boundary must not build up a latent accumulator that snaps
/// forward several pages the instant the user reverses direction.
pub fn page_after_wheel(
    page: usize,
    wheel_accum: f32,
    delta_y: f32,
    len: usize,
    page_size: usize,
) -> (usize, f32) {
    let accum = wheel_accum + delta_y;
    if accum.abs() < WHEEL_PAGE_THRESHOLD {
        return (page, accum);
    }
    let direction: isize = if accum > 0.0 { -1 } else { 1 };
    let new_page = page_step(page, direction, len, page_size);
    let carry = accum % WHEEL_PAGE_THRESHOLD;
    let new_accum = if new_page == page { 0.0 } else { carry };
    (new_page, new_accum)
}

/// The wheel/trackpad accumulator's value on every Overview show (v3
/// paging): always 0, regardless of any residue left over from before the
/// overlay was last hidden. `App::show_tab_overview` assigns this — rather
/// than a bare literal — in its unconditional REQ-OV-14 block (the same one
/// that resets `page`), so a leftover accumulator from a prior session can
/// never survive into a reopen; without it, a small scroll right after
/// reopening could trigger a surprise immediate flip from residue the user
/// never intended (neither the re-host branch nor `hide_tab_overview`
/// touches `wheel_accum` on their own).
pub fn overview_wheel_accum_on_show() -> f32 {
    0.0
}
