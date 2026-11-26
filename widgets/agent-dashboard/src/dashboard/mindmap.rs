//! Mind Map / Topology tab: full-canvas tree visualization of workers by
//! orchestrator hierarchy, branch (with concurrency conflict warning), or
//! state cluster. Clicking any worker node selects it and opens its drawer.

use std::collections::BTreeMap;

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length,
};
use noa_gpui_core::widgets::{
    Badge, Container, ControlSize, Label, ScrollView, SegmentMode, SegmentedControl, Tag, TagStyle,
    Tone, Truncation,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Color;

use crate::fleet::{Fleet, State, Tier, Worker, one_line};

use super::filter::Filter;
use super::graph::{edge, mark};
use super::style::{
    BLUE, GRAY, GREEN, ORANGE, RED, YELLOW, caption, fill_remaining, fixed_width, gap, spacer,
};
use super::{Mark, Registry};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum MapAxis {
    #[default]
    Orchestrator,
    Branch,
    State,
}

impl MapAxis {
    pub(super) const ALL: [MapAxis; 3] = [MapAxis::Orchestrator, MapAxis::Branch, MapAxis::State];

    pub(super) fn title(self) -> &'static str {
        match self {
            MapAxis::Orchestrator => "親（階層）",
            MapAxis::Branch => "ブランチ",
            MapAxis::State => "状態",
        }
    }
}

/// Builds the full-body Mind Map tab into `body`; returns the axis selector
/// `SegmentedControl` widget id.
pub(super) fn build(
    tree: &mut UiTree,
    body: WidgetId,
    fleet: &Fleet,
    filter: &Filter,
    axis: MapAxis,
    reg: &mut Registry,
) -> WidgetId {
    let shell = tree.insert(
        Some(body),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(10.0),
            padding: LayoutRect::length(12.0),
            ..fill_remaining()
        })
        .background(Role::Surface)
        .border(1.0, Role::Separator)
        .radius(12.0),
    );

    // Top toolbar: axis switcher + legend + hint.
    let toolbar = tree.insert(
        Some(shell),
        Container::row().align_items(AlignItems::CENTER).gap(12.0),
    );
    tree.insert(
        Some(toolbar),
        Label::new("マップ表示").font_size(13.0).weight(700),
    );
    let selected_idx = MapAxis::ALL.iter().position(|a| *a == axis).unwrap_or(0);
    let axis_control = tree.insert(
        Some(toolbar),
        SegmentedControl::new(MapAxis::ALL.map(MapAxis::title), SegmentMode::SelectOne)
            .with_selected(&[selected_idx])
            .control_size(ControlSize::Small),
    );
    spacer(tree, toolbar);
    caption(tree, toolbar, "ノードをクリックすると選択・詳細表示します");

    let canvas = tree.insert(
        Some(shell),
        ScrollView::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(16.0),
            padding: LayoutRect::length(6.0),
            ..fill_remaining()
        }),
    );

    let matching: Vec<&Worker> = fleet.workers.iter().filter(|w| filter.matches(w)).collect();
    if matching.is_empty() {
        caption(tree, canvas, "該当するワーカーがありません");
        return axis_control;
    }

    match axis {
        MapAxis::Orchestrator => build_orchestrator_map(tree, canvas, fleet, filter, reg),
        MapAxis::Branch => build_branch_map(tree, canvas, &matching, reg),
        MapAxis::State => build_state_map(tree, canvas, &matching, reg),
    }

    axis_control
}

fn build_orchestrator_map(
    tree: &mut UiTree,
    canvas: WidgetId,
    fleet: &Fleet,
    filter: &Filter,
    reg: &mut Registry,
) {
    let workers = &fleet.workers;
    let keeps = |worker: &Worker| leads_to_match(workers, filter, worker);

    for orchestrator in &fleet.orchestrators {
        let own = orchestrator.pane.and_then(|pane| fleet.worker(pane));
        let children: Vec<&Worker> = workers
            .iter()
            .filter(|w| w.orchestrator == orchestrator.name && w.parent.is_none())
            .filter(|w| w.orchestrator_pane == orchestrator.pane)
            .filter(|w| Some(w.pane) != orchestrator.pane)
            .filter(|w| keeps(w))
            .collect();
        let own_matches = own.is_some_and(|w| filter.matches(w));
        if filter.active() && children.is_empty() && !own_matches {
            continue;
        }
        let row = tree.insert(
            Some(canvas),
            Container::row().align_items(AlignItems::CENTER),
        );
        let (symbol, color) = own.map_or(("◆", BLUE), mark);
        let waiting_count = children.iter().filter(|w| w.needs_attention()).count();
        let root_card = root_hub(
            tree,
            row,
            HubInfo {
                symbol,
                title: &one_line(&orchestrator.name),
                color,
                count: children.len(),
                waiting: waiting_count,
                warning: None,
            },
        );
        // An own pane outside the filter is drawn (its workers match) but
        // not chosen: the drawer shows only what the filter lets through.
        if let Some(own) = own.filter(|_| own_matches) {
            reg.marks
                .push((root_card, own.pane, Mark::Node { normal: color }));
        }
        connect_rich(tree, row, workers, filter, &children, reg);
    }
}

