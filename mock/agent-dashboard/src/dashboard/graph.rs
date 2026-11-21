use noa_gpui_core::style::{AlignItems, FlexDirection, Rect as LayoutRect, length};
use noa_gpui_core::widgets::{Container, Label};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Color;

use crate::fleet::{Fleet, State, Worker};

use super::style::{BLUE, GRAY, GREEN, caption, fixed_width, gap, need_color};

pub(super) fn mark(worker: &Worker) -> (&'static str, Color) {
    match (worker.need(), worker.state) {
        (Some(need), _) => (need.symbol(), need_color(need)),
        (None, State::Finished) => ("✓", GRAY),
        (None, _) => ("●", GREEN),
    }
}

pub(super) fn graph(tree: &mut UiTree, section: WidgetId, fleet: &Fleet) {
    let workers = &fleet.workers;
    tree.insert(
        Some(section),
        Label::new("関連（親 → ワーカー → 孫）")
            .font_size(12.0)
            .weight(700)
            .color(Role::Muted),
    );
    let roots = tree.insert(Some(section), Container::column().gap(12.0));
    for orchestrator in &fleet.orchestrators {
        let row = tree.insert(
            Some(roots),
            Container::row().align_items(AlignItems::CENTER),
        );
        // An orchestrator that is a worker itself is the root, not a child.
        let own = orchestrator.pane.and_then(|pane| fleet.worker(pane));
        let (symbol, color) = own.map_or(("◆", BLUE), mark);
        node(tree, row, symbol, &orchestrator.name, color);
        let children: Vec<&Worker> = workers
            .iter()
            .filter(|w| w.orchestrator == orchestrator.name && w.parent.is_none())
            .filter(|w| w.orchestrator_pane == orchestrator.pane)
            .filter(|w| Some(w.pane) != orchestrator.pane)
            .collect();
        connect(tree, row, workers, &children);
    }
    caption(
        tree,
        section,
        "● 作業中　! 承認待ち　? 質問　× 失敗　… 静止　✓ 完了",
    );
}

pub(super) fn node(tree: &mut UiTree, row: WidgetId, symbol: &str, label: &str, color: Color) {
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
        .border(1.0, color)
        .radius(9.0),
    );
    tree.insert(
        Some(pill),
        Label::new(symbol).font_size(10.0).weight(700).color(color),
    );
    tree.insert(Some(pill), Label::new(label).font_size(11.0));
}

pub(super) fn edge(tree: &mut UiTree, row: WidgetId, length_px: f32) {
    tree.insert(
        Some(row),
        Container::new(fixed_width(length_px))
            .height(1.0)
            .background(Role::Outline),
    );
}

pub(super) fn connect(tree: &mut UiTree, row: WidgetId, workers: &[Worker], children: &[&Worker]) {
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
        node(tree, child_row, symbol, &child.label, color);
        let grandchildren: Vec<&Worker> = workers
            .iter()
            .filter(|w| w.parent == Some(child.pane))
            .collect();
        connect(tree, child_row, workers, &grandchildren);
    }
}
