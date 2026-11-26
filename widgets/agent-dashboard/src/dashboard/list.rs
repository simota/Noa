//! The worker list (dashboard-worker-list AC1–AC8): one row per worker
//! pane, what waits on a person first. A pane can be given more tasks: a
//! row counts panes, not tasks or jobs done.
//!
//! `rows` decides who is in a scope and in what order; `build` lays the
//! list out (its toolbar, then one fixed-height row per worker). The order
//! is decided only when the list is built: `Dashboard::refresh` updates a
//! row's time texts in place (`Pulse::RowTime`, `Pulse::RowQuiet`) and
//! moves nothing, so a clock tick never reorders the rows.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length,
};
use noa_gpui_core::widgets::{
    Button, Container, ControlSize, Label, SegmentMode, SegmentedControl, Tag, Tone, Truncation,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use crate::fleet::{Fleet, Scope, Status, Worker, minutes_text, one_line};

use super::filter::Filter;
use super::pager::ListParts;
use super::panel::{card_list, indicator, mark_card};
use super::style::{
    ORANGE, caption, fill_remaining, fixed_width, gap, joined, spacer, status_color,
};
use super::{Pulse, Registry, View};

/// Every row's height, whatever its texts: 14 px and 12 px lines in 8 px
/// of padding above and below.
pub(super) const ROW_HEIGHT: f32 = 60.0;
const ROW_GAP: f32 = 4.0;
const STATUS_WIDTH: f32 = 112.0;
const AGENT_WIDTH: f32 = 168.0;
const TIME_WIDTH: f32 = 104.0;
const ACTION_WIDTH: f32 = 104.0;

/// The qualifier of a worker whose pane is gone, rebuilt from its journal.
pub(super) const CLOSED_TAG: &str = "閉じた";
/// What a row says in place of a task never sent (never the label: the
/// label has its own column).
pub(super) const NO_TASK: &str = "作業内容未取得";

/// The one thing a row's button does (AC8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowAction {
    /// An approval: focus the pane (nothing approves from here).
    OpenPane,
    /// An input: the drawer, with its send field focused.
    StartSend,
    /// The drawer.
    Detail,
    /// The drawer, with the result file read.
    Result,
}

impl RowAction {
    pub(super) fn of(worker: &Worker) -> Self {
        match worker.status() {
            Status::Approval => RowAction::OpenPane,
            Status::Input => RowAction::StartSend,
            Status::Failed | Status::Running => RowAction::Detail,
            Status::Ended if worker.result.is_some() => RowAction::Result,
            Status::Ended => RowAction::Detail,
        }
    }

    pub(super) fn title(self, status: Status) -> &'static str {
        match (self, status) {
            (RowAction::OpenPane, _) => "承認画面へ",
            (RowAction::StartSend, _) => "入力する",
            (RowAction::Result, _) => "結果を見る",
            (RowAction::Detail, Status::Failed) => "確認する",
            (RowAction::Detail, _) => "詳細",
        }
    }
}

/// The workers of `scope` the filter shows, in the list's order. 現在 and
/// 要対応: what needs attention first, by status (approval, input,
/// failure), the longest wait first, then the pane; then what runs, by its
/// parent group (in the order of `fleet.orchestrators`), the earliest
/// started first (unknown starts last), then the pane. 終了: the open
/// workers that ended, then the closed ones, each in the fleet's order
/// (closed ones come newest-ended first).
pub(super) fn rows<'a>(fleet: &'a Fleet, scope: Scope, filter: &Filter) -> Vec<&'a Worker> {
    let mut rows: Vec<&Worker> = fleet
        .workers
        .iter()
        .filter(|w| w.in_scope(scope) && filter.matches(w))
        .collect();
    let group = |w: &Worker| {
        let at = fleet
            .orchestrators
            .iter()
            .position(|o| o.name == w.orchestrator && o.pane == w.orchestrator_pane);
        at.unwrap_or(usize::MAX)
    };
    match scope {
        Scope::Current | Scope::Attention => rows.sort_by(|a, b| {
            let rank = a.status().rank().cmp(&b.status().rank());
            let order = if a.needs_attention() {
                b.wait_minutes.total_cmp(&a.wait_minutes)
            } else {
                let started = |w: &Worker| w.started_minutes.unwrap_or(f64::NEG_INFINITY);
                group(a)
                    .cmp(&group(b))
                    .then(started(b).total_cmp(&started(a)))
            };
            rank.then(order).then(a.pane.cmp(&b.pane))
        }),
        Scope::Ended => rows.sort_by_key(|w| w.is_closed()),
    }
    rows
}

