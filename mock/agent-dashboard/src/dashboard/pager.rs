//! Paging instead of scrolling (AC3). Each panel list is a clipped column
//! (`panel::card_list`) that shows whole items only: `fit` hides the items
//! before the page's first one and those that would not fit below it, and
//! says in the panel's title row how many are hidden. The wheel turns pages
//! a whole item at a time; a selected worker is paged into view.

use std::cell::Cell;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

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

/// What the last `fit` of one list saw, and what the wheel did to it since.
/// It sits beside the list's indicator (`panel::indicator` puts it there)
/// as a widget so that it lives exactly as long as the list: a rebuild makes
/// the list again with a new, empty memory, which is how `fit` tells a list
/// built again from one it has paged before — no other module's work can
/// overwrite it. It takes no space and draws nothing.
#[derive(Debug, Default)]
pub(super) struct PageMemory {
    seen: Option<Seen>,
    /// The selection still waiting to be paged into view (`view.reveal`)
    /// when the wheel last turned this list's page since its last fit: the
    /// wheel came after that selection, so it decides this list's page.
    /// `wheel` holds only a shared tree, hence the cell.
    wheeled_after: Cell<Option<u64>>,
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
    /// The selected pane, when its item was on the page: a change that
    /// pushes it off brings it back, unless the user has paged since.
    held: Option<u64>,
    look: Look,
}

/// Everything about a list, as laid out, that decides its page.
#[derive(Debug, Clone, PartialEq)]
struct Look {
    /// The list's rect: the room the panel gives it.
    list: Option<Rect>,
    /// Each item: a digest of what it holds, and its rect while it is
    /// shown. Any item can decide the page when it grows or shrinks in
    /// place: a shown one by moving its own rect or a later one, a hidden
    /// one below by fitting after all, a hidden one above by letting the
    /// last page start earlier. A hidden item has no rect, so its digest
    /// stands for its size; it names the item's widget, so a list that now
    /// holds other items (or the same in another order) looks different.
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

/// Where a page ends, as measured.
#[derive(Debug, Clone, Copy)]
struct Page {
    /// One past the page's last item.
    end: usize,
    /// Whether the page's first item is shown without its group's title
    /// above it: with the title it would not fit (see `ListParts::show`).
    bare: bool,
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

    /// Whether `header` is a group's title above `item` — not the wrap grid
    /// that holds it.
    fn titles(&self, tree: &UiTree, header: WidgetId, of: &[usize], item: usize) -> bool {
        of.contains(&item) && !tree.ancestors(self.items[item].0).any(|id| id == header)
    }

    /// Shows the items in `shown` and the headers of any of them; hides the
    /// rest. When `bare`, the title of the first shown item's group is left
    /// out (see `page`): the earlier items are hidden, and their gaps with
    /// them, so that title is the one thing above a page's first item that
    /// can push it out of a list that could hold it. True when anything
    /// changed.
    fn show(&self, tree: &mut UiTree, shown: Range<usize>, bare: bool) -> bool {
        let items = self.items.iter().enumerate().map(|(index, (id, _))| {
            let on = shown.contains(&index);
            (*id, on)
        });
        let headers = self.headers.iter().map(|(id, of)| {
            let left_out = bare && self.titles(tree, *id, of, shown.start);
            let on = !left_out && of.iter().any(|index| shown.contains(index));
            (*id, on)
        });
        let changes: Vec<(WidgetId, bool)> = items.chain(headers).collect();
        let mut changed = false;
        for (id, on) in changes {
            changed |= set_shown(tree, id, on);
        }
        changed
    }

    /// Measures the page from `first` as laid out with every item from
    /// `first` on shown: the first item that ends below the list is cut with
    /// all after it, unless it is `first` itself, which is shown alone and
    /// clipped by the list (it is taller than the list). `None` when `first`
    /// does not fit below its group's title, which it must then be measured
    /// without (`bare`).
    fn page(&self, tree: &UiTree, first: usize, bare: bool) -> Option<Page> {
        let len = self.items.len();
        let Some(list) = tree.window_rect(self.list) else {
            return Some(Page { end: len, bare });
        };
        let bottom = list.y + list.height + SLACK;
        let cut = self.items[first..].iter().position(|(id, _)| {
            tree.window_rect(*id)
                .is_some_and(|item| item.y + item.height > bottom)
        });
        let titled = || {
            let mut headers = self.headers.iter();
            headers.any(|(id, of)| self.titles(tree, *id, of, first))
        };
        let end = match cut {
            Some(0) if !bare && titled() => return None,
            Some(0) => first + 1,
            Some(n) => first + n,
            None => len,
        };
        Some(Page { end, bare })
    }

