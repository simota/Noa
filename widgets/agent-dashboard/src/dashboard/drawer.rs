//! The details drawer for any worker (AC8). `Dashboard` opens it on
//! `View::detail`, reopens it after a rebuild, moves it with the selection,
//! closes it when the worker goes, and asks the feed for its screen tail;
//! this module fills the overlay in the order of dashboard-worker-list AC9:
//! the worker's state, what can be done, its result file (read only when
//! asked, `read_result`) and worktree, then, collapsed, its task and screen
//! tail, and its run information and times.

use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use noa_gpui_core::clipboard::MemoryClipboard;
use noa_gpui_core::style::{AlignItems, Rect as LayoutRect, Size as LayoutSize, auto, length};
use noa_gpui_core::text_edit::TextEditor;
use noa_gpui_core::widget::{MeasureCx, PaintCx, Widget};
use noa_gpui_core::widgets::{
    Button, Container, ControlSize, DisclosureGroup, Label, ScrollView, Tag, TagStyle, TextField,
    Tone, Truncation,
};
use noa_gpui_core::{Key, MeasureInput, Modifiers, Role, Style, UiTree, WidgetId, accesskit};
use noa_gpui_scene::{Point, Size};
use noa_gpui_text::TextStore;

use crate::fleet::{Phase, State, Worker, minutes_text};

use super::graph::mark;
use super::list::{NO_TASK, RowAction, qualifiers, time_text};
use super::style::{caption, fill_remaining, gap, phase_color, set_label, spacer, status_color};
use super::{Feed, Pulse, Registry};
use crate::live::Link;

/// What the drawer says where a worker has no screen tail yet: the feed
/// fetches it (live), or there is none to fetch (demo).
const TAIL_PENDING: &str = "まだ取得していません";
const TAIL_NONE: &str = "なし";

/// How much of a result file the drawer reads, and how many of its lines
/// it shows.
pub(super) const RESULT_MAX_BYTES: u64 = 16 * 1024;
pub(super) const RESULT_MAX_LINES: usize = 200;

pub(super) const NEEDS_CONTROL: &str = "ペインへ移動するには Noa の server-scopes に control が必要です（例: server-scopes = read,control）。control はペインの作成と終了にも使われます";
pub(super) const NEEDS_INPUT: &str = "送信するには Noa の server-scopes に input が必要です（例: server-scopes = read,control,input）";
/// What the drawer and the notice line say of a worker whose pane is gone.
pub(super) const CLOSED_PANE: &str = "ペインは閉じています（ctl の記録から表示）";
/// The button that opens the Repo Browser on the worker's worktree.
pub(super) const DIFF: &str = "差分を見る";
pub(super) const CLOSED_SEND_GUARD: &str = "ペインは閉じているため送信できません";
pub(super) const PERMISSION_SEND_GUARD: &str =
    "承認待ちのペインには安全のため送信できません（開く で直接操作してください）";

/// A worker's result file as the drawer shows it: its text, and whether it
/// was cut short (`RESULT_MAX_BYTES`, `RESULT_MAX_LINES`); or why it could
/// not be read.
pub(super) type Preview = Result<(String, bool), String>;

/// Reads the start of `path`, which must be a regular file: a FIFO or a
/// device would block or never end, and a symbolic link may lead out of
/// the worker's own file. What is checked is the file opened, not the path,
/// so the path cannot be swapped between the check and the read; the open
/// itself neither follows a link nor waits for a FIFO's writer.
pub(super) fn read_result(path: &Path) -> Preview {
    let file = open_regular(path)?;
    let mut bytes = Vec::new();
    file.take(RESULT_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let mut cut = bytes.len() as u64 > RESULT_MAX_BYTES;
    if cut {
        bytes.truncate(RESULT_MAX_BYTES as usize);
        // A character the limit cut in two is left out whole.
        if let Err(e) = std::str::from_utf8(&bytes)
            && e.error_len().is_none()
        {
            bytes.truncate(e.valid_up_to());
        }
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().take(RESULT_MAX_LINES + 1).collect();
    cut |= lines.len() > RESULT_MAX_LINES;
    let kept = &lines[..lines.len().min(RESULT_MAX_LINES)];
    Ok((kept.join("\n"), cut))
}

pub(super) fn open_regular(path: &Path) -> Result<std::fs::File, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) => NOT_REGULAR.to_owned(),
            _ => e.to_string(),
        })?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err(NOT_REGULAR.to_owned());
    }
    Ok(file)
}

