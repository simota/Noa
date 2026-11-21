//! The details drawer for any worker (AC8). `Dashboard` opens it on
//! `View::detail`, reopens it after a rebuild, moves it with the selection,
//! closes it when the worker goes, and asks the feed for its screen tail;
//! this module fills the overlay with the worker's state, task, screen tail
//! and times.

use noa_gpui_core::style::{AlignItems, Rect as LayoutRect, Size as LayoutSize, auto, length};
use noa_gpui_core::widgets::{
    Button, Container, ControlSize, Label, ScrollView, Tag, TagStyle, Tone, Truncation,
};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use crate::fleet::{Phase, State, Worker, minutes_text};

use super::cards::{branch_tag, model_tag};
use super::graph::mark;
use super::style::{caption, fill_remaining, gap, phase_color, set_label};
use super::{Feed, Pulse, Registry};

/// What the drawer says where a worker has no screen tail yet: the feed
/// fetches it (live), or there is none to fetch (demo).
const TAIL_PENDING: &str = "まだ取得していません";
const TAIL_NONE: &str = "なし";

fn state_title(state: State) -> &'static str {
    match state {
        State::Running => "作業中",
        State::Permission => "承認待ち",
        State::Input => "入力待ち",
        State::Finished => "完了",
        State::Error => "失敗",
    }
}

fn started_text(worker: &Worker) -> String {
    match worker.started_minutes {
        Some(minutes) => format!("開始 {}前", minutes_text(minutes)),
        None => "開始 不明".to_owned(),
    }
}

/// `None` unless the worker is done and how long it took is known.
fn took_text(worker: &Worker) -> Option<String> {
    (worker.state == State::Finished)
        .then_some(worker.took_minutes)
        .flatten()
        .map(|minutes| format!("所要 {}", minutes_text(minutes)))
}

fn waited_text(worker: &Worker) -> String {
    format!(
        "人待ち合計 {}",
        minutes_text(worker.minutes_where(Phase::waits_on_a_person))
    )
}

fn phase_text(worker: &Worker, phase: Phase) -> (String, bool) {
    let minutes = worker.minutes_where(|p| p == phase);
    (
        format!("{} {}", phase.title(), minutes_text(minutes)),
        minutes > 0.0,
    )
}

fn section(tree: &mut UiTree, parent: WidgetId, title: &str) -> WidgetId {
    let column = tree.insert(Some(parent), Container::column().gap(4.0));
    caption(tree, column, title);
    column
}

fn text_line(tree: &mut UiTree, parent: WidgetId, text: &str, lines: u32) -> WidgetId {
    tree.insert(
        Some(parent),
        Label::new(text).font_size(12.0).wrap(true).max_lines(lines),
    )
}

fn time_label(tree: &mut UiTree, parent: WidgetId, text: &str) -> WidgetId {
    tree.insert(Some(parent), Label::new(text).font_size(12.0))
}

