//! The synthetic rows that make a restored pane legible: the record/live
//! separator, and the notice shown when a pane's layout came back but its
//! contents did not.
//!
//! Spec: `docs/specs/scrollback-persistence.md` §5 and Stage 0.
//!
//! ## Why these are rows and not an overlay
//!
//! Both of these are content, not chrome. They have to scroll with the
//! history they annotate, survive being copied out with it, and be findable by
//! search — an overlay pinned to the viewport would claim the boundary sits
//! wherever the user happens to have scrolled. Being rows also means the
//! renderer needs to know nothing about them.
//!
//! They are pushed as *history*, never fed through the parser, so nothing here
//! can move the cursor or be mistaken for program output.

use noa_core::{CellAttrs, Color};
use noa_grid::{Cell, Row};

use crate::session_store::civil_from_unix_secs;

/// Build a full-width row of `text`, padded to `cols`, in `attrs`.
///
/// Text longer than the row is truncated rather than wrapped: these lines are
/// annotations, and a soft-wrapped annotation would read as two records.
fn annotation_row(text: &str, cols: u16, attrs: CellAttrs) -> Row {
    let width = usize::from(cols);
    let mut cells: Vec<Cell> = text
        .chars()
        .take(width)
        .map(|ch| Cell {
            ch,
            fg: Color::Default,
            attrs,
            ..Cell::default()
        })
        .collect();
    cells.resize(width, Cell::default());
    Row::from_cells(cells, false, false)
}

/// `2026-07-28 14:03` in the viewer's local time.
fn stamp(saved_at: u64, local_offset_seconds: i64) -> String {
    let clock = civil_from_unix_secs(saved_at as i64 + local_offset_seconds);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        clock.year, clock.month, clock.day, clock.hour, clock.minute
    )
}

/// Pad `label` out to `cols` with a horizontal rule on both sides, so the
/// boundary reads as a line across the pane rather than a stray sentence.
fn ruled(label: &str, cols: u16) -> String {
    let width = usize::from(cols);
    let label = format!(" {label} ");
    if label.chars().count() + 4 > width {
        return label.trim().to_string();
    }
    let remaining = width - label.chars().count();
    let left = remaining / 2;
    let right = remaining - left;
    format!(
        "{}{label}{}",
        "\u{2500}".repeat(left),
        "\u{2500}".repeat(right)
    )
}

/// The boundary row between restored history and the live session.
///
/// This is the whole answer to "is what I am reading live?": everything above
/// it was recorded at the stamped time and is not coming back, everything
/// below is this session.
pub fn separator_row(saved_at: u64, local_offset_seconds: i64, cols: u16) -> Row {
    annotation_row(
        &ruled(
            &format!("record · saved {} · live below", stamp(saved_at, local_offset_seconds)),
            cols,
        ),
        cols,
        CellAttrs::FAINT,
    )
}

/// The Stage 0 notice: shown in a pane whose layout was restored but whose
/// contents were not.
///
/// Restoring the tabs and splits without saying anything about the contents is
/// what makes an empty restored pane feel like a loss rather than a setting —
/// the layout coming back is itself a promise that the output did too. This is
/// the row that keeps that promise honest, and it names the key that would
/// change the answer.
pub fn not_persisted_notice_row(cols: u16) -> Row {
    annotation_row(
        &ruled(
            "layout restored · previous output was not saved · set scrollback-persist = tail to keep it",
            cols,
        ),
        cols,
        CellAttrs::FAINT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(row: &Row) -> String {
        row.cells.iter().map(|cell| cell.ch).collect()
    }

    #[test]
    fn the_separator_names_the_time_it_was_saved() {
        // 2023-11-14 22:13:20 UTC.
        let row = separator_row(1_700_000_000, 0, 80);
        let text = text_of(&row);
        assert!(text.contains("2023-11-14 22:13"), "{text:?}");
        assert!(text.contains("record"), "{text:?}");
        assert!(text.contains("live below"), "{text:?}");
    }

    #[test]
    fn the_separator_honors_the_local_utc_offset() {
        let utc = text_of(&separator_row(1_700_000_000, 0, 80));
        let jst = text_of(&separator_row(1_700_000_000, 9 * 3600, 80));
        assert!(utc.contains("22:13"), "{utc:?}");
        assert!(jst.contains("2023-11-15 07:13"), "{jst:?}");
    }

    #[test]
    fn annotation_rows_fill_exactly_one_grid_width() {
        for cols in [8u16, 40, 80, 200] {
            assert_eq!(separator_row(0, 0, cols).cells.len(), usize::from(cols));
            assert_eq!(
                not_persisted_notice_row(cols).cells.len(),
                usize::from(cols)
            );
        }
    }

    #[test]
    fn an_annotation_never_soft_wraps() {
        // A wrapped annotation would join the line below it into one logical
        // line for copy, search and reflow.
        assert!(!separator_row(0, 0, 20).wrapped);
        assert!(!not_persisted_notice_row(20).wrapped);
    }

    #[test]
    fn a_narrow_pane_truncates_rather_than_overflowing() {
        let row = not_persisted_notice_row(10);
        assert_eq!(row.cells.len(), 10);
        let text = text_of(&row);
        assert!(text.starts_with("layout"), "{text:?}");
    }

    #[test]
    fn the_notice_names_the_key_that_changes_the_answer() {
        let text = text_of(&not_persisted_notice_row(120));
        assert!(
            text.contains("scrollback-persist = tail"),
            "the notice is only useful if it says what to do: {text:?}"
        );
    }

    #[test]
    fn annotations_are_faint_so_they_read_as_chrome_not_output() {
        let row = separator_row(0, 0, 40);
        assert!(row.cells[0].attrs.contains(CellAttrs::FAINT));
    }
}
