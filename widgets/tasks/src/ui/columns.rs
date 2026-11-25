//! The four columns (未着手 / 実行中 / 確認待ち / 完了) and their cards.
//!
//! A card: the source badge (ローカル / `#N`), the title, the labels, the
//! problem of a local file that does not parse, and the last run's agent
//! and age. A card is pressed as a whole (its id is the press's source).

use noa_gpui_core::style::{FlexDirection, Size as LayoutSize, auto, length, percent};
use noa_gpui_core::widgets::{Container, Label, Pressable, ScrollView, Tag};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Color;

use tasks::model::types::{Column, Millis, RunView, Task, TaskRef};

use super::style::{self, COLUMN_BASIS, gap};

/// A problem's colour: a local file that does not parse.
const PROBLEM: Color = Color::hex(0xff9f0a);

/// One card to show: the task and its last run.
pub(super) struct Card<'a> {
    pub task: &'a Task,
    pub last: Option<&'a RunView>,
}

/// What [`build`] made: each column's box and its cards' scroll view, in
/// [`Column::ALL`] order, and every card with its task, in column then
/// card order.
pub(super) struct Built {
    pub columns: [WidgetId; 4],
    pub scrolls: [WidgetId; 4],
    pub cards: Vec<(WidgetId, TaskRef)>,
}

/// Builds the four columns under `host` (a row), the cards of each in
/// `lists` (in [`Column::ALL`] order), `selected` drawn as such.
pub(super) fn build(
    tree: &mut UiTree,
    host: WidgetId,
    lists: &[Vec<Card<'_>>; 4],
    selected: Option<&TaskRef>,
    now: Millis,
) -> Built {
    let mut cards = Vec::new();
    let mut scrolls = [host; 4];
    let columns = Column::ALL.map(|column| {
        let list = &lists[column as usize];
        let id = tree.insert(
            Some(host),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: length(COLUMN_BASIS),
                min_size: LayoutSize {
                    width: length(0.0),
                    height: length(0.0),
                },
                gap: gap(8.0),
                ..Default::default()
            }),
        );
        let head = tree.insert(Some(id), Container::new(style::row(8.0)));
        tree.insert(
            Some(head),
            Label::new(column.label()).font_size(13.0).weight(700),
        );
        tree.insert(
            Some(head),
            Tag::new(list.len().to_string()).tone(style::column_tone(column)),
        );
        let scroll = tree.insert(
            Some(id),
            ScrollView::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(8.0),
                ..style::fill()
            }),
        );
        for card in list {
            let is_selected = selected == Some(&card.task.r#ref);
            let pressed = insert_card(tree, scroll, card, is_selected, now);
            cards.push((pressed, card.task.r#ref.clone()));
        }
        scrolls[column as usize] = scroll;
        id
    });
    Built {
        columns,
        scrolls,
        cards,
    }
}

fn insert_card(
    tree: &mut UiTree,
    parent: WidgetId,
    card: &Card<'_>,
    selected: bool,
    now: Millis,
) -> WidgetId {
    let task = card.task;
    let frame = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            flex_shrink: 0.0,
            size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        })
        .background(Role::Surface)
        .border(
            if selected { 2.0 } else { 1.0 },
            if selected {
                Role::Accent
            } else {
                Role::Separator
            },
        )
        .radius(8.0)
        .clip(true),
    );
    let pressed = tree.insert(
        Some(frame),
        Pressable::new(style::card_style())
            .label(task.title.clone())
            .selected(selected)
            .radius(8.0),
    );
    let top = tree.insert(Some(pressed), Container::new(style::row(6.0)));
    let source = task.r#ref.source();
    tree.insert(
        Some(top),
        Tag::new(task.r#ref.badge()).tone(style::source_tone(source)),
    );
    if let Some(run) = card.last {
        tree.insert(
            Some(top),
            Label::new(format!(
                "{} · {}",
                run.run.agent.command(),
                age(now, run.run.started)
            ))
            .font_size(11.0)
            .color(Role::Muted),
        );
    }
    tree.insert(
        Some(pressed),
        Label::new(task.title.clone())
            .font_size(13.0)
            .wrap(true)
            .max_lines(3),
    );
    if !task.labels.is_empty() {
        let labels = tree.insert(Some(pressed), Container::wrapping_row(4.0, 4.0));
        for label in &task.labels {
            tree.insert(Some(labels), Tag::new(label.clone()));
        }
    }
    if let Some(problem) = &task.problem {
        tree.insert(
            Some(pressed),
            Label::new(problem.clone())
                .font_size(11.0)
                .wrap(true)
                .color(PROBLEM),
        );
    }
    pressed
}

/// How long ago `at` was: たった今, n 分前, n 時間前, n 日前.
pub(super) fn age(now: Millis, at: Millis) -> String {
    let minutes = now.saturating_sub(at) / 60_000;
    match minutes {
        0 => "たった今".to_owned(),
        1..60 => format!("{minutes} 分前"),
        60..1440 => format!("{} 時間前", minutes / 60),
        _ => format!("{} 日前", minutes / 1440),
    }
}
