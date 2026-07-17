//! `FrameSnapshot` — a lock-free-to-render copy of the bits of `Terminal`
//! needed to rebuild a frame's GPU instances. `noa-app` takes this under the
//! `Terminal` mutex and then calls into the renderer unlocked.

use std::sync::Arc;

use noa_grid::{
    Cursor, Row, Screen, SearchState, Selection, SelectionPoint, Terminal, TerminalColors,
};

/// The Cmd+hover target the renderer underlines this frame, set by the
/// caller (`from_terminal` defaults to `None`) — `noa-app` computes it from
/// mouse position + `Cmd` modifier state, outside the `Terminal` lock this
/// snapshot was built under.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HoverLink {
    /// An OSC 8 hyperlink, identified by its `Terminal::hyperlinks` index.
    /// Every viewport cell whose `Cell::hyperlink` equals this index is
    /// underlined — an OSC 8 link highlights as a whole, matching Ghostty,
    /// not just the cell under the pointer.
    Registry(usize),
    /// An explicit run of cells on one viewport row (an auto-detected
    /// plain-text URL, which has no registry entry).
    Range { y: u16, x_start: u16, x_end: u16 },
}

/// One display row of the command palette: either a non-selectable category
/// heading (shown only for the empty query) or a command entry. Built by
/// `noa-app`; the renderer never sees `AppCommand`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteRow {
    /// A muted category heading (F). Not selectable; skipped by navigation.
    Header { label: String },
    /// A command entry: its `title`, an optional resolved keybind `hint`
    /// (`None` when unbound), and the char indices in `title` the current query
    /// matched (`match_positions`, highlighted by the renderer — C).
    Entry {
        title: String,
        hint: Option<String>,
        match_positions: Vec<usize>,
        enabled: bool,
    },
}

/// The open command palette's render payload (`cmd+shift+p`), built by the
/// caller (`noa-app`) from its own palette session — the renderer never sees
/// `AppCommand`. Titles, keybind hints, categories, and match positions are all
/// resolved in the app layer, keeping noa-render terminal- and
/// command-agnostic. `rows` is the full display list (headers + entries) in
/// order; the renderer windows it to fit and highlights `selected`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandPaletteSnapshot {
    /// The live query text (may be empty).
    pub query: String,
    /// The full display list in order (headers interleaved with entries).
    pub rows: Vec<PaletteRow>,
    /// The highlighted row's index into `rows` (always an `Entry`). Out of
    /// range only when there are no entries, which the renderer tolerates.
    pub selected: usize,
    /// Total selectable entries (excludes headers) — the denominator of the
    /// `shown/total` counter when the list is windowed (A).
    pub total_entries: usize,
}

/// The open confirmation dialog's render payload (paste protection / OSC 52
/// clipboard-read prompt), built by `noa-app`. A centered modal box with a
/// message line and a key-hint line; the renderer stays action-agnostic.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfirmDialogSnapshot {
    /// The prompt message (e.g. "Paste 3 lines?").
    pub message: String,
    /// The key-hint line (e.g. "Enter: Paste   Esc: Cancel").
    pub hint: String,
}

/// The inline IME pre-edit (composition) run for this frame, built by the
/// caller (`noa-app`) from the focused surface's `ImeState`. `None` means no
/// composition is in progress. The renderer draws `text` starting at the
/// cursor cell with an underline marking it as uncommitted; the OS candidate
/// window still appears separately. `cursor_byte_range` is winit's reported
/// composition-caret byte range within `text` (currently informational — the
/// run is drawn as a whole underlined span).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preedit {
    /// The composing string (never empty when `Some`).
    pub text: String,
    /// Byte range of the composition caret within `text`, if winit reported one.
    pub cursor_byte_range: Option<(usize, usize)>,
}

/// One kitty-graphics placement projected into this frame's viewport, ready
/// for the image layer to resolve into a destination rectangle. Cell-space
/// like the rest of the snapshot; `grid_x`/`grid_y` may be negative when the
/// image spills above or left of the visible grid (the renderer draws the full
/// quad and lets the pane scissor clip it). `epoch` is copied from the backing
/// image so the renderer can key its texture cache on `(id, epoch)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImagePlacementSnapshot {
    pub image_id: u32,
    pub epoch: u64,
    pub grid_x: i32,
    pub grid_y: i32,
    pub cell_x_off: u16,
    pub cell_y_off: u16,
    pub cols: u16,
    pub rows: u16,
    /// Crop rectangle in image pixels (`[x, y, w, h]`), or `None` for the whole
    /// image.
    pub src: Option<[u32; 4]>,
    pub z: i32,
}

/// The pixel data behind the visible [`ImagePlacementSnapshot`]s. Only images a
/// visible placement references are carried, deduplicated by id; `rgba` is an
/// `Arc` clone (a refcount bump, not a pixel copy) so building this off the
/// `Terminal` lock stays cheap.
#[derive(Clone, Debug)]
pub struct SnapshotImage {
    pub id: u32,
    pub epoch: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}

