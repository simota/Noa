//! A soft-wrapped line as one unit: the run of [`Row`]s chained by
//! `Row::wrapped`, flattened to text for the Cmd+hover link detectors
//! ([`crate::url`], [`crate::path`]) so a URL or path that wraps at the right
//! margin is matched whole.

use std::borrow::{Borrow, Cow};
use std::ops::Range;

use crate::cell::Row;
use crate::path::PathMatch;
use crate::url::UrlMatch;
use noa_core::CellAttrs;

/// How many rows the hover path follows a soft-wrap chain in each direction
/// from the pointer's row. Hover detection runs on every pointer move, and a
/// single unbroken line (minified JSON, a base64 blob) can span thousands of
/// rows. A link running past either end of this window is not matched at
/// all — never as the part of it that happens to fit.
const MAX_ROWS_EACH_WAY: usize = 32;

/// The soft-wrapped line through one visible row, from
/// [`crate::Screen::visible_logical_line`].
pub struct LogicalLine<'a> {
    /// The chained rows, top to bottom.
    pub rows: Vec<Cow<'a, Row>>,
    /// Index in `rows` of the row the line was looked up from.
    pub index: usize,
    /// Per row, the cells that are this line's text. Narrower than the row
    /// where the wrap happened at DECLRMM left/right margins.
    bounds: Vec<Range<usize>>,
    /// The chain goes on above `rows[0]` / below the last row.
    cut_above: bool,
    cut_below: bool,
    viewport_y: u16,
    viewport_rows: u16,
}

impl<'a> LogicalLine<'a> {
    /// `margins` is the screen's `(left, right)` DECLRMM margins, when set.
    pub(crate) fn collect(
        row_at: impl Fn(usize) -> Option<Cow<'a, Row>>,
        storage_y: usize,
        margins: Option<(usize, usize)>,
        viewport_y: u16,
        viewport_rows: u16,
    ) -> Option<Self> {
        let hovered = row_at(storage_y)?;
        let mut before = Vec::new();
        while before.len() < MAX_ROWS_EACH_WAY
            && let Some(y) = storage_y.checked_sub(before.len() + 1)
            && let Some(row) = row_at(y)
            && row.wrapped
        {
            before.push(row);
        }
        let index = before.len();
        let mut rows = before;
        rows.reverse();
        rows.push(hovered);
        while rows.len() - index <= MAX_ROWS_EACH_WAY
            && rows.last().is_some_and(|row| row.wrapped)
            && let Some(row) = row_at(storage_y + rows.len() - index)
        {
            rows.push(row);
        }
        let cut_above = storage_y
            .checked_sub(index + 1)
            .and_then(&row_at)
            .is_some_and(|row| row.wrapped);
        let cut_below = rows.last().is_some_and(|row| row.wrapped)
            && row_at(storage_y + rows.len() - index).is_some();

        // A row wrapped at the right margin was written exactly up to it
        // (the occupancy watermark), and its continuation resumes at the
        // left margin; the cells outside are not part of the line.
        let at_margin = |y: usize| {
            margins.filter(|&(_, right)| {
                y + 1 < rows.len() && rows[y].wrapped && rows[y].occupied() == right + 1
            })
        };
        let bounds = (0..rows.len())
            .map(|y| {
                let start = y
                    .checked_sub(1)
                    .and_then(at_margin)
                    .map_or(0, |(left, _)| left);
                let end = at_margin(y).map_or(rows[y].cells.len(), |(_, right)| right + 1);
                start..end
            })
            .collect();

        Some(LogicalLine {
            rows,
            index,
            bounds,
            cut_above,
            cut_below,
            viewport_y,
            viewport_rows,
        })
    }

    /// The row the line was looked up from.
    pub fn row(&self) -> &Row {
        &self.rows[self.index]
    }

    /// The `https?://` run covering `column` of the looked-up row, if any.
    pub fn url_at(&self, column: u16) -> Option<UrlMatch> {
        crate::url::url_in(&self.text(column)?, &self.rows, (self.index, column))
    }

