//! Paging instead of scrolling (AC3). Each panel list is a clipped column
//! (`panel::card_list`) that shows whole items only: `fit` hides the items
//! before the page's first one and those that would not fit below it, and
//! says in the panel's title row how many are hidden. The wheel turns pages
//! a whole item at a time; a selected worker is paged into view.

use noa_gpui_core::widgets::Label;
use noa_gpui_core::{Dirty, Event, UiTree, WidgetId};
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

use super::View;
use super::style::set_label;

/// Wheel travel that turns one page item (logical pixels).
pub(super) const WHEEL_STEP: f32 = 40.0;

/// How far an item may end below its list and still count as inside it:
/// layout rounds to half pixels.
const SLACK: f32 = 0.5;

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

    /// Shows the items in `shown` and the headers of any of them; hides the
    /// rest. True when anything changed.
    fn show(&self, tree: &mut UiTree, shown: std::ops::Range<usize>) -> bool {
        let items = self.items.iter().enumerate().map(|(index, (id, _))| {
            let on = shown.contains(&index);
            (*id, on)
        });
        let headers = self.headers.iter().map(|(id, of)| {
            let on = of.iter().any(|index| shown.contains(index));
            (*id, on)
        });
        let mut changed = false;
        for (id, on) in items.chain(headers) {
            changed |= set_shown(tree, id, on);
        }
        changed
    }

    /// One past the last item that fits below `first` as laid out with every
    /// item from `first` on shown: the first item that ends below the list
    /// is cut with all after it, unless it is `first` itself (an item taller
    /// than the list is shown alone, clipped by the list).
    fn fits_until(&self, tree: &UiTree, first: usize) -> usize {
        let Some(list) = tree.window_rect(self.list) else {
            return self.items.len();
        };
        let bottom = list.y + list.height + SLACK;
        let cut = self.items[first..].iter().position(|(id, _)| {
            tree.window_rect(*id)
                .is_some_and(|item| item.y + item.height > bottom)
        });
        match cut {
            Some(0) => first + 1,
            Some(n) => first + n,
            None => self.items.len(),
        }
    }

    /// The items not shown below the page that starts at `first`.
    fn hidden_below(&self, tree: &UiTree, first: usize) -> usize {
        let after = self.items.iter().skip(first);
        after.filter(|(id, _)| tree.is_hidden(*id)).count()
    }
}

/// Shows or hides `id`; true when that changed it.
fn set_shown(tree: &mut UiTree, id: WidgetId, on: bool) -> bool {
    if tree.is_hidden(id) != on {
        return false;
    }
    tree.set_hidden(id, !on);
    true
}

/// "上に 2 件 · 下に 3 件", leaving out a part that is zero; `None` when
/// nothing is hidden.
pub(super) fn indicator_text(above: usize, below: usize) -> Option<String> {
    let parts: Vec<String> = [("上", above), ("下", below)]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(side, n)| format!("{side}に {n} 件"))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Pages every list to fit its panel at `viewport` and brings `view.reveal`
