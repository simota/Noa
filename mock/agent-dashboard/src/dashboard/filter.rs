//! The `/` text filter (AC2): which workers the panels show.

use crate::fleet::Worker;

/// The query typed into the header's field, kept across rebuilds (like the
/// feed) so a rebuilt field shows the same text with the caret where it was.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Filter {
    pub(super) query: String,
    /// The caret as a byte index into `query`.
    pub(super) caret: usize,
}

impl Filter {
    pub(super) fn active(&self) -> bool {
        !self.query.trim().is_empty()
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