const NOT_REGULAR: &str = "通常のファイルではありません";

/// The drawer's interactive controls.
pub(super) struct DrawerButtons {
    /// 開く (承認画面へ for an approval) and ペインを閉じる; neither for a
    /// closed worker.
    pub(super) open: Option<WidgetId>,
    /// 入力する, for a worker waiting for input.
    pub(super) start_send: Option<WidgetId>,
    pub(super) close_pane: Option<WidgetId>,
    /// まとめて閉じる, when the filter shows ended workers.
    pub(super) close_ended: Option<WidgetId>,
    /// 差分を見る, when the worker's cwd is known.
    pub(super) diff: Option<WidgetId>,
    pub(super) close: WidgetId,
    pub(super) result: Option<WidgetId>,
    pub(super) result_reload: Option<WidgetId>,
    pub(super) result_copy: Option<WidgetId>,
    pub(super) tail_copy: Option<WidgetId>,
    pub(super) send_field: Option<WidgetId>,
    pub(super) send_button: Option<WidgetId>,
    /// 指示と画面ログ and 実行情報 (`DISCLOSURES`).
    pub(super) disclosures: [WidgetId; 2],
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

/// A single-line editor that charges `scanned`, at every call, with the
/// bytes of text it holds: any call may look at all of them, so the count
/// bounds the work whatever the calls are — a caret moved along a long
/// text is charged that text at every move.
pub(super) struct CountingEditor<'a> {
    editor: TextEditor,
    clipboard: MemoryClipboard,
    scanned: &'a mut usize,
}

impl<'a> CountingEditor<'a> {
    pub(super) fn new(scanned: &'a mut usize) -> Self {
        Self {
            editor: TextEditor::single_line(),
            clipboard: MemoryClipboard::default(),
            scanned,
        }
    }

    fn charge(&mut self) {
        *self.scanned += self.editor.text().len();
    }

    pub(super) fn set_text(&mut self, text: &str) {
        self.editor.set_text(text);
        self.charge();
    }

    pub(super) fn move_to(&mut self, index: usize) {
        self.charge();
        self.editor.move_to(index, false);
    }

    /// The caret one grapheme cluster on.
    pub(super) fn right(&mut self) {
        self.charge();
        let modifiers = Modifiers::default();
        self.editor
            .handle_key(Key::ArrowRight, modifiers, &mut self.clipboard);
    }

    pub(super) fn caret(&self) -> usize {
        self.editor.caret()
    }
}

/// Where each grapheme cluster of `line` (a [`one_line`] text) ends, in
/// time proportional to the line's length. `scanned` counts what the
/// editor holds at each call ([`CountingEditor`]): each step works only
/// inside the piece it is given, so that count bounds the work.
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
    let mut editor = CountingEditor::new(scanned);
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
        editor.move_to(0);
        editor.right();
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
    let column = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: noa_gpui_core::style::FlexDirection::Column,
            gap: gap(5.0),
            padding: LayoutRect {
                left: length(12.0),
                right: length(12.0),
                top: length(10.0),
                bottom: length(10.0),
            },
            ..Default::default()
        })
        .background(Role::SurfaceAlt)
        .border(1.0, Role::Separator)
        .radius(8.0),
    );
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

