use noa_gpui_core::style::{
    AlignItems, Dimension, FlexDirection, JustifyContent, Rect as LayoutRect, Size as LayoutSize,
    auto, length, percent,
};
use noa_gpui_core::widgets::{
    Button, Chart, ChartSeries, Container, ControlSize, Label, LevelIndicator, LevelStyle, Tag,
    TagStyle, Tone, Truncation,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use crate::fleet::{FRESHNESS_MAX, Grouping, State, Worker, minutes_text};

use super::drawer::one_line;
use super::pager::ListParts;
use super::panel::{
    Panel, card_list, group_header, indicator, mark_card, nothing_matches, panel_title,
};
use super::style::{caption, gap, joined};
use super::{Pulse, Registry};

pub(super) const RUNNING_CARD_WIDTH: f32 = 336.0;

pub(super) fn quiet_text(worker: &Worker) -> String {
    format!("静止 {:.0} 分", worker.wait_minutes)
}

/// Only times whose start is known are shown.
pub(super) fn running_caption(worker: &Worker) -> String {
    let started = worker
        .started_minutes
        .map(|minutes| format!("{}前に開始", minutes_text(minutes)))
        .unwrap_or_default();
    let reports = format!("報告 {} 回", worker.reports);
    joined(&[&started, &reports, &worker.since], " · ")
}

pub(super) fn done_caption(worker: &Worker) -> String {
    if worker.since.is_empty() {
        return "完了".to_owned();
    }
    let took = worker
        .took_minutes
        .map(|minutes| format!("所要 {}", minutes_text(minutes)))
        .unwrap_or_default();
    joined(&[&worker.since, &took], " · ")
}

// The worker texts below come from live data: each is drawn as
// `one_line` makes it, as the shaper panics on a paragraph separator that
// stays inside a line.

pub(super) fn task_line(tree: &mut UiTree, parent: WidgetId, worker: &Worker) {
    if worker.task.is_empty() {
        return;
    }
    tree.insert(
        Some(parent),
        Label::new(format!("依頼: {}", one_line(&worker.task)))
            .font_size(12.0)
            .color(Role::Muted)
            .wrap(true)
            .max_lines(2),
    );
}

pub(super) fn detail_line(tree: &mut UiTree, parent: WidgetId, worker: &Worker) {
    if worker.detail.is_empty() {
        return;
    }
    tree.insert(
        Some(parent),
        Label::new(one_line(&worker.detail))
            .font_size(12.0)
            .wrap(true),
    );
}

/// The kind, model and branch tags every card starts with; no branch tag
/// when the branch is unknown.
pub(super) fn identity_tags(tree: &mut UiTree, tags: WidgetId, worker: &Worker) {
    tree.insert(
        Some(tags),
        Tag::new(one_line(&worker.kind)).tone(Tone::Accent),
    );
    model_tag(tree, tags, worker);
    branch_tag(tree, tags, worker);
}

/// A plain command has no model to show.
pub(super) fn model_tag(tree: &mut UiTree, parent: WidgetId, worker: &Worker) {
    if worker.is_agent() {
        tree.insert(Some(parent), Tag::new(one_line(&worker.model_line())));
    }
}

pub(super) fn branch_tag(tree: &mut UiTree, parent: WidgetId, worker: &Worker) {
    if !worker.branch.is_empty() {
        tree.insert(
            Some(parent),
            Tag::new(one_line(&worker.branch)).style(TagStyle::Outlined),
        );
    }
}

/// How the running and done panels lay their cards out.
#[derive(Clone, Copy)]
pub(super) struct Cards {
    pub(super) grouping: Grouping,
    pub(super) output_caption: &'static str,
    /// Whether the filter is on: an empty panel then says so.
    pub(super) filtering: bool,
}

/// A card as `GroupBox::card` draws one (surface fill, 1 px separator
/// border, radius 10, padding 18, a 14 px weight-600 title 10 px above the
/// content, 14 px between content rows), built from containers so that the
/// selection bar and buttons can be its children. Like `GroupBox`, it is a
/// group named by its title for assistive technology: a `Container` is
/// exposed as a group only as a focus group, which changes neither layout
/// nor paint, and Tab still stops on each focusable widget in it while
/// there is at most one (the 詳細 button). The title is drawn as
/// [`one_line`] makes it (it is a worker's label, from live data); the
/// group keeps it as given for assistive technology. Returns the card and
/// the column its content goes in.
pub(super) fn card(
    tree: &mut UiTree,
    parent: WidgetId,
    title: &str,
    width: Dimension,
) -> (WidgetId, WidgetId) {
    let card = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(10.0),
            padding: LayoutRect::length(18.0),
            size: LayoutSize {
                width,
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .background(Role::Surface)
        .border(1.0, Role::Separator)
        .radius(10.0)
        .focus_group(true),
    );
    tree.set_accessible_label(card, title);
    tree.insert(
        Some(card),
        Label::new(one_line(title))
            .font_size(14.0)
            .weight(600)
            .truncation(Truncation::Tail),
    );
    let body = tree.insert(Some(card), Container::column().gap(14.0));
    (card, body)
}

/// A card's last row: its caption (cut when the card is narrow) and a 詳細
/// button that opens the details drawer on `pane`. Returns the caption.
fn caption_row(
    tree: &mut UiTree,
    body: WidgetId,
    text: String,
    pane: u64,
    reg: &mut Registry,
) -> WidgetId {
    let row = tree.insert(
        Some(body),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let slot = tree.insert(
        Some(row),
        Container::new(Style {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            min_size: LayoutSize {
                width: length(0.0),
                height: auto(),
            },
            ..Default::default()
        }),
    );
    let label = tree.insert(
        Some(slot),
        Label::new(text)
            .font_size(11.0)
            .color(Role::Muted)
            .truncation(Truncation::Tail),
    );
    let details = tree.insert(
        Some(row),
        Button::new("詳細").control_size(ControlSize::Small),
    );
    reg.detail_buttons.push((details, pane));
    label
}

pub(super) fn running(
    tree: &mut UiTree,
    column: WidgetId,
    workers: &[&Worker],
    cards: Cards,
    reg: &mut Registry,
) {
    let members: Vec<&Worker> = workers
        .iter()
        .copied()
        .filter(|w| w.state == State::Running)
        .collect();
    let title = panel_title(tree, column, Panel::Running, members.len(), Tone::Success);
    let indicator = indicator(tree, title);
    let scroll = card_list(tree, column, 8.0);
    if cards.filtering && members.is_empty() {
        nothing_matches(tree, scroll);
    }
    let mut items = Vec::new();
    let mut headers = Vec::new();
    for (name, group) in cards.grouping.split(members) {
        let indices: Vec<usize> = (items.len()..items.len() + group.len()).collect();
        if let Some(name) = name {
            // A group's name is a worker's orchestrator, kind or model.
            let header = group_header(tree, scroll, &one_line(name), group.len());
            headers.push((header, indices.clone()));
        }
        let grid = tree.insert(Some(scroll), Container::wrapping_row(8.0, 8.0));
        headers.push((grid, indices));
        for worker in group {
            let card = running_card(tree, grid, worker, cards.output_caption, reg);
            items.push((card, Some(worker.pane)));
        }
    }
    reg.lists[Panel::Running as usize] = Some(ListParts {
        list: scroll,
        items,
        nested: Vec::new(),
        headers,
        indicator,
    });
}

pub(super) fn running_card(
    tree: &mut UiTree,
    grid: WidgetId,
    worker: &Worker,
    output_caption: &str,
    reg: &mut Registry,
) -> WidgetId {
    let pane = worker.pane;
    let (card, body) = card(tree, grid, &worker.label, length(RUNNING_CARD_WIDTH));
    let tags = tree.insert(
        Some(body),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    identity_tags(tree, tags, worker);
    if worker.quiet {
        let id = tree.insert(
            Some(tags),
            Tag::new(quiet_text(worker)).tone(Tone::Critical),
        );
        reg.pulses.push((id, Pulse::QuietTag(pane)));
    }
    task_line(tree, body, worker);
    detail_line(tree, body, worker);

    let meters = tree.insert(
        Some(body),
        Container::row()
            .align_items(AlignItems::END)
            .justify_content(JustifyContent::SPACE_BETWEEN)
            .gap(12.0),
    );
    let output = tree.insert(Some(meters), Container::column().gap(2.0));
    caption(tree, output, output_caption);
    let chart = tree.insert(
        Some(output),
        Chart::line(vec![String::new(); worker.lines_per_tick.len()])
            .series(ChartSeries::new("行", worker.lines_per_tick.clone()))
            .sparkline()
            .width(170.0)
            .height(26.0),
    );
    reg.pulses.push((chart, Pulse::Output(pane)));
    let fresh = tree.insert(Some(meters), Container::column().gap(2.0));
    caption(tree, fresh, "鮮度");
    let level = tree.insert(
        Some(fresh),
        LevelIndicator::new(
            LevelStyle::DiscreteCapacity,
            0.0,
            FRESHNESS_MAX,
            worker.freshness,
        )
        .warning(2.0)
        .critical(1.0)
        .width(80.0),
    );
    reg.pulses.push((level, Pulse::Freshness(pane)));

    let id = caption_row(tree, body, running_caption(worker), pane, reg);
    reg.pulses.push((id, Pulse::RunningCaption(pane)));
    mark_card(tree, card, pane, reg);
    card
}

pub(super) fn done(
    tree: &mut UiTree,
    column: WidgetId,
    workers: &[&Worker],
    cards: Cards,
    reg: &mut Registry,
) {
    let members: Vec<&Worker> = workers
        .iter()
        .copied()
        .filter(|w| w.state == State::Finished)
        .collect();
    let title = panel_title(tree, column, Panel::Done, members.len(), Tone::Neutral);
    let indicator = indicator(tree, title);
    let list = card_list(tree, column, 8.0);
    if cards.filtering && members.is_empty() {
        nothing_matches(tree, list);
    }
    let mut items = Vec::new();
    let mut headers = Vec::new();
    for (name, group) in cards.grouping.split(members) {
        if let Some(name) = name {
            let header = group_header(tree, list, &one_line(name), group.len());
            headers.push((header, (items.len()..items.len() + group.len()).collect()));
        }
        for worker in group {
            let card = done_card(tree, list, worker, reg);
            items.push((card, Some(worker.pane)));
        }
    }
    reg.lists[Panel::Done as usize] = Some(ListParts {
        list,
        items,
        nested: Vec::new(),
        headers,
        indicator,
    });
}

pub(super) fn done_card(
    tree: &mut UiTree,
    list: WidgetId,
    worker: &Worker,
    reg: &mut Registry,
) -> WidgetId {
    let (card, body) = card(tree, list, &worker.label, percent(1.0));
    let tags = tree.insert(
        Some(body),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    identity_tags(tree, tags, worker);
    if worker.has_result {
        tree.insert(Some(tags), Tag::new("結果あり").tone(Tone::Success));
    }
    task_line(tree, body, worker);
    detail_line(tree, body, worker);
    let id = caption_row(tree, body, done_caption(worker), worker.pane, reg);
    reg.pulses.push((id, Pulse::DoneCaption(worker.pane)));
    mark_card(tree, card, worker.pane, reg);
    card
}
