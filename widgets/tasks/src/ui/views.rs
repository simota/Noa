//! The other ways to see the same tasks: リスト (one row per task),
//! タイムライン (the last two weeks: each task's runs as bars from start to
//! end, and when it was closed) and カレンダー (a month: what started and
//! what was closed each day). Every row or item is pressed as a card is:
//! its id goes with the task in [`Built::rows`], and a press opens the
//! detail. Dates are only ones that happened (see [`super::dates`]).

use noa_gpui_core::style::{
    Dimension, FlexDirection, Position, Rect as LayoutRect, Size as LayoutSize, auto, length,
    percent,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Pressable, ScrollView, Tag};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_scene::Color;

use tasks::model::column::column;
use tasks::model::types::{AgentState, Liveness, Millis, ResultFact, RunView, Task, TaskRef};

use super::columns::{Card, age};
use super::dates::{self, Happened};
use super::style::{self, gap};

/// The days the timeline spans, ending today.
pub(super) const TIMELINE_DAYS: usize = 14;
/// The width of a timeline row's task label.
const LABEL_WIDTH: f32 = 240.0;
/// The most items a calendar day lists before 「+N 件」.
const DAY_ITEMS: usize = 3;
const DONE: Color = Color::hex(0x30d158);
const REVIEW: Color = Color::hex(0xff9f0a);
const RUNNING: Color = Color::hex(0x0a84ff);
const ENDED: Color = Color::hex(0x8e8e93);

/// How the tasks are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    #[default]
    Board,
    List,
    Timeline,
    Calendar,
}

impl Layout {
    pub const ALL: [Layout; 4] = [
        Layout::Board,
        Layout::List,
        Layout::Timeline,
        Layout::Calendar,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Layout::Board => "カンバン",
            Layout::List => "リスト",
            Layout::Timeline => "タイムライン",
            Layout::Calendar => "カレンダー",
        }
    }

    /// The `--layout` word.
    pub fn word(self) -> &'static str {
        match self {
            Layout::Board => "board",
            Layout::List => "list",
            Layout::Timeline => "timeline",
            Layout::Calendar => "calendar",
        }
    }

    pub fn from_word(word: &str) -> Option<Layout> {
        Layout::ALL.into_iter().find(|l| l.word() == word)
    }

    /// The next one, `v` cycling through them.
    pub fn next(self) -> Layout {
        let at = Layout::ALL.iter().position(|l| *l == self).unwrap_or(0);
        Layout::ALL[(at + 1) % Layout::ALL.len()]
    }
}

/// What a builder made: the scroll view (kept across a rebuild) and every
/// pressable row or item with its task, in the order shown.
pub(super) struct Built {
    pub scroll: WidgetId,
    pub rows: Vec<(WidgetId, TaskRef)>,
    /// The calendar's month buttons (previous, this month, next).
    pub months: Option<[WidgetId; 3]>,
}

fn scroll(tree: &mut UiTree, host: WidgetId, gap_px: f32) -> WidgetId {
    tree.insert(
        Some(host),
        ScrollView::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(gap_px),
            ..style::fill()
        }),
    )
}

/// A growing box that may shrink below its content's width.
fn grow() -> Style {
    Style {
        flex_grow: 1.0,
        flex_shrink: 1.0,
        flex_basis: length(0.0),
        min_size: LayoutSize {
            width: length(0.0),
            height: auto(),
        },
        ..Default::default()
    }
}