/// A drawer label that time changes, updated in place by [`refresh`]
/// (`Pulse::Drawer`): which of the worker's texts it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Slot {
    /// 状態's qualifiers (a quiet worker's 出力未変化 N 分).
    Qualifiers,
    /// 状態's wait or last update (`list::time_text`).
    Time,
    /// 時間's start.
    Started,
    /// 時間's `since`.
    Since,
    /// 時間's 所要.
    Took,
    /// 時間's 人待ち合計.
    Waited,
    /// 時間's row of one phase (its swatch and its label).
    Phase(Phase),
}

/// The two collapsed parts of the drawer (`View::drawer_open`), in order.
pub(super) const DISCLOSURES: [&str; 2] = ["指示と画面ログ", "実行情報"];

/// Inputs for building the details drawer.
pub(super) struct DrawerInput<'a> {
    pub(super) worker: &'a Worker,
    pub(super) feed: &'a Feed,
    pub(super) preview: Option<&'a Preview>,
    pub(super) draft: &'a str,
    pub(super) send_caret: usize,
    pub(super) confirm_close: bool,
    /// How many ended workers the filter shows (a bulk close's targets),
    /// and whether a first press armed their close; `None` when there are
    /// none.
    pub(super) close_ended: Option<(usize, bool)>,
    /// Whether 指示と画面ログ and 実行情報 are expanded (`DISCLOSURES`).
    pub(super) expanded: [bool; 2],
}

/// A muted wrapped label (`text_line`).
fn muted_line(tree: &mut UiTree, parent: WidgetId, text: &str, lines: u32) -> WidgetId {
    let id = text_line(tree, parent, text, lines);
    tree.update::<Label, _>(id, |l, cx| l.set_color(cx, Role::Muted));
    id
}

fn small_button(tree: &mut UiTree, parent: WidgetId, label: &str, selected: bool) -> WidgetId {
    tree.insert(
        Some(parent),
        Button::new(label)
            .control_size(ControlSize::Small)
            .selected(selected),
    )
}

/// A collapsible part of the drawer: a box like a section's holding a
/// [`DisclosureGroup`], whose children are hidden (and neither drawn, hit
/// nor focused) while it is collapsed. Returns the group.
fn disclosure(tree: &mut UiTree, parent: WidgetId, title: &str, expanded: bool) -> WidgetId {
    let column = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: noa_gpui_core::style::FlexDirection::Column,
            padding: LayoutRect {
                left: length(8.0),
                right: length(12.0),
                top: length(8.0),
                bottom: length(8.0),
            },
            ..Default::default()
        })
        .background(Role::SurfaceAlt)
        .border(1.0, Role::Separator)
        .radius(8.0),
    );
    tree.insert(Some(column), DisclosureGroup::new(title).expanded(expanded))
}

