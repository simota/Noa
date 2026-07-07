//! Search state over the active screen's combined scrollback + live rows.

use std::sync::Arc;

use crate::cell::Row;
use crate::selection::SelectionPoint;
use noa_core::CellAttrs;

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
        self.start.y == point.y
            && self.end.y == point.y
            && self.start.x <= point.x
            && point.x <= self.end.x
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
        self.query = query;
        self.matches = Arc::from(matches.into_boxed_slice());
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
        self.matches.iter().any(|m| m.contains(point))
    }

    pub fn contains_active(&self, point: SelectionPoint) -> bool {
        self.active_match().is_some_and(|m| m.contains(point))
    }
}

/// Number of scalars in `query`, or `None` if the query can never match
/// (empty). Callers hoist this out of their per-row loop.
pub(crate) fn needle_len(query: &str) -> Option<usize> {
    match query.chars().count() {
        0 => None,
        n => Some(n),
    }
}

/// Append every match of `query` within a single row `row` at storage index
/// `storage_y` to `matches`. `needle_chars` is `query.chars().count()` (from
/// [`needle_len`]), hoisted so a full-scrollback scan computes it once. The
/// unit both the live grid and the paged scrollback feed rows through, one at a
/// time, so neither storage needs to hand out a shared iterator.
pub(crate) fn append_row_matches(
    query: &str,
    needle_chars: usize,
    storage_y: usize,
    row: &Row,
    matches: &mut Vec<SearchMatch>,
) {
    let mut text = String::new();
    let mut cells = Vec::new();
    for (x, cell) in row.cells.iter().enumerate() {
        if cell.attrs.contains(CellAttrs::WIDE_SPACER) {
            continue;
        }
        cell.push_text_to(&mut text);
        cells.extend(std::iter::repeat_n(x as u16, cell.text_chars().count()));
    }

    for (byte_start, _) in text.match_indices(query) {
        let start_char = text[..byte_start].chars().count();
        let Some(end_char) = start_char.checked_add(needle_chars - 1) else {
            continue;
        };
        let (Some(&start_x), Some(&end_x)) = (cells.get(start_char), cells.get(end_char)) else {
            continue;
        };
        matches.push(SearchMatch {
            start: SelectionPoint::new(start_x, storage_y),
            end: SelectionPoint::new(end_x, storage_y),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
