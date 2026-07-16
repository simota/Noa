//! The active screen: a grid of rows, the cursor, the scroll region, tab
//! stops, and all the cursor/erase/scroll primitives the [`crate::Terminal`]
//! `Handler` implementation drives.

use crate::cell::{Cell, Row};
use crate::cursor::{Cursor, HorizontalMargins, SavedCursor, ScrollRegion};
use crate::scrollback::PagedScrollback;
use crate::search::{SearchAnchor, SearchMatch, SearchState, append_row_matches, needle_len};
use crate::selection::{Selection, SelectionPoint};
use crate::tabstops::Tabstops;
use noa_core::{CellAttrs, Point};
use noa_vt::{EraseDisplay, EraseLine};
use std::borrow::Cow;
use std::ops::Range;
use unicode_width::UnicodeWidthChar;

/// Default `scrollback-limit`: total bytes of *scrollback* storage kept before
/// page-granular eviction. Matches Ghostty's 10 MB default (`0` disables
/// scrollback). Unlike Ghostty this bounds only the paged history, not the
/// active grid (which is unbounded-small at rows×cols).
const DEFAULT_SCROLLBACK_LIMIT_BYTES: usize = 10_000_000;

/// Hard cap on live kitty placements per screen. Image *data* is bounded by
/// [`crate::kitty::ImageStore`]'s byte quota, but placements (~100 B each) had
/// no ceiling — a client minting a fresh `p=` id per put grows the vec forever.
/// Far above any legitimate simultaneous-placement count.
pub(crate) const KITTY_PLACEMENT_CAP: usize = 4096;

#[derive(Clone, Copy)]
struct ReflowPoint {
    x: u16,
    y: usize,
    pending_wrap: bool,
}

#[derive(Clone, Copy)]
struct ReflowAnchor {
    offset: usize,
    prefer_cell: bool,
}

#[derive(Clone, Copy)]
struct ReflowPosition {
    row: usize,
    x: u16,
}

/// How one logical line moved during a reflow pass: the old stream rows it
/// occupied (`old_start..old_start + old_len`) and the first row it now
/// occupies in the reflowed output (`new_base`). Used to re-anchor Kitty
/// placements onto the new row numbering.
#[derive(Clone, Copy)]
struct LineRemap {
    old_start: usize,
    old_len: usize,
    new_base: usize,
}

struct ReflowedLine {
    rows: Vec<Row>,
    cell_positions: Vec<Option<ReflowPosition>>,
    boundary_positions: Vec<ReflowPosition>,
}

/// A Kitty graphics placement: one on-screen display of a stored image.
///
/// The vertical anchor is a *session-absolute* row (the same coordinate system
/// as shell-integration marks: `rows_evicted + scrollback_len + cursor.y` at
/// creation). Normal scrolling that pushes rows into scrollback needs no
/// adjustment — the absolute coordinate keeps pointing at the same content; only
/// scrolls that discard content (alternate screen, region scrolls) shift or drop
/// placements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KittyPlacement {
    pub image_id: u32,
    /// `p=` placement id (0 = unnamed; the unnamed placement of an image is
    /// overwritten by a later unnamed placement of the same image).
    pub placement_id: u32,
    /// Session-absolute row of the placement's top-left cell.
    pub anchor_abs_row: usize,
    pub anchor_col: u16,
    /// Pixel offset within the starting cell (`X=`/`Y=`, clamped to the cell).
    pub cell_x_off: u16,
    pub cell_y_off: u16,
    /// Source crop `[x, y, w, h]` in image pixels, if the request cropped.
    pub src: Option<[u32; 4]>,
    /// Effective cell span, resolved at creation from `c=`/`r=` or the image size.
    pub cols: u16,
    pub rows: u16,
    pub z: i32,
    /// `U=1` — a virtual placement referenced only by Unicode placeholders; it is
    /// never drawn directly.
    pub is_virtual: bool,
}