fn row_frame(tree: &mut UiTree, parent: WidgetId, selected: bool) -> WidgetId {
    tree.insert(
        Some(parent),
        Container::new(Style {
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
        .radius(6.0)
        .clip(true),
    )
}

fn pressable_row(tree: &mut UiTree, frame: WidgetId, task: &Task, selected: bool) -> WidgetId {
    tree.insert(
        Some(frame),
        Pressable::new(Style {
            padding: LayoutRect {
                left: length(10.0),
                right: length(10.0),
                top: length(6.0),
                bottom: length(6.0),
            },
            size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            ..style::row(8.0)
        })
        .label(task.title.clone())
        .selected(selected)
        .radius(6.0),
    )
}

fn badge(tree: &mut UiTree, parent: WidgetId, task: &Task) {
    tree.insert(
        Some(parent),
        Tag::new(task.r#ref.badge()).tone(style::source_tone(task.r#ref.source())),
    );
}

fn title(tree: &mut UiTree, parent: WidgetId, task: &Task, size: f32) {
    let host = tree.insert(Some(parent), Container::new(grow()));
    tree.insert(
        Some(host),
        Label::new(task.title.clone()).font_size(size).max_lines(1),
    );
}

/// リスト: one row per task, in the board's order of activity: source,
/// column, title, labels, the last run and when the task last changed.
pub(super) fn list(
    tree: &mut UiTree,
    host: WidgetId,
    cards: &[Card<'_>],
    selected: Option<&TaskRef>,
    now: Millis,
) -> Built {
    let scroll = scroll(tree, host, 6.0);
    let mut rows = Vec::new();
    if cards.is_empty() {
        tree.insert(
            Some(scroll),
            Label::new("タスクがありません").color(Role::Muted),
        );
    }
    for card in cards {
        let task = card.task;
        let is_selected = selected == Some(&task.r#ref);
        let frame = row_frame(tree, scroll, is_selected);
        let row = pressable_row(tree, frame, task, is_selected);
        badge(tree, row, task);
        let col = column(task, card.last);
        tree.insert(
            Some(row),
            Tag::new(col.label()).tone(style::column_tone(col)),
        );
        title(tree, row, task, 13.0);
        for label in task.labels.iter().take(3) {
            tree.insert(Some(row), Tag::new(label.clone()));
        }
        if let Some(problem) = &task.problem {
            tree.insert(
                Some(row),
                Label::new(problem.clone())
                    .font_size(11.0)
                    .max_lines(1)
                    .color(REVIEW),
            );
        }
        let last = match card.last {
            Some(run) => format!(
                "{} · {}",
                run.run.agent.command(),
                age(now, run.run.started)
            ),
            None => "実行なし".to_owned(),
        };
        tree.insert(
            Some(row),
            Label::new(last).font_size(11.0).color(Role::Muted),
        );
        tree.insert(
            Some(row),
            Label::new(format!("更新 {}", age(now, task.updated)))
                .font_size(11.0)
                .color(Role::Muted),
        );
        rows.push((row, task.r#ref.clone()));
    }
    Built {
        scroll,
        rows,
        months: None,
    }
}

/// A bar's colour: blue while the agent works, orange once it ended with
/// a report, grey for one that ended without.
fn bar_color(view: &RunView) -> Color {
    let working = match view.liveness {
        Liveness::Starting => true,
        Liveness::Alive { agent } => {
            !matches!(agent, Some(AgentState::Finished | AgentState::Error))
        }
        Liveness::Gone => false,
    };
    if working {
        RUNNING
    } else if matches!(view.result, ResultFact::Written { .. }) {
        REVIEW
    } else {
        ENDED
    }
}

/// The narrowest a bar is drawn, of the track's width: a run of minutes
/// over two weeks is still a mark.
const MIN_BAR: f32 = 0.006;

/// A run's bar from `start` to `end` (0-1 along the track): its left edge
/// and width, at least [`MIN_BAR`] wide (also when `end` is before
/// `start`) and inside the track (a run started just now ends at its
/// right edge).
fn bar(start: f32, end: f32) -> (f32, f32) {
    let width = (end - start).clamp(MIN_BAR, 1.0);
    (start.clamp(0.0, 1.0 - width), width)
}

/// Where `at` sits across `from`..`to`, 0-1.
fn fraction(at: Millis, from: Millis, to: Millis) -> f32 {
    // A reversed range is an empty one at `from`.
    let to = to.max(from);
    let span = to.saturating_sub(from).max(1) as f64;
    (at.clamp(from, to).saturating_sub(from) as f64 / span) as f32
}

fn placed(left: f32, top: f32, width: Dimension, height: f32) -> Style {
    Style {
        position: Position::Absolute,
        inset: LayoutRect {
            left: percent(left),
            top: length(top),
            right: auto(),
            bottom: auto(),
        },
        size: LayoutSize {
            width,
            height: length(height),
        },
        ..Default::default()
    }
}

/// タイムライン: the last [`TIMELINE_DAYS`] days. A row per task that ran
/// or was closed in them, newest activity first: each run a bar from its
/// start to its end (now while it runs; a run that left no end, a short
/// mark), and a dot where the task was closed.
pub(super) fn timeline(
    tree: &mut UiTree,
    host: WidgetId,
    cards: &[Card<'_>],
    runs: &[RunView],
    selected: Option<&TaskRef>,
    now: Millis,
) -> Built {
    let days = dates::last_days(now, TIMELINE_DAYS);
    let (from, to) = (days[0], now.max(days[0] + 1));

    let head = tree.insert(Some(host), Container::new(style::row(0.0)));
    tree.insert(
        Some(head),
        Container::new(Style {
            flex_shrink: 0.0,
            size: LayoutSize {
                width: length(LABEL_WIDTH + 20.0),
                height: auto(),
            },
            ..Default::default()
        }),
    );
    let axis = tree.insert(Some(head), Container::new(grow()));
    let axis = tree.insert(
        Some(axis),
        Container::new(Style {
            position: Position::Relative,
            size: LayoutSize {
                width: percent(1.0),
                height: length(16.0),
            },
            ..Default::default()
        }),
    );
    for (i, day) in days.iter().enumerate() {
        // Every other day, so the dates never crowd.
        if i % 2 == 0 || i + 1 == days.len() {
            let at = tree.insert(
                Some(axis),
                Container::new(placed(fraction(*day, from, to), 0.0, auto(), 16.0)),
            );
            tree.insert(
                Some(at),
                Label::new(dates::month_day(*day))
                    .font_size(10.0)
                    .color(Role::Muted),
            );
        }
    }

    let scroll = scroll(tree, host, 4.0);
    let mut rows = Vec::new();
    let mut shown = 0;
    for card in cards {
        let task = card.task;
        let task_runs: Vec<&RunView> = runs
            .iter()
            .filter(|v| v.run.task == task.r#ref)
            .filter(|v| {
                v.run.started <= to && dates::run_end(v, now).unwrap_or(v.run.started) >= from
            })
            .collect();
        let closed = dates::closed_at(task).filter(|at| (from..=to).contains(at));
        if task_runs.is_empty() && closed.is_none() {
            continue;
        }
        shown += 1;
        let is_selected = selected == Some(&task.r#ref);
        let frame = row_frame(tree, scroll, is_selected);
        let row = pressable_row(tree, frame, task, is_selected);
        let label = tree.insert(
            Some(row),
            Container::new(Style {
                flex_shrink: 0.0,
                size: LayoutSize {
                    width: length(LABEL_WIDTH),
                    height: auto(),
                },
                ..style::row(6.0)
            }),
        );
        badge(tree, label, task);
        title(tree, label, task, 12.0);
        let lane = tree.insert(Some(row), Container::new(grow()));
        let track = tree.insert(
            Some(lane),
            Container::new(Style {
                position: Position::Relative,
                size: LayoutSize {
                    width: percent(1.0),
                    height: length(16.0),
                },
                ..Default::default()
            })
            .background(Role::SurfaceAlt)
            .radius(4.0),
        );
        for v in task_runs {
            let start = fraction(v.run.started, from, to);
            let end = dates::run_end(v, now).map_or(start, |e| fraction(e, from, to));
            let (left, width) = bar(start, end);
            tree.insert(
                Some(track),
                Container::new(placed(left, 3.0, percent(width), 10.0))
                    .background(bar_color(v))
                    .radius(3.0),
            );
        }
        if let Some(at) = closed {
            tree.insert(
                Some(track),
                Container::new(placed(fraction(at, from, to), 3.0, length(10.0), 10.0))
                    .background(DONE)
                    .radius(5.0),
            );
        }
        rows.push((row, task.r#ref.clone()));
    }
    if shown == 0 {
        tree.insert(
            Some(scroll),
            Label::new(format!("この {TIMELINE_DAYS} 日間に実行も完了もありません"))
                .color(Role::Muted),
        );
    }
    Built {
        scroll,
        rows,
        months: None,
    }
}

const WEEKDAYS: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];

/// カレンダー: the month `month` months from this one, six weeks from
/// Sunday; each day lists the runs started (▶ agent) and the tasks closed
/// (✓) on it, at most [`DAY_ITEMS`] and 「+N 件」. Today is marked.
pub(super) fn calendar(
    tree: &mut UiTree,
    host: WidgetId,
    tasks: &[&Task],
    runs: &[RunView],
    month: i32,
    now: Millis,
) -> Built {
    let (year, m) = dates::month_from(now, month);
    let grid = dates::month_grid(year, m);
    let happened = dates::happened(tasks, runs, grid[0], dates::next_midnight(grid[41]));

    let head = tree.insert(Some(host), Container::new(style::row(8.0)));
    tree.insert(
        Some(head),
        Label::new(format!("{year}年{m}月"))
            .font_size(15.0)
            .weight(700),
    );
    style::spacer(tree, head);
    let prev = tree.insert(
        Some(head),
        Button::new("◀ 前の月 ([)").control_size(ControlSize::Small),
    );
    let this = tree.insert(
        Some(head),
        Button::new("今月").control_size(ControlSize::Small),
    );
    let next = tree.insert(
        Some(head),
        Button::new("次の月 (]) ▶").control_size(ControlSize::Small),
    );

    let names = tree.insert(Some(host), Container::new(style::row(6.0)));
    for name in WEEKDAYS {
        let cell = tree.insert(Some(names), Container::new(grow()));
        tree.insert(
            Some(cell),
            Label::new(name).font_size(11.0).color(Role::Muted),
        );
    }

    let scroll = scroll(tree, host, 6.0);
    let mut rows = Vec::new();
    let title_of = |r: &TaskRef| {
        tasks
            .iter()
            .find(|t| &t.r#ref == r)
            .map_or_else(|| r.badge(), |t| t.title.clone())
    };
    for week in grid.chunks(7) {
        let line = tree.insert(
            Some(scroll),
            Container::new(Style {
                flex_shrink: 0.0,
                ..style::row(6.0)
            }),
        );
        for &day in week {
            let next_day = dates::next_midnight(day);
            let in_month = dates::day_of(day).month == m;
            let today = dates::same_day(day, now);
            let cell = tree.insert(
                Some(line),
                Container::new(Style {
                    flex_direction: FlexDirection::Column,
                    gap: gap(3.0),
                    padding: LayoutRect::length(6.0),
                    min_size: LayoutSize {
                        width: length(0.0),
                        height: length(92.0),
                    },
                    align_self: Some(noa_gpui_core::style::AlignItems::STRETCH),
                    ..grow()
                })
                .background(if in_month {
                    Role::Surface
                } else {
                    Role::SurfaceAlt
                })
                .border(
                    if today { 2.0 } else { 1.0 },
                    if today { Role::Accent } else { Role::Separator },
                )
                .radius(6.0)
                .clip(true),
            );
            tree.insert(
                Some(cell),
                Label::new(dates::day_of(day).day.to_string())
                    .font_size(11.0)
                    .weight(if today { 700 } else { 400 })
                    .color(if in_month { Role::Text } else { Role::Muted }),
            );
            let items: Vec<&(Millis, Happened)> = happened
                .iter()
                .filter(|(at, _)| (day..next_day).contains(at))
                .collect();
            for (_, item) in items.iter().take(DAY_ITEMS) {
                let (mark, color) = match item {
                    Happened::RunStarted { agent, .. } => (format!("▶ {agent}"), RUNNING),
                    Happened::Closed { .. } => ("✓".to_owned(), DONE),
                };
                let pressed = tree.insert(
                    Some(cell),
                    Pressable::new(Style {
                        size: LayoutSize {
                            width: percent(1.0),
                            height: auto(),
                        },
                        ..style::row(4.0)
                    })
                    .label(title_of(item.task()))
                    .radius(4.0),
                );
                tree.insert(Some(pressed), Label::new(mark).font_size(10.0).color(color));
                let host = tree.insert(Some(pressed), Container::new(grow()));
                tree.insert(
                    Some(host),
                    Label::new(title_of(item.task()))
                        .font_size(11.0)
                        .max_lines(1),
                );
                rows.push((pressed, item.task().clone()));
            }
            if items.len() > DAY_ITEMS {
                tree.insert(
                    Some(cell),
                    Label::new(format!("+{} 件", items.len() - DAY_ITEMS))
                        .font_size(10.0)
                        .color(Role::Muted),
                );
            }
        }
    }
    Built {
        scroll,
        rows,
        months: Some([prev, this, next]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fraction_stays_in_the_track_whatever_the_range() {
        assert_eq!(fraction(150, 100, 200), 0.5);
        assert_eq!(fraction(100, 100, 200), 0.0);
        assert_eq!(fraction(200, 100, 200), 1.0);
        // Outside the range: clamped to its ends.
        assert_eq!(fraction(0, 100, 200), 0.0);
        assert_eq!(fraction(u64::MAX, 100, 200), 1.0);
        // An empty or reversed range (from >= to): no NaN, no panic.
        for (at, from, to) in [
            (100, 100, 100),
            (5, 100, 50),
            (0, 0, 0),
            (1, 0, 1),
            (2, 0, 1),
        ] {
            let f = fraction(at, from, to);
            assert!((0.0..=1.0).contains(&f), "{at} {from} {to}: {f}");
        }
    }

    #[test]
    fn a_bar_is_at_least_the_minimum_and_inside_the_track() {
        // A long run: as it is.
        assert_eq!(bar(0.25, 0.75), (0.25, 0.5));
        assert_eq!(bar(0.0, 1.0), (0.0, 1.0));
        // No length, or an end before its start: the minimum, where it began.
        assert_eq!(bar(0.5, 0.5), (0.5, MIN_BAR));
        assert_eq!(bar(0.5, 0.494), (0.5, MIN_BAR));
        assert_eq!(bar(0.5, -0.494), (0.5, MIN_BAR));
        // Started just now, at the right edge: moved in to stay visible.
        let (left, width) = bar(1.0, 1.0);
        assert_eq!(width, MIN_BAR);
        assert!(left + width <= 1.0 + f32::EPSILON && left >= 0.0);
        // Wider than the track can never be.
        assert_eq!(bar(-0.5, 1.5), (0.0, 1.0));
    }
}
