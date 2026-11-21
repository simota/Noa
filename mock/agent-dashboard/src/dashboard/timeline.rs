use noa_gpui_core::style::{
    AlignItems, FlexDirection, JustifyContent, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Container, Label, ScrollView, Truncation};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use crate::fleet::{Grouping, Phase, Worker, minutes_text};

use super::panel::group_header;
use super::style::{caption, fill_remaining, gap, phase_color, spacer};

pub(super) const LANE_LABEL_WIDTH: f32 = 104.0;
pub(super) const LANE_TOTAL_WIDTH: f32 = 44.0;
pub(super) const LANE_HEIGHT: f32 = 12.0;
/// What a full lane's grow factors add up to. Grow factors that add up to
/// less than 1 hand out only that fraction of the free space, so a lane is
/// scaled to this whatever its span (a span under a minute is under 1).
pub(super) const LANE_SHARES: f64 = 1000.0;

pub(super) struct TimelineParts {
    pub(super) span: WidgetId,
    pub(super) lanes: WidgetId,
}

pub(super) fn span_text(span: f64) -> String {
    format!("← {}前　　いま →", minutes_text(span))
}

pub(super) fn timeline_span(workers: &[Worker]) -> f64 {
    workers
        .iter()
        .map(Worker::total_minutes)
        .fold(0.0, f64::max)
}

pub(super) fn timeline(
    tree: &mut UiTree,
    section: WidgetId,
    workers: &[Worker],
    grouping: Grouping,
) -> TimelineParts {
    let span = timeline_span(workers);

    let title = tree.insert(
        Some(section),
        Container::row().align_items(AlignItems::CENTER).gap(12.0),
    );
    tree.insert(
        Some(title),
        Label::new("状態の推移")
            .font_size(12.0)
            .weight(700)
            .color(Role::Muted),
    );
    for phase in Phase::LEGEND {
        let item = tree.insert(
            Some(title),
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
    spacer(tree, title);
    let span_caption = caption(tree, title, span_text(span));

    let lanes = tree.insert(
        Some(section),
        ScrollView::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(5.0),
            ..fill_remaining()
        }),
    );
    fill_lanes(tree, lanes, workers, grouping, span);
    TimelineParts {
        span: span_caption,
        lanes,
    }
}

pub(super) fn fill_lanes(
    tree: &mut UiTree,
    lanes: WidgetId,
    workers: &[Worker],
    grouping: Grouping,
    span: f64,
) {
    for (name, group) in grouping.split(workers) {
        if let Some(name) = name {
            group_header(tree, lanes, name, group.len());
        }
        for worker in group {
            lane(tree, lanes, worker, span);
        }
    }
}

pub(super) fn lane(tree: &mut UiTree, lanes: WidgetId, worker: &Worker, span: f64) {
    let row = tree.insert(
        Some(lanes),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let name = tree.insert(Some(row), Container::row().width(LANE_LABEL_WIDTH));
    tree.insert(
        Some(name),
        Label::new(worker.label.as_str())
            .font_size(11.0)
            .truncation(Truncation::Tail),
    );

    let track = tree.insert(
        Some(row),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            size: LayoutSize {
                width: auto(),
                height: length(LANE_HEIGHT),
            },
            ..fill_remaining()
        })
        .background(Role::Background)
        .radius(3.0)
        .clip(true),
    );
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
    let before_start = span - worker.total_minutes();
    if before_start > 0.0 {
        tree.insert(Some(track), Container::new(segment(before_start)));
    }
    for (phase, minutes) in &worker.phases {
        tree.insert(
            Some(track),
            Container::new(segment(*minutes)).background(phase_color(*phase)),
        );
    }

    let total = tree.insert(
        Some(row),
        Container::row()
            .width(LANE_TOTAL_WIDTH)
            .justify_content(JustifyContent::END),
    );
    caption(tree, total, minutes_text(worker.total_minutes()));
}