/// How many workers the filter shows in each scope, in `Scope::ALL`
/// order: what the switch says, and the rows each view lays out.
pub(super) fn counts(fleet: &Fleet, filter: &Filter) -> [usize; 3] {
    Scope::ALL.map(|scope| {
        let shown = fleet.workers.iter().filter(|w| filter.matches(w));
        shown.filter(|w| w.in_scope(scope)).count()
    })
}

/// The workers that need attention but the filter hides (AC7).
pub(super) fn outside_attention(fleet: &Fleet, filter: &Filter) -> usize {
    let attention = fleet.workers.iter().filter(|w| w.needs_attention());
    attention.filter(|w| !filter.matches(w)).count()
}

pub(super) fn outside_text(n: usize) -> String {
    format!("絞り込み外に要対応{n}件")
}

/// What a view of `scope` says with no row: the filter's 該当なし, or the
/// scope's own words.
pub(super) fn empty_text(scope: Scope, filtering: bool) -> &'static str {
    match scope {
        _ if filtering => "該当なし",
        Scope::Current => "作業中のワーカーはいません",
        Scope::Attention => "要対応はありません",
        Scope::Ended => "終了したワーカーはありません",
    }
}

/// The row's qualifiers under its status: 閉じた, 結果あり, 出力未変化 N 分.
pub(super) fn qualifiers(worker: &Worker) -> String {
    let quiet = if worker.quiet && worker.status() == Status::Running {
        format!("出力未変化 {}", minutes_text(worker.wait_minutes))
    } else {
        String::new()
    };
    let closed = if worker.is_closed() { CLOSED_TAG } else { "" };
    let result = if worker.has_result {
        "結果あり"
    } else {
        ""
    };
    joined(&[closed, result, &quiet], " · ")
}

/// The row's wait or last update: 「待ち …」 for one that needs attention,
/// else what the feed says of its output (`since`); 「—」 when unknown.
pub(super) fn time_text(worker: &Worker) -> String {
    let since = one_line(&worker.since);
    match since.as_str() {
        "" => "—".to_owned(),
        _ if worker.needs_attention() => format!("待ち {since}"),
        _ => since,
    }
}

/// The row's second line: the latest report, then the parent.
pub(super) fn report_text(fleet: &Fleet, worker: &Worker) -> String {
    let parent = format!("親 {}", fleet.parent_name(worker));
    joined(&[&one_line(&worker.detail), &parent], " · ")
}

/// The switch's segment titles: 「現在 N」「要対応 N」「終了 N」.
pub(super) fn scope_titles(counts: [usize; 3]) -> [String; 3] {
    std::array::from_fn(|at| format!("{} {}", Scope::ALL[at].title(), counts[at]))
}

/// Builds the list into `body`: a panel with the toolbar (the scope switch,
/// the out-of-filter notice, the pager's indicator, 分析) and the rows of
/// `view.scope`.
pub(super) fn build(
    tree: &mut UiTree,
    body: WidgetId,
    fleet: &Fleet,
    view: &View,
    filter: &Filter,
    reg: &mut Registry,
) {
    let panel = tree.insert(
        Some(body),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(8.0),
            padding: LayoutRect::length(10.0),
            ..fill_remaining()
        })
        .background(Role::Surface)
        .border(1.0, Role::Separator)
        .radius(12.0),
    );
    let toolbar = tree.insert(
        Some(panel),
        Container::row().align_items(AlignItems::CENTER).gap(10.0),
    );
    let counts = counts(fleet, filter);
    let at = Scope::ALL
        .iter()
        .position(|s| *s == view.scope)
        .unwrap_or(0);
    let switch = tree.insert(
        Some(toolbar),
        SegmentedControl::new(scope_titles(counts), SegmentMode::SelectOne)
            .with_selected(&[at])
            .control_size(ControlSize::Small),
    );
    reg.scope_control = Some(switch);
    let outside = outside_attention(fleet, filter);
    if filter.active() && outside > 0 {
        let note = tree.insert(
            Some(toolbar),
            Label::new(outside_text(outside))
                .font_size(12.0)
                .weight(700)
                .color(ORANGE),
        );
        reg.outside_note = Some(note);
        let clear = tree.insert(
            Some(toolbar),
            Button::new("絞り込みを解除").control_size(ControlSize::Small),
        );
        reg.clear_filter = Some(clear);
    }
    spacer(tree, toolbar);
    let indicator = indicator(tree, toolbar);
    reg.analysis_button = Some(tree.insert(
        Some(toolbar),
        Button::new("分析").control_size(ControlSize::Small),
    ));

    let list = card_list(tree, panel, ROW_GAP);
    let rows = rows(fleet, view.scope, filter);
    if rows.is_empty() {
        caption(tree, list, empty_text(view.scope, filter.active()));
    }
    let items = rows
        .iter()
        .map(|worker| (row(tree, list, fleet, worker, reg), worker.pane))
        .collect();
    reg.list = Some(ListParts {
        list,
        items,
        indicator,
    });
}

