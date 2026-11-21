//! Paging instead of scrolling (AC3). Each panel list is a clipped column
//! (`panel::card_list`) that shows whole items only: `fit` hides the items
//! before the page's first one and those that would not fit below it, and
//! says in the panel's title row how many are hidden. The wheel turns pages
//! a whole item at a time; a selected worker is paged into view.

use std::hash::{DefaultHasher, Hash, Hasher};

use noa_gpui_core::style::Display;
use noa_gpui_core::widgets::{Label, Tag};
use noa_gpui_core::{Dirty, Event, PaintCx, Style, UiTree, Widget, WidgetId};
use noa_gpui_scene::{Rect, Size};
use noa_gpui_text::TextStore;

use super::View;
use super::style::set_label;

/// Wheel travel that turns one page item (logical pixels).
pub(super) const WHEEL_STEP: f32 = 40.0;

/// How far an item may end below its list and still count as inside it:
/// layout rounds to half pixels.
const SLACK: f32 = 0.5;

/// What the last `fit` of one list saw. It sits beside the list's indicator
/// (`panel::indicator` puts it there) as a widget so that it lives exactly
/// as long as the list: a rebuild makes the list again with a new, empty
/// memory, which is how `fit` tells a list built again from one it has
/// paged before — no other module's work can overwrite it. It takes no
/// space and draws nothing.
#[derive(Debug, Default)]
pub(super) struct PageMemory {
    seen: Option<Seen>,
}

impl Widget for PageMemory {
    fn style(&self) -> Style {
        Style {
            display: Display::None,
            ..Default::default()
        }
    }

    fn paint(&mut self, _cx: &mut PaintCx) {}
}

/// The page a fit settled on, with what it was fitted for.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    first: usize,
    selected: Option<u64>,
    /// Whether the selected worker's item was on the page: an in-place
    /// change that pushes it off brings it back, unless the wheel has
    /// moved the page since.
    selected_shown: bool,
    look: Look,
}

/// Everything about a list, as laid out, that decides its page.
#[derive(Debug, Clone, PartialEq)]
struct Look {
    /// The list's rect: the room the panel gives it.
    list: Option<Rect>,
    /// The pane each item shows: what the list holds.
    panes: Vec<Option<u64>>,
    /// Each item: a digest of what it holds, and its rect while it is
    /// shown. Any item can decide the page when it grows or shrinks in
    /// place: a shown one by moving its own rect or a later one, a hidden
    /// one below by fitting after all, a hidden one above by letting the
    /// last page start earlier. A hidden item has no rect, so its digest
    /// stands for its size.
    items: Vec<(u64, Option<Rect>)>,
}

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

    /// The list's page memory, beside its indicator.
    fn memory(&self, tree: &UiTree) -> WidgetId {
        let row = tree
            .parent(self.indicator)
            .expect("the indicator is in a title row");
        let mut children = tree.children(row).iter().copied();
        children
            .find(|id| tree.get::<PageMemory>(*id).is_some())
            .expect("panel::indicator puts the page memory beside the indicator")
    }

    /// What decides the list's page now, as last laid out.
    fn look(&self, tree: &UiTree) -> Look {
        let items = self.items.iter().map(|(id, _)| {
            let rect = tree.window_rect(*id).filter(|_| !tree.is_hidden(*id));
            (content(tree, *id), rect)
        });
        Look {
            list: tree.window_rect(self.list),
            panes: self.items.iter().map(|(_, pane)| *pane).collect(),
            items: items.collect(),
        }
    }

    /// Whether the item that shows `pane` is shown.
    fn shows(&self, tree: &UiTree, pane: Option<u64>) -> bool {
        let item = pane.and_then(|pane| self.item_of(pane));
        item.is_some_and(|item| !tree.is_hidden(self.items[item].0))
    }

    /// Remembers the page at `first`, as laid out now, for the next fit.
    fn remember(&self, tree: &mut UiTree, first: usize, selected: Option<u64>) {
        let seen = Seen {
            first,
            selected,
            selected_shown: self.shows(tree, selected),
            look: self.look(tree),
        };
        let memory = self.memory(tree);
        tree.update::<PageMemory, _>(memory, |m, _| m.seen = Some(seen));
    }
}