    /// The path run covering `column` of the looked-up row, if any.
    pub fn path_at(&self, column: u16) -> Option<PathMatch> {
        crate::path::path_in(&self.text(column)?, &self.rows, (self.index, column))
    }

    /// The line's text, when `column` of the looked-up row is part of it.
    fn text(&self, column: u16) -> Option<LineText> {
        self.bounds[self.index]
            .contains(&usize::from(column))
            .then(|| LineText::bounded(&self.rows, &self.bounds, self.cut_above, self.cut_below))
    }

    /// Map a match's `(row index, x)` corners to viewport `(y, x)` corners.
    /// A match containing the looked-up row can still begin above the
    /// viewport or end below it; those ends clamp to the viewport's edge.
    pub fn viewport_span(
        &self,
        start: (usize, u16),
        end: (usize, u16),
    ) -> ((u16, u16), (u16, u16)) {
        let y_of = |row: usize| i64::from(self.viewport_y) + row as i64 - self.index as i64;
        let last_y = self.viewport_rows.saturating_sub(1);
        let last_x = (self.row().cells.len() as u16).saturating_sub(1);
        let start = match y_of(start.0) {
            y if y < 0 => (0, 0),
            y => (y as u16, start.1),
        };
        let end = match y_of(end.0) {
            y if y > i64::from(last_y) => (last_y, last_x),
            y => (y as u16, end.1),
        };
        (start, end)
    }
}

/// The text of a run of rows read as one line, with each char's source cell.
pub(crate) struct LineText {
    pub chars: Vec<char>,
    /// `(row index, cell x)` per entry of `chars`, ascending.
    pub cells: Vec<(usize, u16)>,
    /// The line's text goes on before `chars[0]` / after the last char.
    cut_start: bool,
    cut_end: bool,
}

impl LineText {
    /// Flatten whole `rows` in reading order as one complete line.
    pub fn new<R: Borrow<Row>>(rows: &[R]) -> Self {
        let bounds: Vec<_> = rows.iter().map(|row| 0..row.borrow().cells.len()).collect();
        Self::bounded(rows, &bounds, false, false)
    }

    /// Flatten `rows[y].cells[bounds[y]]` in reading order, skipping what is
    /// layout rather than text: wide-glyph spacer cells, and the filler a
    /// wide glyph leaves at the wrap column when it wraps to the next row.
    pub fn bounded<R: Borrow<Row>>(
        rows: &[R],
        bounds: &[Range<usize>],
        cut_start: bool,
        cut_end: bool,
    ) -> Self {
        let mut chars = Vec::new();
        let mut cells = Vec::new();
        for (y, (row, bounds)) in rows.iter().zip(bounds).enumerate() {
            let row: &Row = row.borrow();
            let pad_x = bounds.end.checked_sub(1).filter(|&x| {
                y + 1 < rows.len()
                    && row.wrapped
                    && row.cells[x].attrs.contains(CellAttrs::WIDE_PAD)
            });
            for x in bounds.clone() {
                let cell = &row.cells[x];
                if cell.attrs.contains(CellAttrs::WIDE_SPACER) || pad_x == Some(x) {
                    continue;
                }
                for ch in cell.text_chars() {
                    chars.push(ch);
                    cells.push((y, x as u16));
                }
            }
        }
        LineText {
            chars,
            cells,
            cut_start,
            cut_end,
        }
    }

    /// Whether a match over `chars[start..end]` may be only the visible part
    /// of something longer: it begins at a cut start, or nothing separates
    /// it from a cut end.
    pub fn cuts(&self, start: usize, end: usize) -> bool {
        (self.cut_start && start == 0)
            || (self.cut_end && !self.chars[end..].iter().any(|c| c.is_whitespace()))
    }

