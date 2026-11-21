//! Paging instead of scrolling (AC3). U1 registers every panel list here;
//! the lists still scroll, so nothing is paged yet. U3 makes the lists
//! clipped columns and fills in `fit` and `wheel`.

use noa_gpui_core::{Event, UiTree, WidgetId};
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

use super::View;

/// One panel's pageable list.
#[derive(Debug)]
pub(super) struct ListParts {
    /// The list container.
    pub(super) list: WidgetId,
    /// The items in order, each with the pane it shows.
    pub(super) items: Vec<(WidgetId, Option<u64>)>,
    /// Panes an item shows besides its own, with the item's index: a graph
    /// root row holds its children's and grandchildren's nodes.
    pub(super) nested: Vec<(u64, usize)>,
    /// Group headers (and wrap grids) with the indices of their items.
    pub(super) headers: Vec<(WidgetId, Vec<usize>)>,
    /// The "上に N 件 · 下に M 件" label in the title row.
    pub(super) indicator: WidgetId,
}

impl ListParts {
    /// The index of the item that shows `pane`, as its own or nested in it.
    pub(super) fn item_of(&self, pane: u64) -> Option<usize> {
        self.items
            .iter()
            .position(|(_, own)| *own == Some(pane))
            .or_else(|| {
                let nested = self.nested.iter().find(|(p, _)| *p == pane);
                nested.map(|(_, item)| *item)
            })
    }
}

/// Pages every list to fit its panel at `viewport`; true when it changed
/// the tree (the caller lays out again). In U1 nothing is paged: every
/// item, header and list stays shown, the indicators hidden, every list
/// on its first page, and there is nothing to reveal.
pub(super) fn fit(
    tree: &mut UiTree,
    _text: &mut TextStore,
    _viewport: Size,
    lists: &[Option<ListParts>; 5],
    view: &mut View,
) -> bool {
    let mut changed = false;
    for (first, parts) in view.first.iter_mut().zip(lists) {
        *first = 0;
        let Some(parts) = parts else {
            continue;
        };
        let items = parts.items.iter().map(|(id, _)| *id);
        let headers = parts.headers.iter().map(|(id, _)| *id);
        for id in items.chain(headers).chain([parts.list]) {
            if tree.is_hidden(id) {
                tree.set_hidden(id, false);
                changed = true;
            }
        }
        if !tree.is_hidden(parts.indicator) {
            tree.set_hidden(parts.indicator, true);
            changed = true;
        }
    }
    // Nothing is paged, so the item that shows the pane to reveal is in
    // view in every list that has one: the reveal is done.
    let in_view = |pane: u64| {
        lists.iter().flatten().all(|parts| {
            parts
                .item_of(pane)
                .is_none_or(|item| !tree.is_hidden(parts.items[item].0))
        })
    };
    if view.reveal.is_some_and(in_view) {
        view.reveal = None;
    }
    changed
}

/// Takes a wheel event over a panel list (true) to page it; false leaves
/// the event to the tree. U1 takes none: the lists scroll.
pub(super) fn wheel(
    _tree: &UiTree,
    _lists: &[Option<ListParts>; 5],
    _view: &mut View,
    _accum: &mut [f32; 5],
    _event: &Event,
) -> bool {
    false
}
