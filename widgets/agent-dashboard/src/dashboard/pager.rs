//! Paging instead of scrolling (AC3). The worker list is a clipped column
//! (`panel::card_list`) that shows whole rows only: `fit` hides the rows
//! before the page's first one and those that would not fit below it, and
//! says in the list's toolbar how many are hidden. The wheel turns pages a
//! whole row at a time; a selection the user makes is paged into view, and
//! a list built again starts at the row its page started at
//! (`View::anchor`).

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
    /// Wheel steps over this list (forward positive) that came after a
    /// selection still waiting to be paged into view (`view.reveal`, the
    /// pane named here): the page that selection brings into view is not
    /// laid out yet, so the steps are counted from it by the next `fit`,
    /// which pages the selection into view first. `wheel` holds only a
    /// shared tree, hence the cell.
    after_reveal: Cell<Option<(u64, isize)>>,
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
    /// The list's rect: the room the window gives it.
    list: Option<Rect>,
    /// Each row: a digest of what it holds, and its rect while it is
    /// shown. A hidden row has no rect, so its digest stands for its size;
    /// it names the row's widget, so a list that now holds other rows (or
    /// the same in another order) looks different.
    items: Vec<(u64, Option<Rect>)>,
}

/// The pageable list.
#[derive(Debug)]
pub(super) struct ListParts {
    /// The list container.
    pub(super) list: WidgetId,
    /// The rows in order, each with the pane it shows.
    pub(super) items: Vec<(WidgetId, u64)>,
    /// The "上に N 件 · 下に M 件" label in the toolbar.
    pub(super) indicator: WidgetId,
}

impl ListParts {
    /// The index of the row that shows `pane`.
    pub(super) fn item_of(&self, pane: u64) -> Option<usize> {
        self.items.iter().position(|(_, own)| *own == pane)
    }

    /// The pane of each row, in order.
    pub(super) fn panes(&self) -> Vec<u64> {
        self.items.iter().map(|(_, pane)| *pane).collect()
    }

    /// Shows the rows in `shown` and hides the rest; true when anything
    /// changed.
    fn show(&self, tree: &mut UiTree, shown: Range<usize>) -> bool {
        let mut changed = false;
        for (index, (id, _)) in self.items.iter().enumerate() {
            changed |= set_shown(tree, *id, shown.contains(&index));
        }
        changed
    }

    /// One past the last row of the page from `first`, as laid out with
    /// every row from `first` on shown: the first row that ends below the
    /// list is cut with all after it, unless it is `first` itself, which is
    /// shown alone and clipped by the list (it is taller than the list).
    fn page(&self, tree: &UiTree, first: usize) -> usize {
        let len = self.items.len();
        let Some(list) = tree.window_rect(self.list) else {
            return len;
        };
        let bottom = list.y + list.height + SLACK;
        let cut = self.items[first..].iter().position(|(id, _)| {
            tree.window_rect(*id)
                .is_some_and(|item| item.y + item.height > bottom)
        });
        match cut {
            Some(0) => first + 1,
            Some(n) => first + n,
            None => len,
        }
    }

    /// The rows not shown below the page that starts at `first`.
    fn hidden_below(&self, tree: &UiTree, first: usize) -> usize {
        let after = self.items.iter().skip(first);
        after.filter(|(id, _)| tree.is_hidden(*id)).count()
    }