    /// The cell span of `chars[start..end]` (non-empty), with the end pulled
    /// over the spacer column when the last glyph is wide — `cells` skips
    /// spacers, so the glyph's right half would otherwise miss the span.
    pub fn span<R: Borrow<Row>>(
        &self,
        rows: &[R],
        start: usize,
        end: usize,
    ) -> ((usize, u16), (usize, u16)) {
        let (end_row, last_x) = self.cells[end - 1];
        let wide = rows[end_row].borrow().cells[last_x as usize]
            .attrs
            .contains(CellAttrs::WIDE);
        (self.cells[start], (end_row, last_x + u16::from(wide)))
    }
}

#[cfg(test)]
mod tests {
    use crate::Terminal;
    use crate::url::detect_url_at;
    use noa_core::GridSize;
    use noa_vt::Stream;

    const TEXT: &[u8] = b"see https://example.com/a/long/path?q=1 ok";

    fn terminal(rows: u16) -> Terminal {
        let mut terminal = Terminal::new(GridSize::new(20, rows));
        Stream::new().feed(TEXT, &mut terminal);
        terminal
    }

    #[test]
    fn collects_the_whole_wrap_chain_from_any_of_its_rows() {
        let terminal = terminal(4);
        for y in 0..3 {
            let line = terminal.primary.visible_logical_line(y).unwrap();
            assert_eq!((line.rows.len(), line.index), (3, usize::from(y)));
        }
        let blank = terminal.primary.visible_logical_line(3).unwrap();
        assert_eq!((blank.rows.len(), blank.index), (1, 0));
        assert!(terminal.primary.visible_logical_line(4).is_none());
    }

    #[test]
    fn maps_a_match_to_viewport_cells() {
        let terminal = terminal(4);
        let line = terminal.primary.visible_logical_line(1).unwrap();
        let url = detect_url_at(&line.rows, line.index, 0).unwrap();
        assert_eq!(
            line.viewport_span((url.start_row, url.start_x), (url.end_row, url.end_x)),
            ((0, 4), (1, 18))
        );
    }

    #[test]
    fn clamps_a_match_that_begins_in_scrollback() {
        // Two rows of viewport for three rows of text: the URL's first row
        // has scrolled off the top.
        let terminal = terminal(2);
        let line = terminal.primary.visible_logical_line(0).unwrap();
        assert_eq!((line.rows.len(), line.index), (3, 1));
        let url = detect_url_at(&line.rows, line.index, 0).unwrap();
        assert_eq!(url.uri, "https://example.com/a/long/path?q=1");
        assert_eq!(
            line.viewport_span((url.start_row, url.start_x), (url.end_row, url.end_x)),
            ((0, 0), (0, 18))
        );
    }

    #[test]
    fn clamps_a_match_that_ends_below_the_viewport() {
        // One row of viewport, scrolled back to the URL's first row.
        let mut terminal = terminal(1);
        terminal.primary.scroll_viewport_up(2);
        let line = terminal.primary.visible_logical_line(0).unwrap();
        assert_eq!((line.rows.len(), line.index), (3, 0));
        let url = detect_url_at(&line.rows, line.index, 4).unwrap();
        assert_eq!(
            line.viewport_span((url.start_row, url.start_x), (url.end_row, url.end_x)),
            ((0, 4), (0, 19))
        );
    }

    fn feed(cols: u16, rows: u16, bytes: &[u8]) -> Terminal {
        let mut terminal = Terminal::new(GridSize::new(cols, rows));
        Stream::new().feed(bytes, &mut terminal);
        terminal
    }

    #[test]
    fn joins_a_url_wrapped_at_the_right_margin() {
        // DECLRMM with margins 1..=15 on a 20-column screen: the wrap leaves
        // five never-written cells to the right of each margin-wrapped row.
        let terminal = feed(20, 4, b"\x1b[?69h\x1b[1;15shttps://example.com/abcdef");
        assert!(terminal.primary.grid[0].wrapped);
        for (y, x) in [(0, 0), (0, 14), (1, 0), (1, 10)] {
            let line = terminal.primary.visible_logical_line(y).unwrap();
            let url = line.url_at(x).expect("inside the margin-wrapped URL");
            assert_eq!(url.uri, "https://example.com/abcdef");
            assert_eq!((url.start_row, url.start_x), (0, 0));
            assert_eq!((url.end_row, url.end_x), (1, 10));
        }
        let line = terminal.primary.visible_logical_line(0).unwrap();
        assert_eq!(line.url_at(17), None, "right of the margin is not the URL");
    }