/// A digest of the widgets under `id`, the texts they show and which of
/// them are hidden: it changes when an item is rebuilt, relabelled or
/// shows or hides a part in place — what can resize it. Whether `id`
/// itself is hidden is the pager's own doing and left out.
fn content(tree: &UiTree, id: WidgetId) -> u64 {
    let mut digest = DefaultHasher::new();
    let mut stack: Vec<WidgetId> = tree.children(id).iter().rev().copied().collect();
    id.hash(&mut digest);
    while let Some(id) = stack.pop() {
        id.hash(&mut digest);
        tree.is_hidden(id).hash(&mut digest);
        if let Some(label) = tree.get::<Label>(id) {
            label.text().hash(&mut digest);
        }
        if let Some(tag) = tree.get::<Tag>(id) {
            tag.text().hash(&mut digest);
        }
        stack.extend(tree.children(id).iter().rev());
    }
    digest.finish()
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

/// Where a list's page is headed.
#[derive(Debug, Clone, Copy)]
enum Aim {
    /// Stay at the first item it has.
    Stay,
    /// Bring this item onto the page.
    Item(usize),
    /// The wheel's travel forward: from the page it started on, one item
    /// further per step up to this first item, while something is still
    /// hidden below the page reached.
    Wheel(usize),
}

/// A list `fit` pages again this time.
struct Job<'a> {
    panel: usize,
    parts: &'a ListParts,
    aim: Aim,
    /// One past the page's last item.
    end: usize,
}