/// Fills the drawer overlay `root` for `worker`; returns its 開く and 閉じる
/// buttons. The time rows register `Pulse::Drawer(pane)` in `reg.pulses`:
/// the labels of the "時間" column (started, since, took, waited, in that
/// order) and each phase's row.
pub(super) fn build(
    tree: &mut UiTree,
    root: WidgetId,
    worker: &Worker,
    feed: &Feed,
    reg: &mut Registry,
) -> (WidgetId, WidgetId) {
    let pane = worker.pane;
    let scroll = tree.insert(
        Some(root),
        ScrollView::new(Style {
            flex_direction: noa_gpui_core::style::FlexDirection::Column,
            gap: gap(10.0),
            ..fill_remaining()
        }),
    );

    // The head: state mark, label (cut first), kind.
    let head = tree.insert(
        Some(scroll),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let (symbol, color) = mark(worker);
    tree.insert(
        Some(head),
        Label::new(symbol).font_size(16.0).weight(700).color(color),
    );
    let slot = tree.insert(
        Some(head),
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
        Label::new(worker.label.as_str())
            .font_size(16.0)
            .weight(700)
            .truncation(Truncation::Tail),
    );
    tree.insert(
        Some(head),
        Tag::new(worker.kind.as_str()).tone(Tone::Accent),
    );

    let tags = tree.insert(
        Some(scroll),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    model_tag(tree, tags, worker);
    branch_tag(tree, tags, worker);
    if worker.has_result {
        tree.insert(Some(tags), Tag::new("結果あり").tone(Tone::Success));
    } else {
        tree.insert(Some(tags), Tag::new("結果なし").style(TagStyle::Outlined));
    }

    let state = section(tree, scroll, "状態");
    text_line(tree, state, state_title(worker.state), 1);
    if !worker.detail.is_empty() {
        text_line(tree, state, &worker.detail, 3);
    }

    let task = section(tree, scroll, "依頼");
    let task_text = if worker.task.is_empty() {
        "不明"
    } else {
        worker.task.as_str()
    };
    text_line(tree, task, task_text, 6);

    let tail = section(tree, scroll, "画面の末尾");
    if worker.screen_tail.is_empty() {
        let empty = if feed.link.is_some() {
            TAIL_PENDING
        } else {
            TAIL_NONE
        };
        text_line(tree, tail, empty, 1);
    } else {
        let screen = tree.insert(
            Some(tail),
            Container::new(Style {
                flex_direction: noa_gpui_core::style::FlexDirection::Column,
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

    let times = section(tree, scroll, "時間");
    let started = time_label(tree, times, &started_text(worker));
    let since = time_label(tree, times, &worker.since);
    tree.set_hidden(since, worker.since.is_empty());
    let took_text = took_text(worker);
    let took = time_label(tree, times, took_text.as_deref().unwrap_or_default());
    tree.set_hidden(took, took_text.is_none());
    let waited = time_label(tree, times, &waited_text(worker));
    for id in [started, since, took, waited] {
        reg.pulses.push((id, Pulse::Drawer(pane)));
    }
    for phase in Phase::LEGEND {
        let (text, shown) = phase_text(worker, phase);
        let row = tree.insert(
            Some(times),
            Container::row().align_items(AlignItems::CENTER).gap(6.0),
        );
        tree.insert(
            Some(row),
            Container::row()
                .width(10.0)
                .height(10.0)
                .background(phase_color(phase))
                .radius(2.0),
        );
        time_label(tree, row, &text);
        tree.set_hidden(row, !shown);
        reg.pulses.push((row, Pulse::Drawer(pane)));
    }

    // The buttons stay below the scrolling content, always reachable.
    let buttons = tree.insert(Some(root), Container::row().gap(8.0));
    let open = tree.insert(
        Some(buttons),
        Button::new("開く").control_size(ControlSize::Small),
    );
    let close = tree.insert(
        Some(buttons),
        Button::new("閉じる").control_size(ControlSize::Small),
    );
    (open, close)
}

/// Brings one of the drawer's time rows up to date (`Pulse::Drawer`): `id`
/// is a label of the "時間" column (told apart by its place there) or a
/// phase's row (told apart by its place among the phase rows).
pub(super) fn refresh(tree: &mut UiTree, id: WidgetId, worker: &Worker) {
    let Some(parent) = tree.parent(id) else {
        return;
    };
    let Some(index) = tree.children(parent).iter().position(|child| *child == id) else {
        return;
    };
    // The column's first child is its caption.
    let index = index.saturating_sub(1);
    if tree.get::<Label>(id).is_some() {
        match index {
            0 => set_label(tree, id, started_text(worker)),
            1 => {
                set_label(tree, id, worker.since.clone());
                tree.set_hidden(id, worker.since.is_empty());
            }
            2 => {
                let text = took_text(worker);
                tree.set_hidden(id, text.is_none());
                if let Some(text) = text {
                    set_label(tree, id, text);
                }
            }
            _ => set_label(tree, id, waited_text(worker)),
        }
    } else if let Some(phase) = index
        .checked_sub(4)
        .and_then(|i| Phase::LEGEND.get(i).copied())
    {
        let (text, shown) = phase_text(worker, phase);
        tree.set_hidden(id, !shown);
        if let Some(label) = tree.children(id).get(1).copied() {
            set_label(tree, label, text);
        }
    }
}
