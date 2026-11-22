use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length,
};
use noa_gpui_core::widgets::{
    Button, Container, ControlSize, Label, Tag, TagStyle, Tone, Truncation,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_text::TextStore;

use crate::fleet::{Need, Tier, Worker, one_line};

use super::cards::{branch_tag, model_tag, task_line};
use super::header::waited_text;
use super::pager::ListParts;
use super::panel::{
    Panel, card_list, group_header, indicator, mark_card, nothing_matches, panel_title,
};
use super::style::{HINT_STRIP_SHRINK, caption, fill_remaining, gap, joined, need_color, spacer};
use super::urgency;
use super::{Pulse, Registry, View};

#[derive(Default)]
pub(super) struct QueueButtons {
    pub(super) open: Vec<(WidgetId, u64)>,
    pub(super) details: Vec<(WidgetId, u64)>,
    /// The card shown expanded, whichever it is.
    pub(super) expanded: Option<u64>,
    /// The title row's 参考情報 button, which shows or folds those cards.
    pub(super) info_toggle: Option<WidgetId>,
}

impl QueueButtons {
    pub(super) fn worker(buttons: &[(WidgetId, u64)], source: WidgetId) -> Option<u64> {
        buttons
            .iter()
            .find(|(id, _)| *id == source)
            .map(|(_, pane)| *pane)
    }
}

/// A queue card draws each of its worker's texts on one line
/// ([`one_line`]): the live feed's already are, but a fleet from elsewhere
/// (the canned one, a test's) is drawn as it is, and the shaper panics on
/// a paragraph separator next to right-to-left text.
pub(super) fn queue_since(worker: &Worker, need: Need) -> String {
    joined(&[need.title(), &one_line(&worker.since)], " ")
}

/// The workers waiting on a person, in the queue's order: the most urgent
/// need first, then the longest wait.
pub(super) fn waiting<'a>(
    workers: impl IntoIterator<Item = &'a Worker>,
) -> Vec<(&'a Worker, Need)> {
    let mut waiting: Vec<(&Worker, Need)> = workers
        .into_iter()
        .filter_map(|w| w.need().map(|need| (w, need)))
        .collect();
    waiting.sort_by(|(a, a_need), (b, b_need)| {
        a_need
            .rank()
            .cmp(&b_need.rank())
            .then(b.wait_minutes.total_cmp(&a.wait_minutes))
    });
    waiting
}

/// The workers with a queue card, in the queue's order: `waiting`, without
/// 参考情報 while it is folded away (`info_open` false).
pub(super) fn cards<'a>(
    workers: impl IntoIterator<Item = &'a Worker>,
    info_open: bool,
) -> Vec<(&'a Worker, Need)> {
    let mut cards = waiting(workers);
    cards.retain(|(_, need)| info_open || need.tier() != Tier::Info);
    cards
}

/// What the queue says when the filter leaves it no card that waits on a
/// decision while others outside the filter do.
pub(super) fn hidden_text(n: usize) -> String {
    format!("該当なし（絞り込みの外に判断待ち {n} 件）")
}

/// The queue panel over `workers` (those the filter lets through); the
/// waited total in its title counts `all` workers. Its list holds, in
/// order: a header and the cards of 判断待ち, the same of 要確認, then, when
/// `view.info_open`, those of 参考情報, which the title row's button shows
/// and folds.
pub(super) fn queue(
    tree: &mut UiTree,
    rail: WidgetId,
    workers: &[&Worker],
    all: &[Worker],
    view: &View,
    filtering: bool,
    reg: &mut Registry,
) -> QueueButtons {
    let info_open = view.info_open;
    let waiting = waiting(workers.iter().copied());
    let count = |tier: Tier| waiting.iter().filter(|(_, n)| n.tier() == tier).count();
    let info = count(Tier::Info);
    let shown = cards(workers.iter().copied(), info_open);
    let expanded = view
        .expanded
        .filter(|pane| shown.iter().any(|(w, _)| w.pane == *pane))
        .or(shown
            .iter()
            .find(|(_, need)| need.tier() != Tier::Info)
            .map(|(worker, _)| worker.pane));

    let urgent = waiting.len() - info;
    let title = panel_title(tree, rail, Panel::Queue, urgent, Tone::Warning);
    let waited = tree.insert(
        Some(title),
        Tag::new(waited_text(all)).style(TagStyle::Outlined),
    );
    reg.pulses.push((waited, Pulse::Waited));
    let info_toggle = (info > 0).then(|| {
        tree.insert(
            Some(title),
            Button::new(format!("{} {info} 件", Tier::Info.title()))
                .control_size(ControlSize::Small),
        )
    });
    let indicator = indicator(tree, title);
    let list = card_list(tree, rail, 8.0);
    if filtering {
        let outside = all
            .iter()
            .filter(|w| w.need().is_some_and(|n| n.tier() == Tier::Decision))
            .count();
        if count(Tier::Decision) == 0 && outside > 0 {
            caption(tree, list, hidden_text(outside));
        } else if shown.is_empty() {
            nothing_matches(tree, list);
        }
    }
    let mut buttons = QueueButtons {
        expanded,
        info_toggle,
        ..QueueButtons::default()
    };
    let mut items = Vec::new();
    let mut headers = Vec::new();
    for tier in [Tier::Decision, Tier::Check, Tier::Info] {
        let members: Vec<(&Worker, Need)> = shown
            .iter()
            .copied()
            .filter(|(_, need)| need.tier() == tier)
            .collect();
        if members.is_empty() {
            continue;
        }
        let header = group_header(tree, list, tier.title(), members.len());
        headers.push((header, (items.len()..items.len() + members.len()).collect()));
        for (worker, need) in members {
            let card = queue_card(tree, list, worker, need, reg);
            if expanded == Some(worker.pane) {
                buttons
                    .open
                    .push((queue_card_body(tree, card, worker), worker.pane));
            } else {
                buttons
                    .details
                    .push((queue_card_summary(tree, card, worker), worker.pane));
            }
            mark_card(tree, card, worker.pane, reg);
            items.push((card, Some(worker.pane)));
        }
    }
    reg.lists[Panel::Queue as usize] = Some(ListParts {
        list,
        items,
        nested: Vec::new(),
        headers,
        indicator,
    });
    buttons
}

