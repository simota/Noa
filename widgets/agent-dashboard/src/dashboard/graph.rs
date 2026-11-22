use noa_gpui_core::style::{AlignItems, FlexDirection, Rect as LayoutRect, length};
use noa_gpui_core::widgets::{Container, Label};
use noa_gpui_core::{Paint, Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Color;

use crate::fleet::{Fleet, State, Worker, one_line};

use super::filter::Filter;
use super::pager::ListParts;
use super::panel::{Panel, card_list, indicator, nothing_matches, title_row};
use super::style::{BLUE, GRAY, GREEN, caption, fixed_width, gap, need_color};
use super::{Mark, Registry};

pub(super) fn mark(worker: &Worker) -> (&'static str, Color) {
    match (worker.need(), worker.state) {
        (Some(need), _) => (need.symbol(), need_color(need)),
        (None, State::Finished) => ("✓", GRAY),
        (None, _) => ("●", GREEN),
    }
}

/// What the graph draws of the fleet under a filter: a node is kept when it
/// or one of its descendants matches, and muted when it does not match
/// itself (it only leads to a match).
struct Pruned<'a> {
    workers: &'a [Worker],
    filter: &'a Filter,
}

impl Pruned<'_> {
    fn children(&self, pane: u64) -> impl Iterator<Item = &Worker> {
        self.workers.iter().filter(move |w| w.parent == Some(pane))
    }

    fn keeps(&self, worker: &Worker) -> bool {
        self.filter.matches(worker) || self.children(worker.pane).any(|c| self.keeps(c))
    }
}

pub(super) fn graph(
    tree: &mut UiTree,
    section: WidgetId,
    fleet: &Fleet,
    filter: &Filter,
    reg: &mut Registry,
) {
    let workers = &fleet.workers;
    let pruned = Pruned { workers, filter };
    let title = title_row(tree, section, Panel::Graph, 8.0);
    caption(tree, title, "（親 → ワーカー → 孫）");
    let indicator = indicator(tree, title);
    let roots = card_list(tree, section, 12.0);
    let mut items = Vec::new();
    let mut nested = Vec::new();
    for orchestrator in &fleet.orchestrators {
        // An orchestrator that is a worker itself is the root, not a child.
        let own = orchestrator.pane.and_then(|pane| fleet.worker(pane));
        let children: Vec<&Worker> = workers
            .iter()
            .filter(|w| w.orchestrator == orchestrator.name && w.parent.is_none())
            .filter(|w| w.orchestrator_pane == orchestrator.pane)
            .filter(|w| Some(w.pane) != orchestrator.pane)
            .filter(|w| pruned.keeps(w))
            .collect();
        let own_matches = own.is_some_and(|w| filter.matches(w));
        if filter.active() && children.is_empty() && !own_matches {
            continue;
        }
        let row = tree.insert(
            Some(roots),
            Container::row().align_items(AlignItems::CENTER),
        );
        let (symbol, color) = own.map_or(("◆", BLUE), mark);
        let muted = filter.active() && !own_matches;
        let pill = node(tree, row, symbol, &orchestrator.name, color, muted);
        if let Some(own) = own.filter(|_| !muted) {
            reg.marks
                .push((pill, own.pane, Mark::Node { normal: color }));
        }
        let mut held = Vec::new();
        connect(tree, row, &pruned, &children, reg, &mut held);
        nested.extend(held.into_iter().map(|pane| (pane, items.len())));
        items.push((row, own.map(|w| w.pane)));
    }
    if items.is_empty() && filter.active() {
        nothing_matches(tree, roots);
    }
    reg.lists[Panel::Graph as usize] = Some(ListParts {
        list: roots,
        items,
        nested,
        headers: Vec::new(),
        indicator,
    });
    let legend = tree.insert(
        Some(section),
        Container::wrapping_row(10.0, 2.0).align_items(AlignItems::CENTER),
    );
    for (symbol, text, color) in [
        ("●", "作業中", GREEN),
        ("!", "承認待ち", super::style::YELLOW),
        ("?", "質問", super::style::ORANGE),
        ("×", "失敗", super::style::RED),
        ("…", "静止", GRAY),
        ("✓", "完了", GRAY),
    ] {
        let item = tree.insert(
            Some(legend),
            Container::row().align_items(AlignItems::CENTER).gap(3.0),
        );
        tree.insert(
            Some(item),
            Label::new(symbol).font_size(10.0).weight(700).color(color),
        );
        caption(tree, item, text);
    }
}

/// A node pill; a muted one (on the way to a match) has a muted label and
/// an outline border. Returns the pill, whose border the selection changes.
pub(super) fn node(
    tree: &mut UiTree,
    row: WidgetId,
    symbol: &str,
    label: &str,
    color: Color,
    muted: bool,
) -> WidgetId {
    let border: Paint = if muted {
        Role::Outline.into()
    } else {
        color.into()
    };
    let pill = tree.insert(
        Some(row),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::CENTER,
            gap: gap(4.0),
            padding: LayoutRect {
                left: length(7.0),
                right: length(7.0),
                top: length(1.0),
                bottom: length(1.0),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .background(Role::SurfaceAlt)
        .border(1.0, border)
        .radius(9.0),
    );
    tree.insert(
        Some(pill),
        Label::new(symbol).font_size(10.0).weight(700).color(color),
    );
    // An orchestrator's or a worker's name: drawn on one line.
    let label = Label::new(one_line(label)).font_size(11.0);
    tree.insert(
        Some(pill),
        if muted {
            label.color(Role::Muted)
        } else {
            label
        },
    );
    pill
}

pub(super) fn edge(tree: &mut UiTree, row: WidgetId, length_px: f32) {
    tree.insert(
        Some(row),
        Container::new(fixed_width(length_px))
            .height(1.0)
            .background(Role::Outline),
    );
}

/// Draws `children` (and theirs) beside `row`; `held` gets every pane
/// drawn, so the pager knows which root row shows it.
fn connect(
    tree: &mut UiTree,
    row: WidgetId,
    pruned: &Pruned,
    children: &[&Worker],
    reg: &mut Registry,
    held: &mut Vec<u64>,
) {
    if children.is_empty() {
        return;
    }
    edge(tree, row, 10.0);
    tree.insert(
        Some(row),
        Container::new(Style {
            align_self: Some(AlignItems::STRETCH),
            ..fixed_width(1.0)
        })
        .background(Role::Outline),
    );
    let column = tree.insert(Some(row), Container::column().gap(4.0));
    for child in children {
        let child_row = tree.insert(
            Some(column),
            Container::row().align_items(AlignItems::CENTER),
        );
        edge(tree, child_row, 8.0);
        let (symbol, color) = mark(child);
        let muted = !pruned.filter.matches(child);
        let pill = node(tree, child_row, symbol, &child.label, color, muted);
        held.push(child.pane);
        if !muted {
            reg.marks
                .push((pill, child.pane, Mark::Node { normal: color }));
        }
        let grandchildren: Vec<&Worker> = pruned
            .children(child.pane)
            .filter(|w| pruned.keeps(w))
            .collect();
        connect(tree, child_row, pruned, &grandchildren, reg, held);
    }
}
