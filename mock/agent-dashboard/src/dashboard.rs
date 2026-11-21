use noa_gpui_core::style::{
    AlignItems, FlexDirection, JustifyContent, LengthPercentage, Rect as LayoutRect,
    Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Badge, Button, ButtonClicked, Chart, ChartSeries, Container, ControlSize, EmptyState, GroupBox,
    Kbd, Label, LevelIndicator, LevelStyle, ScrollView, SegmentMode, SegmentedControl,
    SelectionChanged, Tag, TagStyle, Tone, Truncation,
};
use noa_gpui_core::{
    Appearance, Dirty, Key, Message, Role, Shortcut, ShortcutPressed, Style, Theme, UiTree,
    WidgetId,
};
use noa_gpui_platform::Screen;
use noa_gpui_scene::{Color, Size};
use noa_gpui_text::TextStore;

use crate::fleet::{
    self, FRESHNESS_MAX, Fleet, Grouping, Need, Phase, State, Worker, minutes_text,
};
use crate::live::{Channel, Command, Link, Snapshot};

const HEADER_HEIGHT: f32 = 48.0;
const GUTTER: f32 = 12.0;
const QUEUE_WIDTH: f32 = 400.0;
const DONE_WIDTH: f32 = 270.0;
const RUNNING_CARD_WIDTH: f32 = 336.0;
const LANE_LABEL_WIDTH: f32 = 104.0;
const LANE_TOTAL_WIDTH: f32 = 44.0;
const LANE_HEIGHT: f32 = 12.0;
/// What a full lane's grow factors add up to. Grow factors that add up to
/// less than 1 hand out only that fraction of the free space, so a lane is
/// scaled to this whatever its span (a span under a minute is under 1).
const LANE_SHARES: f64 = 1000.0;
const GRAPH_WIDTH: f32 = 430.0;
const HISTORY_HEIGHT: f32 = 256.0;
/// Small but not zero: flex shrinks in proportion to `flex_shrink` times the
/// basis, so this lets the status label absorb a deficit before the strip.
const HINT_STRIP_SHRINK: f32 = 0.0001;

const GREEN: Color = Color::hex(0x34c759);
const DIM_GREEN: Color = Color::hex(0x2f5d3c);
const YELLOW: Color = Color::hex(0xffcc00);
const ORANGE: Color = Color::hex(0xff9f0a);
const RED: Color = Color::hex(0xff453a);
const GRAY: Color = Color::hex(0x8e8e93);
const BLUE: Color = Color::hex(0x0a84ff);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panel {
    Queue,
    Running,
    Done,
    Timeline,
    Graph,
}

impl Panel {
    const ALL: [Panel; 5] = [
        Panel::Queue,
        Panel::Running,
        Panel::Done,
        Panel::Timeline,
        Panel::Graph,
    ];
    const NOW: [Panel; 3] = [Panel::Queue, Panel::Running, Panel::Done];
    const HISTORY: [Panel; 2] = [Panel::Timeline, Panel::Graph];

    fn title(self) -> &'static str {
        match self {
            Panel::Queue => "要対応",
            Panel::Running => "作業中",
            Panel::Done => "完了",
            Panel::Timeline => "状態の推移",
            Panel::Graph => "関連",
        }
    }

    fn shortcut(self) -> Shortcut {
        let digit = ['1', '2', '3', '4', '5'][self as usize];
        Shortcut::key(Key::Char(digit))
    }

    fn grows(self) -> bool {
        matches!(self, Panel::Running | Panel::Timeline)
    }

    fn grower(self) -> Panel {
        if Self::ALL[..3].contains(&self) {
            Panel::Running
        } else {
            Panel::Timeline
        }
    }

    fn width(self) -> f32 {
        match self {
            Panel::Queue => QUEUE_WIDTH,
            Panel::Done => DONE_WIDTH,
            Panel::Graph => GRAPH_WIDTH,
            Panel::Running | Panel::Timeline => 0.0,
        }
    }

    fn style(self, shown: &[bool; 5]) -> Style {
        if self.grows() {
            fill_remaining()
        } else if shown[self.grower() as usize] {
            fixed_width(self.width())
        } else {
            Style {
                flex_grow: 1.0,
                flex_basis: length(self.width()),
                min_size: LayoutSize {
                    width: length(0.0),
                    height: auto(),
                },
                ..Default::default()
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct View {
    grouping: Grouping,
    expanded: Option<u64>,
    shown: [bool; 5],
}

impl View {
    fn of(grouping: Grouping) -> Self {
        View {
            grouping,
            expanded: None,
            shown: [true; 5],
        }
    }
}

#[derive(Default)]
struct QueueButtons {
    open: Vec<(WidgetId, u64)>,
    details: Vec<(WidgetId, u64)>,
    /// The card shown expanded, whichever it is.
    expanded: Option<u64>,
}

impl QueueButtons {
    fn worker(buttons: &[(WidgetId, u64)], source: WidgetId) -> Option<u64> {
        buttons
            .iter()
            .find(|(id, _)| *id == source)
            .map(|(_, pane)| *pane)
    }
}

/// Where the workers come from: the canned fleet (no link) or the live feed.
#[derive(Default)]
struct Feed {
    fleet: Fleet,
    link: Option<Link>,
    synced_secs: u64,
    channel: Option<Channel>,
}

impl Feed {
    fn demo() -> Self {
        Feed {
            fleet: fleet::canned(),
            ..Feed::default()
        }
    }

    fn status(&self) -> String {
        let n = self.fleet.workers.len();
        match &self.link {
            None => format!("ワーカー {n} · ダミーデータ"),
            Some(Link::Connecting) => "接続中…".to_owned(),
            Some(Link::Live) => format!(
                "ワーカー {n} · 同期 {}前",
                minutes_text(self.synced_secs as f64 / 60.0)
            ),
            Some(Link::Down(reason)) => format!("切断: {reason}"),
        }
    }
}

/// A widget whose content changes as time passes, updated in place by
/// `Dashboard::refresh`; the pane names the worker it shows.
#[derive(Debug, Clone, Copy)]
enum Pulse {
    Status,
    Waited,
    Longest,
    QueueSince(u64),
    QuietTag(u64),
    Output(u64),
    Freshness(u64),
    RunningCaption(u64),
    DoneCaption(u64),
}

struct TimelineParts {
    span: WidgetId,
    lanes: WidgetId,
}

pub struct Dashboard {
    tree: UiTree,
    view: View,
    feed: Feed,
    status: WidgetId,
    grouping_control: WidgetId,
    queue_buttons: QueueButtons,
    pulses: Vec<(WidgetId, Pulse)>,
    timeline: Option<TimelineParts>,
    /// The size the last `prepare` laid the screen out at. A rebuilt screen is
    /// laid out at it at once (see `rebuilt`).
    viewport: Option<Size>,
    #[cfg(test)]
    panels: [Option<WidgetId>; 5],
}

impl Dashboard {
    /// The demo: the canned fleet.
    pub fn new(grouping: Grouping) -> Self {
        Self::showing(View::of(grouping))
    }

    fn showing(view: View) -> Self {
        Self::build(view, Theme::for_appearance(Appearance::Dark), Feed::demo())
    }

    /// Fed by the live feed, connecting until its first snapshot arrives.
    pub fn live(grouping: Grouping, channel: Channel) -> Self {
        let feed = Feed {
            link: Some(Link::Connecting),
            channel: Some(channel),
            ..Feed::default()
        };
        Self::build(
            View::of(grouping),
            Theme::for_appearance(Appearance::Dark),
            feed,
        )
    }

    /// One live snapshot, with no feed behind it.
    pub fn of_snapshot(grouping: Grouping, snapshot: Snapshot) -> Self {
        Self::from_snapshot(View::of(grouping), snapshot)
    }

    fn from_snapshot(view: View, snapshot: Snapshot) -> Self {
        let feed = Feed {
            fleet: snapshot.fleet,
            link: Some(snapshot.link),
            synced_secs: snapshot.synced_secs,
            channel: None,
        };
        Self::build(view, Theme::for_appearance(Appearance::Dark), feed)
    }

    /// Takes a snapshot in. One that changes only what time changes (texts,
    /// meters, lanes) updates those widgets in place, so the scroll positions
    /// and the hover survive; any other change rebuilds the screen.
    fn apply(&mut self, snapshot: Snapshot, text: &mut TextStore) {
        let structural = self.feed.link.as_ref() != Some(&snapshot.link)
            || !self.feed.fleet.same_structure(&snapshot.fleet);
        self.feed.fleet = snapshot.fleet;
        self.feed.link = Some(snapshot.link);
        self.feed.synced_secs = snapshot.synced_secs;
        if structural {
            self.rebuilt(self.view, text);
        } else {
            self.refresh(text);
        }
    }

    fn refresh(&mut self, text: &mut TextStore) {
        let (tree, fleet) = (&mut self.tree, &self.feed.fleet);
        for &(id, pulse) in &self.pulses {
            let worker = |pane: u64| fleet.worker(pane).expect("the same structure");
            match pulse {
                Pulse::Status => set_label(tree, id, self.feed.status()),
                Pulse::Waited => set_tag(tree, id, waited_text(&fleet.workers)),
                Pulse::Longest => {
                    if let Some(worker) = longest(&fleet.workers) {
                        set_tag(tree, id, longest_text(worker));
                    }
                }
                Pulse::QueueSince(pane) => {
                    let worker = worker(pane);
                    if let Some(need) = worker.need() {
                        set_label(tree, id, queue_since(worker, need));
                    }
                }
                Pulse::QuietTag(pane) => set_tag(tree, id, quiet_text(worker(pane))),
                Pulse::Output(pane) => {
                    let values = worker(pane).lines_per_tick.clone();
                    tree.update::<Chart, _>(id, |c, cx| c.set_values(cx, 0, values));
                }
                Pulse::Freshness(pane) => {
                    let value = worker(pane).freshness;
                    tree.update::<LevelIndicator, _>(id, |l, cx| l.set_value(cx, value));
                }
                Pulse::RunningCaption(pane) => set_label(tree, id, running_caption(worker(pane))),
                Pulse::DoneCaption(pane) => set_label(tree, id, done_caption(worker(pane))),
            }
        }
        if let Some(parts) = &self.timeline {
            let span = timeline_span(&fleet.workers);
            set_label(tree, parts.span, span_text(span));
            for lane in tree.children(parts.lanes).to_vec() {
                tree.remove(lane, text);
            }
            fill_lanes(tree, parts.lanes, &fleet.workers, self.view.grouping, span);
        }
    }

    /// Builds the same screen again for a changed view, keeping the theme (the
    /// appearance the OS last set) the screen has now. The old tree's widgets
    /// go through `UiTree::remove` first: that is what releases their text
    /// layouts in the `TextStore`. The new tree is laid out at the old size
    /// before this returns: the window delivers input before it lays out for the
    /// next frame, and a tree without rects would let that input hit nothing.
    /// That layout is not a frame the window presented, so the root is marked
    /// for compositing again: the window then requests a redraw (it asks
    /// `needs_update` after input) and presents (`prepare` returns true), and the
    /// list it presents is the new tree's, reassembled from what this layout
    /// painted.
    fn rebuilt(&mut self, view: View, text: &mut TextStore) {
        let theme = self.tree.theme().clone();
        let roots: Vec<WidgetId> = self
            .tree
            .root()
            .into_iter()
            .chain(self.tree.overlays())
            .collect();
        for root in roots {
            self.tree.remove(root, text);
        }
        let viewport = self.viewport;
        let feed = std::mem::take(&mut self.feed);
        *self = Self::build(view, theme, feed);
        if let Some(viewport) = viewport {
            self.prepare(viewport, text);
            if let Some(root) = self.tree.root() {
                self.tree.invalidate(root, Dirty::COMPOSITE);
            }
        }
    }

    fn build(view: View, theme: Theme, feed: Feed) -> Self {
        let grouping = view.grouping;
        let workers = &feed.fleet.workers;
        let mut pulses = Vec::new();
        let mut tree = UiTree::new();
        tree.set_theme(theme);

        let root = tree.insert(
            None,
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                size: LayoutSize {
                    width: percent(1.0),
                    height: percent(1.0),
                },
                ..Default::default()
            })
            .background(Role::Background),
        );

        let header = header(&mut tree, root, &feed, grouping, &view.shown, &mut pulses);
        for (panel, item) in Panel::ALL.into_iter().zip(header.hints) {
            tree.add_shortcut(panel.shortcut(), item);
        }
        let body = tree.insert(
            Some(root),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(GUTTER),
                padding: LayoutRect::length(GUTTER),
                ..fill_remaining()
            }),
        );
        let mut queue_buttons = QueueButtons::default();
        let mut timeline_parts = None;
        #[cfg(test)]
        let mut panels = [None; 5];
        let now_shown = Panel::NOW.iter().any(|p| view.shown[*p as usize]);
        let rows = [
            (Panel::NOW.as_slice(), fill_remaining()),
            (Panel::HISTORY.as_slice(), history_row(now_shown)),
        ];
        let nobody = feed.link.as_ref().filter(|_| workers.is_empty());
        if let Some(link) = nobody {
            empty(&mut tree, body, link);
        }
        // The demo's numbers stand for lines; the live ones count preview
        // updates.
        let output_caption = if feed.link.is_some() {
            "出力の更新（回 / 5 秒）"
        } else {
            "出力（行 / 間隔）"
        };
        for (members, style) in rows {
            if nobody.is_some() {
                break;
            }
            if !members.iter().any(|p| view.shown[*p as usize]) {
                continue;
            }
            let row = tree.insert(
                Some(body),
                Container::new(Style {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::STRETCH,
                    gap: gap(GUTTER),
                    ..style
                }),
            );
            for &member in members.iter().filter(|p| view.shown[**p as usize]) {
                let id = panel(&mut tree, row, member.style(&view.shown));
                #[cfg(test)]
                {
                    panels[member as usize] = Some(id);
                }
                match member {
                    Panel::Queue => {
                        queue_buttons = queue(&mut tree, id, workers, view.expanded, &mut pulses);
                    }
                    Panel::Running => {
                        let cards = Cards {
                            grouping,
                            output_caption,
                        };
                        running(&mut tree, id, workers, cards, &mut pulses);
                    }
                    Panel::Done => done(&mut tree, id, workers, grouping, &mut pulses),
                    Panel::Timeline => {
                        timeline_parts = Some(timeline(&mut tree, id, workers, grouping));
                    }
                    Panel::Graph => graph(&mut tree, id, &feed.fleet),
                }
            }
        }
        if let Some(channel) = &feed.channel {
            channel.send(Command::Expand(queue_buttons.expanded));
        }

        Self {
            tree,
            view,
            feed,
            status: header.status,
            grouping_control: header.grouping_control,
            queue_buttons,
            pulses,
            timeline: timeline_parts,
            viewport: None,
            #[cfg(test)]
            panels,
        }
    }
}

