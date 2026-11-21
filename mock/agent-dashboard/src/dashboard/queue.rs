use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Tag, Tone, Truncation};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use crate::fleet::{Need, Worker};

use super::Pulse;
use super::cards::{branch_tag, model_tag, task_line};
use super::panel::{card_list, panel_title};
use super::style::{HINT_STRIP_SHRINK, fill_remaining, gap, joined, need_color, spacer};

#[derive(Default)]
pub(super) struct QueueButtons {
    pub(super) open: Vec<(WidgetId, u64)>,
    pub(super) details: Vec<(WidgetId, u64)>,
    /// The card shown expanded, whichever it is.
    pub(super) expanded: Option<u64>,
}

impl QueueButtons {
    pub(super) fn worker(buttons: &[(WidgetId, u64)], source: WidgetId) -> Option<u64> {
        buttons
            .iter()
            .find(|(id, _)| *id == source)
            .map(|(_, pane)| *pane)
    }
}

pub(super) fn queue_since(worker: &Worker, need: Need) -> String {
    joined(&[need.title(), &worker.since], " ")
}

pub(super) fn queue(
    tree: &mut UiTree,
    rail: WidgetId,
    workers: &[Worker],
    expanded: Option<u64>,
    pulses: &mut Vec<(WidgetId, Pulse)>,
) -> QueueButtons {
    let mut waiting: Vec<(&Worker, Need)> = workers
        .iter()
        .filter_map(|w| w.need().map(|need| (w, need)))
        .collect();
    waiting.sort_by(|(a, a_need), (b, b_need)| {
        a_need
            .rank()
            .cmp(&b_need.rank())
            .then(b.wait_minutes.total_cmp(&a.wait_minutes))
    });
    let expanded = expanded
        .filter(|pane| waiting.iter().any(|(w, _)| w.pane == *pane))
        .or(waiting.first().map(|(worker, _)| worker.pane));

    panel_title(tree, rail, "要対応", waiting.len(), Tone::Warning);
    let list = card_list(tree, rail);
    let mut buttons = QueueButtons {
        expanded,
        ..QueueButtons::default()
    };
    for (worker, need) in waiting {
        let card = queue_card(tree, list, worker, need, pulses);
        if expanded == Some(worker.pane) {
            buttons
                .open
                .push((queue_card_body(tree, card, worker), worker.pane));
        } else {
            buttons
                .details
                .push((queue_card_summary(tree, card, worker), worker.pane));
        }
    }
    buttons
}

pub(super) fn queue_card(
    tree: &mut UiTree,
    list: WidgetId,
    worker: &Worker,
    need: Need,
    pulses: &mut Vec<(WidgetId, Pulse)>,
) -> WidgetId {
    let color = need_color(need);
    let card = tree.insert(
        Some(list),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(6.0),
            padding: LayoutRect::length(10.0),
            flex_shrink: 0.0,
            ..Default::default()
        })
        .background(Role::SurfaceAlt)
        .border(1.0, color)
        .radius(8.0),
    );
    let head = tree.insert(
        Some(card),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    tree.insert(
        Some(head),
        Label::new(need.symbol())
            .font_size(15.0)
            .weight(700)
            .color(color),
    );
    // The label takes what the kind and the wait leave and is cut short, so
    // a long pane title cannot push the wait out of the card.
    let label_slot = tree.insert(Some(head), Container::new(fill_remaining()));
    tree.insert(
        Some(label_slot),
        Label::new(worker.label.as_str())
            .weight(700)
            .truncation(Truncation::Tail),
    );
    tree.insert(
        Some(head),
        Tag::new(worker.kind.as_str()).tone(Tone::Accent),
    );
    let since = tree.insert(
        Some(head),
        Label::new(queue_since(worker, need))
            .font_size(12.0)
            .weight(700)
            .color(color),
    );
    pulses.push((since, Pulse::QueueSince(worker.pane)));
    card
}

pub(super) fn queue_card_summary(tree: &mut UiTree, card: WidgetId, worker: &Worker) -> WidgetId {
    let row = tree.insert(
        Some(card),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let detail = tree.insert(Some(row), Container::new(fill_remaining()));
    if !worker.detail.is_empty() {
        tree.insert(
            Some(detail),
            Label::new(worker.detail.as_str())
                .font_size(12.0)
                .color(Role::Muted)
                .truncation(Truncation::Tail),
        );
    }
    tree.insert(
        Some(row),
        Button::new("詳細").control_size(ControlSize::Small),
    )
}

pub(super) fn queue_card_body(tree: &mut UiTree, card: WidgetId, worker: &Worker) -> WidgetId {
    task_line(tree, card, worker);

    if !worker.screen_tail.is_empty() {
        let screen = tree.insert(
            Some(card),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(2.0),
                padding: LayoutRect::length(8.0),
                ..Default::default()
            })
            .background(Role::Background)
            .radius(6.0),
        );
        for line in &worker.screen_tail {
            tree.insert(
                Some(screen),
                Label::new(line.as_str())
                    .font_size(11.0)
                    .font_family("monospace")
                    .truncation(Truncation::Tail),
            );
        }
    }

    // 開く never gives way: the orchestrator's name is cut first, then the
    // tags are clipped.
    let actions = tree.insert(
        Some(card),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let tags = tree.insert(
        Some(actions),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::CENTER,
            gap: gap(8.0),
            flex_shrink: HINT_STRIP_SHRINK,
            min_size: LayoutSize {
                width: length(0.0),
                height: auto(),
            },
            ..Default::default()
        })
        .clip(true),
    );
    branch_tag(tree, tags, worker);
    model_tag(tree, tags, worker);
    spacer(tree, actions);
    let slot = tree.insert(
        Some(actions),
        Container::new(Style {
            flex_shrink: 1.0,
            min_size: LayoutSize {
                width: length(0.0),
                height: auto(),
            },
            ..Default::default()
        }),
    );
    tree.insert(
        Some(slot),
        Label::new(worker.orchestrator.as_str())
            .font_size(11.0)
            .color(Role::Muted)
            .truncation(Truncation::Tail),
    );
    tree.insert(
        Some(actions),
        Button::new("開く").control_size(ControlSize::Small),
    )
}
