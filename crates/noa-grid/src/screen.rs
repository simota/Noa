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
            scroll_shift: 0,
            rows_evicted: 0,
            last_printed: None,
        }
    }

    /// A blank cell carrying the current pen background (background-color-erase).
    fn blank(&self) -> Cell {
        Cell::blank(self.cursor.bg)
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

    fn follow_live_output(&mut self) {
        self.viewport_offset = 0;
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