impl Screen for Dashboard {
    fn tree(&self) -> &UiTree {
        &self.tree
    }

    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }

    fn prepare(&mut self, viewport: Size, text: &mut TextStore) -> bool {
        self.viewport = Some(viewport);
        self.tree.prepare(viewport, text).is_some()
    }

    fn before_frame(&mut self, text: &mut TextStore) {
        let latest = self
            .feed
            .channel
            .as_ref()
            .and_then(|channel| channel.snapshots.try_iter().last());
        if let Some(snapshot) = latest {
            self.apply(snapshot, text);
        }
    }

    fn handle(&mut self, message: Message, text: &mut TextStore) {
        if let Some(ShortcutPressed(pressed)) = message.downcast_ref::<ShortcutPressed>() {
            if let Some(panel) = Panel::ALL.into_iter().find(|p| p.shortcut() == *pressed) {
                let mut shown = self.view.shown;
                shown[panel as usize] = !shown[panel as usize];
                self.rebuilt(View { shown, ..self.view }, text);
            }
            return;
        }
        if let Some(SelectionChanged(index)) = message.downcast_ref::<SelectionChanged>()
            && message.source == self.grouping_control
        {
            self.rebuilt(
                View {
                    grouping: Grouping::ALL[*index],
                    ..self.view
                },
                text,
            );
            return;
        }
        if message.downcast_ref::<ButtonClicked>().is_none() {
            return;
        }
        if let Some(pane) = QueueButtons::worker(&self.queue_buttons.details, message.source) {
            self.rebuilt(
                View {
                    expanded: Some(pane),
                    ..self.view
                },
                text,
            );
        } else if let Some(pane) = QueueButtons::worker(&self.queue_buttons.open, message.source)
            && let Some(worker) = self.feed.fleet.worker(pane)
        {
            let status = match &self.feed.channel {
                Some(channel) => {
                    channel.send(Command::Focus(pane));
                    format!("{} のペインへ移動します", worker.label)
                }
                None => format!("モック: {} のペインへ移動します", worker.label),
            };
            set_label(&mut self.tree, self.status, status);
        }
    }
}

fn gap(px: f32) -> LayoutSize<LengthPercentage> {
    LayoutSize {
        width: length(px),
        height: length(px),
    }
}

fn history_row(now_shown: bool) -> Style {
    if now_shown {
        Style {
            size: LayoutSize {
                width: auto(),
                height: length(HISTORY_HEIGHT),
            },
            flex_shrink: 0.0,
            ..Default::default()
        }
    } else {
        fill_remaining()
    }
}

fn fill_remaining() -> Style {
    Style {
        flex_grow: 1.0,
        flex_basis: length(0.0),
        min_size: LayoutSize {
            width: length(0.0),
            height: length(0.0),
        },
        ..Default::default()
    }
}

fn spacer(tree: &mut UiTree, parent: WidgetId) {
    tree.insert(Some(parent), Container::row().grow(1.0));
}

fn need_color(need: Need) -> Color {
    match need {
        Need::Approval => YELLOW,
        Need::Failure => RED,
        Need::Question => ORANGE,
        Need::Notice => BLUE,
        Need::Quiet => GRAY,
    }
}

fn phase_color(phase: Phase) -> Color {
    match phase {
        Phase::Running => GREEN,
        Phase::Quiet => DIM_GREEN,
        Phase::Permission => YELLOW,
        Phase::Input => ORANGE,
        Phase::Ended => GRAY,
    }
}

struct Header {
    status: WidgetId,
    grouping_control: WidgetId,
    hints: [WidgetId; 5],
}

fn hint_strip(tree: &mut UiTree, bar: WidgetId, shown: &[bool; 5]) -> [WidgetId; 5] {
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

fn header(
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

fn panel(tree: &mut UiTree, parent: WidgetId, style: Style) -> WidgetId {
    tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(8.0),
            padding: LayoutRect::length(10.0),
            ..style
        })
        .background(Role::Surface)
        .radius(10.0),
    )
}

fn fixed_width(width: f32) -> Style {
    Style {
        size: LayoutSize {
            width: length(width),
            height: auto(),
        },
        flex_shrink: 0.0,
        ..Default::default()
    }
}

fn panel_title(tree: &mut UiTree, parent: WidgetId, title: &str, n: usize, tone: Tone) {
    let row = tree.insert(
        Some(parent),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    tree.insert(
        Some(row),
        Label::new(title)
            .font_size(12.0)
            .weight(700)
            .color(Role::Muted),
    );
    tree.insert(Some(row), Badge::count(n as u32).tone(tone));
}

fn card_list(tree: &mut UiTree, parent: WidgetId) -> WidgetId {
    tree.insert(
        Some(parent),
        ScrollView::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(8.0),
            ..fill_remaining()
        }),
    )
}