/// Collect the active screen's visible kitty placements plus the images they
/// reference. Shared by [`FrameSnapshot::from_terminal`] and
/// [`FrameSnapshot::peek`]; both take `&Terminal` here (the projection is a
/// read), so it runs before either path's mutable row extraction.
fn kitty_snapshot(terminal: &Terminal) -> (Vec<ImagePlacementSnapshot>, Vec<SnapshotImage>) {
    let mut placements = terminal.kitty_visible_placements();
    placements.extend(terminal.kitty_placeholder_placements());
    if placements.is_empty() {
        return (Vec::new(), Vec::new());
    }
    // Direct and placeholder placements were each z-sorted in isolation; the
    // renderer's band split needs the combined list z-ascending too.
    placements.sort_by_key(|p| p.z);
    let mut out_placements = Vec::with_capacity(placements.len());
    let mut images: Vec<SnapshotImage> = Vec::new();
    for placement in &placements {
        let Some(image) = terminal.kitty_image(placement.image_id) else {
            // A visible placement whose image is gone can't be drawn; skip it
            // rather than carry a dangling id into the renderer.
            continue;
        };
        out_placements.push(ImagePlacementSnapshot {
            image_id: placement.image_id,
            epoch: image.epoch,
            grid_x: placement.grid_x,
            grid_y: placement.grid_y,
            cell_x_off: placement.cell_x_off,
            cell_y_off: placement.cell_y_off,
            cols: placement.cols,
            rows: placement.rows,
            src: placement.src,
            z: placement.z,
        });
        if !images.iter().any(|existing| existing.id == image.id) {
            images.push(SnapshotImage {
                id: image.id,
                epoch: image.epoch,
                width: image.width,
                height: image.height,
                rgba: Arc::clone(&image.rgba),
            });
        }
    }
    (out_placements, images)
}

/// A snapshot of the active screen taken under the `Terminal` lock.
///
/// WP4 (REQ-PERF-1/2): `row_dirty` is parallel to `rows` (same length, same
/// index order) and reports each row's `noa_grid::Row::dirty` bit as it
/// stood *before* this snapshot cleared it — the renderer's dirty-row patch
/// path consumes it to skip instance work for unchanged rows.
///
/// `Clone` exists for `noa-app`'s synchronized-output hold (DECSET 2026):
/// while a pane's terminal is mid-update under mode 2026, `redraw` reuses a
/// previously-held snapshot instead of reading a torn one, which requires an
/// owned copy independent of the one already queued for this frame's render.
/// Every field is cheap to clone relative to a fresh terminal read (`rows`'
/// cost is the same order as a cache-miss `from_terminal_recycle`, and
/// `images`' pixel data is `Arc`-shared, not copied).
#[derive(Clone)]
pub struct FrameSnapshot {
    pub rows: Vec<Row>,
    pub row_dirty: Vec<bool>,
    /// Rows the viewport slid down over immutable content since the previous
    /// snapshot (scrollback-recording scrolls; `Screen::take_scroll_shift`).
    /// When the pane's invalidation key is otherwise unchanged and
    /// `abs_row_base` advanced by exactly this amount, the renderer
    /// translates its cached row instances up by this many rows instead of
    /// rebuilding the whole pane. `0` means no translation is possible.
    pub scroll_shift: usize,
    pub cursor: Cursor,
    /// Copy-mode cursor in selection/storage coordinates. When present the
    /// renderer suppresses the shell cursor and draws this point as a steady
    /// hollow block without adding GPU bindings.
    pub copy_cursor: Option<SelectionPoint>,
    pub colors: TerminalColors,
    pub selection: Option<Selection>,
    pub search: SearchState,
    /// Storage-index base row of the viewport top (`Screen::visible_row_base`),
    /// in the same coordinate space as `selection` — used only to map viewport
    /// `(x, y)` to selection points ([`Self::is_selected`] and friends). This is
    /// NOT unique across scrollback eviction (an equal number of pushes and
    /// evicts reproduces a prior value), so it must not be used as a cache
    /// invalidation key — see `abs_row_base`.
    pub row_base: usize,
    /// Session-absolute row of the viewport top (`rows_evicted + row_base`),
    /// monotonic as content scrolls. The renderer keys its per-pane frame
    /// invalidation on this so a scroll that evicts and pushes the same number
    /// of rows still forces a full rebuild (a stale `row_base` would falsely
    /// cache-hit and paint shifted history rows).
    pub abs_row_base: usize,
    /// Whether this snapshot came from the alternate screen. Primary and
    /// alternate screens can share the same visible row base and both report no
    /// row damage, so the renderer must key on screen identity to avoid
    /// reusing stale row instances across a DEC screen switch.
    pub active_is_alt: bool,
    pub cols: u16,
    pub rows_n: u16,
    /// Whether this pane owns keyboard focus (both its window is OS-focused
    /// and it is the focused split pane). Cursor rendering uses this to
    /// choose between a solid shape (focused) and a hollow outline
    /// (unfocused) — set by the caller; `from_terminal` defaults to `true`
    /// since a single-pane caller is focused unless told otherwise.
    pub focused: bool,
    /// The current blink-timer phase for `Blinking*` cursor styles: `true`
    /// draws the cursor, `false` draws nothing. Ignored for `Steady*`
    /// styles and for an unfocused pane's hollow outline (which never
    /// blinks). Set by the caller; `from_terminal` defaults to `true`.
    pub cursor_blink_visible: bool,
    /// The Cmd+hover underline target, if any. `None` draws no hover
    /// underline at all.
    pub hover_link: Option<HoverLink>,
    /// The open search-prompt buffer for this pane, if any (Cmd+F). `None`
    /// draws no prompt overlay at all. Set by the caller (`noa-app`, from
    /// its own prompt state); `from_terminal` defaults to `None`. The
    /// overlay's `i/n` counter is derived from `search` alongside this.
    pub search_prompt: Option<String>,
    /// The open command-palette overlay (`cmd+shift+p`) for this pane, if
    /// any. `None` draws no palette. Set by the caller only on the palette's
    /// focused pane so it draws once, not once per split; `from_terminal`
    /// defaults to `None`.
    pub command_palette: Option<CommandPaletteSnapshot>,
    /// The open confirmation dialog (paste protection / clipboard-read), if
    /// any. `None` draws no dialog. Set by the caller only on its bound
    /// window's focused pane; `from_terminal` defaults to `None`.
    pub confirm_dialog: Option<ConfirmDialogSnapshot>,
    /// The inline IME pre-edit run for this pane, if a composition is in
    /// progress. `None` draws nothing. Set by the caller only on the focused
    /// pane (from its surface's `ImeState`); `from_terminal` defaults to
    /// `None`. Drawn inline at the cursor cell with an underline.
    pub preedit: Option<Preedit>,
    /// Kitty-graphics placements visible in this frame's viewport, z-ascending
    /// (back-to-front). Empty unless a client has transmitted and placed an
    /// image. The renderer resolves each to a destination quad via
    /// [`crate::image_layer`] and composites it in one of three z bands.
    pub image_placements: Vec<ImagePlacementSnapshot>,
    /// Pixel data for the images `image_placements` references (deduped by id).
    pub images: Vec<SnapshotImage>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FrameSnapshotReuseKey {
    row_base: usize,
    abs_row_base: usize,
    active_is_alt: bool,
    cols: u16,
    rows_n: u16,
}

/// Row storage retained from the previous terminal-frame snapshot.
///
/// The key records the viewport/screen identity those rows came from. When the
/// next snapshot has the same key, clean rows can keep their previous `Row`
/// storage untouched instead of copying every cell again. A key mismatch
/// degrades to ordinary allocation reuse and reclones every visible row.
#[derive(Default)]
pub struct FrameSnapshotRecycle {
    rows: Vec<Row>,
    key: Option<FrameSnapshotReuseKey>,
}

/// The screen `Terminal` is currently rendering, borrowed mutably so its
/// rows' dirty bits can be consumed-and-cleared in the same locked pass
/// (`Screen::take_visible_rows_with_damage`). Mirrors `Terminal::active()`'s
/// selection logic; kept here rather than as a new `noa-grid` API because
/// `Terminal::primary`/`alt`/`active_is_alt` are already public fields (WP4
/// frozen contract: the only new `noa-grid` surface is the one `Screen`
/// method).
fn active_screen_mut(terminal: &mut Terminal) -> &mut Screen {
    if terminal.active_is_alt {
        terminal.alt.as_mut().unwrap_or(&mut terminal.primary)
    } else {
        &mut terminal.primary
    }
}

impl FrameSnapshot {
    /// Clone the active screen's rows + cursor out of `terminal`, consuming
    /// (and clearing) each visible row's dirty bit in the same lock.
    pub fn from_terminal(terminal: &mut Terminal) -> Self {
        Self::from_terminal_recycled(terminal, Vec::new())
    }

