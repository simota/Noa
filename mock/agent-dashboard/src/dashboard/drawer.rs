//! The details drawer for any worker (AC8). `Dashboard` opens it on
//! `View::detail`, reopens it after a rebuild, moves it with the selection,
//! closes it when the worker goes, and asks the feed for its screen tail;
//! this module fills the overlay with the worker's state, task, screen tail
//! and times.

use noa_gpui_core::clipboard::MemoryClipboard;
use noa_gpui_core::style::{AlignItems, Rect as LayoutRect, Size as LayoutSize, auto, length};
use noa_gpui_core::text_edit::TextEditor;
use noa_gpui_core::widget::{MeasureCx, PaintCx, Widget};
use noa_gpui_core::widgets::{
    Button, Container, ControlSize, Label, ScrollView, Tag, TagStyle, Tone, Truncation,
};
use noa_gpui_core::{Key, MeasureInput, Modifiers, Role, Style, UiTree, WidgetId, accesskit};
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
    /// The whole text, as given.
    text: String,
    /// `text` on one line ([`one_line`]), which is what is drawn: a tag is
    /// one line tall, so the lines of a text with line breaks (live input)
    /// would be drawn over each other, and a paragraph separator would make
    /// the shaper panic.
    line: String,
    /// Where each grapheme cluster of `line` ends: a cut keeps whole
    /// clusters, so that an emoji sequence or a combining mark is never
    /// split.
    ends: Vec<usize>,
    tone: Tone,
    look: TagStyle,
    /// Never cut narrower than this (the full text, when it is narrower).
    floor: f32,
    shrink: f32,
    /// The tag that is drawn: `line`, or a cut of it; none when not even an
    /// ellipsis fits the width.
    tag: Option<Tag>,
    /// The full line's width as a tag.
    full: Option<f32>,
    /// What fits a width, for the widths asked for so far: layout measures
    /// several times per pass.
    fits: Vec<(f32, Option<String>)>,
}

/// `text` made safe to draw as one line: every control character (line
/// breaks, tabs, NEL U+0085, the information separators U+001C–U+001E) and
/// the line and paragraph separators U+2028 and U+2029 become a space, and
/// a CR LF pair is one line break, so one space. Worker texts come from
/// live data, and the text shaper (cosmic-text 0.19) splits its lines only
/// at CR and LF: a paragraph separator left inside a line, with
/// left-to-right text on one side and right-to-left text on the other,
/// makes it panic (`assert_eq!(line_rtl, rtl)`). Bidi controls such as
/// isolates are not control characters and are kept.
///
/// The rule the header's status text follows (`health::one_line`, private
/// to that module), but for CR LF: the drawer's tags have always shown a
/// CR LF line break as one space.
pub(super) fn one_line(text: &str) -> String {
    let mut line = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && chars.peek() == Some(&'\n') {
            // The LF that follows becomes the space.
            continue;
        }
        if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
            line.push(' ');
        } else {
            line.push(c);
        }
    }
    line
}

/// How many bytes of a line the editor is given to find where one
/// grapheme cluster ends; a cluster that may run past them is looked for
/// again in twice as many.
const CLUSTER_PIECE: usize = 64;

/// Where each grapheme cluster of `line` (a [`one_line`] text) ends, in
/// time proportional to the line's length. `scanned` counts the bytes
/// handed to the editor: each step works only inside the piece it is
/// given, so that count bounds the work.
///
/// Each step gives the editor the piece of the line from the last cluster
/// end and moves its caret one cluster on. A cluster end is a boundary
/// whatever comes before it, so the rules that look back (emoji
/// sequences, regional indicator pairs, Indic conjuncts) see all they need
/// inside the piece; only an end found at the piece's own end may be the
/// piece's end rather than the cluster's, and then a longer piece is
/// tried. Moving one editor's caret along the whole line instead costs
/// time in the square of a cluster's length: each move goes back to the
/// start of the cluster it lands in.
pub(super) fn cluster_ends(line: &str, scanned: &mut usize) -> Vec<usize> {
    let mut editor = TextEditor::single_line();
    let mut clipboard = MemoryClipboard::default();
    let mut ends = Vec::new();
    let mut start = 0;
    let mut piece_len = CLUSTER_PIECE;
    while start < line.len() {
        let mut stop = (start + piece_len).min(line.len());
        while !line.is_char_boundary(stop) {
            stop += 1;
        }
        let piece = &line[start..stop];
        // A one-line text has no control characters: the editor takes the
        // piece as it is, so its offsets are the piece's.
        editor.set_text(piece);
        editor.move_to(0, false);
        editor.handle_key(Key::ArrowRight, Modifiers::default(), &mut clipboard);
        *scanned += piece.len();
        let end = editor.caret();
        if end == piece.len() && stop < line.len() {
            piece_len *= 2;
            continue;
        }
        start += end;
        ends.push(start);
        piece_len = CLUSTER_PIECE;
    }
    ends
}