    /// The items not shown below the page that starts at `first`.
    fn hidden_below(&self, tree: &UiTree, first: usize) -> usize {
        let after = self.items.iter().skip(first);
        after.filter(|(id, _)| tree.is_hidden(*id)).count()
    }

    /// The list's page memory, beside its indicator.
    fn memory<'t>(&self, tree: &'t UiTree) -> (WidgetId, &'t PageMemory) {
        let row = tree
            .parent(self.indicator)
            .expect("the indicator is in a title row");
        let mut children = tree.children(row).iter();
        children
            .find_map(|id| Some((*id, tree.get::<PageMemory>(*id)?)))
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
            items: items.collect(),
        }
    }

    /// `pane` when its item is shown.
    fn shown(&self, tree: &UiTree, pane: Option<u64>) -> Option<u64> {
        let item = pane.and_then(|pane| self.item_of(pane))?;
        (!tree.is_hidden(self.items[item].0))
            .then_some(pane)
            .flatten()
    }

    /// Remembers the page at `first`, as laid out now, for the next fit.
    fn remember(&self, tree: &mut UiTree, first: usize, selected: Option<u64>) {
        let seen = Seen {
            first,
            held: self.shown(tree, selected),
            look: self.look(tree),
        };
        let (memory, _) = self.memory(tree);
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
    /// Keep the first item.
    Stay,
    /// Bring this item onto the page.
    Item(usize),
    /// The wheel's travel forward: from the page it started on, one item
    /// further per step up to this first item, while something is still
    /// hidden below the page reached.
    Forward(usize),
    /// The first item the wheel turned back to, as it is.
    Back,
}

impl Aim {
    /// Whether the page shows earlier items again while everything to the
    /// end still fits: a page the wheel turned stays where it was turned.
    fn fills_back(self) -> bool {
        matches!(self, Aim::Stay | Aim::Item(_))
    }
}

/// A list `fit` pages again this time.
struct Job<'a> {
    panel: usize,
    parts: &'a ListParts,
    aim: Aim,
    page: Page,
}

/// Lays out the page from each `(parts, first)` and measures it: twice for
/// a page whose first item does not fit below its group's title, the second
/// time without the title. Sets `changed` when it changed the tree.
fn measure(
    tree: &mut UiTree,
    text: &mut TextStore,
    viewport: Size,
    pages: &[(&ListParts, usize)],
    changed: &mut bool,
) -> Vec<Page> {
    let mut lay_out = |tree: &mut UiTree, bare: &dyn Fn(usize) -> bool| {
        let mut shown = false;
        for (k, (parts, first)) in pages.iter().enumerate() {
            shown |= parts.show(tree, *first..parts.items.len(), bare(k));
        }
        if shown {
            *changed = true;
            tree.prepare(viewport, text);
        }
    };
    lay_out(tree, &|_| false);
    let measured: Vec<Option<Page>> = pages
        .iter()
        .map(|(parts, first)| parts.page(tree, *first, false))
        .collect();
    if measured.iter().all(Option::is_some) {
        return measured.into_iter().flatten().collect();
    }
    lay_out(tree, &|k| measured[k].is_none());
    let pages = pages.iter().zip(measured);
    pages
        .map(|((parts, first), page)| {
            page.unwrap_or_else(|| {
                let bare = parts.page(tree, *first, true);
                bare.expect("a bare page is always measured")
            })
        })
        .collect()
}

