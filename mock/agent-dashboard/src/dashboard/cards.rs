use noa_gpui_core::style::{AlignItems, JustifyContent};
use noa_gpui_core::widgets::{
    Chart, ChartSeries, Container, GroupBox, Label, LevelIndicator, LevelStyle, Tag, TagStyle, Tone,
};
use noa_gpui_core::{Role, UiTree, WidgetId};

use crate::fleet::{FRESHNESS_MAX, Grouping, State, Worker, minutes_text};

use super::Pulse;
use super::panel::{card_list, group_header, panel_title};
use super::style::{caption, joined};

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

pub(super) fn task_line(tree: &mut UiTree, parent: WidgetId, worker: &Worker) {
    if worker.task.is_empty() {
        return;
    }
    tree.insert(
        Some(parent),
        Label::new(format!("依頼: {}", worker.task))
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
        Label::new(worker.detail.as_str())
            .font_size(12.0)
            .wrap(true),
    );
}

/// The kind, model and branch tags every card starts with; no branch tag
/// when the branch is unknown.
pub(super) fn identity_tags(tree: &mut UiTree, tags: WidgetId, worker: &Worker) {
    tree.insert(
        Some(tags),
        Tag::new(worker.kind.as_str()).tone(Tone::Accent),
    );
    model_tag(tree, tags, worker);
    branch_tag(tree, tags, worker);
}

/// A plain command has no model to show.
pub(super) fn model_tag(tree: &mut UiTree, parent: WidgetId, worker: &Worker) {
    if worker.is_agent() {
        tree.insert(Some(parent), Tag::new(worker.model_line()));
    }
}

pub(super) fn branch_tag(tree: &mut UiTree, parent: WidgetId, worker: &Worker) {
    if !worker.branch.is_empty() {
        tree.insert(
            Some(parent),
            Tag::new(worker.branch.as_str()).style(TagStyle::Outlined),
        );
    }
}

/// How the running and done panels lay their cards out.
#[derive(Clone, Copy)]
pub(super) struct Cards {
    pub(super) grouping: Grouping,
    pub(super) output_caption: &'static str,
}

pub(super) fn running(
    tree: &mut UiTree,
    column: WidgetId,
    workers: &[Worker],
    cards: Cards,
    pulses: &mut Vec<(WidgetId, Pulse)>,
) {
    let members: Vec<&Worker> = workers
        .iter()
        .filter(|w| w.state == State::Running)
        .collect();
    panel_title(tree, column, "作業中", members.len(), Tone::Success);
    let scroll = card_list(tree, column);
    for (name, group) in cards.grouping.split(members) {
        if let Some(name) = name {
            group_header(tree, scroll, name, group.len());
        }
        let grid = tree.insert(Some(scroll), Container::wrapping_row(8.0, 8.0));
        for worker in group {
            running_card(tree, grid, worker, cards.output_caption, pulses);
        }
    }
}

pub(super) fn running_card(
    tree: &mut UiTree,
    grid: WidgetId,
    worker: &Worker,
    output_caption: &str,
    pulses: &mut Vec<(WidgetId, Pulse)>,
) {
    let pane = worker.pane;
    let card = tree.insert(
        Some(grid),
        GroupBox::card(worker.label.as_str()).width(RUNNING_CARD_WIDTH),
    );
    let tags = tree.insert(
        Some(card),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    identity_tags(tree, tags, worker);
    if worker.quiet {
        let id = tree.insert(
            Some(tags),
            Tag::new(quiet_text(worker)).tone(Tone::Critical),
        );
        pulses.push((id, Pulse::QuietTag(pane)));
    }
    task_line(tree, card, worker);
    detail_line(tree, card, worker);

    let meters = tree.insert(
        Some(card),
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
    pulses.push((chart, Pulse::Output(pane)));
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
    pulses.push((level, Pulse::Freshness(pane)));

    let id = caption(tree, card, running_caption(worker));
    pulses.push((id, Pulse::RunningCaption(pane)));
}

pub(super) fn done(
    tree: &mut UiTree,
    column: WidgetId,
    workers: &[Worker],
    grouping: Grouping,
    pulses: &mut Vec<(WidgetId, Pulse)>,
) {
    let members: Vec<&Worker> = workers
        .iter()
        .filter(|w| w.state == State::Finished)
        .collect();
    panel_title(tree, column, "完了", members.len(), Tone::Neutral);
    let list = card_list(tree, column);
    for (name, group) in grouping.split(members) {
        if let Some(name) = name {
            group_header(tree, list, name, group.len());
        }
        for worker in group {
            done_card(tree, list, worker, pulses);
        }
    }
}

pub(super) fn done_card(
    tree: &mut UiTree,
    list: WidgetId,
    worker: &Worker,
    pulses: &mut Vec<(WidgetId, Pulse)>,
) {
    let card = tree.insert(
        Some(list),
        GroupBox::card(worker.label.as_str()).fill_width(),
    );
    let tags = tree.insert(
        Some(card),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    identity_tags(tree, tags, worker);
    if worker.has_result {
        tree.insert(Some(tags), Tag::new("結果あり").tone(Tone::Success));
    }
    task_line(tree, card, worker);
    detail_line(tree, card, worker);
    let id = caption(tree, card, done_caption(worker));
    pulses.push((id, Pulse::DoneCaption(worker.pane)));
}