    /// Like [`Self::from_terminal`], but reuses `rows_buf`'s row/cell
    /// allocations (typically the previous frame's `FrameSnapshot::rows`,
    /// handed back by the caller) so a steady-state frame clones the grid
    /// without fresh heap allocation.
    pub fn from_terminal_recycled(terminal: &mut Terminal, mut rows_buf: Vec<Row>) -> Self {
        Self::from_terminal_with_recycle_inner(terminal, &mut rows_buf, false)
    }

    /// Like [`Self::from_terminal_recycled`], but also reuses clean row content
    /// when the recycle key proves the viewport did not move or change shape.
    pub fn from_terminal_recycle(
        terminal: &mut Terminal,
        mut recycle: FrameSnapshotRecycle,
    ) -> Self {
        let colors = terminal.colors.clone();
        let (image_placements, images) = kitty_snapshot(terminal);
        let active_is_alt = terminal.active_is_alt;
        let screen = active_screen_mut(terminal);
        let row_base = screen.visible_row_base();
        let abs_row_base = screen.rows_evicted() + row_base;
        let cols = screen.cols;
        let rows_n = screen.rows;
        let key = FrameSnapshotReuseKey {
            row_base,
            abs_row_base,
            active_is_alt,
            cols,
            rows_n,
        };
        let reuse_clean_rows = recycle.key == Some(key);
        let snapshot = Self::from_screen_recycle(
            active_is_alt,
            colors,
            image_placements,
            images,
            screen,
            &mut recycle.rows,
            reuse_clean_rows,
        );
        debug_assert_eq!(snapshot.reuse_key(), key);
        snapshot
    }

