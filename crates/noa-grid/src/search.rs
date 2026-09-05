//! Search state over the active screen's combined scrollback + live rows.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::cell::Row;
use crate::selection::SelectionPoint;
use noa_core::CellAttrs;

/// Searchable immutable history and live rows, safe to scan without a terminal lock.
pub struct SearchSnapshot {
    pub(crate) history: crate::scrollback::HistorySnapshot,
    pub(crate) live: Vec<Row>,
    pub(crate) history_len: usize,
    pub(crate) rows_evicted: usize,
    pub(crate) coordinate_generation: u64,
    pub(crate) cols: u16,
    pub(crate) anchor: SearchAnchor,
}

impl SearchSnapshot {
    pub fn find_matches(
        &self,
        query: &str,
        mut cancelled: impl FnMut() -> bool,
    ) -> Option<Vec<SearchMatch>> {
        let Some(mut search) = RowSearch::new(query) else {
            return Some(Vec::new());
        };
        let mut matches = Vec::new();
        if !self.history.for_each_row(|y, row| {
            if cancelled() {
                return false;
            }
            search.append_matches(y, row, &mut matches);
            true
        }) {
            return None;
        }
        for (i, row) in self.live.iter().enumerate() {
            if cancelled() {
                return None;
            }
            search.append_matches(self.history_len + i, row, &mut matches);
        }
        Some(matches)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SearchMatch {
    pub start: SelectionPoint,
    pub end: SelectionPoint,
}

/// Which match to activate when a (re)query lands ([`SearchState::set_query`]).
/// Matches are ordered by storage position, so "nearest" is resolved with a
/// directional preference and falls through to the other side only when no
/// match exists on the preferred one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchAnchor {
    /// Activate the nearest match starting at or before the point — a fresh
    /// query anchors here at the viewport bottom, so the bottom-most visible
    /// match wins rather than the oldest scrollback row. Falls through to the
    /// first match when every match lies after the point.
    Backward(SelectionPoint),
    /// Activate the nearest match starting at or after the point — an
    /// incremental query edit anchors here at the previous active match, so
    /// extending the query keeps the active match in place instead of
    /// resetting to the top. Falls through to the last match when every match
    /// lies before the point.
    Forward(SelectionPoint),
}

impl SearchMatch {
    pub fn contains(&self, point: SelectionPoint) -> bool {
        (self.start.y, self.start.x) <= (point.y, point.x)
            && (point.y, point.x) <= (self.end.y, self.end.x)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SearchState {
    query: String,
    matches: Arc<[SearchMatch]>,
    active: Option<usize>,
}

impl SearchState {
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn matches(&self) -> &[SearchMatch] {
        &self.matches[..]
    }

    /// Matches are stored in row/column order by the screen search. Limit
    /// rendering and hit testing to the requested row, independent of the
    /// number of matches elsewhere in scrollback.
    pub fn matches_on_row(&self, row: usize) -> &[SearchMatch] {
        let start = self.matches.partition_point(|m| m.end.y < row);
        let matches = &self.matches[start..];
        let end = matches.partition_point(|m| m.start.y <= row);
        &matches[..end]
    }

    pub fn active_match(&self) -> Option<SearchMatch> {
        self.active.and_then(|idx| self.matches.get(idx).copied())
    }

    /// The 0-based index of the active match into [`SearchState::matches`],
    /// or `None` when there is no query or no matches — the search prompt
    /// overlay derives its `i/n` counter from this plus `matches().len()`.
    pub fn active_index(&self) -> Option<usize> {
        self.active
    }

    pub fn set_query(&mut self, query: String, matches: Vec<SearchMatch>, anchor: SearchAnchor) {
        self.set_shared_query(query, Arc::from(matches.into_boxed_slice()), anchor);
    }

    pub(crate) fn set_shared_query(
        &mut self,
        query: String,
        matches: Arc<[SearchMatch]>,
        anchor: SearchAnchor,
    ) {
        self.query = query;
        self.matches = matches;
        self.active = self.anchored_index(anchor);
    }

    fn anchored_index(&self, anchor: SearchAnchor) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        let key = |m: &SearchMatch| (m.start.y, m.start.x);
        Some(match anchor {
            SearchAnchor::Backward(point) => self
                .matches
                .partition_point(|m| key(m) <= (point.y, point.x))
                .saturating_sub(1),
            SearchAnchor::Forward(point) => self
                .matches
                .partition_point(|m| key(m) < (point.y, point.x))
                .min(self.matches.len() - 1),
        })
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn next_match(&mut self) -> Option<SearchMatch> {
        if self.matches.is_empty() {
            self.active = None;
            return None;
        }

        let next = self.active.map_or(0, |idx| (idx + 1) % self.matches.len());
        self.active = Some(next);
        self.matches.get(next).copied()
    }

    pub fn previous_match(&mut self) -> Option<SearchMatch> {
        if self.matches.is_empty() {
            self.active = None;
            return None;
        }

        let previous = self.active.map_or(0, |idx| {
            if idx == 0 {
                self.matches.len() - 1
            } else {
                idx - 1
            }
        });
        self.active = Some(previous);
        self.matches.get(previous).copied()
    }

    pub fn contains(&self, point: SelectionPoint) -> bool {
        self.matches_on_row(point.y)
            .iter()
            .any(|m| m.contains(point))
    }

    pub fn contains_active(&self, point: SelectionPoint) -> bool {
        self.active_match().is_some_and(|m| m.contains(point))
    }
}

/// Scratch space shared by every row of a query, including materialized
/// scrollback. Mapping UTF-8 bytes to columns avoids rescanning the text
/// prefix for every hit and preserves wide/combining-cell coordinates.
pub(crate) struct RowSearch<'a> {
    query: &'a str,
    text: String,
    byte_columns: Vec<u16>,
    prefix: Vec<usize>,
    matched: usize,
    positions: VecDeque<SelectionPoint>,
    continuation: Option<usize>,
}

impl<'a> RowSearch<'a> {
    pub(crate) fn new(query: &'a str) -> Option<Self> {
        if query.is_empty() {
            return None;
        }
        let mut prefix = vec![0; query.len()];
        for i in 1..query.len() {
            let mut n = prefix[i - 1];
            while n > 0 && query.as_bytes()[i] != query.as_bytes()[n] {
                n = prefix[n - 1];
            }
            if query.as_bytes()[i] == query.as_bytes()[n] {
                n += 1;
            }
            prefix[i] = n;
        }
        Some(Self {
            query,
            text: String::new(),
            byte_columns: Vec::new(),
            prefix,
            matched: 0,
            positions: VecDeque::new(),
            continuation: None,
        })
    }