/// Pages every list to fit its panel at `viewport`; true when it changed
/// the tree (the caller lays out again).
///
/// A list keeps its page while nothing that decides it changed: the same
/// room, the same items holding the same content laid out the same way,
/// the same first item. Then `fit` touches nothing, so a frame with nothing
/// new (the live window wakes every second) is neither laid out again nor
/// presented. Otherwise:
///
/// - a list built again (no memory: a rebuild, a filter change, another
///   server) starts at its first item;
/// - the selected worker is brought into view when the selection just
///   changed (`view.reveal`), or when the list was built again, holds other
///   panes or got another size, or when it was on the page and an item
///   changed in place; a page the wheel moved away from the selection
///   otherwise stays where the wheel put it;
/// - the wheel's steps forward stop at the first page that hides nothing
///   below (`wheel` cannot lay out to see it);
/// - earlier items are shown again while everything to the end still fits
///   (a larger window, an item that shrank).
///
/// The fit is measured, not computed: card heights change in place (a
/// queue card expands or collapses on a volatile snapshot, lanes are made
/// again), so every item from the page's first one on is shown, laid out,
/// and those that end below the list hidden again. A hidden item's place is
/// free, so hiding a later item never moves an earlier one: one layout is
/// enough for each first item tried.
pub(super) fn fit(
    tree: &mut UiTree,
    text: &mut TextStore,
    viewport: Size,
    lists: &[Option<ListParts>; 5],
    view: &mut View,
) -> bool {
    let mut jobs = Vec::new();
    for (panel, parts) in lists.iter().enumerate() {
        let Some(parts) = parts else {
            continue;
        };
        let memory = parts.memory(tree);
        let seen = tree.get::<PageMemory>(memory).and_then(|m| m.seen.clone());
        let look = parts.look(tree);
        let last = parts.items.len().saturating_sub(1);
        let first = &mut view.first[panel];
        let (moved, held, from) = match &seen {
            None => {
                *first = 0;
                (true, false, 0)
            }
            Some(seen) => {
                let moved = seen.look.list != look.list
                    || seen.look.panes != look.panes
                    || seen.selected != view.selected;
                // The wheel is the only other hand on `first`.
                let held = seen.selected_shown && seen.first == *first;
                (moved, held, seen.first.min(last))
            }
        };
        let pane = view.reveal.or(view.selected.filter(|_| moved || held));
        let target = pane.and_then(|pane| parts.item_of(pane));
        *first = (*first).min(last);
        let on_page = target.is_none_or(|item| look.items[item].1.is_some());
        let kept = seen
            .as_ref()
            .is_some_and(|seen| seen.first == *first && seen.look == look);
        if kept && on_page {
            if seen.is_some_and(|seen| seen.selected != view.selected) {
                parts.remember(tree, *first, view.selected);
            }
            continue;
        }
        let aim = match target {
            Some(item) => Aim::Item(item),
            None if from < *first => {
                let to = std::mem::replace(first, from);
                Aim::Wheel(to)
            }
            None => Aim::Stay,
        };
        let end = parts.items.len();
        jobs.push(Job {
            panel,
            parts,
            aim,
            end,
        });
    }
    view.reveal = None;
    if jobs.is_empty() {
        return false;
    }
    let mut changed = false;
    let first = &mut view.first;

    // Each round moves a list's first item one step toward its aim, so a
    // list of n items needs at most n rounds.
    let mut open: Vec<usize> = (0..jobs.len()).collect();
    while !open.is_empty() {
        let mut shown = false;
        for &j in &open {
            let Job { panel, parts, .. } = jobs[j];
            shown |= parts.show(tree, first[panel]..parts.items.len());
        }
        if shown {
            changed = true;
            tree.prepare(viewport, text);
        }
        open.retain(|&j| {
            let job = &mut jobs[j];
            let first = &mut first[job.panel];
            let len = job.parts.items.len();
            job.end = job.parts.fits_until(tree, *first);
            match job.aim {
                Aim::Item(item) if item < *first => *first = item,
                Aim::Item(item) if item >= job.end => *first += 1,
                Aim::Wheel(to) if *first < to && job.end < len => *first += 1,
                _ => return false,
            }
            true
        });
    }

    // Shows the item before the page while everything to the end fits.
    let mut open: Vec<usize> = (0..jobs.len())
        .filter(|&j| jobs[j].end == jobs[j].parts.items.len() && first[jobs[j].panel] > 0)
        .collect();
    while !open.is_empty() {
        for &j in &open {
            let Job { panel, parts, .. } = jobs[j];
            changed |= parts.show(tree, first[panel] - 1..parts.items.len());
        }
        tree.prepare(viewport, text);
        open.retain(|&j| {
            let Job { panel, parts, .. } = jobs[j];
            let earlier = first[panel] - 1;
            let fits = parts.fits_until(tree, earlier) == parts.items.len();
            if fits {
                first[panel] = earlier;
            }
            fits && earlier > 0
        });
    }

    let mut settled = false;
    for job in &jobs {
        let first = first[job.panel];
        settled |= job.parts.show(tree, first..job.end);
        let hidden = indicator_text(first, job.parts.items.len() - job.end);
        settled |= set_indicator(tree, job.parts.indicator, hidden);
    }
    if settled {
        changed = true;
        tree.prepare(viewport, text);
    }
    for job in &jobs {
        job.parts.remember(tree, first[job.panel], view.selected);
    }
    if changed && let Some(root) = tree.root() {
        // The layouts inside this fit took the changes with them: the
        // caller's second layout must still hand the window a frame to
        // present.
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
/// is hidden above. A wheel over anything else — the header, a panel's
/// title, an overlay above a list — is left to the tree (false).
///
/// Travel that turns no page leaves nothing behind to spend later: the
/// remainder is dropped when the wheel turns the other way, and all of it
/// when a step meets the end. How far the list pages then depends on the
/// travel only, not on how the platform cut it into events. Whether a step
/// forward meets the end depends on the page the step before reached,
/// which only a layout tells: `wheel` refuses a step from the page on
/// screen when it hides nothing below, and `fit` takes back the steps past
/// the first page that hides nothing below.
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
    let travel = -delta.y;
    let accum = &mut accum[panel];
    if *accum * travel < 0.0 {
        *accum = 0.0;
    }
    *accum += travel;
    let first = &mut view.first[panel];
    let last = parts.items.len().saturating_sub(1);
    let on_last_page = parts.hidden_below(tree, *first) == 0;
    while *accum >= WHEEL_STEP {
        if on_last_page || *first >= last {
            *accum = 0.0;
            break;
        }
        *accum -= WHEEL_STEP;
        *first += 1;
    }
    while *accum <= -WHEEL_STEP {
        if *first == 0 {
            *accum = 0.0;
            break;
        }
        *accum += WHEEL_STEP;
        *first -= 1;
    }
    true
}