/// into view in each list that has it; true when it changed the tree (the
/// caller lays out again).
///
/// The fit is measured, not computed: card heights change in place (a queue
/// card expands or collapses on a volatile snapshot, lanes are made again),
/// so every item from the page's first one on is shown, laid out, and those
/// that end below the list hidden again. A hidden item's place is free, so
/// hiding a later item never moves an earlier one: one layout is enough
/// for each first item tried.
pub(super) fn fit(
    tree: &mut UiTree,
    text: &mut TextStore,
    viewport: Size,
    lists: &[Option<ListParts>; 5],
    view: &mut View,
) -> bool {
    for (first, parts) in view.first.iter_mut().zip(lists) {
        if let Some(parts) = parts {
            *first = (*first).min(parts.items.len().saturating_sub(1));
        }
    }
    let mut changed = false;
    let mut ends = [0; 5];
    // Each round moves a list's first item one step toward the pane to
    // reveal, so a list of n items needs at most n rounds.
    let rounds = lists.iter().flatten().map(|p| p.items.len()).max();
    for _ in 0..=rounds.unwrap_or(0) {
        let mut shown_more = false;
        for (first, parts) in view.first.iter().zip(lists) {
            if let Some(parts) = parts {
                shown_more |= parts.show(tree, *first..parts.items.len());
            }
        }
        if shown_more {
            changed = true;
            tree.prepare(viewport, text);
        }
        for ((first, parts), end) in view.first.iter().zip(lists).zip(&mut ends) {
            if let Some(parts) = parts {
                *end = parts.fits_until(tree, *first);
                changed |= parts.show(tree, *first..*end);
            }
        }
        let Some(pane) = view.reveal else {
            break;
        };
        let mut moved = false;
        for ((first, parts), end) in view.first.iter_mut().zip(lists).zip(ends) {
            let Some(item) = parts.as_ref().and_then(|parts| parts.item_of(pane)) else {
                continue;
            };
            if item < *first {
                *first = item;
                moved = true;
            } else if item >= end {
                *first += 1;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    view.reveal = None;
    for ((first, parts), end) in view.first.iter().zip(lists).zip(ends) {
        if let Some(parts) = parts {
            let indicator = indicator_text(*first, parts.items.len() - end);
            changed |= set_indicator(tree, parts.indicator, indicator);
        }
    }
    if changed && let Some(root) = tree.root() {
        // The layout inside this fit took the changes with it: the caller's
        // second layout must still hand the window a frame to present.
        tree.invalidate(root, Dirty::COMPOSITE);
    }
    changed
}

/// Shows `text` in the indicator, or hides it for `None`; true when that
/// changed it.
fn set_indicator(tree: &mut UiTree, id: WidgetId, text: Option<String>) -> bool {
    let Some(text) = text else {
        return set_shown(tree, id, false);
    };
    let same = tree.get::<Label>(id).is_some_and(|l| l.text() == text);
    if !same {
        set_label(tree, id, text);
    }
    set_shown(tree, id, true) || !same
}

/// Takes a wheel event over a panel list (true) to page it: every full
/// `WHEEL_STEP` forward shows the list from one item later while something
/// is hidden below, every step back from one item earlier while something
/// is hidden above. The caller fits the pages again (no rebuild). A wheel
/// over anything else — the header, a panel's title, an overlay above a
/// list — is left to the tree (false).
pub(super) fn wheel(
    tree: &UiTree,
    lists: &[Option<ListParts>; 5],
    view: &mut View,
    accum: &mut [f32; 5],
    event: &Event,
) -> bool {
    let Event::Wheel { pos, delta } = event else {
        return false;
    };
    // The list must be what is drawn at `pos`: the hit is inside it, not in
    // an overlay above it (the details drawer covers the done panel).
    let Some(hit) = tree.hit_test(*pos) else {
        return false;
    };
    let over = lists.iter().enumerate().find_map(|(panel, parts)| {
        let parts = parts.as_ref()?;
        tree.ancestors(hit)
            .any(|id| id == parts.list)
            .then_some((panel, parts))
    });
    let Some((panel, parts)) = over else {
        return false;
    };
    // Positive `delta.y` moves content down, toward the start.
    accum[panel] -= delta.y;
    let first = &mut view.first[panel];
    let mut below = parts.hidden_below(tree, *first);
    while accum[panel] >= WHEEL_STEP {
        accum[panel] -= WHEEL_STEP;
        if below == 0 {
            // At the end: travel past it does not pile up for the way back.
            accum[panel] = 0.0;
            break;
        }
        *first += 1;
        below -= 1;
    }
    while accum[panel] <= -WHEEL_STEP {
        accum[panel] += WHEEL_STEP;
        if *first == 0 {
            accum[panel] = 0.0;
            break;
        }
        *first -= 1;
    }
    true
}
