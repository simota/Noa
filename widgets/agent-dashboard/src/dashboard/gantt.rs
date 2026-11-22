//! Full-screen Timeline / Gantt tab: concurrency & human-wait congestion chart
//! synced above an expanded per-worker Gantt view with time scale selection.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, JustifyContent, Rect as LayoutRect, Size as LayoutSize, auto,
    length, percent,
};
use noa_gpui_core::widgets::{
    Chart, ChartSeries, Container, ControlSize, Label, ScrollView, SegmentMode, SegmentedControl,
    Tag, Tone, Truncation,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use crate::fleet::{Grouping, Phase, Worker, minutes_text, one_line};

use super::graph::mark;
use super::panel::group_header;
use super::style::{GREEN, ORANGE, caption, fill_remaining, gap, phase_color, spacer};
use super::timeline::{LANE_SHARES, span_text, timeline_span};
use super::{Mark, Registry};

const CONCURRENCY_BUCKETS: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum TimeScale {
    #[default]
    All,
    Hour,
    Quarter,
}

impl TimeScale {
    pub(super) const ALL: [TimeScale; 3] = [TimeScale::All, TimeScale::Hour, TimeScale::Quarter];

    pub(super) fn title(self) -> &'static str {
        match self {
            TimeScale::All => "全期間",
            TimeScale::Hour => "60 分",
            TimeScale::Quarter => "15 分",
        }
    }

    pub(super) fn window_minutes(self, full_span: f64) -> f64 {
        match self {
            TimeScale::All => full_span,
            TimeScale::Hour => 60.0,
            TimeScale::Quarter => 15.0,
        }
    }
}

/// Concurrency series over `CONCURRENCY_BUCKETS` time slices ending at "now".
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Concurrency {
    pub(super) running: Vec<f64>,
    pub(super) waiting: Vec<f64>,
    pub(super) peak_running: usize,
    pub(super) peak_waiting: usize,
}

/// Samples each worker's phase timeline across `CONCURRENCY_BUCKETS` points in
/// `[now - span, now]`.
pub(super) fn compute_concurrency(workers: &[&Worker], span: f64) -> Concurrency {
    let mut running = vec![0.0; CONCURRENCY_BUCKETS];
    let mut waiting = vec![0.0; CONCURRENCY_BUCKETS];
    if span <= 0.0 || workers.is_empty() {
        return Concurrency {
            running,
            waiting,
            peak_running: 0,
            peak_waiting: 0,
        };
    }
    let step = span / CONCURRENCY_BUCKETS as f64;
    for worker in workers {
        let total = worker.total_minutes();
        let start = span - total;
        for bucket in 0..CONCURRENCY_BUCKETS {
            let t = (bucket as f64 + 0.5) * step;
            if t < start {
                continue;
            }
            let offset = t - start;
            if let Some(phase) = phase_at(worker, offset) {
                match phase {
                    Phase::Running => running[bucket] += 1.0,
                    Phase::Permission | Phase::Input => waiting[bucket] += 1.0,
                    Phase::Quiet | Phase::Ended => {}
                }
            }
        }
    }
    let peak_running = running.iter().copied().fold(0.0_f64, f64::max) as usize;
    let peak_waiting = waiting.iter().copied().fold(0.0_f64, f64::max) as usize;
    Concurrency {
        running,
        waiting,
        peak_running,
        peak_waiting,
    }
}

fn phase_at(worker: &Worker, offset_from_start: f64) -> Option<Phase> {
    let mut cursor = 0.0;
    for (phase, mins) in &worker.phases {
        let dur = mins.max(0.0);
        if offset_from_start <= cursor + dur {
            return Some(*phase);
        }
        cursor += dur;
    }
    worker.phases.last().map(|(p, _)| *p)
}

/// Clips a worker's phases to the trailing `window` minutes ending at "now".
fn clipped_phases(worker: &Worker, window: f64) -> Vec<(Phase, f64)> {
    let total = worker.total_minutes();
    let mut drop_before = (total - window).max(0.0);
    let mut out = Vec::new();
    for &(phase, mins) in &worker.phases {
        let dur = mins.max(0.0);
        if drop_before >= dur {
            drop_before -= dur;
            continue;
        }
        let kept = dur - drop_before;
        drop_before = 0.0;
        if kept > 0.0 {
            out.push((phase, kept));
        }
    }
    out
}

