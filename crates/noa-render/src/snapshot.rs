//! `FrameSnapshot` — a lock-free-to-render copy of the bits of `Terminal`
//! needed to rebuild a frame's GPU instances. `noa-app` takes this under the
//! `Terminal` mutex and then calls into the renderer unlocked.

use noa_grid::{Cursor, Row, Terminal, TerminalColors};

/// A snapshot of the active screen taken under the `Terminal` lock.
///
/// Inc-1 clones every row rather than diffing dirty rows — correctness over
/// micro-optimization; dirty-row diffing is a natural inc≥2 follow-up.
pub struct FrameSnapshot {
    pub rows: Vec<Row>,
    pub cursor: Cursor,
    pub colors: TerminalColors,
    pub cols: u16,
    pub rows_n: u16,
}

impl FrameSnapshot {
    /// Clone the active screen's rows + cursor out of `terminal`.
    pub fn from_terminal(terminal: &Terminal) -> Self {
        let screen = terminal.active();
        let mut cursor = screen.cursor;
        if screen.viewport_offset() > 0 {
            cursor.visible = false;
        }
        FrameSnapshot {
            rows: screen.visible_rows(),
            cursor,
            colors: terminal.colors.clone(),
            cols: screen.cols,
            rows_n: screen.rows,
        }
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

        let snap = FrameSnapshot::from_terminal(&term);

        assert_eq!(snap.rows[0].cells[0].ch, 'A');
        assert_eq!(snap.rows[1].cells[0].ch, 'B');
        assert!(!snap.cursor.visible);
    }

    #[test]
    fn snapshot_carries_terminal_color_overrides() {
        let mut term = Terminal::new(GridSize::new(2, 2));
        term.colors.set_default_fg(Rgb::new(1, 2, 3));

        let snap = FrameSnapshot::from_terminal(&term);

        assert_eq!(snap.colors.default_fg(), Some(Rgb::new(1, 2, 3)));
    }
}