    fn from_terminal_with_recycle_inner(
        terminal: &mut Terminal,
        rows_buf: &mut Vec<Row>,
        reuse_clean_rows: bool,
    ) -> Self {
        let colors = terminal.colors.clone();
        let (image_placements, images) = kitty_snapshot(terminal);
        let active_is_alt = terminal.active_is_alt;
        let screen = active_screen_mut(terminal);
        Self::from_screen_recycle(
            active_is_alt,
            colors,
            image_placements,
            images,
            screen,
            rows_buf,
            reuse_clean_rows,
        )
    }

    fn from_screen_recycle(
        active_is_alt: bool,
        colors: TerminalColors,
        image_placements: Vec<ImagePlacementSnapshot>,
        images: Vec<SnapshotImage>,
        screen: &mut Screen,
        rows_buf: &mut Vec<Row>,
        reuse_clean_rows: bool,
    ) -> Self {
        let mut cursor = screen.cursor;
        if screen.viewport_offset() > 0 {
            cursor.visible = false;
        }
        let row_base = screen.visible_row_base();
        let abs_row_base = screen.rows_evicted() + row_base;
        let cols = screen.cols;
        let rows_n = screen.rows;
        let selection = screen.selection;
        let search = screen.search.clone();
        let scroll_shift = screen.take_scroll_shift();
        let mut row_dirty = Vec::new();
        screen.take_visible_rows_with_damage_into_reusing_clean(
            rows_buf,
            &mut row_dirty,
            reuse_clean_rows && scroll_shift == 0,
        );
        FrameSnapshot {
            rows: std::mem::take(rows_buf),
            row_dirty,
            scroll_shift,
            cursor,
            copy_cursor: None,
            colors,
            selection,
            search,
            row_base,
            abs_row_base,
            active_is_alt,
            cols,
            rows_n,
            focused: true,
            cursor_blink_visible: true,
            hover_link: None,
            search_prompt: None,
            command_palette: None,
            confirm_dialog: None,
            preedit: None,
            image_placements,
            images,
        }
    }

    fn reuse_key(&self) -> FrameSnapshotReuseKey {
        FrameSnapshotReuseKey {
            row_base: self.row_base,
            abs_row_base: self.abs_row_base,
            active_is_alt: self.active_is_alt,
            cols: self.cols,
            rows_n: self.rows_n,
        }
    }

    pub fn into_recycle(self) -> FrameSnapshotRecycle {
        let key = self.reuse_key();
        FrameSnapshotRecycle {
            rows: self.rows,
            key: Some(key),
        }
    }

    /// Read-only counterpart to [`Self::from_terminal`] — takes `&Terminal`
    /// and does not consume row damage. This is the Session Overview mirror's
    /// only snapshot source: the overview render path must never lock a
    /// tab's `Terminal` itself (spec REQ-NF-6), so `noa-app`'s io thread
    /// calls this instead while it already holds that lock feeding pty
    /// bytes in, and publishes the result for the overview to read
    /// lock-free (see `noa-app`'s `io_thread::feed_terminal`).
    ///
    /// `row_dirty` is fixed to all `true` (full re-shape every call) rather
    /// than reporting real damage: overview tiles redraw at 10Hz at most
    /// (`OVERVIEW_TILE_MIN_RENDER_INTERVAL` in `noa-app`), so re-shaping
    /// every row each time is cheap, and leaving the real per-row dirty
    /// bits untouched keeps them intact for the tab's own
    /// damage-driven redraw (`Self::from_terminal`) to consume later. The
    /// cursor is always hidden, mirroring the "not the focused pane"
    /// convention background panes already use within one window — an
    /// overview tile is never the pane the user is typing into.
    pub fn peek(terminal: &Terminal) -> Self {
        let colors = terminal.colors.clone();
        let (image_placements, images) = kitty_snapshot(terminal);
        let screen = terminal.active();
        let mut cursor = screen.cursor;
        cursor.visible = false;
        let row_base = screen.visible_row_base();
        let abs_row_base = screen.rows_evicted() + row_base;
        let cols = screen.cols;
        let rows_n = screen.rows;
        let selection = screen.selection;
        let search = screen.search.clone();
        let rows = screen.visible_rows();
        let row_dirty = vec![true; rows.len()];
        FrameSnapshot {
            rows,
            row_dirty,
            scroll_shift: 0,
            cursor,
            copy_cursor: None,
            colors,
            selection,
            search,
            row_base,
            abs_row_base,
            active_is_alt: terminal.active_is_alt,
            cols,
            rows_n,
            focused: true,
            cursor_blink_visible: true,
            hover_link: None,
            search_prompt: None,
            command_palette: None,
            confirm_dialog: None,
            preedit: None,
            image_placements,
            images,
        }
    }