    pub(crate) fn append_matches(
        &mut self,
        storage_y: usize,
        row: &Row,
        matches: &mut Vec<SearchMatch>,
    ) {
        self.text.clear();
        self.byte_columns.clear();
        let continuing = self.continuation == Some(storage_y);
        if !continuing {
            self.matched = 0;
            self.positions.clear();
        }
        self.continuation = row.wrapped.then(|| storage_y.saturating_add(1));
        for (x, cell) in row.cells.iter().enumerate() {
            if cell.attrs.contains(CellAttrs::WIDE_SPACER) {
                continue;
            }
            cell.push_text_to(&mut self.text);
            self.byte_columns.resize(self.text.len(), x as u16);
        }

        if !continuing && !row.wrapped {
            for (start, _) in self.text.match_indices(self.query) {
                matches.push(SearchMatch {
                    start: SelectionPoint::new(self.byte_columns[start], storage_y),
                    end: SelectionPoint::new(
                        self.byte_columns[start + self.query.len() - 1],
                        storage_y,
                    ),
                });
            }
            return;
        }

        // Streaming KMP keeps only one query's worth of coordinates, even for
        // a logical line spanning the entire scrollback. Reset after a hit to
        // preserve str::match_indices' non-overlapping semantics.
        for (i, byte) in self.text.bytes().enumerate() {
            let point = SelectionPoint::new(self.byte_columns[i], storage_y);
            if self.positions.len() == self.query.len() {
                self.positions.pop_front();
            }
            self.positions.push_back(point);
            while self.matched > 0 && byte != self.query.as_bytes()[self.matched] {
                self.matched = self.prefix[self.matched - 1];
            }
            if byte == self.query.as_bytes()[self.matched] {
                self.matched += 1;
            }
            if self.matched == self.query.len() {
                matches.push(SearchMatch {
                    start: *self.positions.front().unwrap(),
                    end: point,
                });
                self.matched = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_survive_history_packing_eviction_and_live_edits() {
        let mut terminal = crate::Terminal::new(noa_core::GridSize::new(80, 8));
        let mut stream = noa_vt::Stream::new();
        stream.feed("needle\r\n".repeat(2000).as_bytes(), &mut terminal);
        let snapshot = terminal.active().search_snapshot();
        let before = snapshot.find_matches("needle", || false).unwrap();
        assert_eq!(before.len(), 2000);
        stream.feed("more\r\n".repeat(2000).as_bytes(), &mut terminal);
        stream.feed(b"\x1b[3Jchanged", &mut terminal);
        assert_eq!(snapshot.find_matches("needle", || false).unwrap(), before);
        assert!(!terminal.apply_search_snapshot(&snapshot, "needle".into(), Arc::from(before)));
        assert!(snapshot.find_matches("needle", || true).is_none());
    }

    #[test]
    fn search_crosses_soft_wraps_and_highlights_each_physical_row() {
        let mut terminal = crate::Terminal::new(noa_core::GridSize::new(4, 3));
        noa_vt::Stream::new().feed(b"abcdef", &mut terminal);
        terminal.set_search_query("cde");
        let state = &terminal.active().search;
        assert_eq!(
            state.matches(),
            &[SearchMatch {
                start: SelectionPoint::new(2, 0),
                end: SelectionPoint::new(0, 1),
            }]
        );
        for p in [(2, 0), (3, 0), (0, 1)] {
            assert!(state.contains(SelectionPoint::new(p.0, p.1)));
        }
        for p in [(1, 0), (1, 1)] {
            assert!(!state.contains(SelectionPoint::new(p.0, p.1)));
        }
        assert_eq!(state.matches_on_row(1).len(), 1);
    }

    #[test]
    fn search_joins_scrollback_to_live_grid_but_not_hard_newlines() {
        let mut terminal = crate::Terminal::new(noa_core::GridSize::new(4, 1));
        noa_vt::Stream::new().feed(b"abcdef", &mut terminal);
        terminal.set_search_query("cde");
        assert_eq!(terminal.active().search.matches().len(), 1);
        let mut terminal = crate::Terminal::new(noa_core::GridSize::new(4, 3));
        noa_vt::Stream::new().feed(b"abcd\r\nef", &mut terminal);
        terminal.set_search_query("cde");
        assert!(terminal.active().search.matches().is_empty());
    }

    #[test]
    fn search_preserves_wide_combining_and_nonoverlapping_matches_across_wraps() {
        let mut terminal = crate::Terminal::new(noa_core::GridSize::new(4, 4));
        noa_vt::Stream::new().feed("ab日e\u{301}aaa".as_bytes(), &mut terminal);
        terminal.set_search_query("日e\u{301}");
        assert_eq!(
            terminal.active().search.matches(),
            &[SearchMatch {
                start: SelectionPoint::new(2, 0),
                end: SelectionPoint::new(0, 1),
            }]
        );
        terminal.set_search_query("aa");
        assert_eq!(terminal.active().search.matches().len(), 1);
    }

    // Preserve the scalar-based mapping as an independent reference for the
    // byte-column implementation, including non-overlapping match semantics.
    fn append_scalar_matches(
        query: &str,
        chars: usize,
        y: usize,
        row: &Row,
        matches: &mut Vec<SearchMatch>,
    ) {
        let mut text = String::new();
        let mut columns = Vec::new();
        for (x, cell) in row.cells.iter().enumerate() {
            if !cell.attrs.contains(CellAttrs::WIDE_SPACER) {
                cell.push_text_to(&mut text);
                columns.extend(std::iter::repeat_n(x as u16, cell.text_chars().count()));
            }
        }
        for (start, _) in text.match_indices(query) {
            let scalar = text[..start].chars().count();
            matches.push(SearchMatch {
                start: SelectionPoint::new(columns[scalar], y),
                end: SelectionPoint::new(columns[scalar + chars - 1], y),
            });
        }
    }

    #[test]
    fn row_search_matches_scalar_reference_with_unicode_and_reused_buffers() {
        let mut terminal = crate::Terminal::new(noa_core::GridSize::new(40, 4));
        noa_vt::Stream::new().feed(
            "aaaa 日本語 e\u{301} 👩\u{200d}💻\r\nx\r\n日本 aaaa e\u{301}".as_bytes(),
            &mut terminal,
        );
        for query in [
            "a",
            "aa",
            "aaa",
            "日本",
            "本語",
            "e\u{301}",
            "\u{301}",
            "👩\u{200d}💻",
            " ",
            "absent",
        ] {
            let mut search = RowSearch::new(query).unwrap();
            let mut actual = Vec::new();
            let mut expected = Vec::new();
            for (y, row) in terminal.primary.grid.iter().enumerate() {
                search.append_matches(y, row, &mut actual);
                append_scalar_matches(query, query.chars().count(), y, row, &mut expected);
            }
            assert_eq!(actual, expected, "query {query:?}");
        }
        assert!(RowSearch::new("").is_none());
    }

    #[test]
    fn matches_on_row_equals_a_full_scan_at_boundaries_and_gaps() {
        let mut state = SearchState::default();
        assert!(state.matches_on_row(0).is_empty());
        let matches = vec![
            SearchMatch {
                start: SelectionPoint::new(1, 2),
                end: SelectionPoint::new(3, 2),
            },
            SearchMatch {
                start: SelectionPoint::new(6, 2),
                end: SelectionPoint::new(7, 2),
            },
            SearchMatch {
                start: SelectionPoint::new(0, 4),
                end: SelectionPoint::new(1, 4),
            },
            SearchMatch {
                start: SelectionPoint::new(0, usize::MAX),
                end: SelectionPoint::new(0, usize::MAX),
            },
        ];
        state.set_query("x".into(), matches, top_anchor());
        for y in [0, 1, 2, 3, 4, 5, usize::MAX] {
            let reference: Vec<_> = state
                .matches()
                .iter()
                .copied()
                .filter(|m| m.start.y == y)
                .collect();
            assert_eq!(state.matches_on_row(y), reference);
            for x in 0..9 {
                let point = SelectionPoint::new(x, y);
                assert_eq!(
                    state.contains(point),
                    state.matches().iter().any(|m| m.contains(point))
                );
            }
        }
    }

    #[test]
    #[ignore = "release-mode search performance comparison"]
    fn search_performance_probe() {
        use std::hint::black_box;
        use std::time::Instant;

        let mut terminal = crate::Terminal::new(noa_core::GridSize::new(200, 1));
        noa_vt::Stream::new().feed("日本語 abc e\u{301} ".repeat(12).as_bytes(), &mut terminal);
        let row = &terminal.primary.grid[0];
        let iterations = 20_000;
        for query in ["a", "日本", " ", "absent"] {
            let mut matches = Vec::new();
            let chars = query.chars().count();
            let start = Instant::now();
            for y in 0..iterations {
                matches.clear();
                append_scalar_matches(query, chars, y, black_box(row), &mut matches);
                black_box(&matches);
            }
            let reference = start.elapsed();
            let mut search = RowSearch::new(query).unwrap();
            let start = Instant::now();
            for y in 0..iterations {
                matches.clear();
                search.append_matches(y, black_box(row), &mut matches);
                black_box(&matches);
            }
            eprintln!(
                "search query={query:?} rows={iterations}: scalar={reference:?} scratch={:?}",
                start.elapsed()
            );
        }

        let mut state = SearchState::default();
        state.set_query(
            "x".into(),
            matches_at(&(0..100_000).collect::<Vec<_>>()),
            top_anchor(),
        );
        let start = Instant::now();
        for y in 49_980..50_020 {
            for _ in 0..100 {
                let row = black_box(y);
                black_box(
                    black_box(state.matches())
                        .iter()
                        .filter(|m| m.start.y == row)
                        .count(),
                );
            }
        }
        let reference = start.elapsed();
        let start = Instant::now();
        for y in 49_980..50_020 {
            for _ in 0..100 {
                black_box(black_box(&state).matches_on_row(black_box(y)));
            }
        }
        eprintln!(
            "highlight 100k matches, 40 rows x 100: scan={reference:?} indexed={:?}",
            start.elapsed()
        );
    }

    fn matches_at(ys: &[usize]) -> Vec<SearchMatch> {
        ys.iter()
            .map(|&y| SearchMatch {
                start: SelectionPoint::new(0, y),
                end: SelectionPoint::new(0, y),
            })
            .collect()
    }

    /// Anchor at the very top of storage — activates the first match, i.e.
    /// the pre-anchor behavior, for tests that only exercise navigation.
    fn top_anchor() -> SearchAnchor {
        SearchAnchor::Backward(SelectionPoint::new(0, 0))
    }

    #[test]
    fn active_index_tracks_the_active_match_through_navigation() {
        let mut state = SearchState::default();
        assert_eq!(state.active_index(), None, "no query yet");

        state.set_query("x".to_string(), matches_at(&[0, 3, 7]), top_anchor());
        assert_eq!(state.active_index(), Some(0), "first match auto-activates");

        state.next_match();
        assert_eq!(state.active_index(), Some(1));

        state.next_match();
        assert_eq!(state.active_index(), Some(2));

        state.next_match();
        assert_eq!(
            state.active_index(),
            Some(0),
            "wraps back to the first match"
        );

        state.previous_match();
        assert_eq!(state.active_index(), Some(2), "wraps backward too");

        state.clear();
        assert_eq!(state.active_index(), None);
    }

    #[test]
    fn active_index_is_none_when_query_has_no_matches() {
        let mut state = SearchState::default();
        state.set_query("x".to_string(), Vec::new(), top_anchor());
        assert_eq!(state.active_index(), None);
    }

    #[test]
    fn backward_anchor_activates_the_nearest_match_at_or_before_the_point() {
        let mut state = SearchState::default();

        state.set_query(
            "x".to_string(),
            matches_at(&[2, 5, 9]),
            SearchAnchor::Backward(SelectionPoint::new(0, 6)),
        );
        assert_eq!(
            state.active_index(),
            Some(1),
            "y=5 is the nearest at-or-before y=6"
        );

        state.set_query(
            "x".to_string(),
            matches_at(&[2, 5, 9]),
            SearchAnchor::Backward(SelectionPoint::new(0, 5)),
        );
        assert_eq!(
            state.active_index(),
            Some(1),
            "an exact hit counts as at-or-before"
        );

        state.set_query(
            "x".to_string(),
            matches_at(&[2, 5, 9]),
            SearchAnchor::Backward(SelectionPoint::new(0, 1)),
        );
        assert_eq!(
            state.active_index(),
            Some(0),
            "every match after the anchor falls through to the first"
        );
    }

    #[test]
    fn forward_anchor_activates_the_nearest_match_at_or_after_the_point() {
        let mut state = SearchState::default();

        state.set_query(
            "x".to_string(),
            matches_at(&[2, 5, 9]),
            SearchAnchor::Forward(SelectionPoint::new(0, 5)),
        );
        assert_eq!(state.active_index(), Some(1), "an exact hit stays put");

        state.set_query(
            "x".to_string(),
            matches_at(&[2, 5, 9]),
            SearchAnchor::Forward(SelectionPoint::new(0, 6)),
        );
        assert_eq!(
            state.active_index(),
            Some(2),
            "y=9 is the nearest at-or-after y=6"
        );

        state.set_query(
            "x".to_string(),
            matches_at(&[2, 5, 9]),
            SearchAnchor::Forward(SelectionPoint::new(0, 10)),
        );
        assert_eq!(
            state.active_index(),
            Some(2),
            "every match before the anchor falls through to the last"
        );
    }

    #[test]
    fn anchors_break_same_row_ties_on_the_column() {
        let mut state = SearchState::default();
        let matches = vec![
            SearchMatch {
                start: SelectionPoint::new(2, 4),
                end: SelectionPoint::new(3, 4),
            },
            SearchMatch {
                start: SelectionPoint::new(8, 4),
                end: SelectionPoint::new(9, 4),
            },
        ];

        state.set_query(
            "x".to_string(),
            matches.clone(),
            SearchAnchor::Backward(SelectionPoint::new(5, 4)),
        );
        assert_eq!(state.active_index(), Some(0));

        state.set_query(
            "x".to_string(),
            matches,
            SearchAnchor::Forward(SelectionPoint::new(5, 4)),
        );
        assert_eq!(state.active_index(), Some(1));
    }
}
