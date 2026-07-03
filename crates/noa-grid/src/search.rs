//! Search state over the active screen's combined scrollback + live rows.

use crate::cell::Row;
use crate::selection::SelectionPoint;
use noa_core::CellAttrs;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SearchMatch {
    pub start: SelectionPoint,
    pub end: SelectionPoint,
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
    matches: Vec<SearchMatch>,
    active: Option<usize>,
}

impl SearchState {
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn matches(&self) -> &[SearchMatch] {
        &self.matches
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

    pub fn set_query(&mut self, query: String, matches: Vec<SearchMatch>) {
        self.query = query;
        self.matches = matches;
        self.active = (!self.matches.is_empty()).then_some(0);
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

pub(crate) fn compute_matches<'a, I>(query: &str, rows: I) -> Vec<SearchMatch>
where
    I: IntoIterator<Item = (usize, &'a Row)>,
{
    if query.is_empty() {
        return Vec::new();
    }

    let needle_chars = query.chars().count();
    if needle_chars == 0 {
        return Vec::new();
    }

    let mut matches = Vec::new();
    for (storage_y, row) in rows {
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
            let (Some(&start_x), Some(&end_x)) = (cells.get(start_char), cells.get(end_char))
            else {
                continue;
            };
            matches.push(SearchMatch {
                start: SelectionPoint::new(start_x, storage_y),
                end: SelectionPoint::new(end_x, storage_y),
            });
        }
    }

    matches
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

    #[test]
    fn active_index_tracks_the_active_match_through_navigation() {
        let mut state = SearchState::default();
        assert_eq!(state.active_index(), None, "no query yet");

        state.set_query("x".to_string(), matches_at(&[0, 3, 7]));
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
        state.set_query("x".to_string(), Vec::new());
        assert_eq!(state.active_index(), None);
    }
}
