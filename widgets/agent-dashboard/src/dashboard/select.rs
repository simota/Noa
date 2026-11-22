//! Keyboard selection (AC1): the order J / K walk and how a selection
//! survives a change of what is shown.

use crate::fleet::{Fleet, State, Worker};

use super::View;
use super::filter::Filter;
use super::panel::Panel;
use super::queue::cards;

/// The order the NOW row shows workers in: the queue's cards in its sort,
/// then the running cards, then the done cards (each in `grouping.split`
/// order), of shown panels only, matching the filter only, each pane once.
/// With none of the three shown, the timeline's lane order.
pub(super) fn order(fleet: &Fleet, view: &View, filter: &Filter) -> Vec<u64> {
    let matching: Vec<&Worker> = fleet.workers.iter().filter(|w| filter.matches(w)).collect();
    let shown = |panel: Panel| view.shown[panel as usize];
    let in_state = |state: State| -> Vec<&Worker> {
        matching
            .iter()
            .copied()
            .filter(|w| w.state == state)
            .collect()
    };
    let mut panes: Vec<u64> = Vec::new();
    let mut add = |workers: Vec<&Worker>| {
        for worker in workers {
            if !panes.contains(&worker.pane) {
                panes.push(worker.pane);
            }
        }
    };
    if !Panel::NOW.into_iter().any(shown) {
        add(flat(view, matching.iter().copied()));
        return panes;
    }
    if shown(Panel::Queue) {
        add(cards(matching.iter().copied(), view.info_open)
            .into_iter()
            .map(|(w, _)| w)
            .collect());
    }
    if shown(Panel::Running) {
        add(flat(view, in_state(State::Running)));
    }
    if shown(Panel::Done) {
        add(flat(view, in_state(State::Finished)));
    }
    panes
}

fn flat<'a>(view: &View, workers: impl IntoIterator<Item = &'a Worker>) -> Vec<&'a Worker> {
    view.grouping
        .split(workers)
        .into_iter()
        .flat_map(|(_, group)| group)
        .collect()
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

/// The next waiting worker in queue order (wrapping around): jumps straight
/// to the top urgent worker from anywhere, or cycles through waiting ones.
pub(super) fn next_waiting(
    fleet: &Fleet,
    view: &View,
    filter: &Filter,
    selected: Option<u64>,
) -> Option<u64> {
    let matching = fleet.workers.iter().filter(|w| filter.matches(w));
    let waiting: Vec<u64> = cards(matching, view.info_open)
        .into_iter()
        .map(|(w, _)| w.pane)
        .collect();
    if waiting.is_empty() {
        return None;
    }
    match selected.and_then(|pane| waiting.iter().position(|p| *p == pane)) {
        Some(index) => Some(waiting[(index + 1) % waiting.len()]),
        None => Some(waiting[0]),
    }
}