/// Brings a queue card's look up to its worker's urgency step
/// (`Pulse::QueueCard`), in place: only the card's border and fill change.
pub(super) fn refresh_card(tree: &mut UiTree, id: WidgetId, worker: &Worker) {
    let Some(need) = worker.need() else { return };
    let step = urgency::step(worker, need);
    tree.update::<Container, _>(id, |card, cx| {
        card.set_border(cx, Some(urgency::border(need, step)));
        card.set_background(cx, Some(urgency::fill(need, step)));
    });
}

pub(super) fn queue_card(
    tree: &mut UiTree,
    list: WidgetId,
    worker: &Worker,
    need: Need,
    reg: &mut Registry,
) -> WidgetId {
    let color = need_color(need);
    let step = urgency::step(worker, need);
    let card = tree.insert(
        Some(list),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(6.0),
            padding: LayoutRect::length(10.0),
            flex_shrink: 0.0,
            ..Default::default()
        })
        .background(urgency::fill(need, step))
        .border(urgency::border(need, step).0, color)
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
        Label::new(one_line(&worker.label))
            .weight(700)
            .truncation(Truncation::Tail),
    );
    tree.insert(
        Some(head),
        Tag::new(one_line(&worker.kind)).tone(Tone::Accent),
    );
    let since = tree.insert(
        Some(head),
        Label::new(queue_since(worker, need))
            .font_size(12.0)
            .weight(700)
            .color(color),
    );
    reg.pulses.push((since, Pulse::QueueSince(worker.pane)));
    reg.pulses.push((card, Pulse::QueueCard(worker.pane)));
    card
}

/// Turns a queue card into the expanded one (task, screen tail, 開く) or a
/// collapsed one (detail, 詳細) in place: the card, its head and its
/// selection bar stay, and it ends up as `queue` builds a card in that
/// state. Returns its new 開く or 詳細.
pub(super) fn set_expanded(
    tree: &mut UiTree,
    card: WidgetId,
    worker: &Worker,
    expanded: bool,
    text: &mut TextStore,
) -> WidgetId {
    // A card's children: its head, what `queue_card_body` or
    // `queue_card_summary` added, and the selection bar (`mark_card`).
    let children = tree.children(card).to_vec();
    let (&bar, content) = children[1..].split_last().expect("a head and a bar");
    for &id in content {
        tree.remove(id, text);
    }
    let button = if expanded {
        queue_card_body(tree, card, worker)
    } else {
        queue_card_summary(tree, card, worker)
    };
    let last = tree.children(card).len() - 1;
    tree.move_child(bar, last);
    button
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
            Label::new(one_line(&worker.detail))
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

    if !worker.card_tail().is_empty() {
        let screen = tree.insert(
            Some(card),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(2.0),
                padding: LayoutRect {
                    left: length(10.0),
                    right: length(10.0),
                    top: length(8.0),
                    bottom: length(8.0),
                },
                ..Default::default()
            })
            .background(Role::Background)
            .border(1.0, Role::Separator)
            .radius(6.0),
        );
        for line in worker.card_tail() {
            tree.insert(
                Some(screen),
                Label::new(one_line(line))
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
        Label::new(one_line(&worker.orchestrator))
            .font_size(11.0)
            .color(Role::Muted)
            .truncation(Truncation::Tail),
    );
    tree.insert(
        Some(actions),
        Button::new("開く")
            .control_size(ControlSize::Small)
            .selected(true),
    )
}