    #[test]
    fn joins_a_url_wrapped_between_both_margins() {
        let terminal = feed(
            20,
            4,
            b"\x1b[?69h\x1b[3;15s\x1b[1;3Hhttps://example.com/abcdef",
        );
        assert!(terminal.primary.grid[0].wrapped);
        for (y, x) in [(0, 2), (0, 14), (1, 2), (1, 14)] {
            let line = terminal.primary.visible_logical_line(y).unwrap();
            let url = line.url_at(x).expect("inside the margin-wrapped URL");
            assert_eq!(url.uri, "https://example.com/abcdef");
            assert_eq!((url.start_row, url.start_x), (0, 2));
            assert_eq!((url.end_row, url.end_x), (1, 14));
        }
        let line = terminal.primary.visible_logical_line(1).unwrap();
        assert_eq!(line.url_at(0), None, "left of the margin is not the URL");
    }

    #[test]
    fn joins_a_path_wrapped_at_the_right_margin() {
        let terminal = feed(20, 4, b"\x1b[?69h\x1b[1;15ssrc/very/long/dir/file.rs:7");
        let line = terminal.primary.visible_logical_line(1).unwrap();
        let path = line.path_at(0).expect("inside the margin-wrapped path");
        assert_eq!(path.path, "src/very/long/dir/file.rs");
        assert_eq!(path.line, Some(7));
    }

    #[test]
    fn rejects_a_url_running_past_the_end_of_the_window() {
        let text = format!("https://e.co/{}", "a".repeat(600));
        let terminal = feed(4, 200, text.as_bytes());
        let line = terminal.primary.visible_logical_line(0).unwrap();
        assert_eq!(line.url_at(0), None);
    }

    #[test]
    fn rejects_a_path_cut_by_either_end_of_the_window() {
        let text = format!("/{}", "ab/".repeat(200));
        let terminal = feed(4, 200, text.as_bytes());
        let head = terminal.primary.visible_logical_line(0).unwrap();
        assert_eq!(head.path_at(0), None, "the path runs past the window's end");
        let middle = terminal.primary.visible_logical_line(75).unwrap();
        assert_eq!(middle.path_at(0), None, "the path begins before the window");
    }

    #[test]
    fn rejects_a_path_whose_head_is_before_the_window() {
        // 151 rows of one path token, ending inside the window of its last
        // row: only the window's start cuts it.
        let text = format!("/{}end.rs zz", "ab/".repeat(199));
        let terminal = feed(4, 200, text.as_bytes());
        let line = terminal.primary.visible_logical_line(150).unwrap();
        assert_eq!((line.rows.len(), line.index), (34, 32));
        assert_eq!(line.path_at(0), None);
    }

    #[test]
    fn keeps_a_whole_url_inside_a_cut_window() {
        let text = format!("see https://e.co/x {}", "y".repeat(600));
        let terminal = feed(4, 200, text.as_bytes());
        let line = terminal.primary.visible_logical_line(1).unwrap();
        assert_eq!(line.url_at(0).map(|m| m.uri), Some("https://e.co/x".into()));
    }

    #[test]
    fn follows_a_long_line_only_a_bounded_distance() {
        let mut terminal = Terminal::new(GridSize::new(4, 200));
        Stream::new().feed("x".repeat(4 * 150).as_bytes(), &mut terminal);
        let line = terminal.primary.visible_logical_line(75).unwrap();
        assert_eq!(
            (line.rows.len(), line.index),
            (2 * super::MAX_ROWS_EACH_WAY + 1, super::MAX_ROWS_EACH_WAY)
        );
    }
}