fn build_branch_map(tree: &mut UiTree, canvas: WidgetId, workers: &[&Worker], reg: &mut Registry) {
    let mut groups: BTreeMap<String, Vec<&Worker>> = BTreeMap::new();
    for worker in workers {
        let branch = if worker.branch.is_empty() {
            "（ブランチ不明）".to_owned()
        } else {
            one_line(&worker.branch)
        };
        groups.entry(branch).or_default().push(worker);
    }
    for (branch, members) in groups {
        let active = members
            .iter()
            .filter(|w| !matches!(w.state, State::Finished | State::Error))
            .count();
        let conflict = (active > 1 && branch != "（ブランチ不明）")
            .then(|| format!("並列 {active} 体（競合注意）"));
        let color = if conflict.is_some() { ORANGE } else { BLUE };
        let row = tree.insert(
            Some(canvas),
            Container::row().align_items(AlignItems::CENTER),
        );
        let waiting = members.iter().filter(|w| w.needs_attention()).count();
        root_hub(
            tree,
            row,
            HubInfo {
                symbol: "⎇",
                title: &branch,
                color,
                count: members.len(),
                waiting,
                warning: conflict.as_deref(),
            },
        );
        connect_flat_list(tree, row, &members, reg);
    }
}

fn build_state_map(tree: &mut UiTree, canvas: WidgetId, workers: &[&Worker], reg: &mut Registry) {
    type StateCluster = (&'static str, &'static str, Color, fn(&Worker) -> bool);
    let clusters: [StateCluster; 4] = [
        ("!", "判断待ち", YELLOW, |w| {
            w.need().is_some_and(|n| n.tier() == Tier::Decision)
        }),
        ("×", "要確認（失敗）", RED, |w| {
            w.state == State::Error
        }),
        ("●", "作業中", GREEN, |w| w.state == State::Running),
        ("✓", "完了", GRAY, |w| w.state == State::Finished),
    ];
    for (symbol, title, color, pred) in clusters {
        let members: Vec<&Worker> = workers.iter().copied().filter(|w| pred(w)).collect();
        if members.is_empty() {
            continue;
        }
        let row = tree.insert(
            Some(canvas),
            Container::row().align_items(AlignItems::CENTER),
        );
        root_hub(
            tree,
            row,
            HubInfo {
                symbol,
                title,
                color,
                count: members.len(),
                waiting: 0,
                warning: None,
            },
        );
        connect_flat_list(tree, row, &members, reg);
    }
}

struct HubInfo<'a> {
    symbol: &'a str,
    title: &'a str,
    color: Color,
    count: usize,
    waiting: usize,
    warning: Option<&'a str>,
}