    /// The list's page memory, beside its indicator.
    fn memory<'t>(&self, tree: &'t UiTree) -> (WidgetId, &'t PageMemory) {
        let row = tree
            .parent(self.indicator)
            .expect("the indicator is in the toolbar");
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

    /// `pane` when its row is shown.
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

/// The list's page as `fit` moves it toward its aim.
struct Job {
    aim: Aim,
    /// Wheel steps to take once `aim` is reached (forward positive): the
    /// wheel turned the list after the selection the aim brings into view.
    then: isize,
}

impl Job {
    /// The first row after one round from `first`, whose page ends at
    /// `end` of `len` rows; `None` when the list is settled.
    fn step(&mut self, first: usize, end: usize, len: usize) -> Option<usize> {
        match self.aim {
            Aim::Item(item) if item < first => Some(item),
            Aim::Item(item) if item >= end => Some(first + 1),
            Aim::Item(_) if self.then != 0 => {
                // The selection is on the page: the wheel's steps follow.
                let steps = std::mem::take(&mut self.then);
                if steps > 0 {
                    self.aim = Aim::Forward(first + steps.unsigned_abs());
                    self.step(first, end, len)
                } else {
                    self.aim = Aim::Back;
                    let back = first.saturating_sub(steps.unsigned_abs());
                    (back != first).then_some(back)
                }
            }
            Aim::Forward(to) if first < to && end < len => Some(first + 1),
            _ => None,
        }
    }
}

/// Lays out the page from `first` with every row from it on shown and
/// measures where it ends. Sets `changed` when it changed the tree.
fn measure(
    tree: &mut UiTree,
    text: &mut TextStore,
    viewport: Size,
    parts: &ListParts,
    first: usize,
    changed: &mut bool,
) -> usize {
    if parts.show(tree, first..parts.items.len()) {
        *changed = true;
        tree.prepare(viewport, text);
    }
    parts.page(tree, first)
}

/// Pages the list to fit its room at `viewport`; true when it changed the
/// tree (the caller lays out again). Writes the pane of the page's first
/// row to `view.anchor`.
///
/// The list keeps its page while nothing that decides it changed: the same
/// room, the same rows holding the same content laid out the same way, the
/// same first row. Then `fit` touches nothing, so a frame with nothing new
/// (the live window wakes every second) is neither laid out again nor
/// presented. Otherwise its page is decided by one rule — **the user's most
/// recent action wins**:
///
/// - a list built again (no memory: a live update, a rebuild) starts at the
///   row its page started at (`view.anchor`), else at its first row kept
///   in range, and aims nowhere unless the user just selected a worker: a
///   row that came above the page moves nothing the user is reading, and
///   the selection never moves. With no anchor (the first build; the user
///   changed the scope or the filter, which clears it) it starts at the
///   top, with the selected worker in view;
/// - a selection made since the list's last fit (`view.reveal`) is paged
///   into view; when the wheel turned the list after that selection
///   (`PageMemory::after_reveal`), its steps are then taken from the page
///   that shows the selection — the page a fit between the two would have
///   left, so a frame drawn between them or not makes no difference;
/// - the wheel's page holds when no selection came since: its steps
///   forward stop at the first page that hides nothing below (`wheel`
///   cannot lay out to see it);
/// - with no user action since the last fit (a new size, a row changed in
///   place), the page keeps its first row; the selected worker is brought
///   back only when it was on the page (the user had not paged away from
///   it) and the change pushed it off. Then the page starts at the
///   earliest row from which everything to the end fits, if there is one
///   before it (a larger window).
///
/// The fit is measured, not computed: every row from the page's first one
/// on is shown, laid out, and those that end below the list hidden again.
/// A hidden row's place is free, so hiding a later row never moves an
/// earlier one: one layout is enough for each first row tried.
pub(super) fn fit(
    tree: &mut UiTree,
    text: &mut TextStore,
    viewport: Size,
    list: Option<&ListParts>,
    view: &mut View,
) -> bool {
    let Some(parts) = list else {
        view.reveal = None;
        return false;
    };
    let len = parts.items.len();
    let (_, memory) = parts.memory(tree);
    let after_reveal = memory.after_reveal.take();
    let then = after_reveal
        .filter(|(pane, _)| view.reveal == Some(*pane))
        .map_or(0, |(_, steps)| steps);
    let seen = memory.seen.clone();
    let look = parts.look(tree);
    let item = |pane: Option<u64>| pane.and_then(|pane| parts.item_of(pane));
    let selected = item(view.selected);
    let revealed = item(view.reveal);
    let anchored = item(view.anchor);
    let anchor = view.anchor;
    let first = &mut view.first;
    let kept = seen
        .as_ref()
        .is_some_and(|seen| seen.first == *first && seen.look == look);
    let aim = match &seen {
        None if anchor.is_some() => {
            if let Some(at) = anchored {
                *first = at;
            }
            revealed.map_or(Aim::Stay, Aim::Item)
        }
        None => {
            *first = 0;
            revealed.or(selected).map_or(Aim::Stay, Aim::Item)
        }
        Some(seen) => match revealed {
            Some(item) => Aim::Item(item),
            None if seen.first < *first => Aim::Forward(std::mem::replace(first, seen.first)),
            None if seen.first > *first => Aim::Back,
            None if seen.held.is_some() && seen.held == view.selected => {
                selected.map_or(Aim::Stay, Aim::Item)
            }
            None => Aim::Stay,
        },
    };
    *first = (*first).min(len.saturating_sub(1));
    view.reveal = None;
    let on_page = match aim {
        Aim::Item(item) => look.items[item].1.is_some(),
        _ => true,
    };
    if kept && on_page && then == 0 {
        let held = parts.shown(tree, view.selected);
        if seen.is_some_and(|seen| seen.held != held) {
            parts.remember(tree, view.first, view.selected);
        }
        view.anchor = parts.items.get(view.first).map(|(_, pane)| *pane);
        return false;
    }
    let mut changed = false;
    let mut job = Job { aim, then };

    // Each round moves the first row one step toward the aim (the wheel's
    // steps back after a selection are one round), so a list of n rows
    // needs at most 2n + 1 rounds.
    let mut end = measure(tree, text, viewport, parts, *first, &mut changed);
    while let Some(step) = job.step(*first, end, len) {
        *first = step;
        end = measure(tree, text, viewport, parts, *first, &mut changed);
    }

    // A page starts at the earliest row from which everything to the end
    // fits, when there is one before its first.
    if job.aim.fills_back() {
        for earlier in 0..*first {
            let page = measure(tree, text, viewport, parts, earlier, &mut changed);
            if page == len {
                *first = earlier;
                end = page;
                break;
            }
        }
    }

    let first = *first;
    let mut settled = parts.show(tree, first..end);
    let hidden = indicator_text(first, len - end);
    settled |= set_indicator(tree, parts.indicator, hidden);
    if settled {
        changed = true;
        tree.prepare(viewport, text);
    }
    parts.remember(tree, first, view.selected);
    view.anchor = parts.items.get(first).map(|(_, pane)| *pane);
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

/// Pages the list with a wheel event over it: every full `WHEEL_STEP`
/// forward shows the list from one row later while something is hidden
/// below, every step back from one row earlier while something is hidden
/// above. While a selection waits to be paged into view (`view.reveal`),
/// the steps are kept for `fit` to take from the page that shows it (see
/// `PageMemory::after_reveal`), which stops them at either end. True when
/// that turned the page: the caller then pages and presents. Travel that
/// turns no page (less than a step, sideways, past either end) and a wheel
/// over anything else — the header, the toolbar, an overlay above the
/// list — are left to the tree (false), where nothing under the list takes
/// a wheel: the frame is not presented again.
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
    list: Option<&ListParts>,
    view: &mut View,
    accum: &mut f32,
    event: &Event,
) -> bool {
    let Event::Wheel { pos, delta } = event else {
        return false;
    };
    let Some(parts) = list else {
        return false;
    };
    // The list must be what is drawn at `pos`: the hit is inside it, not in
    // an overlay above it (the details drawer covers its right side).
    let Some(hit) = tree.hit_test(*pos) else {
        return false;
    };
    if !tree.ancestors(hit).any(|id| id == parts.list) {
        return false;
    }
    // Positive `delta.y` moves content down, toward the start.
    let travel = -delta.y;
    if *accum * travel < 0.0 {
        *accum = 0.0;
    }
    *accum += travel;
    let (_, memory) = parts.memory(tree);
    if let Some(pane) = view.reveal.filter(|pane| parts.item_of(*pane).is_some()) {
        // The page that will show the selection is not laid out yet, so
        // neither end is known: `fit` stops the steps at either.
        let steps = (*accum / WHEEL_STEP).trunc();
        if steps == 0.0 {
            return false;
        }
        *accum -= steps * WHEEL_STEP;
        let before = memory.after_reveal.get();
        let before = before.filter(|(p, _)| *p == pane).map_or(0, |(_, n)| n);
        memory
            .after_reveal
            .set(Some((pane, before + steps as isize)));
        return true;
    }
    let first = &mut view.first;
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
    *first != from
}
