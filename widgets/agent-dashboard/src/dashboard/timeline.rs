use noa_gpui_core::style::{
    AlignItems, FlexDirection, JustifyContent, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Badge, Container, Label, Tone, Truncation};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use crate::fleet::{Grouping, Phase, Worker, minutes_text, one_line};

use super::pager::ListParts;
use super::panel::{Panel, card_list, group_header, indicator, nothing_matches, title_row};
use super::style::{caption, fill_remaining, phase_color, spacer};
use super::{Mark, Registry};

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
    pub(super) indicator: WidgetId,
}

pub(super) fn span_text(span: f64) -> String {
    format!("← {}前　　いま →", minutes_text(span))
}

pub(super) fn timeline_span(workers: &[&Worker]) -> f64 {
    workers
        .iter()
        .map(|w| w.total_minutes())
        .fold(0.0, f64::max)
}

/// The timeline panel over `workers` (those the filter lets through); its
/// span is the longest of theirs.
pub(super) fn timeline(
    tree: &mut UiTree,
    section: WidgetId,
    workers: &[&Worker],
    grouping: Grouping,
    filtering: bool,
    reg: &mut Registry,
) {
    let span = timeline_span(workers);

    let title = title_row(tree, section, Panel::Timeline, 12.0);
    // One lane per worker shown, counted like the other panels' cards.
    tree.insert(
        Some(title),
        Badge::count(workers.len() as u32).tone(Tone::Neutral),
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
    let indicator = indicator(tree, title);

    let lanes = card_list(tree, section, 5.0);
    let parts = TimelineParts {
        span: span_caption,
        lanes,
        indicator,
    };
    fill_lanes(tree, &parts, workers, grouping, filtering, span, reg);
    reg.timeline = Some(parts);
}

/// Makes the lanes and registers them (pager items, selection marks);
/// says so when the filter (on when `filtering`) leaves none.
pub(super) fn fill_lanes(
    tree: &mut UiTree,
    parts: &TimelineParts,
    workers: &[&Worker],
    grouping: Grouping,
    filtering: bool,
    span: f64,
    reg: &mut Registry,
) {
    if filtering && workers.is_empty() {
        nothing_matches(tree, parts.lanes);
    }
    let mut items = Vec::new();
    let mut headers = Vec::new();
    for (name, group) in grouping.split(workers.iter().copied()) {
        if let Some(name) = name {
            let header = group_header(tree, parts.lanes, name, group.len());
            headers.push((header, (items.len()..items.len() + group.len()).collect()));
        }
        for worker in group {
            let row = lane(tree, parts.lanes, worker, span);
            reg.marks.push((row, worker.pane, Mark::Lane));
            items.push((row, Some(worker.pane)));
        }
    }
    reg.lists[Panel::Timeline as usize] = Some(ListParts {
        list: parts.lanes,
        items,
        nested: Vec::new(),
        headers,
        indicator: parts.indicator,
    });
}

/// One lane; returns its row, which the selection fills.
pub(super) fn lane(tree: &mut UiTree, lanes: WidgetId, worker: &Worker, span: f64) -> WidgetId {
    let row = tree.insert(
        Some(lanes),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::CENTER,
            gap: super::style::gap(8.0),
            padding: noa_gpui_core::style::Rect {
                left: length(6.0),
                right: length(6.0),
                top: length(2.0),
                bottom: length(2.0),
            },
            ..Default::default()
        })
        .radius(4.0),
    );
    let name = tree.insert(Some(row), Container::row().width(LANE_LABEL_WIDTH));
    tree.insert(
        Some(name),
        Label::new(one_line(&worker.label))
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
        .border(1.0, Role::Separator)
        .radius(4.0)
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
    row
}