/// A row's column of fixed width.
fn column(tree: &mut UiTree, row: WidgetId, width: f32) -> WidgetId {
    tree.insert(
        Some(row),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(2.0),
            min_size: LayoutSize {
                width: length(0.0),
                height: auto(),
            },
            ..fixed_width(width)
        }),
    )
}

/// One line of a row's text, cut at its column's edge.
fn line(text: impl Into<String>, size: f32) -> Label {
    Label::new(text)
        .font_size(size)
        .truncation(Truncation::Tail)
}

/// One worker's row: 状態, 作業・最新の報告, エージェント, 待ち／更新, 操作.
/// A focus group named by the worker's label, with the selection bar of a
/// card; the row registers itself for a click that selects it, its time
/// texts for `refresh`, and its button.
fn row(
    tree: &mut UiTree,
    list: WidgetId,
    fleet: &Fleet,
    worker: &Worker,
    reg: &mut Registry,
) -> WidgetId {
    let pane = worker.pane;
    let status = worker.status();
    let row = tree.insert(
        Some(list),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::CENTER,
            gap: gap(12.0),
            padding: LayoutRect {
                left: length(12.0),
                right: length(12.0),
                top: length(8.0),
                bottom: length(8.0),
            },
            size: LayoutSize {
                width: auto(),
                height: length(ROW_HEIGHT),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .background(Role::SurfaceAlt)
        .radius(8.0)
        .focus_group(true),
    );
    tree.set_accessible_label(row, &worker.label);

    let state = column(tree, row, STATUS_WIDTH);
    let title = line(status.title(), 14.0);
    let title = match status_color(status) {
        Some(color) => title.weight(700).color(color),
        None => title.color(Role::Muted),
    };
    tree.insert(Some(state), title);
    let qualifiers = qualifiers(worker);
    if !qualifiers.is_empty() {
        let id = tree.insert(Some(state), line(qualifiers, 12.0).color(Role::Muted));
        if worker.quiet && status == Status::Running {
            reg.pulses.push((id, Pulse::RowQuiet(pane)));
        }
    }

    let work = tree.insert(
        Some(row),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(2.0),
            ..fill_remaining()
        }),
    );
    let task = match worker.task.as_str() {
        "" => line(NO_TASK, 14.0).color(Role::Muted),
        task => line(one_line(task), 14.0),
    };
    tree.insert(Some(work), task);
    tree.insert(
        Some(work),
        line(report_text(fleet, worker), 12.0).color(Role::Muted),
    );

    let agent = tree.insert(
        Some(row),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::CENTER,
            gap: gap(6.0),
            min_size: LayoutSize {
                width: length(0.0),
                height: auto(),
            },
            ..fixed_width(AGENT_WIDTH)
        })
        .clip(true),
    );
    tree.insert(
        Some(agent),
        Tag::new(one_line(&worker.kind)).tone(Tone::Accent),
    );
    let label_slot = tree.insert(Some(agent), Container::new(fill_remaining()));
    tree.insert(Some(label_slot), line(one_line(&worker.label), 12.0));

    let time = column(tree, row, TIME_WIDTH);
    let time_label = line(time_text(worker), 12.0);
    let time_label = match status_color(status).filter(|_| worker.needs_attention()) {
        Some(color) => time_label.weight(700).color(color),
        None => time_label.color(Role::Muted),
    };
    let id = tree.insert(Some(time), time_label);
    reg.pulses.push((id, Pulse::RowTime(pane)));

    let action = RowAction::of(worker);
    let slot = column(tree, row, ACTION_WIDTH);
    let button = tree.insert(
        Some(slot),
        Button::new(action.title(status)).control_size(ControlSize::Small),
    );
    reg.row_actions.push((button, pane, action));
    reg.rows.push((row, pane));
    mark_card(tree, row, pane, reg);
    row
}