fn root_hub(tree: &mut UiTree, row: WidgetId, info: HubInfo<'_>) -> WidgetId {
    let HubInfo {
        symbol,
        title,
        color,
        count,
        waiting,
        warning,
    } = info;
    let hub = tree.insert(
        Some(row),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(4.0),
            padding: LayoutRect {
                left: length(12.0),
                right: length(12.0),
                top: length(8.0),
                bottom: length(8.0),
            },
            size: LayoutSize {
                width: length(196.0),
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .background(Role::SurfaceAlt)
        .border(1.5, color)
        .radius(10.0),
    );
    let head = tree.insert(
        Some(hub),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    tree.insert(
        Some(head),
        Label::new(symbol).font_size(13.0).weight(700).color(color),
    );
    let slot = tree.insert(Some(head), Container::new(fill_remaining()));
    tree.insert(
        Some(slot),
        Label::new(title)
            .font_size(12.0)
            .weight(700)
            .truncation(Truncation::Tail),
    );
    tree.insert(Some(head), Badge::count(count as u32).tone(Tone::Neutral));
    if waiting > 0 {
        tree.insert(
            Some(hub),
            Tag::new(format!("要対応 {waiting}")).tone(Tone::Warning),
        );
    }
    if let Some(warning) = warning {
        tree.insert(Some(hub), Tag::new(warning).tone(Tone::Warning));
    }
    hub
}

/// Whether `worker` matches the filter, or one of its workers does, down
/// the tree: the map draws it then.
fn leads_to_match(workers: &[Worker], filter: &Filter, worker: &Worker) -> bool {
    filter.matches(worker)
        || workers
            .iter()
            .filter(|c| c.parent == Some(worker.pane))
            .any(|c| leads_to_match(workers, filter, c))
}

fn connect_rich(
    tree: &mut UiTree,
    row: WidgetId,
    all: &[Worker],
    filter: &Filter,
    children: &[&Worker],
    reg: &mut Registry,
) {
    if children.is_empty() {
        return;
    }
    edge(tree, row, 14.0);
    tree.insert(
        Some(row),
        Container::new(Style {
            align_self: Some(AlignItems::STRETCH),
            ..fixed_width(1.5)
        })
        .background(Role::Outline),
    );
    let col = tree.insert(Some(row), Container::column().gap(8.0));
    for child in children {
        let child_row = tree.insert(Some(col), Container::row().align_items(AlignItems::CENTER));
        edge(tree, child_row, 12.0);
        let muted = filter.active() && !filter.matches(child);
        rich_node(tree, child_row, child, muted, reg);
        let grandchildren: Vec<&Worker> = all
            .iter()
            .filter(|w| w.parent == Some(child.pane))
            .filter(|w| leads_to_match(all, filter, w))
            .collect();
        connect_rich(tree, child_row, all, filter, &grandchildren, reg);
    }
}

fn connect_flat_list(tree: &mut UiTree, row: WidgetId, members: &[&Worker], reg: &mut Registry) {
    if members.is_empty() {
        return;
    }
    edge(tree, row, 14.0);
    tree.insert(
        Some(row),
        Container::new(Style {
            align_self: Some(AlignItems::STRETCH),
            ..fixed_width(1.5)
        })
        .background(Role::Outline),
    );
    let col = tree.insert(Some(row), Container::column().gap(8.0));
    for worker in members {
        let child_row = tree.insert(Some(col), Container::row().align_items(AlignItems::CENTER));
        edge(tree, child_row, 12.0);
        rich_node(tree, child_row, worker, false, reg);
    }
}

fn rich_node(tree: &mut UiTree, row: WidgetId, worker: &Worker, muted: bool, reg: &mut Registry) {
    let (symbol, color) = mark(worker);
    let border_color = if muted { GRAY } else { color };
    let node = tree.insert(
        Some(row),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(4.0),
            padding: LayoutRect {
                left: length(10.0),
                right: length(10.0),
                top: length(7.0),
                bottom: length(7.0),
            },
            size: LayoutSize {
                width: length(296.0),
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .background(Role::SurfaceAlt)
        .border(1.0, border_color)
        .radius(8.0)
        .clip(true),
    );
    if !muted {
        reg.marks
            .push((node, worker.pane, Mark::Node { normal: color }));
    }

    let head = tree.insert(
        Some(node),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    tree.insert(
        Some(head),
        Label::new(symbol).font_size(12.0).weight(700).color(color),
    );
    let slot = tree.insert(Some(head), Container::new(fill_remaining()));
    let title = Label::new(one_line(&worker.label))
        .font_size(12.0)
        .weight(700)
        .truncation(Truncation::Tail);
    tree.insert(
        Some(slot),
        if muted {
            title.color(Role::Muted)
        } else {
            title
        },
    );
    tree.insert(
        Some(head),
        Tag::new(one_line(&worker.kind)).tone(Tone::Accent),
    );

    let meta = tree.insert(
        Some(node),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    if worker.is_agent() {
        tree.insert(Some(meta), Tag::new(one_line(&worker.model_line())));
    }
    if !worker.branch.is_empty() {
        tree.insert(
            Some(meta),
            Tag::new(one_line(&worker.branch)).style(TagStyle::Outlined),
        );
    }
    if !worker.since.is_empty() {
        caption(tree, meta, one_line(&worker.since));
    }

    if !worker.task.is_empty() {
        tree.insert(
            Some(node),
            Label::new(one_line(&worker.task))
                .font_size(11.0)
                .color(Role::Muted)
                .truncation(Truncation::Tail),
        );
    }
}
