//! Plain-text `https?://` URL detection within a single row, for the
//! Cmd+hover auto-link feature (`noa-app` mouse hover path). Operates on one
//! [`Row`] at a time — wrapped multi-row URLs (a URL that soft-wraps across
//! the `Row::wrapped` boundary) are out of scope for v1 and are not
//! detected.
//!
//! Char-index-to-cell-x mapping follows the same WIDE_SPACER-skipping
//! approach as [`crate::search::compute_matches`].

use crate::cell::Row;
use noa_core::CellAttrs;

/// A detected URL run, in both cell-x (for underline rendering) and text
/// (for opening) form.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UrlMatch {
    pub start_x: u16,
    pub end_x: u16,
    pub uri: String,
}

const URL_SCHEMES: [&str; 2] = ["https://", "http://"];
const TRAILING_PUNCTUATION: [char; 8] = ['.', ',', ';', ':', '!', '?', ')', '"'];

fn is_run_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>')
}

/// Trim `run`'s trailing punctuation, never shrinking below `min_len` (the
/// scheme's own length, so a lone `http://` is never mangled). A trailing
/// `)` is only trimmed when it has no matching `(` earlier in the run
/// (Wikipedia-style URLs like `.../Rust_(programming_language)` keep their
/// balanced closing paren).
fn trim_trailing_punctuation(run: &[char], min_len: usize) -> usize {
    let mut len = run.len();
    while len > min_len {
        let last = run[len - 1];
        if last == ')' {
            let open = run[..len].iter().filter(|&&c| c == '(').count();
            let close = run[..len].iter().filter(|&&c| c == ')').count();
            if open == close {
                break;
            }
            len -= 1;
            continue;
        }
        if TRAILING_PUNCTUATION.contains(&last) {
            len -= 1;
            continue;
        }
        break;
    }
    len
}

/// Find the `https?://` run covering `column` in `row`, if any.
pub fn detect_url_at_column(row: &Row, column: u16) -> Option<UrlMatch> {
    let mut text = String::new();
    let mut cell_x = Vec::new();
    for (x, cell) in row.cells.iter().enumerate() {
        if cell.attrs.contains(CellAttrs::WIDE_SPACER) {
            continue;
        }
        cell.push_text_to(&mut text);
        cell_x.extend(std::iter::repeat_n(x as u16, cell.text_chars().count()));
    }
    let chars: Vec<char> = text.chars().collect();

    for scheme in URL_SCHEMES {
        let mut search_from = 0;
        while let Some(rel_byte) = text[search_from..].find(scheme) {
            let byte_start = search_from + rel_byte;
            search_from = byte_start + scheme.len();

            let char_start = text[..byte_start].chars().count();
            let scheme_len = scheme.chars().count();
            let mut raw_end = char_start + scheme_len;
            while raw_end < chars.len() && !is_run_boundary(chars[raw_end]) {
                raw_end += 1;
            }

            let run = &chars[char_start..raw_end];
            let trimmed_len = trim_trailing_punctuation(run, scheme_len);
            let end = char_start + trimmed_len;

            let (Some(&start_x), Some(&end_x)) = (cell_x.get(char_start), cell_x.get(end - 1))
            else {
                continue;
            };
            if start_x <= column && column <= end_x {
                return Some(UrlMatch {
                    start_x,
                    end_x,
                    uri: chars[char_start..end].iter().collect(),
                });
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Cell;

    fn row_from_str(text: &str) -> Row {
        let cols = text.chars().count() as u16;
        let mut row = Row::new(cols.max(1));
        for (x, ch) in text.chars().enumerate() {
            row.cells[x] = Cell {
                ch,
                ..Cell::default()
            };
        }
        row
    }

    #[test]
    fn detects_plain_https_url() {
        let row = row_from_str("see https://example.com/path here");
        let m = detect_url_at_column(&row, 10).expect("column inside the URL should match");
        assert_eq!(m.uri, "https://example.com/path");
        assert_eq!(row.cells[m.start_x as usize].ch, 'h');
        assert_eq!(row.cells[m.end_x as usize].ch, 'h'); // last char of "path"
    }

    #[test]
    fn detects_plain_http_url_at_start_of_row() {
        let row = row_from_str("http://x.io rest");
        let m = detect_url_at_column(&row, 0).expect("column 0 is the scheme's 'h'");
        assert_eq!(m.uri, "http://x.io");
        assert_eq!(m.start_x, 0);
    }

    #[test]
    fn trims_trailing_sentence_punctuation() {
        let row = row_from_str("go to https://example.com/page, now.");
        let m = detect_url_at_column(&row, 10).unwrap();
        assert_eq!(m.uri, "https://example.com/page");
    }

    #[test]
    fn keeps_balanced_trailing_paren_but_trims_unbalanced_one() {
        let row = row_from_str("(https://en.wikipedia.org/wiki/Rust_(lang)))");
        let m = detect_url_at_column(&row, 6).unwrap();
        assert_eq!(m.uri, "https://en.wikipedia.org/wiki/Rust_(lang)");
    }

    #[test]
    fn maps_columns_correctly_past_a_wide_cjk_run() {
        // "見" occupies columns 0-1 (WIDE + WIDE_SPACER); a space is at
        // column 2; the URL starts at column 3.
        let mut row = Row::new(20);
        row.cells[0] = Cell {
            ch: '見',
            attrs: CellAttrs::WIDE,
            ..Cell::default()
        };
        row.cells[1] = Cell {
            ch: ' ',
            attrs: CellAttrs::WIDE_SPACER,
            ..Cell::default()
        };
        for (i, ch) in " http://x.io".chars().enumerate() {
            row.cells[2 + i] = Cell {
                ch,
                ..Cell::default()
            };
        }

        assert_eq!(
            detect_url_at_column(&row, 0),
            None,
            "the CJK cell itself is not part of the URL"
        );
        let m = detect_url_at_column(&row, 3).expect("column 3 is the scheme's 'h'");
        assert_eq!(m.uri, "http://x.io");
        assert_eq!(m.start_x, 3);
        assert_eq!(m.end_x, 13);
    }

    #[test]
    fn no_match_on_plain_text_row() {
        let row = row_from_str("no links on this row at all");
        assert_eq!(detect_url_at_column(&row, 5), None);
    }

    #[test]
    fn no_match_when_column_is_outside_the_url_run() {
        let row = row_from_str("prefix https://example.com suffix words");
        assert_eq!(detect_url_at_column(&row, 0), None);
        assert_eq!(detect_url_at_column(&row, row.cells.len() as u16 - 1), None);
    }
}