    /// Refresh an existing read-only overview snapshot in place, reusing the
    /// visible row/cell buffers while preserving [`Self::peek`]'s semantics.
    pub fn peek_into(terminal: &Terminal, snapshot: &mut Self) {
        let colors = terminal.colors.clone();
        let (image_placements, images) = kitty_snapshot(terminal);
        let screen = terminal.active();
        let mut cursor = screen.cursor;
        cursor.visible = false;
        let row_base = screen.visible_row_base();
        let abs_row_base = screen.rows_evicted() + row_base;
        let cols = screen.cols;
        let rows_n = screen.rows;
        let selection = screen.selection;
        let search = screen.search.clone();
        screen.visible_rows_into(&mut snapshot.rows);
        snapshot.row_dirty.clear();
        snapshot.row_dirty.resize(snapshot.rows.len(), true);
        snapshot.scroll_shift = 0;
        snapshot.cursor = cursor;
        snapshot.copy_cursor = None;
        snapshot.colors = colors;
        snapshot.selection = selection;
        snapshot.search = search;
        snapshot.row_base = row_base;
        snapshot.abs_row_base = abs_row_base;
        snapshot.active_is_alt = terminal.active_is_alt;
        snapshot.cols = cols;
        snapshot.rows_n = rows_n;
        snapshot.focused = true;
        snapshot.cursor_blink_visible = true;
        snapshot.hover_link = None;
        snapshot.search_prompt = None;
        snapshot.command_palette = None;
        snapshot.confirm_dialog = None;
        snapshot.preedit = None;
        snapshot.image_placements = image_placements;
        snapshot.images = images;
    }

    /// Refresh a publish slot for the Session Overview.
    ///
    /// If the previously published [`Arc`] is unique to the slot, this mutates
    /// its [`FrameSnapshot`] in place and reuses row/cell allocations. If the
    /// render thread still holds a clone, it publishes a fresh snapshot instead
    /// so already-borrowed overview frames remain immutable.
    pub fn refresh_peek_slot(slot: &mut Option<Arc<Self>>, terminal: &Terminal) {
        if let Some(snapshot) = slot.as_mut().and_then(Arc::get_mut) {
            Self::peek_into(terminal, snapshot);
            return;
        }
        *slot = Some(Arc::new(Self::peek(terminal)));
    }

    pub fn is_selected(&self, x: u16, y: u16) -> bool {
        let Some(selection) = self.selection else {
            return false;
        };
        selection.contains(SelectionPoint::new(x, self.row_base + y as usize))
    }

    pub fn is_search_match(&self, x: u16, y: u16) -> bool {
        self.search
            .contains(SelectionPoint::new(x, self.row_base + y as usize))
    }