/// Fills the drawer overlay `root` for `input.worker`, its result file
/// shown as `input.preview` when one was read (dashboard-worker-list AC9):
/// the head (mark, label, kind), then 状態, 操作, 結果・差分, and the
/// collapsed 指示と画面ログ and 実行情報; まとめて閉じる and 閉じる stay below
/// the scrolling content. What time changes registers `Pulse::Drawer` in
/// `reg.pulses`, each label with its [`Slot`].
pub(super) fn build(
    tree: &mut UiTree,
    root: WidgetId,
    input: DrawerInput<'_>,
    reg: &mut Registry,
) -> DrawerButtons {
    let DrawerInput {
        worker,
        feed,
        preview,
        draft,
        send_caret,
        confirm_close,
        close_ended,
        expanded,
    } = input;
    let pane = worker.pane;
    let pulse = |reg: &mut Registry, id: WidgetId, slot: Slot| {
        reg.pulses.push((id, Pulse::Drawer(pane, slot)));
    };
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

    // 1. 状態: the row's status and qualifiers, the reason or the latest
    // report, the wait or last update, the parent.
    let state = section(tree, scroll, "状態");
    let status_row = tree.insert(
        Some(state),
        Container::wrapping_row(8.0, 2.0).align_items(AlignItems::CENTER),
    );
    let status = worker.status();
    let mut title = Label::new(status.title()).font_size(14.0);
    title = match status_color(status) {
        Some(color) => title.weight(700).color(color),
        None => title.color(Role::Muted),
    };
    tree.insert(Some(status_row), title);
    let qualifiers = qualifiers(worker);
    let qualifier = tree.insert(
        Some(status_row),
        Label::new(qualifiers.as_str())
            .font_size(12.0)
            .color(Role::Muted),
    );
    tree.set_hidden(qualifier, qualifiers.is_empty());
    pulse(reg, qualifier, Slot::Qualifiers);
    if !worker.detail.is_empty() {
        text_line(tree, state, &worker.detail, 3);
    }
    let time = muted_line(tree, state, &time_text(worker), 1);
    tree.set_hidden(time, worker.since.is_empty());
    pulse(reg, time, Slot::Time);
    let parent = format!("親 {}", feed.fleet.parent_name(worker));
    muted_line(tree, state, &parent, 2);

    // 2. 操作: the row's action where it does more than this drawer shows
    // (承認画面へ in place of 開く, 入力する), 開く, ペインを閉じる, and the
    // send field with its guards and hints.
    let actions = section(tree, scroll, "操作");
    let (mut open, mut start_send, mut close_pane) = (None, None, None);
    if worker.is_closed() {
        muted_line(tree, actions, CLOSED_PANE, 2);
    } else {
        let row = tree.insert(Some(actions), Container::wrapping_row(6.0, 6.0));
        let action = RowAction::of(worker);
        if action == RowAction::StartSend {
            start_send = Some(small_button(tree, row, action.title(status), true));
        }
        let open_label = match action {
            RowAction::OpenPane => action.title(status),
            _ => "開く",
        };
        let primary = action == RowAction::OpenPane;
        open = Some(small_button(tree, row, open_label, primary));
        let close_label = if confirm_close {
            "本当に閉じる"
        } else {
            "ペインを閉じる"
        };
        close_pane = Some(small_button(tree, row, close_label, confirm_close));
        if feed.link.as_ref() == Some(&Link::Live) && !feed.can_focus {
            muted_line(tree, actions, NEEDS_CONTROL, 4);
        }
    }
    let (send_field, send_button) = if worker.is_closed() {
        muted_line(tree, actions, CLOSED_SEND_GUARD, 2);
        (None, None)
    } else if worker.state == State::Permission {
        muted_line(tree, actions, PERMISSION_SEND_GUARD, 3);
        (None, None)
    } else {
        let row = tree.insert(
            Some(actions),
            Container::row().align_items(AlignItems::CENTER).gap(6.0),
        );
        let field = tree.insert(
            Some(row),
            TextField::new("メッセージを入力（Enter で送信）", 268.0),
        );
        let caret = send_caret.min(draft.len());
        tree.update::<TextField, _>(field, |f, cx| {
            f.set_text(cx, draft);
            f.select(cx, caret..caret);
        });
        let send = small_button(tree, row, "送信", true);
        if feed.link.as_ref() == Some(&Link::Live) && !feed.can_input {
            muted_line(tree, actions, NEEDS_INPUT, 3);
        } else if feed.link.as_ref() == Some(&Link::Live) && !feed.safe_send {
            muted_line(tree, actions, crate::live::UNSAFE_SEND, 3);
        }
        (Some(field), Some(send))
    };

    // 3. 結果・差分: the result file and 差分を見る.
    let results = section(tree, scroll, "結果・差分");
    let (result, result_reload, result_copy) = match worker.result.as_ref() {
        Some(_) => {
            let (toggle, reload, copy) = result_section(tree, results, preview);
            (Some(toggle), reload, copy)
        }
        None => {
            muted_line(tree, results, NO_RESULT_FILE, 2);
            (None, None, None)
        }
    };
    // Open or closed, a worker with a known cwd has a worktree to look at.
    let diff = (!worker.cwd.is_empty()).then(|| {
        let row = tree.insert(Some(results), Container::row().gap(6.0));
        small_button(tree, row, DIFF, false)
    });

    // 4. 指示と画面ログ (collapsed): the task and the screen tail.
    let instructions = disclosure(tree, scroll, DISCLOSURES[0], expanded[0]);
    let task = tree.insert(
        Some(instructions),
        Container::new(Style {
            flex_direction: noa_gpui_core::style::FlexDirection::Column,
            gap: gap(4.0),
            ..Default::default()
        }),
    );
    caption(tree, task, "依頼");
    if worker.task.is_empty() {
        muted_line(tree, task, NO_TASK, 1);
    } else {
        text_line(tree, task, &worker.task, 6);
    }
    let tail_copy = screen_tail(tree, instructions, worker, feed);

    // 5. 実行情報 (collapsed): model, branch, cwd, pane ID, times.
    let run = disclosure(tree, scroll, DISCLOSURES[1], expanded[1]);
    if worker.is_agent() || !worker.branch.is_empty() {
        // The model and the branch, each cut to the drawer's width when it
        // is longer.
        let tags = tree.insert(
            Some(run),
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
    }
    let id = worker.closed_pane.unwrap_or(pane);
    text_line(tree, run, &format!("ペイン ID {id}"), 1);
    if !worker.cwd.is_empty() {
        muted_line(tree, run, &worker.cwd, 2);
    }
    let times = tree.insert(
        Some(run),
        Container::new(Style {
            flex_direction: noa_gpui_core::style::FlexDirection::Column,
            gap: gap(5.0),
            ..Default::default()
        }),
    );
    caption(tree, times, "時間");
    let started = time_label(tree, times, &started_text(worker));
    pulse(reg, started, Slot::Started);
    let since = time_label(tree, times, &worker.since);
    tree.set_hidden(since, worker.since.is_empty());
    pulse(reg, since, Slot::Since);
    let took_text = took_text(worker);
    let took = time_label(tree, times, took_text.as_deref().unwrap_or_default());
    tree.set_hidden(took, took_text.is_none());
    pulse(reg, took, Slot::Took);
    let waited = time_label(tree, times, &waited_text(worker));
    pulse(reg, waited, Slot::Waited);
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
        pulse(reg, row, Slot::Phase(phase));
    }

    // The bulk close acts on the filter's workers, not this one: a row of
    // its own above the drawer's 閉じる.
    let close_ended = close_ended.map(|(count, armed)| {
        let row = tree.insert(
            Some(root),
            Container::row().align_items(AlignItems::CENTER).gap(8.0),
        );
        spacer(tree, row);
        let label = if armed {
            format!("本当にまとめて閉じる（{count} 件）")
        } else {
            format!("完了・失敗をまとめて閉じる（{count} 件）")
        };
        small_button(tree, row, &label, armed)
    });

    // 閉じる stays below the scrolling content, always reachable.
    let buttons = tree.insert(
        Some(root),
        Container::new(Style {
            flex_direction: noa_gpui_core::style::FlexDirection::Row,
            align_items: AlignItems::CENTER,
            gap: gap(8.0),
            padding: LayoutRect {
                left: length(0.0),
                right: length(0.0),
                top: length(8.0),
                bottom: length(0.0),
            },
            ..Default::default()
        }),
    );
    spacer(tree, buttons);
    let close = small_button(tree, buttons, "閉じる", false);
    DrawerButtons {
        open,
        start_send,
        close_pane,
        close_ended,
        diff,
        close,
        result,
        result_reload,
        result_copy,
        tail_copy,
        send_field,
        send_button,
        disclosures: [instructions, run],
    }
}