/// Builds the full-body Timeline / Gantt tab into `body`; returns the scale
/// selector `SegmentedControl` widget id.
pub(super) fn build(
    tree: &mut UiTree,
    body: WidgetId,
    workers: &[&Worker],
    grouping: Grouping,
    scale: TimeScale,
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

    let full_span = timeline_span(workers);
    let span = scale.window_minutes(full_span);

    // Top toolbar: scale control + legend + span text.
    let toolbar = tree.insert(
        Some(shell),
        Container::row().align_items(AlignItems::CENTER).gap(12.0),
    );
    tree.insert(
        Some(toolbar),
        Label::new("タイムライン・並列度")
            .font_size(13.0)
            .weight(700),
    );
    let selected_idx = TimeScale::ALL.iter().position(|s| *s == scale).unwrap_or(0);
    let scale_control = tree.insert(
        Some(toolbar),
        SegmentedControl::new(TimeScale::ALL.map(TimeScale::title), SegmentMode::SelectOne)
            .with_selected(&[selected_idx])
            .control_size(ControlSize::Small),
    );
    for phase in Phase::LEGEND {
        let item = tree.insert(
            Some(toolbar),
            Container::row().align_items(AlignItems::CENTER).gap(4.0),
        );
        tree.insert(
            Some(item),
            Container::row()
                .width(10.0)
                .height(10.0)
                .background(phase_color(phase))
                .radius(2.0),
        );
        caption(tree, item, phase.title());
    }
    spacer(tree, toolbar);
    caption(tree, toolbar, span_text(span));

    if workers.is_empty() {
        caption(tree, shell, "表示するワーカーがありません");
        return scale_control;
    }

    // Concurrency & congestion summary strip.
    let conc = compute_concurrency(workers, span);
    let chart_row = tree.insert(
        Some(shell),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::STRETCH,
            gap: gap(12.0),
            flex_shrink: 0.0,
            ..Default::default()
        }),
    );
    concurrency_card(
        tree,
        chart_row,
        "同時稼働数（作業中）",
        format!("最大 {} 体", conc.peak_running),
        conc.running,
        GREEN,
    );
    concurrency_card(
        tree,
        chart_row,
        "人待ち渋滞数（承認・入力待ち）",
        format!("最大 {} 体", conc.peak_waiting),
        conc.waiting,
        ORANGE,
    );

    // Full-height scrollable Gantt lanes.
    let lanes = tree.insert(
        Some(shell),
        ScrollView::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(6.0),
            ..fill_remaining()
        }),
    );

    for (name, group) in grouping.split(workers.iter().copied()) {
        if let Some(name) = name {
            group_header(tree, lanes, name, group.len());
        }
        for worker in group {
            gantt_row(tree, lanes, worker, span, reg);
        }
    }

    scale_control
}

fn concurrency_card(
    tree: &mut UiTree,
    parent: WidgetId,
    title: &str,
    peak: String,
    values: Vec<f64>,
    color: noa_gpui_scene::Color,
) {
    let card = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(6.0),
            padding: LayoutRect {
                left: length(12.0),
                right: length(12.0),
                top: length(8.0),
                bottom: length(8.0),
            },
            ..fill_remaining()
        })
        .background(Role::SurfaceAlt)
        .border(1.0, Role::Separator)
        .radius(8.0),
    );
    let head = tree.insert(
        Some(card),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    tree.insert(
        Some(head),
        Container::row()
            .width(8.0)
            .height(8.0)
            .background(color)
            .radius(4.0),
    );
    caption(tree, head, title);
    spacer(tree, head);
    tree.insert(
        Some(head),
        Label::new(peak).font_size(12.0).weight(700).color(color),
    );
    tree.insert(
        Some(card),
        Chart::line(vec![String::new(); values.len()])
            .series(ChartSeries::new(title, values))
            .sparkline()
            .width(500.0)
            .height(44.0),
    );
}

fn gantt_row(tree: &mut UiTree, lanes: WidgetId, worker: &Worker, span: f64, reg: &mut Registry) {
    let (symbol, color) = mark(worker);
    let row = tree.insert(
        Some(lanes),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::CENTER,
            gap: gap(10.0),
            padding: LayoutRect {
                left: length(8.0),
                right: length(8.0),
                top: length(5.0),
                bottom: length(5.0),
            },
            ..Default::default()
        })
        .background(Role::SurfaceAlt)
        .border(1.0, Role::Separator)
        .radius(6.0),
    );
    reg.marks.push((row, worker.pane, Mark::Lane));

    tree.insert(
        Some(row),
        Label::new(symbol).font_size(12.0).weight(700).color(color),
    );
    let name_col = tree.insert(Some(row), Container::row().width(120.0));
    tree.insert(
        Some(name_col),
        Label::new(one_line(&worker.label))
            .font_size(12.0)
            .weight(600)
            .truncation(Truncation::Tail),
    );
    tree.insert(
        Some(row),
        Tag::new(one_line(&worker.kind)).tone(Tone::Accent),
    );

    let track = tree.insert(
        Some(row),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            size: LayoutSize {
                width: auto(),
                height: length(18.0),
            },
            ..fill_remaining()
        })
        .background(Role::Background)
        .border(1.0, Role::Separator)
        .radius(4.0)
        .clip(true),
    );
    let phases = clipped_phases(worker, span);
    let shown_total: f64 = phases.iter().map(|(_, m)| *m).sum();
    let scale = if span > 0.0 { LANE_SHARES / span } else { 0.0 };
    let segment = |minutes: f64| Style {
        flex_grow: (minutes * scale) as f32,
        flex_basis: length(0.0),
        size: LayoutSize {
            width: auto(),
            height: percent(1.0),
        },
        ..Default::default()
    };
    let before = (span - shown_total).max(0.0);
    if before > 0.0 {
        tree.insert(Some(track), Container::new(segment(before)));
    }
    for (phase, minutes) in phases {
        tree.insert(
            Some(track),
            Container::new(segment(minutes)).background(phase_color(phase)),
        );
    }

    let waited = worker.minutes_where(Phase::waits_on_a_person);
    let stats = tree.insert(
        Some(row),
        Container::row()
            .width(116.0)
            .justify_content(JustifyContent::END)
            .gap(6.0),
    );
    caption(tree, stats, minutes_text(worker.total_minutes()));
    if waited > 0.0 {
        tree.insert(
            Some(stats),
            Label::new(format!("待 {}", minutes_text(waited)))
                .font_size(11.0)
                .weight(600)
                .color(ORANGE),
        );
    }
}
