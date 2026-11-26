//! Keyboard selection (AC1): the order J / K walk and how a selection
//! survives a change of what is shown.

use crate::fleet::Fleet;

use super::View;
use super::filter::Filter;
use super::list;

/// The order J / K walk: the rows of the scope shown, as `list::rows`
/// orders them.
pub(super) fn order(fleet: &Fleet, view: &View, filter: &Filter) -> Vec<u64> {
    let rows = list::rows(fleet, view.scope, filter);
    rows.into_iter().map(|w| w.pane).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Next,
    Prev,
}

/// The selection after one step through `order`: from nothing to the first
/// (Next) or last (Prev); no wrap at either end.
pub(super) fn step(order: &[u64], selected: Option<u64>, step: Step) -> Option<u64> {
    let at = selected.and_then(|pane| order.iter().position(|p| *p == pane));
    let index = match (at, step) {
        (None, Step::Next) => 0,
        (None, Step::Prev) => order.len().checked_sub(1)?,
        (Some(i), Step::Next) => (i + 1).min(order.len() - 1),
        (Some(i), Step::Prev) => i.saturating_sub(1),
    };
    order.get(index).copied()
}

/// The selection once the order changed from `old` to `new`: kept while its
/// pane is still there, else the pane now at its old index (clamped), or
/// nothing when `new` is empty.
pub(super) fn reselect(old: &[u64], new: &[u64], selected: Option<u64>) -> Option<u64> {
    let pane = selected?;
    if new.contains(&pane) {
        return Some(pane);
    }
    let at = old.iter().position(|p| *p == pane).unwrap_or(0);
    new.get(at.min(new.len().checked_sub(1)?)).copied()
}

/// The next worker that needs attention in `order` after `selected`
/// (wrapping around), or the first one: N jumps straight to the top of the
/// list's attention rows from anywhere, then cycles through them.
pub(super) fn next_waiting(fleet: &Fleet, order: &[u64], selected: Option<u64>) -> Option<u64> {
    let waiting: Vec<u64> = order
        .iter()
        .copied()
        .filter(|pane| fleet.worker(*pane).is_some_and(|w| w.needs_attention()))
        .collect();
    if waiting.is_empty() {
        return None;
    }
    match selected.and_then(|pane| waiting.iter().position(|p| *p == pane)) {
        Some(index) => Some(waiting[(index + 1) % waiting.len()]),
        None => Some(waiting[0]),
    }
}