/// What 結果・差分 says of a worker with no result file to read.
pub(super) const NO_RESULT_FILE: &str = "表示できる結果ファイルはありません";

/// The screen tail's box under `parent`: its caption and コピー (when
/// there is a tail), and its lines or why there are none. Returns コピー.
fn screen_tail(
    tree: &mut UiTree,
    parent: WidgetId,
    worker: &Worker,
    feed: &Feed,
) -> Option<WidgetId> {
    let tail = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: noa_gpui_core::style::FlexDirection::Column,
            gap: gap(6.0),
            ..Default::default()
        }),
    );
    let tail_head = tree.insert(
        Some(tail),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    caption(tree, tail_head, "画面の末尾");
    spacer(tree, tail_head);
    let tail_copy =
        (!worker.screen_tail.is_empty()).then(|| small_button(tree, tail_head, "コピー", false));
    if worker.screen_tail.is_empty() {
        let empty = if worker.is_closed() {
            TAIL_NONE
        } else if feed.link.is_some() {
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
    tail_copy
}

/// The result file under `column` (結果・差分): what was read, if it was,
/// and a 表示 button, or 隠す, 再読込 and コピー buttons.
fn result_section(
    tree: &mut UiTree,
    column: WidgetId,
    preview: Option<&Preview>,
) -> (WidgetId, Option<WidgetId>, Option<WidgetId>) {
    match preview {
        None => {}
        Some(Err(reason)) => {
            caption(tree, column, format!("読めません: {}", one_line(reason)));
        }
        Some(Ok((text, cut))) => {
            let body = tree.insert(
                Some(column),
                Container::new(Style {
                    flex_direction: noa_gpui_core::style::FlexDirection::Column,
                    gap: gap(2.0),
                    padding: LayoutRect::length(8.0),
                    ..Default::default()
                })
                .background(Role::Background)
                .radius(6.0),
            );
            for line in text.lines() {
                tree.insert(
                    Some(body),
                    Label::new(one_line(line))
                        .font_size(11.0)
                        .font_family("monospace")
                        .wrap(true),
                );
            }
            if *cut {
                let kib = RESULT_MAX_BYTES / 1024;
                let text =
                    format!("先頭だけを表示しています（{kib} KiB・{RESULT_MAX_LINES} 行まで）");
                caption(tree, column, text);
            }
        }
    }
    let row = tree.insert(Some(column), Container::row().gap(6.0));
    let label = if preview.is_some() {
        "隠す"
    } else {
        "表示"
    };
    let toggle = small_button(tree, row, label, false);
    let reload = preview
        .is_some()
        .then(|| small_button(tree, row, "再読込", false));
    let copy = matches!(preview, Some(Ok(_))).then(|| small_button(tree, row, "コピー", false));
    (toggle, reload, copy)
}

/// Brings the drawer label `id` up to date: the text of its `slot`.
pub(super) fn refresh(tree: &mut UiTree, id: WidgetId, slot: Slot, worker: &Worker) {
    match slot {
        Slot::Qualifiers => {
            let text = qualifiers(worker);
            tree.set_hidden(id, text.is_empty());
            set_label(tree, id, text);
        }
        Slot::Time => {
            tree.set_hidden(id, worker.since.is_empty());
            set_label(tree, id, time_text(worker));
        }
        Slot::Started => set_label(tree, id, started_text(worker)),
        Slot::Since => {
            set_label(tree, id, worker.since.clone());
            tree.set_hidden(id, worker.since.is_empty());
        }
        Slot::Took => {
            let text = took_text(worker);
            tree.set_hidden(id, text.is_none());
            if let Some(text) = text {
                set_label(tree, id, text);
            }
        }
        Slot::Waited => set_label(tree, id, waited_text(worker)),
        Slot::Phase(phase) => {
            let (text, shown) = phase_text(worker, phase);
            tree.set_hidden(id, !shown);
            if let Some(label) = tree.children(id).get(1).copied() {
                set_label(tree, label, text);
            }
        }
    }
}