/// Pages every list to fit its panel at `viewport`; true when it changed
/// the tree (the caller lays out again).
///
/// A list keeps its page while nothing that decides it changed: the same
/// room, the same items holding the same content laid out the same way,
/// the same first item. Then `fit` touches nothing, so a frame with nothing
/// new (the live window wakes every second) is neither laid out again nor
/// presented. Otherwise its page is decided by one rule — **the user's most
/// recent action wins**:
///
/// - a list built again (no memory: a rebuild, a filter change, another
///   server) starts at its first item, with the selected worker in view;
/// - a selection made since the list's last fit (`view.reveal`) is paged
///   into view, unless the wheel turned this list's page after it
///   (`PageMemory::wheeled_after`): then, as when no selection came since,
///   the wheel's page holds — its steps forward stop at the first page that
///   hides nothing below (`wheel` cannot lay out to see it);
/// - with no user action since the last fit (a new size, an item changed
///   in place), the page keeps its first item; the selected worker is
///   brought back only when it was on the page (the user had not paged
///   away from it) and the change pushed it off. Then, when the page
///   reaches the end of the list (a larger window, an item that shrank),
///   it starts at the earliest item from which everything to the end fits.
///
/// The fit is measured, not computed: card heights change in place (a
/// queue card expands or collapses on a volatile snapshot, lanes are made
/// again), so every item from the page's first one on is shown, laid out,
/// and those that end below the list hidden again. A hidden item's place is
/// free, so hiding a later item never moves an earlier one: one layout is
/// enough for each first item tried (two when its group's title must go).
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
        let (_, memory) = parts.memory(tree);
        let wheeled_after = memory.wheeled_after.take();
        let seen = memory.seen.clone();
        let look = parts.look(tree);
        let first = &mut view.first[panel];
        let kept = seen
            .as_ref()
            .is_some_and(|seen| seen.first == *first && seen.look == look);
        let item = |pane: Option<u64>| pane.and_then(|pane| parts.item_of(pane));
        let selected = item(view.selected);
        let reveal = view.reveal.filter(|pane| wheeled_after != Some(*pane));
        let aim = match &seen {
            None => {
                *first = 0;
                selected.map_or(Aim::Stay, Aim::Item)
            }
            Some(seen) => match item(reveal) {
                Some(item) => Aim::Item(item),
                None if seen.first < *first => Aim::Forward(std::mem::replace(first, seen.first)),
                None if seen.first > *first => Aim::Back,
                None if seen.held.is_some() && seen.held == view.selected => {
                    selected.map_or(Aim::Stay, Aim::Item)
                }
                None => Aim::Stay,
            },
        };
        *first = (*first).min(parts.items.len().saturating_sub(1));
        let on_page = match aim {
            Aim::Item(item) => look.items[item].1.is_some(),
            _ => true,
        };
        if kept && on_page {
            let held = parts.shown(tree, view.selected);
            if seen.is_some_and(|seen| seen.held != held) {
                parts.remember(tree, *first, view.selected);
            }
            continue;
        }
        let page = Page {
            end: parts.items.len(),
            bare: false,
        };
        jobs.push(Job {
            panel,
            parts,
            aim,
            page,
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
        let pages: Vec<(&ListParts, usize)> = open
            .iter()
            .map(|&j| (jobs[j].parts, first[jobs[j].panel]))
            .collect();
        let pages = measure(tree, text, viewport, &pages, &mut changed);
        let mut next = Vec::new();
        for (j, page) in open.into_iter().zip(pages) {
            let job = &mut jobs[j];
            job.page = page;
            let first = &mut first[job.panel];
            let len = job.parts.items.len();
            *first = match job.aim {
                Aim::Item(item) if item < *first => item,
                Aim::Item(item) if item >= page.end => *first + 1,
                Aim::Forward(to) if *first < to && page.end < len => *first + 1,
                _ => continue,
            };
            next.push(j);
        }
        open = next;
    }

    // A page that reaches the end starts at the earliest item from which
    // everything to the end fits. Cards wrapped in rows do not fit by
    // count (two high cards share a row only when they start it), so every
    // earlier first item is tried, earliest first, not just the one before.
    let mut open: Vec<(usize, usize)> = (0..jobs.len())
        .filter(|&j| {
            let job = &jobs[j];
            job.aim.fills_back() && job.page.end == job.parts.items.len()
        })
        .filter(|&j| first[jobs[j].panel] > 0)
        .map(|j| (j, 0))
        .collect();
    while !open.is_empty() {
        let pages: Vec<(&ListParts, usize)> = open
            .iter()
            .map(|&(j, earlier)| (jobs[j].parts, earlier))
            .collect();
        let pages = measure(tree, text, viewport, &pages, &mut changed);
        let mut next = Vec::new();
        for ((j, earlier), page) in open.into_iter().zip(pages) {
            let job = &mut jobs[j];
            if page.end == job.parts.items.len() {
                first[job.panel] = earlier;
                job.page = page;
            } else if earlier + 1 < first[job.panel] {
                next.push((j, earlier + 1));
            }
        }
        open = next;
    }

    let mut settled = false;
    for job in &jobs {
        let first = first[job.panel];
        let Page { end, bare } = job.page;
        settled |= job.parts.show(tree, first..end, bare);
        let hidden = indicator_text(first, job.parts.items.len() - end);
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

/// Pages a panel list with a wheel event over it: every full `WHEEL_STEP`
/// forward shows the list from one item later while something is hidden
/// below, every step back from one item earlier while something is hidden
/// above. True when that turned the page: the caller then pages and
/// presents. Travel that turns no page (less than a step, sideways, past
/// either end) and a wheel over anything else — the header, a panel's
/// title, an overlay above a list — are left to the tree (false), where
/// nothing under a list takes a wheel: the frame is not presented again.
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
    let from = *first;
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
    if *first == from {
        return false;
    }
    let (_, memory) = parts.memory(tree);
    memory.wheeled_after.set(view.reveal);
    true
}