    pub fn is_active_search_match(&self, x: u16, y: u16) -> bool {
        self.search
            .contains_active(SelectionPoint::new(x, self.row_base + y as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_core::{GridSize, Rgb};
    use noa_grid::Terminal;

    fn put(term: &mut Terminal, y: usize, ch: char) {
        term.primary.grid[y].cells[0].ch = ch;
        // Direct cell pokes bypass the print path's occupancy tracking.
        term.primary.grid[y].mark_occupied(1);
    }

    #[test]
    fn snapshot_uses_viewport_rows_and_hides_live_cursor() {
        let mut term = Terminal::new(GridSize::new(2, 2));
        put(&mut term, 0, 'A');
        put(&mut term, 1, 'B');
        term.primary.scroll_up_region(1);
        put(&mut term, 1, 'C');
        term.scroll_viewport_up(1);

        let snap = FrameSnapshot::from_terminal(&mut term);

        assert_eq!(snap.rows[0].cells[0].ch, 'A');
        assert_eq!(snap.rows[1].cells[0].ch, 'B');
        assert!(!snap.cursor.visible);
    }

    #[test]
    fn peek_does_not_consume_row_damage_and_hides_the_cursor() {
        let mut term = Terminal::new(GridSize::new(2, 2));
        // Consume the fresh terminal's initial all-dirty state first, so
        // the assertion below exercises a row marked dirty by ordinary
        // output, not first-frame init.
        term.primary.take_visible_rows_with_damage();
        put(&mut term, 0, 'A');
        term.primary.grid[0].dirty = true;

        let snap = FrameSnapshot::peek(&term);

        assert_eq!(snap.rows[0].cells[0].ch, 'A');
        assert!(
            snap.row_dirty.iter().all(|&dirty| dirty),
            "peek reports every row dirty (full re-shape)"
        );
        assert!(!snap.cursor.visible, "peek always hides the cursor");

        let (_, row_dirty) = term.primary.take_visible_rows_with_damage();
        assert!(
            row_dirty[0],
            "peek must not clear the real dirty bit meant for from_terminal to consume"
        );
    }

    #[test]
    fn from_terminal_defaults_preedit_to_none() {
        let mut term = Terminal::new(GridSize::new(2, 2));
        let snap = FrameSnapshot::from_terminal(&mut term);
        assert!(snap.preedit.is_none());
        // The read-only peek path must default it too.
        assert!(FrameSnapshot::peek(&term).preedit.is_none());
    }

    #[test]
    fn snapshot_carries_terminal_color_overrides() {
        let mut term = Terminal::new(GridSize::new(2, 2));
        term.colors.set_default_fg(Rgb::new(1, 2, 3));

        let snap = FrameSnapshot::from_terminal(&mut term);

        assert_eq!(snap.colors.default_fg(), Some(Rgb::new(1, 2, 3)));
    }

    #[test]
    fn snapshot_keeps_combining_cell_text() {
        let mut term = Terminal::new(GridSize::new(2, 2));
        term.primary.grid[0].cells[0].ch = 'a';
        term.primary.grid[0].cells[0].push_combining('\u{301}');
        term.primary.grid[0].mark_occupied(1);

        let snap = FrameSnapshot::from_terminal(&mut term);

        assert_eq!(snap.rows[0].cells[0].text(), "a\u{301}");
    }

    #[test]
    fn peek_into_reuses_visible_row_storage() {
        let mut term = Terminal::new(GridSize::new(2, 2));
        put(&mut term, 0, 'A');
        let mut snap = FrameSnapshot::peek(&term);
        let row0_ptr = snap.rows[0].cells.as_ptr();
        snap.hover_link = Some(HoverLink::Range {
            y: 0,
            x_start: 0,
            x_end: 1,
        });

        put(&mut term, 0, 'B');
        FrameSnapshot::peek_into(&term, &mut snap);

        assert_eq!(snap.rows[0].cells.as_ptr(), row0_ptr);
        assert_eq!(snap.rows[0].cells[0].ch, 'B');
        assert!(snap.row_dirty.iter().all(|&dirty| dirty));
        assert!(snap.hover_link.is_none());
    }

    #[test]
    fn refresh_peek_slot_reuses_unique_arc_snapshot() {
        let mut term = Terminal::new(GridSize::new(2, 1));
        put(&mut term, 0, 'A');
        let mut slot = Some(std::sync::Arc::new(FrameSnapshot::peek(&term)));
        let first_ptr = std::sync::Arc::as_ptr(slot.as_ref().unwrap());

        put(&mut term, 0, 'B');
        FrameSnapshot::refresh_peek_slot(&mut slot, &term);

        let snap = slot.as_ref().unwrap();
        assert_eq!(std::sync::Arc::as_ptr(snap), first_ptr);
        assert_eq!(snap.rows[0].cells[0].ch, 'B');
    }

    #[test]
    fn refresh_peek_slot_replaces_shared_arc_snapshot() {
        let mut term = Terminal::new(GridSize::new(2, 1));
        put(&mut term, 0, 'A');
        let mut slot = Some(std::sync::Arc::new(FrameSnapshot::peek(&term)));
        let held = std::sync::Arc::clone(slot.as_ref().unwrap());

        put(&mut term, 0, 'B');
        FrameSnapshot::refresh_peek_slot(&mut slot, &term);

        let snap = slot.as_ref().unwrap();
        assert!(!std::sync::Arc::ptr_eq(&held, snap));
        assert_eq!(held.rows[0].cells[0].ch, 'A');
        assert_eq!(snap.rows[0].cells[0].ch, 'B');
    }

    #[test]
    fn recycled_snapshot_keeps_clean_rows_when_viewport_key_matches() {
        let mut term = Terminal::new(GridSize::new(2, 2));
        put(&mut term, 0, 'A');
        put(&mut term, 1, 'B');
        let first = FrameSnapshot::from_terminal(&mut term);
        let row0_ptr = first.rows[0].cells.as_ptr();
        let recycle = first.into_recycle();

        put(&mut term, 1, 'C');
        term.primary.grid[1].dirty = true;
        let second = FrameSnapshot::from_terminal_recycle(&mut term, recycle);

        assert_eq!(second.row_dirty, vec![false, true]);
        assert_eq!(second.rows[0].cells[0].ch, 'A');
        assert_eq!(second.rows[0].cells.as_ptr(), row0_ptr);
        assert_eq!(second.rows[1].cells[0].ch, 'C');
    }

    #[test]
    fn recycled_snapshot_reclones_rows_when_viewport_key_changes() {
        let mut term = Terminal::new(GridSize::new(2, 2));
        put(&mut term, 0, 'A');
        put(&mut term, 1, 'B');
        let recycle = FrameSnapshot::from_terminal(&mut term).into_recycle();

        term.primary.scroll_up_region(1);
        put(&mut term, 1, 'C');
        let snap = FrameSnapshot::from_terminal_recycle(&mut term, recycle);

        assert_eq!(snap.rows[0].cells[0].ch, 'B');
        assert_eq!(snap.rows[1].cells[0].ch, 'C');
    }

    #[test]
    fn recycled_snapshot_reclones_rows_for_a_pinned_scroll() {
        let mut term = Terminal::new(GridSize::new(2, 3));
        put(&mut term, 0, 'A');
        put(&mut term, 1, 'B');
        put(&mut term, 2, 'C');
        let first = FrameSnapshot::from_terminal(&mut term);
        let base = (first.row_base, first.abs_row_base);
        let recycle = first.into_recycle();

        term.primary.set_viewport_locked(true);
        put(&mut term, 0, 'X');
        term.primary.grid[0].dirty = true;
        term.primary.scroll_up_region(1);
        let snap = FrameSnapshot::from_terminal_recycle(&mut term, recycle);

        assert_eq!((snap.row_base, snap.abs_row_base), base);
        assert_eq!(snap.scroll_shift, 1);
        assert_eq!(snap.rows[0].cells[0].ch, 'X');
        assert_eq!(snap.rows[1].cells[0].ch, 'B');
        assert_eq!(snap.rows[2].cells[0].ch, 'C');
    }

    #[test]
    fn recycled_snapshot_reclones_rows_when_size_changes() {
        let mut term = Terminal::new(GridSize::new(2, 2));
        put(&mut term, 0, 'A');
        let recycle = FrameSnapshot::from_terminal(&mut term).into_recycle();

        term.resize(GridSize::new(3, 2));
        term.primary.grid[0].cells[2].ch = 'C';
        term.primary.grid[0].mark_occupied(3);
        let snap = FrameSnapshot::from_terminal_recycle(&mut term, recycle);

        assert_eq!(snap.cols, 3);
        assert_eq!(snap.rows[0].cells[0].ch, 'A');
        assert_eq!(snap.rows[0].cells[2].ch, 'C');
    }

    #[test]
    fn recycled_snapshot_reclones_rows_when_active_screen_changes() {
        let mut term = Terminal::new(GridSize::new(3, 1));
        let mut stream = noa_vt::Stream::new();
        stream.feed(b"PRI", &mut term);
        let recycle = FrameSnapshot::from_terminal(&mut term).into_recycle();

        stream.feed(b"\x1b[?1049hALT", &mut term);
        let snap = FrameSnapshot::from_terminal_recycle(&mut term, recycle);

        assert!(snap.active_is_alt);
        assert_eq!(snap.rows[0].cells[0].ch, 'A');
        assert_eq!(snap.rows[0].cells[1].ch, 'L');
        assert_eq!(snap.rows[0].cells[2].ch, 'T');
    }

    #[test]
    fn snapshot_projects_selection_onto_visible_rows() {
        let mut term = Terminal::new(GridSize::new(2, 2));
        put(&mut term, 0, 'A');
        put(&mut term, 1, 'B');
        term.primary.scroll_up_region(1);
        put(&mut term, 1, 'C');
        term.scroll_viewport_up(1);
        term.set_viewport_selection(
            noa_core::Point { x: 1, y: 0 },
            noa_core::Point { x: 0, y: 1 },
        );

        let snap = FrameSnapshot::from_terminal(&mut term);

        assert_eq!(snap.row_base, 0);
        assert!(!snap.is_selected(0, 0));
        assert!(snap.is_selected(1, 0));
        assert!(snap.is_selected(0, 1));
        assert!(!snap.is_selected(1, 1));
    }

    #[test]
    fn snapshot_projects_search_matches_onto_visible_rows() {
        let mut term = Terminal::new(GridSize::new(2, 2));
        put(&mut term, 0, 'A');
        put(&mut term, 1, 'B');
        term.primary.scroll_up_region(1);
        put(&mut term, 1, 'C');
        term.set_search_query("B");
        term.scroll_viewport_up(1);

        let snap = FrameSnapshot::from_terminal(&mut term);

        assert!(snap.is_search_match(0, 1));
        assert!(snap.is_active_search_match(0, 1));
        assert!(!snap.is_search_match(0, 0));
    }

    #[test]
    fn abs_row_base_stays_unique_across_scrollback_eviction() {
        // Regression: the renderer keyed frame invalidation on the storage-index
        // `row_base`, which repeats when equal numbers of rows are pushed and
        // evicted (viewport pinned to the top of retained history reads
        // `row_base == 0` both before and after an eviction). `abs_row_base` is
        // session-absolute (`rows_evicted + row_base`) and must differ across an
        // eviction so the renderer never cache-hits shifted history rows.
        let mut term = Terminal::new(GridSize::new(2, 2));
        // Cap below one full 64 KiB scrollback page (~65.8 KB for 8192 packed
        // cells) so that as soon as a second page opens, the first is evicted
        // whole (eviction is page-granular, at `PAGE_CELL_CAPACITY` = 8192 cells).
        term.set_scrollback_limit_bytes(40_000);

        // Baseline: empty scrollback, viewport at the top → row_base 0, abs 0.
        let before = FrameSnapshot::from_terminal(&mut term);
        assert_eq!((before.row_base, before.abs_row_base), (0, 0));

        // Push > 1 page of two-cell rows so the front page is evicted. Both cells
        // are non-blank so trailing-blank trimming keeps two packed cells/row
        // (~4096 rows/page); 4200 rows seals page 1 and starts page 2.
        for _ in 0..4200 {
            put(&mut term, 0, 'x');
            term.primary.grid[0].cells[1].ch = 'y';
            term.primary.grid[0].mark_occupied(2);
            term.primary.scroll_up_region(1);
        }
        assert!(
            term.primary.rows_evicted() > 0,
            "expected the front scrollback page to have evicted"
        );

        // Pin the viewport back to the top: same storage-index row_base as the
        // baseline, but a strictly larger session-absolute base.
        term.scroll_viewport_to_top();
        let after = FrameSnapshot::from_terminal(&mut term);

        assert_eq!(
            after.row_base, before.row_base,
            "storage-index row_base collides across eviction (the original bug)"
        );
        assert!(
            after.abs_row_base > before.abs_row_base,
            "abs_row_base must strictly advance across eviction, staying unique"
        );
        assert_eq!(
            after.abs_row_base,
            term.primary.rows_evicted() + after.row_base,
            "abs_row_base is rows_evicted + row_base"
        );
    }

    // ── Kitty-graphics snapshot construction ────────────────────────────

    /// Minimal base64 encoder for building direct-transfer APC payloads (the
    /// grid's own encoder is crate-private).
    fn base64(data: &[u8]) -> Vec<u8> {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        for chunk in data.chunks(3) {
            let b0 = chunk[0];
            let b1 = chunk.get(1).copied().unwrap_or(0);
            let b2 = chunk.get(2).copied().unwrap_or(0);
            let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
            out.push(ALPHABET[(n >> 18 & 63) as usize]);
            out.push(ALPHABET[(n >> 12 & 63) as usize]);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6 & 63) as usize]
            } else {
                b'='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[(n & 63) as usize]
            } else {
                b'='
            });
        }
        out
    }

