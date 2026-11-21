//! The details drawer for any worker (AC8). `Dashboard` opens it on
//! `View::detail`, reopens it after a rebuild, moves it with the selection,
//! closes it when the worker goes, and asks the feed for its screen tail;
//! this module fills the overlay with the worker's state, task, screen tail
//! and times.

use noa_gpui_core::style::{AlignItems, Rect as LayoutRect, Size as LayoutSize, auto, length};
use noa_gpui_core::widget::{MeasureCx, PaintCx, Widget};
use noa_gpui_core::widgets::{
    Button, Container, ControlSize, Label, ScrollView, Tag, TagStyle, Tone, Truncation,
};
use noa_gpui_core::{MeasureInput, Role, Style, UiTree, WidgetId, accesskit};
use noa_gpui_scene::{Point, Size};
use noa_gpui_text::TextStore;

use crate::fleet::{Phase, State, Worker, minutes_text};

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
        None => "不明".to_owned(),
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

const ELLIPSIS: char = '\u{2026}';

/// The kind gives way to the label in the head row, down to this width:
/// enough for about ten characters of a command name.
const KIND_FLOOR: f32 = 96.0;

/// How much faster than the label the kind shrinks in the head row (flex
/// shrink factors): the label is cut only once the kind is at its floor.
const KIND_SHRINK: f32 = 1000.0;

/// A [`Tag`] that is cut with an ellipsis to the width its row leaves it.
/// `Tag` measures its whole text and never shrinks, so a long branch or
/// command name would run past the drawer's edge; this widget gives the
/// tag inside it a cut text whenever layout offers less than the full
/// width, and draws it as the tag draws itself. The full text stays its
/// tooltip and accessibility label.
pub(super) struct FitTag {
    text: String,
    tone: Tone,
    look: TagStyle,
    /// Never cut narrower than this (the full text, when it is narrower).
    floor: f32,
    shrink: f32,
    /// The tag that is drawn: `text`, or a cut of it.
    tag: Tag,
    /// The full text's width as a tag.
    full: Option<f32>,
    /// What fits a width, for the widths asked for so far: layout measures
    /// several times per pass.
    fits: Vec<(f32, String)>,
}

impl FitTag {
    pub(super) fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            tag: Tag::new(text.as_str()),
            text,
            tone: Tone::Neutral,
            look: TagStyle::Pill,
            floor: 0.0,
            shrink: 1.0,
            full: None,
            fits: Vec::new(),
        }
    }

    pub(super) fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self.tag = self.make(&self.text);
        self
    }

    pub(super) fn style(mut self, look: TagStyle) -> Self {
        self.look = look;
        self.tag = self.make(&self.text);
        self
    }

    fn floor(mut self, floor: f32) -> Self {
        self.floor = floor;
        self
    }

    fn shrink(mut self, shrink: f32) -> Self {
        self.shrink = shrink;
        self
    }

    /// The whole text.
    pub(super) fn text(&self) -> &str {
        &self.text
    }

    /// The text as drawn: the whole text, or its start and an ellipsis.
    pub(super) fn shown_text(&self) -> &str {
        self.tag.text()
    }

    fn make(&self, text: &str) -> Tag {
        Tag::new(text).tone(self.tone).style(self.look)
    }

    /// How wide `text` is as this tag.
    fn width_of(&self, cx: &mut MeasureCx, input: MeasureInput, text: &str) -> f32 {
        let mut tag = self.make(text);
        let width = tag.measure(cx, input).width;
        tag.removed(cx.text);
        width
    }

    /// The longest start of the text that fits `width` with an ellipsis
    /// (the ellipsis alone when nothing does), or the whole text if it fits.
    fn fit(&mut self, cx: &mut MeasureCx, input: MeasureInput, width: f32) -> String {
        if let Some((_, fit)) = self.fits.iter().find(|(w, _)| *w == width) {
            return fit.clone();
        }
        let full = match self.full {
            Some(full) => full,
            None => *self.full.insert(self.width_of(cx, input, &self.text)),
        };
        let fit = if full <= width + 0.01 {
            self.text.clone()
        } else {
            // The first `n` characters and an ellipsis.
            let cut = |n: usize| {
                let end = self
                    .text
                    .char_indices()
                    .nth(n)
                    .map_or(self.text.len(), |(i, _)| i);
                format!("{}{ELLIPSIS}", self.text[..end].trim_end())
            };
            // `lo` characters fit; `hi` do not (the whole text does not).
            let (mut lo, mut hi) = (0, self.text.chars().count());
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                if self.width_of(cx, input, &cut(mid)) <= width + 0.01 {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            cut(lo)
        };
        if self.fits.len() == 8 {
            self.fits.remove(0);
        }
        self.fits.push((width, fit.clone()));
        fit
    }
}

impl Widget for FitTag {
    fn style(&self) -> Style {
        Style {
            flex_shrink: self.shrink,
            ..Default::default()
        }
    }

    fn measures(&self) -> bool {
        true
    }

    fn measure(&mut self, cx: &mut MeasureCx, input: MeasureInput) -> Size {
        // As `Label` reads the width it may take.
        let shown = match input.known.width.or(input.wrap_width()) {
            Some(width) => self.fit(cx, input, width.max(self.floor)),
            None => self.text.clone(),
        };
        if shown != self.tag.text() {
            self.tag.removed(cx.text);
            self.tag = self.make(&shown);
        }
        self.tag.measure(cx, input)
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        self.tag.paint(cx);
    }

    fn tooltip(&self, _local: Point) -> Option<String> {
        (self.shown_text() != self.text()).then(|| self.text().to_owned())
    }

    fn accessibility_role(&self) -> Option<accesskit::Role> {
        Some(accesskit::Role::Label)
    }

    fn accessibility(&self, node: &mut accesskit::Node) {
        node.set_label(self.text());
    }

    fn removed(&mut self, text: &mut TextStore) {
        self.tag.removed(text);
    }
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

    // The head: state mark, label, kind. The kind is cut first, down to
    // `KIND_FLOOR`; only then the label.
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
        FitTag::new(worker.kind.as_str())
            .tone(Tone::Accent)
            .floor(KIND_FLOOR)
            .shrink(KIND_SHRINK),
    );

    // The model and the branch as the cards show them, each cut to the
    // drawer's width when it is longer.
    let tags = tree.insert(
        Some(scroll),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    if worker.is_agent() {
        tree.insert(Some(tags), FitTag::new(worker.model_line()));
    }
    if !worker.branch.is_empty() {
        tree.insert(
            Some(tags),
            FitTag::new(worker.branch.as_str()).style(TagStyle::Outlined),
        );
    }
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
        caption(tree, tail, empty);
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
