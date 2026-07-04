//! `FrameSnapshot` — a lock-free-to-render copy of the bits of `Terminal`
//! needed to rebuild a frame's GPU instances. `noa-app` takes this under the
//! `Terminal` mutex and then calls into the renderer unlocked.

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

/// The open command palette's render payload (`cmd+shift+p`), built by the
/// caller (`noa-app`) from its own palette session — the renderer never sees
/// `AppCommand`. Titles and keybind hints are already resolved to strings in
/// the app layer, keeping noa-render terminal- and command-agnostic. `rows`
/// is the filtered entry list in display order; `selected` indexes into it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandPaletteSnapshot {
    /// The live query text (may be empty).
    pub query: String,
    /// One `(title, keybind hint)` pair per filtered entry, in display order.
    /// The hint is `None` for an unbound command.
    pub rows: Vec<(String, Option<String>)>,
    /// The highlighted row's index into `rows`. Out of range only when
    /// `rows` is empty (nothing highlighted), which the renderer tolerates.
    pub selected: usize,
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

/// A snapshot of the active screen taken under the `Terminal` lock.
///
/// WP4 (REQ-PERF-1/2): `row_dirty` is parallel to `rows` (same length, same
/// index order) and reports each row's `noa_grid::Row::dirty` bit as it
/// stood *before* this snapshot cleared it — the renderer's dirty-row patch
/// path consumes it to skip instance work for unchanged rows.
pub struct FrameSnapshot {
    pub rows: Vec<Row>,
    pub row_dirty: Vec<bool>,
    pub cursor: Cursor,
    pub colors: TerminalColors,
    pub selection: Option<Selection>,
    pub search: SearchState,
    pub row_base: usize,
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
        let colors = terminal.colors.clone();
        let screen = active_screen_mut(terminal);
        let mut cursor = screen.cursor;
        if screen.viewport_offset() > 0 {
            cursor.visible = false;
        }
        let row_base = screen.visible_row_base();
        let cols = screen.cols;
        let rows_n = screen.rows;
        let selection = screen.selection;
        let search = screen.search.clone();
        let (rows, row_dirty) = screen.take_visible_rows_with_damage();
        FrameSnapshot {
            rows,
            row_dirty,
            cursor,
            colors,
            selection,
            search,
            row_base,
            cols,
            rows_n,
            focused: true,
            cursor_blink_visible: true,
            hover_link: None,
            search_prompt: None,
            command_palette: None,
            confirm_dialog: None,
        }
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

        let snap = FrameSnapshot::from_terminal(&mut term);

        assert_eq!(snap.rows[0].cells[0].text(), "a\u{301}");
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
}