fn group_header(tree: &mut UiTree, parent: WidgetId, name: &str, n: usize) {
    tree.insert(
        Some(parent),
        Label::new(format!("{name} · {n}"))
            .font_size(11.0)
            .weight(700)
            .color(Role::Muted),
    );
}

fn caption(tree: &mut UiTree, parent: WidgetId, text: impl Into<String>) -> WidgetId {
    tree.insert(
        Some(parent),
        Label::new(text).font_size(11.0).color(Role::Muted),
    )
}

fn set_label(tree: &mut UiTree, id: WidgetId, text: String) {
    tree.update::<Label, _>(id, |l, cx| l.set_text(cx, text));
}

fn set_tag(tree: &mut UiTree, id: WidgetId, text: String) {
    tree.update::<Tag, _>(id, |t, cx| t.set_text(cx, text));
}

fn waited_text(workers: &[Worker]) -> String {
    let waited: f64 = workers
        .iter()
        .map(|w| w.minutes_where(Phase::waits_on_a_person))
        .sum();
    format!("人待ち合計 {}", minutes_text(waited))
}

/// The worker waiting longest on a person. Only these needs have a wait
/// text, known or a lower bound; a 通知 or a quiet worker has none.
fn longest(workers: &[Worker]) -> Option<&Worker> {
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

/// The parts that are known, joined by `separator`.
fn joined(parts: &[&str], separator: &str) -> String {
    parts
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(separator)
}

fn longest_text(worker: &Worker) -> String {
    joined(&["最長", &worker.label, &worker.since], " ")
}

fn queue_since(worker: &Worker, need: Need) -> String {
    joined(&[need.title(), &worker.since], " ")
}

fn quiet_text(worker: &Worker) -> String {
    format!("静止 {:.0} 分", worker.wait_minutes)
}

/// Only times whose start is known are shown.
fn running_caption(worker: &Worker) -> String {
    let started = worker
        .started_minutes
        .map(|minutes| format!("{}前に開始", minutes_text(minutes)))
        .unwrap_or_default();
    let reports = format!("報告 {} 回", worker.reports);
    joined(&[&started, &reports, &worker.since], " · ")
}

fn done_caption(worker: &Worker) -> String {
    if worker.since.is_empty() {
        return "完了".to_owned();
    }
    let took = worker
        .took_minutes
        .map(|minutes| format!("所要 {}", minutes_text(minutes)))
        .unwrap_or_default();
    joined(&[&worker.since, &took], " · ")
}

fn span_text(span: f64) -> String {
    format!("← {}前　　いま →", minutes_text(span))
}

fn empty(tree: &mut UiTree, body: WidgetId, link: &Link) {
    let state = match link {
        Link::Connecting => EmptyState::new("Noa に接続しています…"),
        Link::Live => EmptyState::new("エージェントのペインが見つかりません").message(
            "claude / codex / agy が動いているペインと、+ctl で起動したワーカーがここに出ます",
        ),
        Link::Down(reason) => EmptyState::new("エージェントのペインが見つかりません")
            .message(format!("Noa に接続できません: {reason}")),
    };
    state.width(560.0).insert(tree, body);
}

fn task_line(tree: &mut UiTree, parent: WidgetId, worker: &Worker) {
    if worker.task.is_empty() {
        return;
    }
    tree.insert(
        Some(parent),
        Label::new(format!("依頼: {}", worker.task))
            .font_size(12.0)
            .color(Role::Muted)
            .wrap(true)
            .max_lines(2),
    );
}

fn detail_line(tree: &mut UiTree, parent: WidgetId, worker: &Worker) {
    if worker.detail.is_empty() {
        return;
    }
    tree.insert(
        Some(parent),
        Label::new(worker.detail.as_str())
            .font_size(12.0)
            .wrap(true),
    );
}

/// The kind, model and branch tags every card starts with; no branch tag
/// when the branch is unknown.
fn identity_tags(tree: &mut UiTree, tags: WidgetId, worker: &Worker) {
    tree.insert(
        Some(tags),
        Tag::new(worker.kind.as_str()).tone(Tone::Accent),
    );
    model_tag(tree, tags, worker);
    branch_tag(tree, tags, worker);
}

/// A plain command has no model to show.
fn model_tag(tree: &mut UiTree, parent: WidgetId, worker: &Worker) {
    if worker.is_agent() {
        tree.insert(Some(parent), Tag::new(worker.model_line()));
    }
}

fn branch_tag(tree: &mut UiTree, parent: WidgetId, worker: &Worker) {
    if !worker.branch.is_empty() {
        tree.insert(
            Some(parent),
            Tag::new(worker.branch.as_str()).style(TagStyle::Outlined),
        );
    }
}

fn queue(
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

fn queue_card(
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
    tree.insert(Some(head), Label::new(worker.label.as_str()).weight(700));
    tree.insert(
        Some(head),
        Tag::new(worker.kind.as_str()).tone(Tone::Accent),
    );
    spacer(tree, head);
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

fn queue_card_summary(tree: &mut UiTree, card: WidgetId, worker: &Worker) -> WidgetId {
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

fn queue_card_body(tree: &mut UiTree, card: WidgetId, worker: &Worker) -> WidgetId {
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

/// How the running and done panels lay their cards out.
#[derive(Clone, Copy)]
struct Cards {
    grouping: Grouping,
    output_caption: &'static str,
}

fn running(
    tree: &mut UiTree,
    column: WidgetId,
    workers: &[Worker],
    cards: Cards,
    pulses: &mut Vec<(WidgetId, Pulse)>,
) {
    let members: Vec<&Worker> = workers
        .iter()
        .filter(|w| w.state == State::Running)
        .collect();
    panel_title(tree, column, "作業中", members.len(), Tone::Success);
    let scroll = card_list(tree, column);
    for (name, group) in cards.grouping.split(members) {
        if let Some(name) = name {
            group_header(tree, scroll, name, group.len());
        }
        let grid = tree.insert(Some(scroll), Container::wrapping_row(8.0, 8.0));
        for worker in group {
            running_card(tree, grid, worker, cards.output_caption, pulses);
        }
    }
}

fn running_card(
    tree: &mut UiTree,
    grid: WidgetId,
    worker: &Worker,
    output_caption: &str,
    pulses: &mut Vec<(WidgetId, Pulse)>,
) {
    let pane = worker.pane;
    let card = tree.insert(
        Some(grid),
        GroupBox::card(worker.label.as_str()).width(RUNNING_CARD_WIDTH),
    );
    let tags = tree.insert(
        Some(card),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    identity_tags(tree, tags, worker);
    if worker.quiet {
        let id = tree.insert(
            Some(tags),
            Tag::new(quiet_text(worker)).tone(Tone::Critical),
        );
        pulses.push((id, Pulse::QuietTag(pane)));
    }
    task_line(tree, card, worker);
    detail_line(tree, card, worker);

    let meters = tree.insert(
        Some(card),
        Container::row()
            .align_items(AlignItems::END)
            .justify_content(JustifyContent::SPACE_BETWEEN)
            .gap(12.0),
    );
    let output = tree.insert(Some(meters), Container::column().gap(2.0));
    caption(tree, output, output_caption);
    let chart = tree.insert(
        Some(output),
        Chart::line(vec![String::new(); worker.lines_per_tick.len()])
            .series(ChartSeries::new("行", worker.lines_per_tick.clone()))
            .sparkline()
            .width(170.0)
            .height(26.0),
    );
    pulses.push((chart, Pulse::Output(pane)));
    let fresh = tree.insert(Some(meters), Container::column().gap(2.0));
    caption(tree, fresh, "鮮度");
    let level = tree.insert(
        Some(fresh),
        LevelIndicator::new(
            LevelStyle::DiscreteCapacity,
            0.0,
            FRESHNESS_MAX,
            worker.freshness,
        )
        .warning(2.0)
        .critical(1.0)
        .width(80.0),
    );
    pulses.push((level, Pulse::Freshness(pane)));

    let id = caption(tree, card, running_caption(worker));
    pulses.push((id, Pulse::RunningCaption(pane)));
}

fn done(
    tree: &mut UiTree,
    column: WidgetId,
    workers: &[Worker],
    grouping: Grouping,
    pulses: &mut Vec<(WidgetId, Pulse)>,
) {
    let members: Vec<&Worker> = workers
        .iter()
        .filter(|w| w.state == State::Finished)
        .collect();
    panel_title(tree, column, "完了", members.len(), Tone::Neutral);
    let list = card_list(tree, column);
    for (name, group) in grouping.split(members) {
        if let Some(name) = name {
            group_header(tree, list, name, group.len());
        }
        for worker in group {
            done_card(tree, list, worker, pulses);
        }
    }
}

fn done_card(
    tree: &mut UiTree,
    list: WidgetId,
    worker: &Worker,
    pulses: &mut Vec<(WidgetId, Pulse)>,
) {
    let card = tree.insert(
        Some(list),
        GroupBox::card(worker.label.as_str()).fill_width(),
    );
    let tags = tree.insert(
        Some(card),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    identity_tags(tree, tags, worker);
    if worker.has_result {
        tree.insert(Some(tags), Tag::new("結果あり").tone(Tone::Success));
    }
    task_line(tree, card, worker);
    detail_line(tree, card, worker);
    let id = caption(tree, card, done_caption(worker));
    pulses.push((id, Pulse::DoneCaption(worker.pane)));
}

fn timeline_span(workers: &[Worker]) -> f64 {
    workers
        .iter()
        .map(Worker::total_minutes)
        .fold(0.0, f64::max)
}

fn timeline(
    tree: &mut UiTree,
    section: WidgetId,
    workers: &[Worker],
    grouping: Grouping,
) -> TimelineParts {
    let span = timeline_span(workers);

    let title = tree.insert(
        Some(section),
        Container::row().align_items(AlignItems::CENTER).gap(12.0),
    );
    tree.insert(
        Some(title),
        Label::new("状態の推移")
            .font_size(12.0)
            .weight(700)
            .color(Role::Muted),
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

    let lanes = tree.insert(
        Some(section),
        ScrollView::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(5.0),
            ..fill_remaining()
        }),
    );
    fill_lanes(tree, lanes, workers, grouping, span);
    TimelineParts {
        span: span_caption,
        lanes,
    }
}

fn fill_lanes(
    tree: &mut UiTree,
    lanes: WidgetId,
    workers: &[Worker],
    grouping: Grouping,
    span: f64,
) {
    for (name, group) in grouping.split(workers) {
        if let Some(name) = name {
            group_header(tree, lanes, name, group.len());
        }
        for worker in group {
            lane(tree, lanes, worker, span);
        }
    }
}

fn lane(tree: &mut UiTree, lanes: WidgetId, worker: &Worker, span: f64) {
    let row = tree.insert(
        Some(lanes),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let name = tree.insert(Some(row), Container::row().width(LANE_LABEL_WIDTH));
    tree.insert(
        Some(name),
        Label::new(worker.label.as_str())
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
        .radius(3.0)
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
}

fn mark(worker: &Worker) -> (&'static str, Color) {
    match (worker.need(), worker.state) {
        (Some(need), _) => (need.symbol(), need_color(need)),
        (None, State::Finished) => ("✓", GRAY),
        (None, _) => ("●", GREEN),
    }
}

fn graph(tree: &mut UiTree, section: WidgetId, fleet: &Fleet) {
    let workers = &fleet.workers;
    tree.insert(
        Some(section),
        Label::new("関連（親 → ワーカー → 孫）")
            .font_size(12.0)
            .weight(700)
            .color(Role::Muted),
    );
    let roots = tree.insert(Some(section), Container::column().gap(12.0));
    for orchestrator in &fleet.orchestrators {
        let row = tree.insert(
            Some(roots),
            Container::row().align_items(AlignItems::CENTER),
        );
        // An orchestrator that is a worker itself is the root, not a child.
        let own = orchestrator.pane.and_then(|pane| fleet.worker(pane));
        let (symbol, color) = own.map_or(("◆", BLUE), mark);
        node(tree, row, symbol, &orchestrator.name, color);
        let children: Vec<&Worker> = workers
            .iter()
            .filter(|w| w.orchestrator == orchestrator.name && w.parent.is_none())
            .filter(|w| Some(w.pane) != orchestrator.pane)
            .collect();
        connect(tree, row, workers, &children);
    }
    caption(
        tree,
        section,
        "● 作業中　! 承認待ち　? 質問　× 失敗　… 静止　✓ 完了",
    );
}

fn node(tree: &mut UiTree, row: WidgetId, symbol: &str, label: &str, color: Color) {
    let pill = tree.insert(
        Some(row),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::CENTER,
            gap: gap(4.0),
            padding: LayoutRect {
                left: length(7.0),
                right: length(7.0),
                top: length(1.0),
                bottom: length(1.0),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .background(Role::SurfaceAlt)
        .border(1.0, color)
        .radius(9.0),
    );
    tree.insert(
        Some(pill),
        Label::new(symbol).font_size(10.0).weight(700).color(color),
    );
    tree.insert(Some(pill), Label::new(label).font_size(11.0));
}

fn edge(tree: &mut UiTree, row: WidgetId, length_px: f32) {
    tree.insert(
        Some(row),
        Container::new(fixed_width(length_px))
            .height(1.0)
            .background(Role::Outline),
    );
}

fn connect(tree: &mut UiTree, row: WidgetId, workers: &[Worker], children: &[&Worker]) {
    if children.is_empty() {
        return;
    }
    edge(tree, row, 10.0);
    tree.insert(
        Some(row),
        Container::new(Style {
            align_self: Some(AlignItems::STRETCH),
            ..fixed_width(1.0)
        })
        .background(Role::Outline),
    );
    let column = tree.insert(Some(row), Container::column().gap(4.0));
    for child in children {
        let child_row = tree.insert(
            Some(column),
            Container::row().align_items(AlignItems::CENTER),
        );
        edge(tree, child_row, 8.0);
        let (symbol, color) = mark(child);
        node(tree, child_row, symbol, &child.label, color);
        let grandchildren: Vec<&Worker> = workers
            .iter()
            .filter(|w| w.parent == Some(child.pane))
            .collect();
        connect(tree, child_row, workers, &grandchildren);
    }
}

#[cfg(test)]
mod tests {
    use noa_gpui_core::{Event, Modifiers, PointerButton};
    use noa_gpui_scene::{DisplayList, DrawCommand, Point, Size};

    use super::*;

    fn window() -> Size {
        Size::new(1440.0, 900.0)
    }

    fn laid_out(grouping: Grouping) -> (Dashboard, TextStore) {
        let mut dashboard = Dashboard::new(grouping);
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(window(), &mut text);
        (dashboard, text)
    }

    fn bottom(tree: &UiTree, id: WidgetId) -> f32 {
        tree.window_origin(id).unwrap().y + tree.layout(id).unwrap().rect.height
    }

    fn click(dashboard: &mut Dashboard, text: &mut TextStore, pos: Point) {
        let button = PointerButton::Primary;
        dashboard.dispatch(Event::PointerMove { pos }, text);
        dashboard.dispatch(Event::PointerDown { pos, button }, text);
        dashboard.dispatch(Event::PointerUp { pos, button }, text);
        dashboard.prepare(window(), text);
    }

    fn label_of(dashboard: &Dashboard, pane: u64) -> &str {
        &dashboard.feed.fleet.worker(pane).unwrap().label
    }

    fn center(tree: &UiTree, id: WidgetId) -> Point {
        let origin = tree.window_origin(id).unwrap();
        let rect = tree.layout(id).unwrap().rect;
        Point::new(origin.x + rect.width / 2.0, origin.y + rect.height / 2.0)
    }

    #[test]
    fn every_card_needing_you_shows_without_scrolling() {
        for grouping in Grouping::ALL {
            let (dashboard, _text) = laid_out(grouping);
            let tree = &dashboard.tree;
            let buttons = &dashboard.queue_buttons;
            assert_eq!(buttons.open.len() + buttons.details.len(), 5);
            for (button, label) in buttons.open.iter().chain(&buttons.details) {
                let card = tree.parent(tree.parent(*button).unwrap()).unwrap();
                let list = tree.parent(card).unwrap();
                assert!(
                    bottom(tree, card) <= bottom(tree, list),
                    "{label} ends at {} below the list at {} ({grouping:?})",
                    bottom(tree, card),
                    bottom(tree, list)
                );
            }
        }
    }

    #[test]
    fn the_most_urgent_card_starts_expanded() {
        let (dashboard, _text) = laid_out(Grouping::Orchestrator);
        let open: Vec<&str> = dashboard
            .queue_buttons
            .open
            .iter()
            .map(|(_, pane)| label_of(&dashboard, *pane))
            .collect();
        assert_eq!(open, ["fix-tests"]);
    }

    #[test]
    fn details_expands_that_card_instead() {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        let (button, label) = dashboard.queue_buttons.details[1];
        let pos = center(&dashboard.tree, button);
        click(&mut dashboard, &mut text, pos);
        assert_eq!(dashboard.view.expanded, Some(label));
        let open: Vec<u64> = dashboard
            .queue_buttons
            .open
            .iter()
            .map(|(_, pane)| *pane)
            .collect();
        assert_eq!(open, [label]);
    }

    #[test]
    fn open_names_the_pane_it_would_go_to() {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        let (button, label) = dashboard.queue_buttons.open[0];
        let pos = center(&dashboard.tree, button);
        click(&mut dashboard, &mut text, pos);
        let status = dashboard.tree.get::<Label>(dashboard.status).unwrap();
        let label = label_of(&dashboard, label);
        assert!(status.text().contains(label), "{}", status.text());
    }

    #[test]
    fn the_last_segment_removes_the_groups() {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        let control = dashboard.grouping_control;
        let origin = dashboard.tree.window_origin(control).unwrap();
        let rect = dashboard.tree.layout(control).unwrap().rect;
        let pos = Point::new(origin.x + rect.width - 12.0, origin.y + rect.height / 2.0);
        click(&mut dashboard, &mut text, pos);
        assert_eq!(dashboard.view.grouping, Grouping::Flat);
    }

    fn shown_only(hidden: &[Panel]) -> [bool; 5] {
        let mut shown = [true; 5];
        for panel in hidden {
            shown[*panel as usize] = false;
        }
        shown
    }

    fn showing(grouping: Grouping, shown: [bool; 5]) -> (Dashboard, TextStore) {
        let mut dashboard = Dashboard::showing(View {
            grouping,
            expanded: None,
            shown,
        });
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(window(), &mut text);
        (dashboard, text)
    }

    fn rect(tree: &UiTree, id: WidgetId) -> [f32; 4] {
        let origin = tree.window_origin(id).unwrap();
        let size = tree.layout(id).unwrap().rect;
        [origin.x, origin.y, size.width, size.height]
    }

    fn assert_rect(actual: [f32; 4], expected: [f32; 4], what: &str) {
        for (a, e) in actual.iter().zip(expected) {
            assert!((a - e).abs() <= 0.5, "{what}: {actual:?} vs {expected:?}");
        }
    }

    fn assert_row_spans(dashboard: &Dashboard, members: &[Panel]) {
        let rects: Vec<[f32; 4]> = members
            .iter()
            .filter_map(|p| dashboard.panels[*p as usize])
            .map(|id| rect(&dashboard.tree, id))
            .collect();
        let (first, last) = (rects.first().unwrap(), rects.last().unwrap());
        assert!((first[0] - GUTTER).abs() <= 0.5, "{rects:?}");
        assert!(
            (last[0] + last[2] - (window().width - GUTTER)).abs() <= 0.5,
            "{rects:?}"
        );
        for pair in rects.windows(2) {
            let gap = pair[1][0] - (pair[0][0] + pair[0][2]);
            assert!((gap - GUTTER).abs() <= 0.5, "{rects:?}");
        }
    }

    #[test]
    fn all_panels_on_keeps_todays_layout() {
        let expected = [
            [12.0, 60.0, 400.0, 560.0],
            [424.0, 60.0, 722.0, 560.0],
            [1158.0, 60.0, 270.0, 560.0],
            [12.0, 632.0, 974.0, 256.0],
            [998.0, 632.0, 430.0, 256.0],
        ];
        for grouping in Grouping::ALL {
            let (dashboard, _text) = showing(grouping, [true; 5]);
            for panel in Panel::ALL {
                let id = dashboard.panels[panel as usize].unwrap();
                let actual = rect(&dashboard.tree, id);
                assert_rect(actual, expected[panel as usize], &format!("{panel:?}"));
            }
        }
    }

    #[test]
    fn a_hidden_panel_leaves_its_row_to_the_others() {
        for hidden in Panel::ALL {
            let (dashboard, _text) = showing(Grouping::Orchestrator, shown_only(&[hidden]));
            assert!(dashboard.panels[hidden as usize].is_none());
            assert_row_spans(&dashboard, &Panel::NOW);
            assert_row_spans(&dashboard, &Panel::HISTORY);
        }
        // Without its grower a row splits its free width equally over the
        // remaining panels, on top of each one's fixed width: the 1416 px row
        // less 12 px gap less 670 px fixed leaves 734 px, 367 px each.
        let row = window().width - 2.0 * GUTTER;
        let width_of = |dashboard: &Dashboard, panel: Panel| {
            rect(&dashboard.tree, dashboard.panels[panel as usize].unwrap())[2]
        };
        let (dashboard, _text) = showing(Grouping::Orchestrator, shown_only(&[Panel::Running]));
        let free = row - GUTTER - QUEUE_WIDTH - DONE_WIDTH;
        for (panel, fixed) in [(Panel::Queue, QUEUE_WIDTH), (Panel::Done, DONE_WIDTH)] {
            let width = width_of(&dashboard, panel);
            assert!(
                (width - (fixed + free / 2.0)).abs() <= 0.5,
                "{panel:?} is {width} px with running off"
            );
        }
        let (dashboard, _text) = showing(Grouping::Orchestrator, shown_only(&[Panel::Timeline]));
        let width = width_of(&dashboard, Panel::Graph);
        assert!(
            (width - row).abs() <= 0.5,
            "graph is {width} px with timeline off"
        );
        let body_height = window().height - HEADER_HEIGHT - 2.0 * GUTTER;
        let (dashboard, _text) = showing(Grouping::Orchestrator, shown_only(&Panel::NOW));
        assert_row_spans(&dashboard, &Panel::HISTORY);
        let id = dashboard.panels[Panel::Timeline as usize].unwrap();
        assert_eq!(rect(&dashboard.tree, id)[3].round(), body_height.round());
        let (dashboard, _text) = showing(Grouping::Orchestrator, shown_only(&Panel::HISTORY));
        assert_row_spans(&dashboard, &Panel::NOW);
        let id = dashboard.panels[Panel::Queue as usize].unwrap();
        assert_eq!(rect(&dashboard.tree, id)[3].round(), body_height.round());
    }

    #[test]
    fn the_queue_fits_with_any_other_panels_hidden() {
        let others = [Panel::Running, Panel::Done, Panel::Timeline, Panel::Graph];
        for grouping in Grouping::ALL {
            for mask in 0..16u8 {
                let hidden: Vec<Panel> = others
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| mask >> i & 1 == 1)
                    .map(|(_, p)| *p)
                    .collect();
                let (dashboard, _text) = showing(grouping, shown_only(&hidden));
                let tree = &dashboard.tree;
                let buttons = &dashboard.queue_buttons;
                assert_eq!(buttons.open.len() + buttons.details.len(), 5);
                for (button, label) in buttons.open.iter().chain(&buttons.details) {
                    let card = tree.parent(tree.parent(*button).unwrap()).unwrap();
                    let list = tree.parent(card).unwrap();
                    assert!(
                        bottom(tree, card) <= bottom(tree, list),
                        "{label} overflows with {hidden:?} hidden ({grouping:?})"
                    );
                }
            }
        }
    }

    const KEYS: [(char, Panel, &str); 5] = [
        ('1', Panel::Queue, "要対応"),
        ('2', Panel::Running, "作業中"),
        ('3', Panel::Done, "完了"),
        ('4', Panel::Timeline, "状態の推移"),
        ('5', Panel::Graph, "関連"),
    ];

    fn press(dashboard: &mut Dashboard, text: &mut TextStore, digit: char, modifiers: Modifiers) {
        let key = Key::Char(digit);
        dashboard.dispatch(Event::KeyDown { key, modifiers }, text);
        dashboard.prepare(window(), text);
    }

    fn hint_strip_of(dashboard: &Dashboard) -> WidgetId {
        let tree = &dashboard.tree;
        let slot = tree.parent(dashboard.status).unwrap();
        let bar = tree.parent(slot).unwrap();
        let at = tree.children(bar).iter().position(|c| *c == slot).unwrap();
        tree.children(bar)[at - 1]
    }

    #[test]
    fn each_shortcut_hides_and_restores_its_panel() {
        let expected = [
            [12.0, 60.0, 400.0, 560.0],
            [424.0, 60.0, 722.0, 560.0],
            [1158.0, 60.0, 270.0, 560.0],
            [12.0, 632.0, 974.0, 256.0],
            [998.0, 632.0, 430.0, 256.0],
        ];
        for modifiers in [Modifiers::default(), Modifiers::on_apple()] {
            for (digit, panel, _) in KEYS {
                let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
                press(&mut dashboard, &mut text, digit, modifiers);
                for other in Panel::ALL {
                    let present = dashboard.panels[other as usize].is_some();
                    assert_eq!(present, other != panel, "{other:?} after hiding {panel:?}");
                }
                press(&mut dashboard, &mut text, digit, modifiers);
                let id = dashboard.panels[panel as usize].unwrap();
                let actual = rect(&dashboard.tree, id);
                assert_rect(actual, expected[panel as usize], &format!("{panel:?}"));
            }
        }
    }

    #[test]
    fn the_header_shows_each_shortcut() {
        for shown in [[true; 5], [false; 5]] {
            let (dashboard, _text) = showing(Grouping::Orchestrator, shown);
            let tree = &dashboard.tree;
            let items = tree.children(hint_strip_of(&dashboard));
            assert_eq!(items.len(), 5);
            for ((digit, _, title), item) in KEYS.into_iter().zip(items) {
                let parts = tree.children(*item);
                let kbd = tree.get::<Kbd>(parts[0]).unwrap();
                assert_eq!(kbd.keys(), [digit.to_string()].as_slice());
                let label = tree.get::<Label>(parts[1]).unwrap();
                assert_eq!(label.text(), title);
            }
        }
    }

    /// Width of one hint item, worked out from its parts and not read back from
    /// the layout: a 20 px keycap, the 3 px item gap, the title (full-width
    /// characters are one em, 11 px, wide) and 2 px padding on each side.
    fn expected_item_width(panel: Panel) -> f32 {
        20.0 + 3.0 + 11.0 * panel.title().chars().count() as f32 + 4.0
    }

    /// The strip is the five items and four 8 px gaps: 332 px.
    fn expected_strip_width() -> f32 {
        Panel::ALL.map(expected_item_width).iter().sum::<f32>() + 4.0 * 8.0
    }

    fn assert_strip_holds_its_items(dashboard: &Dashboard, what: &str) {
        let tree = &dashboard.tree;
        let strip = rect(tree, hint_strip_of(dashboard));
        let items = tree.children(hint_strip_of(dashboard));
        assert_eq!(items.len(), 5, "{what}");
        for (panel, item) in Panel::ALL.into_iter().zip(items) {
            let item = rect(tree, *item);
            assert!(
                (item[2] - expected_item_width(panel)).abs() <= 0.5,
                "{what}: {panel:?} item is {} px, expected {}",
                item[2],
                expected_item_width(panel)
            );
            assert!(
                item[0] >= strip[0] - 0.5 && item[0] + item[2] <= strip[0] + strip[2] + 0.5,
                "{what}: {panel:?} item {item:?} is not inside the strip {strip:?}"
            );
        }
        assert!(
            (strip[2] - expected_strip_width()).abs() <= 0.5,
            "{what}: strip is {} px, expected {}",
            strip[2],
            expected_strip_width()
        );
    }

    #[test]
    fn the_hint_strip_fits_the_header() {
        let open = |dashboard: &mut Dashboard, text: &mut TextStore| {
            let (button, _) = dashboard.queue_buttons.open[0];
            let pos = center(&dashboard.tree, button);
            click(dashboard, text, pos);
        };
        let (default, _default_text) = laid_out(Grouping::Orchestrator);
        let (mut opened, mut opened_text) = laid_out(Grouping::Orchestrator);
        open(&mut opened, &mut opened_text);
        let queue_only = shown_only(&[Panel::Running, Panel::Done, Panel::Timeline, Panel::Graph]);
        let (mut narrow, mut narrow_text) = showing(Grouping::Orchestrator, queue_only);
        let long = "モック: とても長いワーカー名のペインへ移動します ".repeat(6);
        let status = narrow.status;
        narrow
            .tree
            .update::<Label, _>(status, |l, cx| l.set_text(cx, long));
        narrow.prepare(window(), &mut narrow_text);
        let (off, _off_text) = showing(Grouping::Orchestrator, [false; 5]);
        let default_widths: Vec<f32> = {
            let tree = &default.tree;
            let bar = tree.parent(hint_strip_of(&default)).unwrap();
            tree.children(bar)[..6]
                .iter()
                .map(|c| rect(tree, *c)[2])
                .collect()
        };
        for dashboard in [&default, &off, &opened, &narrow] {
            let tree = &dashboard.tree;
            let strip = hint_strip_of(dashboard);
            let bar = tree.parent(strip).unwrap();
            let rects: Vec<[f32; 4]> = tree.children(bar).iter().map(|c| rect(tree, *c)).collect();
            for pair in rects.windows(2) {
                assert!(pair[0][0] + pair[0][2] <= pair[1][0] + 0.5, "{rects:?}");
            }
            for (rect, expected) in rects.iter().zip(&default_widths) {
                assert!((rect[2] - expected).abs() <= 0.5, "{rects:?}");
            }
            let last = rects.last().unwrap();
            assert!(
                last[0] + last[2] <= window().width - 16.0 + 0.5,
                "{rects:?}"
            );
            assert_strip_holds_its_items(dashboard, "header state");
        }
        // The status label lives inside its own container; it must stay inside
        // it, and where its text is longer than the container it is cut short.
        for (name, dashboard, cut) in [
            ("default", &default, false),
            ("all off", &off, false),
            ("after 開く", &opened, true),
            ("long status", &narrow, true),
        ] {
            let tree = &dashboard.tree;
            let slot = rect(tree, tree.parent(dashboard.status).unwrap());
            let label = rect(tree, dashboard.status);
            assert!(
                label[0] >= slot[0] - 0.5 && label[0] + label[2] <= slot[0] + slot[2] + 0.5,
                "{name}: status label {label:?} is not inside its container {slot:?}"
            );
            let status = tree.get::<Label>(dashboard.status).unwrap();
            assert_eq!(
                status.shown_text() != status.text(),
                cut,
                "{name}: shows {:?} of {:?}",
                status.shown_text(),
                status.text()
            );
        }
        let (default_tree, default_strip) = (&default.tree, hint_strip_of(&default));
        let default_rect = rect(default_tree, default_strip);
        let mut states: Vec<(String, [bool; 5])> = Panel::ALL
            .map(|p| (format!("{p:?} off"), shown_only(&[p])))
            .into();
        states.push(("all off".into(), [false; 5]));
        for (name, shown) in states {
            let (dashboard, _text) = showing(Grouping::Orchestrator, shown);
            let actual = rect(&dashboard.tree, hint_strip_of(&dashboard));
            assert_rect(actual, default_rect, &format!("hint strip, {name}"));
        }
    }

    fn layout_count(view: View) -> usize {
        let mut dashboard = Dashboard::showing(view);
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(window(), &mut text);
        text.layouts().len()
    }

    fn view_of(dashboard: &Dashboard) -> View {
        dashboard.view
    }

    #[test]
    fn hide_and_restore_cycles_release_the_old_text_layouts() {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        let fresh = layout_count(view_of(&dashboard));
        assert_eq!(text.layouts().len(), fresh);
        for _ in 0..3 {
            press(&mut dashboard, &mut text, '1', Modifiers::default());
            press(&mut dashboard, &mut text, '1', Modifiers::default());
        }
        assert_eq!(text.layouts().len(), fresh, "after hide/restore cycles");
        // A screen with a panel off owns fewer layouts than one with all on.
        press(&mut dashboard, &mut text, '2', Modifiers::default());
        assert_eq!(text.layouts().len(), layout_count(view_of(&dashboard)));
        assert!(text.layouts().len() < fresh);
    }

    #[test]
    fn a_grouping_change_releases_the_old_text_layouts() {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        let control = dashboard.grouping_control;
        let origin = dashboard.tree.window_origin(control).unwrap();
        let rect = dashboard.tree.layout(control).unwrap().rect;
        let pos = Point::new(origin.x + rect.width - 12.0, origin.y + rect.height / 2.0);
        click(&mut dashboard, &mut text, pos);
        assert_eq!(dashboard.view.grouping, Grouping::Flat);
        assert_eq!(text.layouts().len(), layout_count(view_of(&dashboard)));
    }

    #[test]
    fn a_details_click_releases_the_old_text_layouts() {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        let (button, label) = dashboard.queue_buttons.details[1];
        let pos = center(&dashboard.tree, button);
        click(&mut dashboard, &mut text, pos);
        assert_eq!(dashboard.view.expanded, Some(label));
        assert_eq!(text.layouts().len(), layout_count(view_of(&dashboard)));
    }

    fn last_segment(dashboard: &Dashboard) -> Point {
        let control = dashboard.grouping_control;
        let origin = dashboard.tree.window_origin(control).unwrap();
        let rect = dashboard.tree.layout(control).unwrap().rect;
        Point::new(origin.x + rect.width - 12.0, origin.y + rect.height / 2.0)
    }

    #[test]
    fn a_press_right_after_a_panel_toggle_hits_the_grouping_control() {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        let pos = last_segment(&dashboard);
        let button = PointerButton::Primary;
        let key = Key::Char('2');
        let modifiers = Modifiers::default();
        // The window delivers input before it lays out for the next frame:
        // no `prepare` between the toggle and the press.
        dashboard.dispatch(Event::KeyDown { key, modifiers }, &mut text);
        assert!(dashboard.panels[Panel::Running as usize].is_none());
        dashboard.dispatch(Event::PointerDown { pos, button }, &mut text);
        dashboard.prepare(window(), &mut text);
        dashboard.dispatch(Event::PointerUp { pos, button }, &mut text);
        assert_eq!(dashboard.view.grouping, Grouping::Flat);
    }

    #[test]
    fn a_press_right_after_a_details_click_hits_the_open_button() {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        let (details, label) = dashboard.queue_buttons.details[1];
        let pos = center(&dashboard.tree, details);
        let button = PointerButton::Primary;
        dashboard.dispatch(Event::PointerDown { pos, button }, &mut text);
        dashboard.dispatch(Event::PointerUp { pos, button }, &mut text);
        assert_eq!(dashboard.view.expanded, Some(label));
        // The new 開く button's place is known from the rebuilt tree before any
        // `prepare`, so the press can be aimed at it.
        let (open, _) = dashboard.queue_buttons.open[0];
        let pos = center(&dashboard.tree, open);
        dashboard.dispatch(Event::PointerDown { pos, button }, &mut text);
        dashboard.prepare(window(), &mut text);
        dashboard.dispatch(Event::PointerUp { pos, button }, &mut text);
        let status = dashboard.tree.get::<Label>(dashboard.status).unwrap();
        let label = label_of(&dashboard, label);
        assert!(status.text().contains(label), "{}", status.text());
    }

    /// The display list as plain text, with each text layout given by its size:
    /// layout ids depend on the `TextStore` that made them.
    fn frame_of(list: &DisplayList, text: &TextStore) -> Vec<String> {
        list.commands()
            .iter()
            .map(|command| match command {
                DrawCommand::Text {
                    rect,
                    layout,
                    color,
                } => format!("Text {rect:?} {color:?} {:?}", text.size(*layout)),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// What the window does after input and on the next frame: it redraws only if
    /// `needs_update`, and presents only if `prepare` says the list changed. The
    /// list it presents must be the rebuilt screen's, the same as a screen built
    /// for that view from scratch.
    fn assert_next_frame_shows_the_rebuild(
        dashboard: &mut Dashboard,
        text: &mut TextStore,
        before: &[String],
    ) {
        assert!(
            dashboard.needs_update(window()),
            "no redraw is requested after the rebuild"
        );
        assert!(
            dashboard.prepare(window(), text),
            "the frame after the rebuild presents nothing"
        );
        let shown = frame_of(dashboard.display_list(), text);
        let fresh = Dashboard::showing(dashboard.view);
        assert_eq!(shown, fresh_frame(fresh));
        assert_ne!(shown, before, "the frame still shows the old screen");
    }

    fn fresh_frame(mut fresh: Dashboard) -> Vec<String> {
        let mut fresh_text = TextStore::new(Default::default());
        fresh.prepare(window(), &mut fresh_text);
        frame_of(fresh.display_list(), &fresh_text)
    }

    #[test]
    fn a_panel_toggle_reaches_the_next_frame() {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        let before = frame_of(dashboard.display_list(), &text);
        let key = Key::Char('2');
        let modifiers = Modifiers::default();
        dashboard.dispatch(Event::KeyDown { key, modifiers }, &mut text);
        assert!(dashboard.panels[Panel::Running as usize].is_none());
        assert_next_frame_shows_the_rebuild(&mut dashboard, &mut text, &before);
    }

    #[test]
    fn a_grouping_change_reaches_the_next_frame() {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        let before = frame_of(dashboard.display_list(), &text);
        let pos = last_segment(&dashboard);
        let button = PointerButton::Primary;
        dashboard.dispatch(Event::PointerDown { pos, button }, &mut text);
        dashboard.dispatch(Event::PointerUp { pos, button }, &mut text);
        assert_eq!(dashboard.view.grouping, Grouping::Flat);
        assert_next_frame_shows_the_rebuild(&mut dashboard, &mut text, &before);
    }

    #[test]
    fn a_details_click_reaches_the_next_frame() {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        let before = frame_of(dashboard.display_list(), &text);
        let (details, label) = dashboard.queue_buttons.details[1];
        let pos = center(&dashboard.tree, details);
        let button = PointerButton::Primary;
        dashboard.dispatch(Event::PointerDown { pos, button }, &mut text);
        dashboard.dispatch(Event::PointerUp { pos, button }, &mut text);
        assert_eq!(dashboard.view.expanded, Some(label));
        assert_next_frame_shows_the_rebuild(&mut dashboard, &mut text, &before);
    }

    #[test]
    fn a_rebuild_keeps_the_appearance() {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        dashboard.set_system_appearance(Appearance::Light);
        assert_eq!(dashboard.tree.theme().appearance, Appearance::Light);
        press(&mut dashboard, &mut text, '1', Modifiers::default());
        assert!(dashboard.panels[Panel::Queue as usize].is_none());
        assert_eq!(dashboard.tree.theme().appearance, Appearance::Light);
    }

    #[test]
    fn the_grouping_control_stays_reachable_at_1140() {
        let size = Size::new(1140.0, 900.0);
        let mut dashboard = Dashboard::new(Grouping::Orchestrator);
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(size, &mut text);
        let control = dashboard.grouping_control;
        let origin = dashboard.tree.window_origin(control).unwrap();
        let width = dashboard.tree.layout(control).unwrap().rect.width;
        let right = origin.x + width;
        assert!(right <= size.width - 16.0 + 0.5, "control ends at {right}");
        let pos = Point::new(right - 12.0, origin.y + 8.0);
        let button = PointerButton::Primary;
        dashboard.dispatch(Event::PointerMove { pos }, &mut text);
        dashboard.dispatch(Event::PointerDown { pos, button }, &mut text);
        dashboard.dispatch(Event::PointerUp { pos, button }, &mut text);
        assert_eq!(dashboard.view.grouping, Grouping::Flat);
    }

    fn live(fleet: Fleet, synced_secs: u64) -> Snapshot {
        Snapshot {
            fleet,
            link: Link::Live,
            synced_secs,
        }
    }

    /// The canned fleet with six more running workers, so that the running
    /// list scrolls at the default window size.
    fn busy_fleet() -> Fleet {
        let mut fleet = fleet::canned();
        for n in 0..6 {
            let mut worker = fleet.workers[0].clone();
            worker.pane = 100 + n;
            worker.label = format!("extra-{n}");
            fleet.workers.push(worker);
        }
        fleet
    }

    /// The same workers a while later: only what time changes differs.
    fn later(fleet: &Fleet) -> Fleet {
        let mut later = fleet.clone();
        for worker in &mut later.workers {
            worker.since = format!("later {}", worker.pane);
            worker.wait_minutes += 1.0;
            worker.reports += 1;
            worker.lines_per_tick.iter_mut().for_each(|v| *v += 1.0);
            worker.freshness = 2.0;
            worker.phases.push((Phase::Running, 5.0));
        }
        later
    }

    fn pulse_id(dashboard: &Dashboard, wanted: impl Fn(Pulse) -> bool) -> WidgetId {
        let (id, _) = dashboard.pulses.iter().find(|(_, p)| wanted(*p)).unwrap();
        *id
    }

    #[test]
    fn a_volatile_snapshot_updates_in_place() {
        let fleet = busy_fleet();
        let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet.clone(), 1));
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(window(), &mut text);
        let list = list_of(&dashboard);
        dashboard
            .tree
            .update::<ScrollView, _>(list, |s, cx| s.set_offset(cx, 40.0));
        dashboard.prepare(window(), &mut text);
        let offset = |d: &Dashboard| d.tree.get::<ScrollView>(list).map(ScrollView::offset);
        assert_eq!(offset(&dashboard), Some(40.0), "the running list scrolls");
        let chart = pulse_id(&dashboard, |p| matches!(p, Pulse::Output(11)));
        let since = pulse_id(&dashboard, |p| matches!(p, Pulse::QueueSince(14)));

        let later = later(&fleet);
        dashboard.apply(live(later.clone(), 4), &mut text);
        dashboard.prepare(window(), &mut text);

        assert_eq!(
            offset(&dashboard),
            Some(40.0),
            "the running list was rebuilt"
        );
        let values = &dashboard.tree.get::<Chart>(chart).unwrap().series_list()[0].values;
        assert_eq!(values, &later.worker(11).unwrap().lines_per_tick);
        let since = dashboard.tree.get::<Label>(since).unwrap();
        assert_eq!(since.text(), "承認待ち later 14");
        let status = dashboard.tree.get::<Label>(dashboard.status).unwrap();
        assert!(status.text().contains("同期 4 秒前"), "{}", status.text());
        // The lanes were made again and the old ones' text layouts released:
        // the screen shows and owns what one built for the later fleet does.
        let mut fresh = Dashboard::of_snapshot(Grouping::Orchestrator, live(later, 4));
        let mut fresh_text = TextStore::new(Default::default());
        fresh.prepare(window(), &mut fresh_text);
        let fresh_list = list_of(&fresh);
        fresh
            .tree
            .update::<ScrollView, _>(fresh_list, |s, cx| s.set_offset(cx, 40.0));
        fresh.prepare(window(), &mut fresh_text);
        assert_eq!(
            frame_of(dashboard.display_list(), &text),
            frame_of(fresh.display_list(), &fresh_text)
        );
        assert_eq!(text.layouts().len(), fresh_text.layouts().len());
    }

    fn list_of(dashboard: &Dashboard) -> WidgetId {
        let running = dashboard.panels[Panel::Running as usize].unwrap();
        dashboard.tree.children(running)[1]
    }

    #[test]
    fn a_structural_snapshot_rebuilds_the_screen() {
        let fleet = fleet::canned();
        let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet.clone(), 1));
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(window(), &mut text);
        let before = frame_of(dashboard.display_list(), &text);
        let mut changed = fleet;
        let worker = changed.workers.iter_mut().find(|w| w.pane == 12).unwrap();
        worker.state = State::Permission;
        worker.detail = "Bash: cargo publish".into();
        dashboard.apply(live(changed.clone(), 1), &mut text);
        let buttons = &dashboard.queue_buttons;
        assert_eq!(buttons.open.len() + buttons.details.len(), 6);
        assert!(buttons.details.iter().any(|(_, pane)| *pane == 12));
        assert!(dashboard.needs_update(window()));
        assert!(dashboard.prepare(window(), &mut text));
        let shown = frame_of(dashboard.display_list(), &text);
        let fresh = Dashboard::of_snapshot(Grouping::Orchestrator, live(changed, 1));
        assert_eq!(shown, fresh_frame(fresh));
        assert_ne!(shown, before);
    }

    #[test]
    fn with_no_workers_the_body_says_why() {
        let down = Snapshot {
            fleet: Fleet::default(),
            link: Link::Down("no token".into()),
            synced_secs: 0,
        };
        let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, down);
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(window(), &mut text);
        assert!(dashboard.panels.iter().all(Option::is_none));
        let tree = &dashboard.tree;
        let texts = labels_under(tree, tree.root().unwrap());
        assert!(
            texts
                .iter()
                .any(|t| t.contains("Noa に接続できません: no token")),
            "{texts:?}"
        );
        let status = tree.get::<Label>(dashboard.status).unwrap();
        assert_eq!(status.text(), "切断: no token");
    }

    fn labels_under(tree: &UiTree, id: WidgetId) -> Vec<String> {
        let own = tree.get::<Label>(id).map(|l| l.text().to_owned());
        own.into_iter()
            .chain(
                tree.children(id)
                    .iter()
                    .flat_map(|child| labels_under(tree, *child)),
            )
            .collect()
    }

    #[test]
    fn a_live_screen_follows_its_feed() {
        let (channel, snapshots, commands) = Channel::detached();
        let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(window(), &mut text);
        assert_eq!(commands.try_recv(), Ok(Command::Expand(None)));
        let status = dashboard.tree.get::<Label>(dashboard.status).unwrap();
        assert_eq!(status.text(), "接続中…");

        snapshots.send(live(fleet::canned(), 0)).unwrap();
        snapshots.send(live(fleet::canned(), 2)).unwrap();
        dashboard.before_frame(&mut text);
        dashboard.prepare(window(), &mut text);
        assert_eq!(commands.try_recv(), Ok(Command::Expand(Some(14))));
        let status = dashboard.tree.get::<Label>(dashboard.status).unwrap();
        assert_eq!(status.text(), "ワーカー 9 · 同期 2 秒前");

        let (open, pane) = dashboard.queue_buttons.open[0];
        let pos = center(&dashboard.tree, open);
        click(&mut dashboard, &mut text, pos);
        assert_eq!(commands.try_recv(), Ok(Command::Focus(pane)));
        let status = dashboard.tree.get::<Label>(dashboard.status).unwrap();
        assert_eq!(status.text(), "fix-tests のペインへ移動します");
    }

    fn text_of(dashboard: &Dashboard, wanted: impl Fn(Pulse) -> bool) -> String {
        let id = pulse_id(dashboard, wanted);
        dashboard.tree.get::<Label>(id).unwrap().text().to_owned()
    }

    #[test]
    fn unknown_times_and_results_are_not_shown() {
        let mut fleet = fleet::canned();
        for worker in &mut fleet.workers {
            worker.started_minutes = None;
            worker.took_minutes = None;
            worker.has_result = false;
        }
        fleet
            .workers
            .iter_mut()
            .find(|w| w.pane == 18)
            .unwrap()
            .since = String::new();
        let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(window(), &mut text);
        assert_eq!(
            text_of(&dashboard, |p| matches!(p, Pulse::RunningCaption(11))),
            "報告 7 回 · 8 秒前に出力"
        );
        assert_eq!(
            text_of(&dashboard, |p| matches!(p, Pulse::DoneCaption(18))),
            "完了"
        );
        assert_eq!(
            text_of(&dashboard, |p| matches!(p, Pulse::DoneCaption(17))),
            "4 分前に完了"
        );
        let tree = &dashboard.tree;
        let tags = tags_under(tree, tree.root().unwrap());
        assert!(!tags.iter().any(|t| t == "結果あり"), "{tags:?}");
    }

    fn tags_under(tree: &UiTree, id: WidgetId) -> Vec<String> {
        let own = tree.get::<Tag>(id).map(|t| t.text().to_owned());
        own.into_iter()
            .chain(
                tree.children(id)
                    .iter()
                    .flat_map(|child| tags_under(tree, *child)),
            )
            .collect()
    }

    #[test]
    fn the_demo_done_cards_keep_their_times_and_results() {
        let (dashboard, _text) = laid_out(Grouping::Orchestrator);
        assert_eq!(
            text_of(&dashboard, |p| matches!(p, Pulse::DoneCaption(17))),
            "4 分前に完了 · 所要 16 分"
        );
        let tree = &dashboard.tree;
        let tags = tags_under(tree, tree.root().unwrap());
        assert_eq!(tags.iter().filter(|t| *t == "結果あり").count(), 2);
    }

    #[test]
    fn a_long_lane_label_is_cut_inside_its_column() {
        let mut fleet = fleet::canned();
        fleet.workers[0].label = "noa-gpui ウィジェットによるUI拡張案".into();
        let mut dashboard = Dashboard::of_snapshot(Grouping::Flat, live(fleet, 0));
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(window(), &mut text);
        let tree = &dashboard.tree;
        let lanes = dashboard.timeline.as_ref().unwrap().lanes;
        let column = tree.children(tree.children(lanes)[0])[0];
        let label = tree.children(column)[0];
        let shown = tree.get::<Label>(label).unwrap();
        assert_ne!(shown.shown_text(), shown.text());
        assert!(shown.shown_text().ends_with('…'), "{}", shown.shown_text());
        let (column, label) = (rect(tree, column), rect(tree, label));
        assert!(
            label[0] >= column[0] - 0.5 && label[0] + label[2] <= column[0] + column[2] + 0.5,
            "label {label:?} is not inside its column {column:?}"
        );
    }

    const REPORTED_ORCHESTRATOR: &str = "noa-gpui ウィジェットによるUI拡張案";

    fn graph_labels(dashboard: &Dashboard) -> Vec<String> {
        let graph = dashboard.panels[Panel::Graph as usize].unwrap();
        labels_under(&dashboard.tree, graph)
    }

    #[test]
    fn the_orchestrator_is_the_graph_root_not_its_own_child() {
        let fleet = crate::live::tests::reported_fleet();
        let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(window(), &mut text);
        let labels = graph_labels(&dashboard);
        let at = |name: &str| labels.iter().position(|l| l == name).unwrap();
        let named = labels
            .iter()
            .filter(|l| *l == REPORTED_ORCHESTRATOR)
            .count();
        assert_eq!(named, 1, "{labels:?}");
        // The root carries its own pane's state: running.
        assert_eq!(labels[at(REPORTED_ORCHESTRATOR) - 1], "●", "{labels:?}");
        assert!(at("demo-sleep") > at(REPORTED_ORCHESTRATOR), "{labels:?}");
        assert!(at("demo-agent") > at(REPORTED_ORCHESTRATOR), "{labels:?}");
        assert_eq!(labels[at("単独") - 1], "◆", "{labels:?}");
        assert!(at("Claude Code") > at("単独"), "{labels:?}");
        assert_eq!(labels.iter().filter(|l| *l == "◆").count(), 1);
    }

    fn tags_of(dashboard: &Dashboard) -> Vec<String> {
        tags_under(&dashboard.tree, dashboard.tree.root().unwrap())
    }

    #[test]
    fn the_longest_wait_tag_needs_a_wait_text() {
        let fleet = crate::live::tests::reported_fleet();
        let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(window(), &mut text);
        let tags = tags_of(&dashboard);
        assert!(!tags.iter().any(|t| t.starts_with("最長")), "{tags:?}");

        let mut fleet = fleet::canned();
        let mut notice = fleet.workers[0].clone();
        notice.pane = 99;
        notice.state = State::Finished;
        notice.attention = true;
        notice.since = String::new();
        notice.wait_minutes = 600.0;
        fleet.workers.push(notice);
        let dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
        let longest: Vec<String> = tags_of(&dashboard)
            .into_iter()
            .filter(|t| t.starts_with("最長"))
            .collect();
        assert_eq!(longest, ["最長 spec-review 6 分"]);
    }

    #[test]
    fn a_plain_command_has_no_model_tag() {
        let reported = crate::live::tests::reported_fleet();
        let sleeper = reported.worker(33).unwrap().clone();
        let mut fleet = Fleet {
            orchestrators: reported.orchestrators.clone(),
            workers: Vec::new(),
        };
        for (pane, state) in [
            (33, State::Running),
            (35, State::Finished),
            (36, State::Error),
        ] {
            fleet.workers.push(Worker {
                pane,
                state,
                ..sleeper.clone()
            });
        }
        let mut dashboard = Dashboard::of_snapshot(Grouping::Flat, live(fleet, 0));
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(window(), &mut text);
        assert_eq!(
            dashboard.queue_buttons.open.len(),
            1,
            "the queue card is expanded"
        );
        let tags = tags_of(&dashboard);
        // Kind tags: the running, done and queue cards.
        assert_eq!(tags.iter().filter(|t| *t == "sleep").count(), 3, "{tags:?}");
        assert!(!tags.iter().any(|t| t.contains("モデル")), "{tags:?}");
    }

    fn inside(inner: [f32; 4], outer: [f32; 4]) -> bool {
        inner[0] >= outer[0] - 0.5
            && inner[1] >= outer[1] - 0.5
            && inner[0] + inner[2] <= outer[0] + outer[2] + 0.5
            && inner[1] + inner[3] <= outer[1] + outer[3] + 0.5
    }

    /// Two lanes whose phases are given in seconds.
    fn fleet_in_seconds(lanes: [&[(Phase, f64)]; 2]) -> Fleet {
        let canned = fleet::canned();
        let workers = lanes
            .iter()
            .zip(&canned.workers)
            .map(|(phases, worker)| Worker {
                phases: phases.iter().map(|(p, secs)| (*p, secs / 60.0)).collect(),
                ..worker.clone()
            })
            .collect();
        Fleet {
            workers,
            orchestrators: canned.orchestrators,
        }
    }

    #[test]
    fn lanes_reach_now_whatever_the_span() {
        let cases: [[&[(Phase, f64)]; 2]; 2] = [
            [
                &[
                    (Phase::Running, 20.0),
                    (Phase::Permission, 10.0),
                    (Phase::Running, 9.0),
                ],
                &[(Phase::Running, 3.0)],
            ],
            [
                &[(Phase::Running, 2.0), (Phase::Input, 1.5)],
                &[(Phase::Quiet, 0.5), (Phase::Ended, 1.0)],
            ],
        ];
        for lanes in cases {
            let fleet = fleet_in_seconds(lanes);
            let span: f64 = lanes[0].iter().map(|(_, secs)| secs).sum();
            let mut dashboard = Dashboard::of_snapshot(Grouping::Flat, live(fleet, 0));
            let mut text = TextStore::new(Default::default());
            dashboard.prepare(window(), &mut text);
            let tree = &dashboard.tree;
            let rows = tree.children(dashboard.timeline.as_ref().unwrap().lanes);
            assert_eq!(rows.len(), 2);
            for (row, phases) in rows.iter().zip(lanes) {
                let track = rect(tree, tree.children(*row)[1]);
                let segments: Vec<[f32; 4]> = tree
                    .children(tree.children(*row)[1])
                    .iter()
                    .map(|s| rect(tree, *s))
                    .collect();
                let last = segments.last().unwrap();
                assert!(
                    (last[0] + last[2] - (track[0] + track[2])).abs() <= 0.5,
                    "span {span} s: lane ends at {} of a track ending at {}",
                    last[0] + last[2],
                    track[0] + track[2]
                );
                let lane: f64 = phases.iter().map(|(_, secs)| secs).sum();
                let mut durations: Vec<f64> = phases.iter().map(|(_, secs)| *secs).collect();
                if lane < span {
                    durations.insert(0, span - lane);
                }
                assert_eq!(segments.len(), durations.len());
                for (segment, secs) in segments.iter().zip(&durations) {
                    let expected = track[2] * (*secs / span) as f32;
                    assert!(
                        (segment[2] - expected).abs() <= 0.5,
                        "span {span} s: a {secs} s segment is {} px, expected {expected}",
                        segment[2]
                    );
                }
            }
        }
    }

    #[test]
    fn the_queue_card_keeps_its_buttons_inside() {
        let long_line = "x".repeat(200);
        let mut fleet = fleet::canned();
        for worker in &mut fleet.workers {
            worker.orchestrator = REPORTED_ORCHESTRATOR.into();
            worker.branch = "factory/agent-dashboard".into();
        }
        let expanded = fleet.workers.iter_mut().find(|w| w.pane == 14).unwrap();
        expanded.screen_tail.push(long_line.clone());
        let collapsed = fleet.workers.iter_mut().find(|w| w.pane == 19).unwrap();
        collapsed.detail = "とても長い詳細 ".repeat(30);
        let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(window(), &mut text);
        let tree = &dashboard.tree;

        let (open, pane) = dashboard.queue_buttons.open[0];
        assert_eq!(pane, 14);
        let actions = tree.parent(open).unwrap();
        let card = tree.parent(actions).unwrap();
        assert!(
            inside(rect(tree, open), rect(tree, card)),
            "開く leaves its card"
        );
        let slot = tree.children(actions)[2];
        let name = tree.get::<Label>(tree.children(slot)[0]).unwrap();
        assert_eq!(name.text(), REPORTED_ORCHESTRATOR);
        assert_ne!(
            name.shown_text(),
            name.text(),
            "the orchestrator is not cut"
        );
        let at = tree
            .children(card)
            .iter()
            .position(|c| *c == actions)
            .unwrap();
        let screen = tree.children(card)[at - 1];
        let lines = tree.children(screen);
        let last = tree.get::<Label>(*lines.last().unwrap()).unwrap();
        assert_eq!(last.text(), long_line);
        assert_ne!(last.shown_text(), last.text(), "the long line is not cut");
        for line in lines {
            assert!(
                inside(rect(tree, *line), rect(tree, screen)),
                "a screen line leaves its box"
            );
        }
        assert!(inside(rect(tree, screen), rect(tree, card)));

        for (details, _) in &dashboard.queue_buttons.details {
            let card = tree.parent(tree.parent(*details).unwrap()).unwrap();
            assert!(
                inside(rect(tree, *details), rect(tree, card)),
                "詳細 leaves its card"
            );
        }
    }
}
