//! Plain-text filesystem path detection for the Cmd+hover auto-link feature
//! (`noa-app` mouse hover path), over one logical line: a single row, or the
//! run of rows a path soft-wraps across. Mirrors [`crate::url`]'s line-text
//! and cell mapping, with path-specific boundaries and
//! trailing-punctuation trimming for `/abs/path`, `~/path`,
//! `./rel/path`, `../rel/path`, and bare relative paths with an interior
//! slash (`src/main.rs`), optionally suffixed with `:LINE` or `:LINE:COL`
//! (rustc/grep style).
//!
//! This module is filesystem-agnostic by design: it does no existence
//! checks. `noa-app` is responsible for resolving the detected path (against
//! `Terminal::cwd` or `$HOME`) and gating the hover on the resolved path
//! actually existing — that's what makes the bare-relative-path heuristic
//! (any token with an interior `/`) safe against false positives.

use std::borrow::Borrow;

use crate::cell::Row;
use crate::logical_line::LineText;

/// A detected path run, in both cell (for underline rendering) and text
/// (for resolving/opening) form. `start_row`/`end_row` index the rows passed
/// to [`detect_path_at`]; the run covers every cell between its two corners
/// in reading order. `line`/`column` come from a trailing `:LINE` or
/// `:LINE:COL` suffix, if present; the suffix is included in the run (so it
/// underlines too) but excluded from `path`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PathMatch {
    pub start_row: usize,
    pub start_x: u16,
    pub end_row: usize,
    pub end_x: u16,
    pub path: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

// Superset of url.rs's set: every closing form of the leading decoration
// `candidate_start` strips (`]`, `}`, `` ` ``) must be trimmed here too, or
// `[src/main.rs]` existence-checks with the `]` attached and never links.
const TRAILING_PUNCTUATION: [char; 11] = ['.', ',', ';', ':', '!', '?', ')', '"', ']', '}', '`'];

fn is_run_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>')
}

/// Trim `run`'s trailing punctuation, never shrinking to empty. Balanced
/// trailing parentheses belong to the path; unmatched closing ones are
/// decoration.
fn trim_trailing_punctuation(run: &[char]) -> usize {
    let mut len = run.len();
    while len > 1 {
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
        if last == '.' {
            // A trailing `/.` or `/..` is a real path component (current /
            // parent directory), not sentence punctuation — stop trimming.
            // An ellipsis (`path...`) still trims down to at most `/..`.
            let kept = &run[..len];
            if kept.ends_with(&['/', '.']) || kept.ends_with(&['/', '.', '.']) {
                break;
            }
        }
        if TRAILING_PUNCTUATION.contains(&last) {
            len -= 1;
            continue;
        }
        break;
    }
    len
}

/// Whether `token` looks like a path candidate at all: absolute, home- or
/// dot-relative, or a bare relative path with an interior slash (so a plain
/// word like `hello` never qualifies, but `src/main.rs` does).
fn looks_like_path(token: &[char]) -> bool {
    let s: String = token.iter().collect();
    if s.starts_with('/') || s.starts_with("~/") || s.starts_with("./") || s.starts_with("../") {
        return true;
    }
    // Bare relative: needs an interior '/' (not just a leading one, which
    // is already covered above and would otherwise double-count "/foo").
    s.contains('/')
}

/// A character that can legitimately continue a path token. Anything else
/// (`(`, `[`, `=`, `` ` ``, …) acts as leading decoration a path may start
/// right after.
fn is_path_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '~' | '/')
}

/// Whether an explicit path prefix (`/`, `~/`, `./`, `../`) starts at `i`.
fn explicit_prefix_at(run: &[char], i: usize) -> bool {
    match run[i] {
        '/' => true,
        '~' => run.get(i + 1) == Some(&'/'),
        '.' => {
            run.get(i + 1) == Some(&'/')
                || (run.get(i + 1) == Some(&'.') && run.get(i + 2) == Some(&'/'))
        }
        _ => false,
    }
}

