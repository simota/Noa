use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Container, ControlSize, Kbd, Label, SegmentMode, SegmentedControl, Tag, TagStyle, Tone,
    Truncation,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use crate::fleet::{Grouping, Need, Phase, State, Worker, minutes_text};

use super::panel::Panel;
use super::style::{HEADER_HEIGHT, HINT_STRIP_SHRINK, caption, gap, joined, spacer};
use super::{Feed, Pulse};

pub(super) struct Header {
    pub(super) status: WidgetId,
    pub(super) grouping_control: WidgetId,
    pub(super) hints: [WidgetId; 5],
}

pub(super) fn hint_strip(tree: &mut UiTree, bar: WidgetId, shown: &[bool; 5]) -> [WidgetId; 5] {
    let strip = tree.insert(
        Some(bar),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::CENTER,
            gap: gap(8.0),
            // Next to the status label it is the last to give way: the status
            // label takes the whole deficit while it can, and the strip only
            // the rest (clipped), so the left-hand items and the grouping
            // control are never pushed off screen.
            flex_shrink: HINT_STRIP_SHRINK,
            min_size: LayoutSize {
                width: length(0.0),
                height: auto(),
            },
            ..Default::default()
        })
        .clip(true),
    );
    Panel::ALL.map(|panel| {
        // On and off share the label's font, size and padding, so the strip is
        // the same size in either state; on is told apart by a filled pill and
        // the text colour.
        let on = shown[panel as usize];
        let item = Container::row()
            .align_items(AlignItems::CENTER)
            .gap(3.0)
            .padding(2.0)
            .radius(4.0);
        let item = tree.insert(
            Some(strip),
            if on {
                item.background(Role::SurfaceAlt)
            } else {
                item
            },
        );
        tree.insert(Some(item), Kbd::shortcut(panel.shortcut()));
        let label = Label::new(panel.title()).font_size(11.0);
        tree.insert(
            Some(item),
            if on { label } else { label.color(Role::Muted) },
        );
        item
    })
}

pub(super) fn header(
    tree: &mut UiTree,
    root: WidgetId,
    feed: &Feed,
    grouping: Grouping,
    shown: &[bool; 5],
    pulses: &mut Vec<(WidgetId, Pulse)>,
) -> Header {
    let workers = &feed.fleet.workers;
    let bar = tree.insert(
        Some(root),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::CENTER,
            gap: gap(10.0),
            padding: LayoutRect {
                left: length(16.0),
                right: length(16.0),
                top: length(0.0),
                bottom: length(0.0),
            },
            size: LayoutSize {
                width: percent(1.0),
                height: length(HEADER_HEIGHT),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .background(Role::Surface)
        .border(1.0, Role::Separator),
    );
    tree.insert(
        Some(bar),
        Label::new("NOA / AGENTS").font_size(16.0).weight(700),
    );
    let count = |matches: &dyn Fn(&Worker) -> bool| workers.iter().filter(|w| matches(w)).count();
    let tallies = [
        (
            "作業中",
            count(&|w| w.state == State::Running),
            Tone::Success,
        ),
        ("要対応", count(&|w| w.need().is_some()), Tone::Warning),
        (
            "完了",
            count(&|w| w.state == State::Finished),
            Tone::Neutral,
        ),
    ];
    for (name, n, tone) in tallies {
        tree.insert(Some(bar), Tag::new(format!("{name} {n}")).tone(tone));
    }

    let waited = tree.insert(
        Some(bar),
        Tag::new(waited_text(workers)).style(TagStyle::Outlined),
    );
    pulses.push((waited, Pulse::Waited));
    if let Some(worker) = longest(workers) {
        let id = tree.insert(
            Some(bar),
            Tag::new(longest_text(worker))
                .style(TagStyle::Outlined)
                .tone(Tone::Warning),
        );
        pulses.push((id, Pulse::Longest));
    }

    spacer(tree, bar);
    let hints = hint_strip(tree, bar, shown);
    let status_slot = tree.insert(
        Some(bar),
        Container::new(Style {
            flex_shrink: 1.0,
            min_size: LayoutSize {
                width: length(0.0),
                height: auto(),
            },
            ..Default::default()
        }),
    );
    let status = tree.insert(
        Some(status_slot),
        Label::new(feed.status())
            .font_size(12.0)
            .color(Role::Muted)
            .truncation(Truncation::Tail),
    );
    pulses.push((status, Pulse::Status));
    caption(tree, bar, "グループ");
    let selected = Grouping::ALL
        .iter()
        .position(|candidate| *candidate == grouping)
        .unwrap_or(0);
    let control = tree.insert(
        Some(bar),
        SegmentedControl::new(Grouping::ALL.map(Grouping::title), SegmentMode::SelectOne)
            .with_selected(&[selected])
            .control_size(ControlSize::Small),
    );
    Header {
        status,
        grouping_control: control,
        hints,
    }
}

pub(super) fn waited_text(workers: &[Worker]) -> String {
    let waited: f64 = workers
        .iter()
        .map(|w| w.minutes_where(Phase::waits_on_a_person))
        .sum();
    format!("人待ち合計 {}", minutes_text(waited))
}

/// The worker waiting longest on a person. Only these needs have a wait
/// text, known or a lower bound; a 通知 or a quiet worker has none.
pub(super) fn longest(workers: &[Worker]) -> Option<&Worker> {
    workers
        .iter()
        .filter(|w| {
            matches!(
                w.need(),
                Some(Need::Approval | Need::Failure | Need::Question)
            )
        })
        .max_by(|a, b| a.wait_minutes.total_cmp(&b.wait_minutes))
}

/// Longer worker labels are cut to this many characters in the header's
/// "longest" tag, which cannot truncate itself: an uncut pane title there
/// squeezes the link status to nothing.
pub(super) const LONGEST_LABEL_CHARS: usize = 12;

pub(super) fn longest_text(worker: &Worker) -> String {
    let label = if worker.label.chars().count() > LONGEST_LABEL_CHARS {
        let cut: String = worker.label.chars().take(LONGEST_LABEL_CHARS - 1).collect();
        format!("{cut}…")
    } else {
        worker.label.clone()
    };
    joined(&["最長", &label, &worker.since], " ")
}