impl KittyPlacement {
    /// Whether the placement's cell rectangle covers session-absolute `(row, col)`.
    pub(crate) fn covers_abs(&self, abs_row: usize, col: u16) -> bool {
        abs_row >= self.anchor_abs_row
            && abs_row < self.anchor_abs_row + self.rows as usize
            && col >= self.anchor_col
            && col < self.anchor_col.saturating_add(self.cols)
    }
}

/// A placement projected into the current viewport for the renderer.
#[derive(Clone, Debug, PartialEq)]
pub struct VisibleKittyPlacement {
    pub image_id: u32,
    pub placement_id: u32,
    /// Viewport cell coordinates; may be negative when the image spills above or
    /// to the left of the visible area.
    pub grid_x: i32,
    pub grid_y: i32,
    pub cell_x_off: u16,
    pub cell_y_off: u16,
    pub cols: u16,
    pub rows: u16,
    pub src: Option<[u32; 4]>,
    pub z: i32,
}

pub struct Screen {
    pub rows: u16,
    pub cols: u16,
    pub grid: Vec<Row>,
    pub cursor: Cursor,
    pub selection: Option<Selection>,
    pub search: SearchState,
    pub saved_cursor: Option<SavedCursor>,
    pub region: ScrollRegion,
    pub horizontal_margins: Option<HorizontalMargins>,
    pub tabstops: Tabstops,
    /// Kitty graphics placements anchored on this screen (empty on most screens).
    pub kitty_placements: Vec<KittyPlacement>,
    scrollback: PagedScrollback,
    scrollback_enabled: bool,
    viewport_offset: usize,
    /// Keep the currently visible rows anchored while new output enters
    /// scrollback. Unlike the ordinary `viewport_offset > 0` scroll lock,
    /// this also pins a viewport activated at the live bottom (`offset == 0`).
    viewport_locked: bool,
    /// Copy-mode points mirrored from [`crate::CopyModeState`] so structural
    /// screen edits can transform them at the same time as their content.
    copy_mode_cursor: Option<SelectionPoint>,
    copy_mode_anchor: Option<SelectionPoint>,
    copy_mode_cursor_was_evicted: bool,
    copy_mode_anchor_was_evicted: bool,
    /// Changes when an edit collapses the retained coordinate space in a way
    /// that cannot safely preserve every copy-mode point.
    coordinate_generation: u64,
    /// Rows evicted from the front of the scrollback over this screen's whole
    /// lifetime. Lets callers keep session-absolute row coordinates (e.g.
    /// shell-integration marks) stable across scrollback trimming: a stored
    /// `rows_evicted + history_index` stays valid, and a coordinate below the
    /// current `rows_evicted` denotes content that has scrolled off for good.
    rows_evicted: usize,
    /// Rows scrolled off the top by scrollback-recording full-viewport
    /// scrolls since the last snapshot (`take_scroll_shift`). Lets the
    /// renderer treat such a scroll as a translation of its cached row
    /// instances instead of a full-pane rebuild.
    scroll_shift: usize,
    last_printed: Option<char>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TrackedCopyModePoints {
    pub(crate) cursor: SelectionPoint,
    pub(crate) anchor: Option<SelectionPoint>,
    pub(crate) cursor_was_evicted: bool,
    pub(crate) anchor_was_evicted: bool,
}
mod edit;
mod print;
mod reflow;
mod text;

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_scrollback(cols, rows, true)
    }

    pub fn alternate(cols: u16, rows: u16) -> Self {
        Self::with_scrollback(cols, rows, false)
    }

    fn with_scrollback(cols: u16, rows: u16, scrollback_enabled: bool) -> Self {
        Screen {
            rows,
            cols,
            grid: (0..rows).map(|_| Row::new(cols)).collect(),
            cursor: Cursor::default(),
            selection: None,
            search: SearchState::default(),
            saved_cursor: None,
            region: ScrollRegion {
                top: 0,
                bottom: rows.saturating_sub(1),
            },
            horizontal_margins: None,
            tabstops: Tabstops::new(cols),
            kitty_placements: Vec::new(),
            scrollback: PagedScrollback::new(if scrollback_enabled {
                DEFAULT_SCROLLBACK_LIMIT_BYTES
            } else {
                0
            }),
            scrollback_enabled,
            viewport_offset: 0,
            viewport_locked: false,
            copy_mode_cursor: None,
            copy_mode_anchor: None,
            copy_mode_cursor_was_evicted: false,
            copy_mode_anchor_was_evicted: false,
            coordinate_generation: 0,
            scroll_shift: 0,
            rows_evicted: 0,
            last_printed: None,
        }
    }

    /// A blank cell carrying the current pen background (background-color-erase).
    fn blank(&self) -> Cell {
        Cell::blank(self.cursor.bg)
    }

    pub(crate) const fn coordinate_generation(&self) -> u64 {
        self.coordinate_generation
    }

    pub(crate) const fn copy_mode_points(&self) -> Option<TrackedCopyModePoints> {
        match self.copy_mode_cursor {
            Some(cursor) => Some(TrackedCopyModePoints {
                cursor,
                anchor: self.copy_mode_anchor,
                cursor_was_evicted: self.copy_mode_cursor_was_evicted,
                anchor_was_evicted: self.copy_mode_anchor_was_evicted,
            }),
            None => None,
        }
    }

    pub(crate) fn set_copy_mode_points(
        &mut self,
        cursor: SelectionPoint,
        anchor: Option<SelectionPoint>,
    ) {
        self.copy_mode_cursor = Some(cursor);
        self.copy_mode_anchor = anchor;
        self.copy_mode_cursor_was_evicted = false;
        self.copy_mode_anchor_was_evicted = false;
    }

    pub(crate) fn clear_copy_mode_points(&mut self) {
        self.copy_mode_cursor = None;
        self.copy_mode_anchor = None;
        self.copy_mode_cursor_was_evicted = false;
        self.copy_mode_anchor_was_evicted = false;
    }

    pub(super) fn shift_copy_mode_points_up(&mut self, rows: usize) {
        if let Some(cursor) = &mut self.copy_mode_cursor {
            self.copy_mode_cursor_was_evicted |= cursor.y < rows;
            cursor.y = cursor.y.saturating_sub(rows);
        }
        if let Some(anchor) = &mut self.copy_mode_anchor {
            self.copy_mode_anchor_was_evicted |= anchor.y < rows;
            anchor.y = anchor.y.saturating_sub(rows);
        }
    }

    pub(super) fn shift_tracked_points_down_from(&mut self, first_row: usize, rows: usize) {
        let shift = |point: &mut SelectionPoint| {
            if point.y >= first_row {
                point.y = point.y.saturating_add(rows);
            }
        };
        if let Some(cursor) = &mut self.copy_mode_cursor {
            shift(cursor);
        }
        if let Some(anchor) = &mut self.copy_mode_anchor {
            shift(anchor);
        }
        if let Some(selection) = &mut self.selection {
            shift(&mut selection.anchor);
            shift(&mut selection.focus);
        }
    }

    pub(super) fn invalidate_coordinate_space(&mut self) {
        self.coordinate_generation = self.coordinate_generation.wrapping_add(1);
        self.clear_copy_mode_points();
    }

    /// The character last passed through [`Screen::print`] on this screen,
    /// used by `REP` (`CSI b`) and restored verbatim by the client-mode
    /// seed protocol (`crate::terminal::seed`).
    pub(crate) fn last_printed(&self) -> Option<char> {
        self.last_printed
    }

    /// Seed-only: sets `last_printed` without touching grid content, so the
    /// synthetic seed can recreate a replica's `REP` state exactly even
    /// when the source's last-printed character is no longer the last cell
    /// visited while repainting the grid.
    pub(crate) fn set_last_printed(&mut self, ch: char) {
        self.last_printed = Some(ch);
    }

    fn pen_attrs(&self) -> CellAttrs {
        let mut attrs = self.cursor.attrs;
        attrs.remove(CellAttrs::WIDE | CellAttrs::WIDE_SPACER);
        attrs
    }

    fn print_width(c: char) -> usize {
        c.width().unwrap_or(1).min(2)
    }

    fn left_margin(&self) -> u16 {
        self.horizontal_margins.map_or(0, |m| m.left)
    }

    fn right_margin(&self) -> u16 {
        self.horizontal_margins
            .map_or(self.cols.saturating_sub(1), |m| m.right)
    }

    fn clamp_x_to_margins(&self, x: u16) -> u16 {
        x.max(self.left_margin()).min(self.right_margin())
    }

    fn clear_wide_at(row: &mut Row, x: usize, blank: &Cell) {
        if x >= row.cells.len() {
            return;
        }

        if row.cells[x].attrs.contains(CellAttrs::WIDE_SPACER) && x > 0 {
            row.cells[x - 1].set_from(blank);
        }
        if row.cells[x].attrs.contains(CellAttrs::WIDE) && x + 1 < row.cells.len() {
            row.cells[x + 1].set_from(blank);
        }
        row.cells[x].set_from(blank);
    }

    fn sanitize_wide_row(row: &mut Row, blank: &Cell) {
        let mut x = 0;
        while x < row.cells.len() {
            let attrs = row.cells[x].attrs;
            if attrs.contains(CellAttrs::WIDE) {
                if x + 1 >= row.cells.len()
                    || !row.cells[x + 1].attrs.contains(CellAttrs::WIDE_SPACER)
                {
                    row.cells[x].set_from(blank);
                } else {
                    x += 2;
                    continue;
                }
            } else if attrs.contains(CellAttrs::WIDE_SPACER)
                && (x == 0 || !row.cells[x - 1].attrs.contains(CellAttrs::WIDE))
            {
                row.cells[x].set_from(blank);
            }
            x += 1;
        }
    }

    /// Post-burst memory trim: settle deferred scrollback rows into packed
    /// pages and release the flood-sized scratch buffers (recycled-row pool,
    /// spare page, parked pack workers). Cheap when there is nothing to
    /// settle; never touches visible content.
    pub fn trim_memory(&mut self) {
        let evicted = self.scrollback.trim_memory();
        self.note_scrollback_evictions(evicted);
    }

    /// Keep a historical viewport anchored when rows enter scrollback. An
    /// explicit live-bottom lock pins only full-height translations; a partial
    /// region leaves fixed live rows in place and must keep them visible.
    fn pin_viewport_for_scrollback_push(&mut self, n: usize, full_height: bool) {
        if self.viewport_offset > 0 || (self.viewport_locked && full_height) {
            self.viewport_offset = (self.viewport_offset + n).min(self.max_viewport_offset());
        }
    }

    fn max_viewport_offset(&self) -> usize {
        self.scrollback.len()
    }

    fn clamp_viewport(&mut self) {
        self.viewport_offset = self.viewport_offset.min(self.max_viewport_offset());
    }

    fn records_scrollback_for_region(&self, top: usize, _bottom: usize) -> bool {
        self.scrollback_enabled && top == 0
    }

    fn push_scrollback_row(&mut self, row: Row) {
        if !self.scrollback_enabled {
            return;
        }
        let evicted = self.scrollback.push_row(&row);
        self.note_scrollback_evictions(evicted);
    }

    /// Book-keeping owed after pushing rows into scrollback: shift the
    /// selection and placements past any evicted rows and re-clamp the
    /// viewport. Split from [`Self::push_scrollback_row`] so scroll paths
    /// can push borrowed rows directly and settle the eviction debt once.
    fn note_scrollback_evictions(&mut self, evicted: usize) {
        if evicted > 0 {
            self.rows_evicted += evicted;
            self.selection = self
                .selection
                .and_then(|selection| selection.shift_rows_up(evicted));
            self.shift_copy_mode_points_up(evicted);
            self.prune_evicted_placements();
        }
        self.clamp_viewport();
    }

    fn row_with_blank(cols: u16, blank: &Cell) -> Row {
        let mut row = Row::new(cols);
        if blank != &Cell::default() {
            row.clear(blank);
        }
        row
    }

    fn is_default_blank(cell: &Cell) -> bool {
        *cell == Cell::default()
    }

    // ── Kitty graphics placements ───────────────────────────────────

    /// Session-absolute row of grid row 0 (the top of the live area).
    fn live_area_abs_top(&self) -> usize {
        self.rows_evicted + self.scrollback.len()
    }
}