impl FitTag {
    pub(super) fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let line = one_line(&text);
        let ends = cluster_ends(&line, &mut 0);
        Self {
            tag: Some(Tag::new(line.as_str())),
            text,
            line,
            ends,
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
        self.tag = Some(self.make(&self.line));
        self
    }

    pub(super) fn style(mut self, look: TagStyle) -> Self {
        self.look = look;
        self.tag = Some(self.make(&self.line));
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

    /// The whole text, as given.
    pub(super) fn text(&self) -> &str {
        &self.text
    }

    /// The text as drawn: the whole text on one line, its start and an
    /// ellipsis, or nothing.
    pub(super) fn shown_text(&self) -> &str {
        self.tag.as_ref().map_or("", Tag::text)
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

    /// The whole line if it fits `width`, else its longest start that fits
    /// with an ellipsis, or `None` when not even the ellipsis does.
    fn fit(&mut self, cx: &mut MeasureCx, input: MeasureInput, width: f32) -> Option<String> {
        if let Some((_, fit)) = self.fits.iter().find(|(w, _)| *w == width) {
            return fit.clone();
        }
        let full = match self.full {
            Some(full) => full,
            None => *self.full.insert(self.width_of(cx, input, &self.line)),
        };
        let fits = |this: &Self, cx: &mut MeasureCx, text: &str| {
            this.width_of(cx, input, text) <= width + 0.01
        };
        let fit = if full <= width + 0.01 {
            Some(self.line.clone())
        } else {
            // The first `n` grapheme clusters and an ellipsis, without the
            // clusters before it that are only white space: a cluster is
            // kept or left out whole, never cut down to its non-space part
            // (U+0600 and the space after it are one cluster).
            let cut = |n: usize| {
                let start_of = |i: usize| i.checked_sub(1).map_or(0, |last| self.ends[last]);
                let mut n = n;
                while n > 0
                    && self.line[start_of(n - 1)..self.ends[n - 1]]
                        .trim()
                        .is_empty()
                {
                    n -= 1;
                }
                format!("{}{ELLIPSIS}", &self.line[..start_of(n)])
            };
            if fits(self, cx, &cut(0)) {
                // `lo` clusters fit; `hi` do not (the whole line does not).
                let (mut lo, mut hi) = (0, self.ends.len());
                while hi - lo > 1 {
                    let mid = (lo + hi) / 2;
                    if fits(self, cx, &cut(mid)) {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                Some(cut(lo))
            } else {
                None
            }
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
            None => Some(self.line.clone()),
        };
        if shown.as_deref() != self.tag.as_ref().map(Tag::text) {
            if let Some(tag) = &mut self.tag {
                tag.removed(cx.text);
            }
            self.tag = shown.map(|shown| self.make(&shown));
        }
        // Nothing fits: nothing is drawn, and it takes no room.
        self.tag
            .as_mut()
            .map_or(Size::new(0.0, 0.0), |tag| tag.measure(cx, input))
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        if let Some(tag) = &mut self.tag {
            tag.paint(cx);
        }
    }

    /// The whole line when it is cut: the tooltip is drawn, so it is the
    /// one-line text too.
    fn tooltip(&self, _local: Point) -> Option<String> {
        (self.shown_text() != self.line).then(|| self.line.clone())
    }

    fn accessibility_role(&self) -> Option<accesskit::Role> {
        Some(accesskit::Role::Label)
    }

    fn accessibility(&self, node: &mut accesskit::Node) {
        node.set_label(self.text());
    }

    fn removed(&mut self, text: &mut TextStore) {
        if let Some(tag) = &mut self.tag {
            tag.removed(text);
        }
    }
}

fn section(tree: &mut UiTree, parent: WidgetId, title: &str) -> WidgetId {
    let column = tree.insert(Some(parent), Container::column().gap(4.0));
    caption(tree, column, title);
    column
}

/// A wrapped label for `text`, which may come from live data: made one
/// line first ([`one_line`]), and wrapped from there.
fn text_line(tree: &mut UiTree, parent: WidgetId, text: &str, lines: u32) -> WidgetId {
    tree.insert(
        Some(parent),
        Label::new(one_line(text))
            .font_size(12.0)
            .wrap(true)
            .max_lines(lines),
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
        Label::new(one_line(&worker.label))
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
                Label::new(one_line(line))
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
