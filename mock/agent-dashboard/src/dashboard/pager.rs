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
    /// Group headers (and wrap grids) with the indices of their items.
    pub(super) headers: Vec<(WidgetId, Vec<usize>)>,
    /// The "上に N 件 · 下に M 件" label in the title row.
    pub(super) indicator: WidgetId,
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
    view.reveal = None;
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