    fn apc(ctrl: &str, data: &[u8]) -> Vec<u8> {
        let mut out = b"\x1b_G".to_vec();
        out.extend_from_slice(ctrl.as_bytes());
        out.push(b';');
        out.extend_from_slice(&base64(data));
        out.extend_from_slice(b"\x1b\\");
        out
    }

    /// A 20×24 terminal with 10×20 px cells carrying one placed 1×1 image (id 1).
    fn term_with_image() -> Terminal {
        use noa_vt::Stream;
        let mut term = Terminal::new(GridSize::new(20, 24));
        term.set_pixel_metrics(10, 20, 200, 480);
        let mut stream = Stream::new();
        stream.feed(
            &apc("a=T,f=32,s=1,v=1,i=1,C=1", &[10, 20, 30, 40]),
            &mut term,
        );
        term
    }

    #[test]
    fn from_terminal_carries_visible_placement_and_referenced_image() {
        let mut term = term_with_image();
        let snap = FrameSnapshot::from_terminal(&mut term);

        assert_eq!(snap.image_placements.len(), 1);
        let placement = snap.image_placements[0];
        assert_eq!(placement.image_id, 1);
        assert_eq!(placement.grid_y, 0);
        assert_eq!((placement.cols, placement.rows), (1, 1));

        assert_eq!(snap.images.len(), 1);
        assert_eq!(snap.images[0].id, 1);
        assert_eq!(&snap.images[0].rgba[..], &[10, 20, 30, 40]);
        // Placement epoch is copied from the backing image.
        assert_eq!(placement.epoch, snap.images[0].epoch);
    }