/// Where the path candidate actually starts within a boundary-delimited
/// `run`, or `None` if the run holds no candidate. Shell/log output wraps
/// paths in decoration the run boundaries don't split — `(/tmp/file)`,
/// `path=/tmp/file`, `` `~/x` `` — so an explicit prefix counts anywhere in
/// the run as long as it isn't mid-path (preceded by a path character).
/// Bare relative paths (`src/main.rs`) are only recognized from the first
/// non-decoration character, since an interior slash alone is too weak an
/// anchor to restart on.
fn candidate_start(run: &[char]) -> Option<usize> {
    for i in 0..run.len() {
        if explicit_prefix_at(run, i) && (i == 0 || !is_path_char(run[i - 1])) {
            return Some(i);
        }
    }
    let first_plausible = run
        .iter()
        .position(|&c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-'))?;
    let decorations_only = run[..first_plausible]
        .iter()
        .all(|&c| matches!(c, '(' | '[' | '{' | '`' | '='));
    decorations_only.then_some(first_plausible)
}

/// Split a trailing `:LINE` or `:LINE:COL` suffix (digits only) off `token`.
/// Returns the byte-length-in-chars of the path portion (without the
/// suffix) plus the parsed line/column, or `(token.len(), None, None)` if no
/// such suffix is present.
fn split_line_col_suffix(token: &[char]) -> (usize, Option<u32>, Option<u32>) {
    // Walk backwards over up to two ":<digits>" groups.
    let mut groups = Vec::new();
    let mut end = token.len();
    for _ in 0..2 {
        let Some(colon_idx) = token[..end].iter().rposition(|&c| c == ':') else {
            break;
        };
        let digits = &token[colon_idx + 1..end];
        if digits.is_empty() || !digits.iter().all(|c| c.is_ascii_digit()) {
            break;
        }
        let value: String = digits.iter().collect();
        let Ok(n) = value.parse::<u32>() else {
            break;
        };
        groups.push(n);
        end = colon_idx;
    }
    match groups.len() {
        0 => (token.len(), None, None),
        1 => (end, Some(groups[0]), None),
        // Groups were collected back-to-front: the first pop is COL, the
        // second is LINE.
        _ => (end, Some(groups[1]), Some(groups[0])),
    }
}

/// Find the path run covering `column` in `row`, if any.
pub fn detect_path_at_column(row: &Row, column: u16) -> Option<PathMatch> {
    detect_path_at(std::slice::from_ref(row), 0, column)
}

/// Find the path run covering `column` of `rows[row]`, reading `rows` as one
/// soft-wrapped line.
pub fn detect_path_at<R: Borrow<Row>>(rows: &[R], row: usize, column: u16) -> Option<PathMatch> {
    path_in(&LineText::new(rows), rows, (row, column))
}

/// Find the path run covering `target` (`(row index, column)`) in `line`,
/// the text of `rows`.
pub(crate) fn path_in<R: Borrow<Row>>(
    line: &LineText,
    rows: &[R],
    target: (usize, u16),
) -> Option<PathMatch> {
    let chars = &line.chars;

    let mut i = 0;
    while i < chars.len() {
        // A quote opens a single token running to its matching close quote —
        // the shell-quoted spelling of a path with spaces
        // (`"/Users/me/My File.txt"`). The quotes themselves stay outside
        // the match.
        if matches!(chars[i], '"' | '\'')
            && let Some(close) = chars[i + 1..].iter().position(|&c| c == chars[i])
        {
            let start = i + 1;
            let end = start + close;
            i = end + 1;
            if let Some(m) = match_in_run(rows, line, target, start, end) {
                return Some(m);
            }
            continue;
        }
        if is_run_boundary(chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        while end < chars.len() {
            let c = chars[end];
            if is_run_boundary(c) {
                // A backslash-escaped whitespace continues the token — the
                // unquoted shell spelling (`/Users/me/My\ File.txt`).
                if c.is_whitespace() && chars[end - 1] == '\\' {
                    end += 1;
                    continue;
                }
                break;
            }
            end += 1;
        }
        i = end;
        if let Some(m) = match_in_run(rows, line, target, start, end) {
            return Some(m);
        }
    }

    None
}

/// Run the path-candidate machinery over `chars[start..end]` (one token) and
/// return its match if it covers `target` (`(row index, column)`).
fn match_in_run<R: Borrow<Row>>(
    rows: &[R],
    text: &LineText,
    target: (usize, u16),
    start: usize,
    end: usize,
) -> Option<PathMatch> {
    let raw_run = &text.chars[start..end];
    // A `://` anywhere in the run marks it as a URL, not a path — URL
    // detection already runs first in the hover-link caller, and the
    // candidate-start scan below would otherwise latch onto the `//`.
    if raw_run.windows(3).any(|w| w == [':', '/', '/']) {
        return None;
    }
    let offset = candidate_start(raw_run)?;
    let candidate = &raw_run[offset..];
    let trimmed_len = trim_trailing_punctuation(candidate);
    let run = &candidate[..trimmed_len];
    if !looks_like_path(run) {
        return None;
    }

    let (path_len, line, col) = split_line_col_suffix(run);
    let path = unescape_whitespace(&run[..path_len]);
    if path.is_empty() || !looks_like_path(&run[..path_len]) {
        return None;
    }

    let match_start = start + offset;
    let match_end = match_start + trimmed_len; // includes suffix, excludes trimmed punctuation
    let (first, last) = text.span(rows, match_start, match_end);
    (first <= target && target <= last && !text.cuts(start, end)).then_some(PathMatch {
        start_row: first.0,
        start_x: first.1,
        end_row: last.0,
        end_x: last.1,
        path,
        line,
        column: col,
    })
}

/// Collapse `\<whitespace>` escapes into the bare whitespace so the escaped
/// on-screen spelling probes (and opens) the real filename.
fn unescape_whitespace(run: &[char]) -> String {
    let mut out = String::with_capacity(run.len());
    let mut iter = run.iter().peekable();
    while let Some(&c) = iter.next() {
        if c == '\\' && iter.peek().is_some_and(|next| next.is_whitespace()) {
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Cell;
    use noa_core::CellAttrs;

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
    fn detects_absolute_path() {
        let row = row_from_str("open /usr/local/bin/rustc now");
        let m = detect_path_at_column(&row, 8).expect("column inside the path should match");
        assert_eq!(m.path, "/usr/local/bin/rustc");
        assert_eq!(m.line, None);
        assert_eq!(m.column, None);
    }

    #[test]
    fn detects_home_relative_path() {
        let row = row_from_str("see ~/notes/todo.md please");
        let m = detect_path_at_column(&row, 5).unwrap();
        assert_eq!(m.path, "~/notes/todo.md");
    }

    #[test]
    fn detects_dot_and_dotdot_relative_paths() {
        let row = row_from_str("./scripts/build.sh ../other/file.rs");
        let m1 = detect_path_at_column(&row, 0).unwrap();
        assert_eq!(m1.path, "./scripts/build.sh");
        let m2 = detect_path_at_column(&row, 20).unwrap();
        assert_eq!(m2.path, "../other/file.rs");
    }

    #[test]
    fn detects_bare_relative_path_with_interior_slash() {
        let row = row_from_str("error in src/main.rs today");
        let m = detect_path_at_column(&row, 9).unwrap();
        assert_eq!(m.path, "src/main.rs");
    }

    #[test]
    fn parses_trailing_line_suffix() {
        let row = row_from_str("crates/foo/lib.rs:42 failed");
        let m = detect_path_at_column(&row, 0).unwrap();
        assert_eq!(m.path, "crates/foo/lib.rs");
        assert_eq!(m.line, Some(42));
        assert_eq!(m.column, None);
        // The suffix is included in the highlight range.
        assert_eq!(m.end_x as usize, "crates/foo/lib.rs:42".chars().count() - 1);
    }

    #[test]
    fn parses_trailing_line_and_column_suffix() {
        let row = row_from_str("crates/foo/lib.rs:42:7 failed");
        let m = detect_path_at_column(&row, 0).unwrap();
        assert_eq!(m.path, "crates/foo/lib.rs");
        assert_eq!(m.line, Some(42));
        assert_eq!(m.column, Some(7));
    }

    #[test]
    fn trims_trailing_punctuation_after_line_col_suffix() {
        let row = row_from_str("see src/main.rs:42:7: for details");
        let m = detect_path_at_column(&row, 4).unwrap();
        assert_eq!(m.path, "src/main.rs");
        assert_eq!(m.line, Some(42));
        assert_eq!(m.column, Some(7));
    }

    #[test]
    fn maps_columns_correctly_past_a_wide_cjk_run() {
        // "見" occupies columns 0-1 (WIDE + WIDE_SPACER); a space is at
        // column 2; the path starts at column 3.
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
        for (i, ch) in " /tmp/x.txt".chars().enumerate() {
            row.cells[2 + i] = Cell {
                ch,
                ..Cell::default()
            };
        }

        assert_eq!(
            detect_path_at_column(&row, 0),
            None,
            "the CJK cell itself is not part of the path"
        );
        let m = detect_path_at_column(&row, 3).expect("column 3 is the path's '/'");
        assert_eq!(m.path, "/tmp/x.txt");
        assert_eq!(m.start_x, 3);
    }

    #[test]
    fn starts_after_leading_decoration_paren() {
        let row = row_from_str("moved (/tmp/file) away");
        let m = detect_path_at_column(&row, 8).unwrap();
        assert_eq!(m.path, "/tmp/file");
        assert_eq!(row.cells[m.start_x as usize].ch, '/');
        assert_eq!(row.cells[m.end_x as usize].ch, 'e');
    }

    #[test]
    fn starts_after_an_equals_sign() {
        let row = row_from_str("config path=/tmp/file loaded");
        let m = detect_path_at_column(&row, 14).unwrap();
        assert_eq!(m.path, "/tmp/file");
        assert_eq!(row.cells[m.start_x as usize].ch, '/');
    }

    #[test]
    fn starts_home_prefix_after_flag_equals() {
        let row = row_from_str("run --out=~/dir/result.txt now");
        let m = detect_path_at_column(&row, 12).unwrap();
        assert_eq!(m.path, "~/dir/result.txt");
    }

    #[test]
    fn strips_decoration_before_a_bare_relative_path() {
        let row = row_from_str("see (src/main.rs) here");
        let m = detect_path_at_column(&row, 6).unwrap();
        assert_eq!(m.path, "src/main.rs");
        assert_eq!(row.cells[m.start_x as usize].ch, 's');
    }

    #[test]
    fn trims_closing_bracket_matching_a_stripped_opening_one() {
        let row = row_from_str("entry [src/main.rs] listed");
        let m = detect_path_at_column(&row, 8).unwrap();
        assert_eq!(m.path, "src/main.rs");
        assert_eq!(row.cells[m.start_x as usize].ch, 's');
        assert_eq!(row.cells[m.end_x as usize].ch, 's');
    }

    #[test]
    fn trims_closing_backtick_matching_a_stripped_opening_one() {
        let row = row_from_str("run `~/notes/todo.md` now");
        let m = detect_path_at_column(&row, 6).unwrap();
        assert_eq!(m.path, "~/notes/todo.md");
    }

    #[test]
    fn no_match_on_the_decoration_cell_itself() {
        let row = row_from_str("moved (/tmp/file) away");
        assert_eq!(detect_path_at_column(&row, 6), None);
    }

    #[test]
    fn includes_the_spacer_column_of_a_trailing_wide_char() {
        // "/tmp/資" — `資` is WIDE at column 5 with its spacer at column 6.
        let mut row = Row::new(20);
        for (i, ch) in "/tmp/".chars().enumerate() {
            row.cells[i] = Cell {
                ch,
                ..Cell::default()
            };
        }
        row.cells[5] = Cell {
            ch: '資',
            attrs: CellAttrs::WIDE,
            ..Cell::default()
        };
        row.cells[6] = Cell {
            ch: ' ',
            attrs: CellAttrs::WIDE_SPACER,
            ..Cell::default()
        };

        let m = detect_path_at_column(&row, 6)
            .expect("the wide char's spacer column is part of the link");
        assert_eq!(m.path, "/tmp/資");
        assert_eq!(m.start_x, 0);
        assert_eq!(m.end_x, 6);
    }

    #[test]
    fn keeps_a_trailing_parent_directory_component() {
        let row = row_from_str("cd /tmp/project/.. now");
        let m = detect_path_at_column(&row, 5).unwrap();
        assert_eq!(m.path, "/tmp/project/..");
    }

    #[test]
    fn keeps_a_trailing_current_directory_component() {
        let row = row_from_str("ls target/debug/. done");
        let m = detect_path_at_column(&row, 4).unwrap();
        assert_eq!(m.path, "target/debug/.");
    }

    #[test]
    fn trims_a_sentence_period_after_a_parent_directory_component() {
        let row = row_from_str("go to /tmp/project/... then");
        let m = detect_path_at_column(&row, 8).unwrap();
        assert_eq!(m.path, "/tmp/project/..");
    }

    #[test]
    fn detects_a_double_quoted_path_with_spaces() {
        let row = row_from_str("open \"/Users/me/My File.txt\" now");
        let m = detect_path_at_column(&row, 20).unwrap();
        assert_eq!(m.path, "/Users/me/My File.txt");
        // The quotes stay outside the underline.
        assert_eq!(row.cells[m.start_x as usize].ch, '/');
        assert_eq!(row.cells[m.end_x as usize].ch, 't');
    }

    #[test]
    fn detects_a_single_quoted_relative_path_with_spaces() {
        let row = row_from_str("cat 'src/my file.rs' done");
        let m = detect_path_at_column(&row, 10).unwrap();
        assert_eq!(m.path, "src/my file.rs");
    }

    #[test]
    fn detects_a_backslash_escaped_space_and_unescapes_it() {
        let row = row_from_str("cat /Users/me/My\\ File.txt done");
        let m = detect_path_at_column(&row, 18).unwrap();
        assert_eq!(m.path, "/Users/me/My File.txt");
        // The on-screen escape (backslash included) is what underlines.
        assert_eq!(row.cells[m.start_x as usize].ch, '/');
        assert_eq!(row.cells[m.end_x as usize].ch, 't');
    }

    #[test]
    fn unmatched_quote_still_detects_the_following_path() {
        let row = row_from_str("say \"oops /tmp/file here");
        let m = detect_path_at_column(&row, 12).unwrap();
        assert_eq!(m.path, "/tmp/file");
    }

    #[test]
    fn no_match_on_plain_words() {
        let row = row_from_str("no paths on this row at all");
        assert_eq!(detect_path_at_column(&row, 5), None);
    }

    #[test]
    fn no_match_on_urls() {
        let row = row_from_str("see https://example.com/path here");
        assert_eq!(detect_path_at_column(&row, 10), None);
    }

    #[test]
    fn detects_a_path_soft_wrapped_across_rows() {
        let mut terminal = crate::Terminal::new(noa_core::GridSize::new(20, 4));
        noa_vt::Stream::new().feed(b"at src/very/long/dir/name/file.rs:12:3 ok", &mut terminal);
        let rows = &terminal.primary.grid;
        assert!(rows[0].wrapped && rows[1].wrapped);
        let rows = [&rows[0], &rows[1], &rows[2]];

        for (row, column) in [(0, 3), (0, 19), (1, 0), (1, 17)] {
            let found = detect_path_at(&rows, row, column).expect("inside the wrapped path");
            assert_eq!(found.path, "src/very/long/dir/name/file.rs");
            assert_eq!((found.line, found.column), (Some(12), Some(3)));
            assert_eq!((found.start_row, found.start_x), (0, 3));
            assert_eq!((found.end_row, found.end_x), (1, 17));
        }
        assert_eq!(detect_path_at(&rows, 1, 18), None);
        assert_eq!(detect_path_at(&rows, 2, 0), None);
    }
}
