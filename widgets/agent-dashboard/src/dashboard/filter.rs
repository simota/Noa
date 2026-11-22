//! The `/` text filter (AC2): which workers the panels show.

use noa_gpui_core::Event;

use crate::fleet::{Worker, one_line};

/// The query typed into the header's field, kept across rebuilds (like the
/// feed) so a rebuilt field shows the same text with the caret where it was.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Filter {
    pub(super) query: String,
    /// The caret as a byte index into `query`.
    pub(super) caret: usize,
}

impl Filter {
    /// The filter is on once anything is typed, blanks too: the header
    /// shows the match count, an empty panel says "該当なし" and the graph
    /// mutes what does not match, all by this one rule. (A query of blanks
    /// matches every worker, so it hides no one.)
    pub(super) fn active(&self) -> bool {
        !self.query.is_empty()
    }

    pub(super) fn matches(&self, worker: &Worker) -> bool {
        matches(worker, &self.query)
    }
}

/// Every whitespace-separated term of `query` (ASCII space and U+3000 both
/// separate), lowercased, is part of the worker's label, agent, model,
/// branch or task. An empty query matches every worker.
pub(super) fn matches(worker: &Worker, query: &str) -> bool {
    let fields = [
        &worker.label,
        &worker.kind,
        &worker.model,
        &worker.branch,
        &worker.task,
    ]
    .map(|field| field.to_lowercase());
    query
        .split(char::is_whitespace)
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .all(|term| fields.iter().any(|field| field.contains(&term)))
}

/// Typed text and an IME's composition as the filter field is given them:
/// on one line ([`one_line`]), with the composition's cursor moved to match.
/// The field drops control characters itself but keeps a line or
/// paragraph separator, and the shaper panics on a paragraph separator
/// next to right-to-left text. A paste does not come as an event (the
/// field reads the clipboard): `on_one_line` puts it right after.
pub(super) fn one_line_input(event: Event) -> Event {
    match event {
        Event::Text(text) => Event::Text(one_line(&text)),
        Event::Composition { text, cursor } => {
            // A byte offset into `text` to the same place in its one line.
            let at = |i: usize| text.get(..i).map_or(i, |head| one_line(head).len());
            let cursor = cursor.map(|(start, end)| (at(start), at(end)));
            Event::Composition {
                text: one_line(&text),
                cursor,
            }
        }
        event => event,
    }
}

/// `query` with the caret at byte `caret`, on one line: the text and where
/// the caret is in it. `None` when it already is.
pub(super) fn on_one_line(query: &str, caret: usize) -> Option<(String, usize)> {
    let line = one_line(query);
    if line == query {
        return None;
    }
    let caret = query
        .get(..caret)
        .map_or(line.len(), |head| one_line(head).len());
    Some((line, caret))
}