    #[test]
    fn snapshot_image_rgba_is_an_arc_clone_not_a_copy() {
        let mut term = term_with_image();
        let snap = FrameSnapshot::from_terminal(&mut term);
        let store_rgba = &term.kitty_image(1).expect("image present").rgba;
        assert!(
            std::sync::Arc::ptr_eq(&snap.images[0].rgba, store_rgba),
            "snapshot must share the store's Arc, not deep-copy the pixels"
        );
    }

    #[test]
    fn duplicate_placements_of_one_image_dedup_the_image_list() {
        use noa_vt::Stream;
        let mut term = term_with_image();
        // Place the same image again at a different row (C=1 keeps the cursor).
        let mut stream = Stream::new();
        stream.feed(b"\x1b[3;1H", &mut term);
        stream.feed(&apc("a=p,i=1,p=2,C=1", &[]), &mut term);

        let snap = FrameSnapshot::from_terminal(&mut term);
        assert_eq!(snap.image_placements.len(), 2, "two placements");
        assert_eq!(snap.images.len(), 1, "one shared image, deduped by id");
    }

    #[test]
    fn peek_builds_the_same_image_snapshot_as_from_terminal() {
        let term = term_with_image();
        let snap = FrameSnapshot::peek(&term);
        assert_eq!(snap.image_placements.len(), 1);
        assert_eq!(snap.images.len(), 1);
        assert_eq!(snap.images[0].id, 1);
    }

    #[test]
    fn snapshot_resolves_unicode_placeholder_into_a_placement() {
        use noa_core::{Color, Rgb};
        use noa_vt::Stream;
        // A 30×40 image placed as a virtual 3×2 cell grid (U=1), drawn nowhere
        // directly — only via placeholder cells.
        let mut term = Terminal::new(GridSize::new(20, 24));
        term.set_pixel_metrics(10, 20, 200, 480);
        let mut stream = Stream::new();
        stream.feed(
            &apc(
                "a=T,f=32,s=30,v=40,i=1,U=1,c=3,r=2,C=1",
                &vec![0u8; 30 * 40 * 4],
            ),
            &mut term,
        );
        // One placeholder cell at (0,0): image id 1, image row 0, column 0.
        let cell = &mut term.primary.grid[0].cells[0];
        cell.ch = noa_grid::PLACEHOLDER;
        cell.fg = Color::Rgb(Rgb::new(0, 0, 1));
        cell.push_combining('\u{0305}'); // row 0
        cell.push_combining('\u{0305}'); // column 0

        let snap = FrameSnapshot::from_terminal(&mut term);
        assert_eq!(
            snap.image_placements.len(),
            1,
            "placeholder yields a placement"
        );
        let p = snap.image_placements[0];
        assert_eq!((p.grid_x, p.grid_y), (0, 0));
        assert_eq!((p.cols, p.rows), (1, 1));
        assert_eq!(p.src, Some([0, 0, 10, 20]), "top-left image tile");
        assert_eq!(snap.images.len(), 1, "the referenced image is carried");
        assert_eq!(snap.images[0].id, 1);
    }
}
