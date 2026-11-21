use noa_gpui_core::style::{
    AlignItems, FlexDirection, JustifyContent, LengthPercentage, Rect as LayoutRect,
    Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Badge, Button, ButtonClicked, Chart, ChartSeries, Container, ControlSize, GroupBox, Kbd, Label,
    LevelIndicator, LevelStyle, ScrollView, SegmentMode, SegmentedControl, SelectionChanged, Tag,
    TagStyle, Tone, Truncation,
};
use noa_gpui_core::{
    Appearance, Key, Message, Role, Shortcut, ShortcutPressed, Style, Theme, UiTree, WidgetId,
};
use noa_gpui_platform::Screen;
use noa_gpui_scene::Color;
use noa_gpui_text::TextStore;

use crate::fleet::{self, FRESHNESS_MAX, Grouping, Need, Phase, State, Worker};

const HEADER_HEIGHT: f32 = 48.0;
const GUTTER: f32 = 12.0;
const QUEUE_WIDTH: f32 = 400.0;
const DONE_WIDTH: f32 = 270.0;
const RUNNING_CARD_WIDTH: f32 = 336.0;
const LANE_LABEL_WIDTH: f32 = 104.0;
const LANE_TOTAL_WIDTH: f32 = 44.0;
const LANE_HEIGHT: f32 = 12.0;
const GRAPH_WIDTH: f32 = 430.0;
const HISTORY_HEIGHT: f32 = 256.0;

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
    expanded: Option<&'static str>,
    shown: [bool; 5],
}

#[derive(Default)]
struct QueueButtons {
    open: Vec<(WidgetId, &'static str)>,
    details: Vec<(WidgetId, &'static str)>,
}

impl QueueButtons {
    fn worker(buttons: &[(WidgetId, &'static str)], source: WidgetId) -> Option<&'static str> {
        buttons
            .iter()
            .find(|(id, _)| *id == source)
            .map(|(_, label)| *label)
    }
}

pub struct Dashboard {
    tree: UiTree,
    view: View,
    status: WidgetId,
    grouping_control: WidgetId,
    queue_buttons: QueueButtons,
    #[cfg_attr(not(test), allow(dead_code))]
    panels: [Option<WidgetId>; 5],
}

impl Dashboard {
    pub fn new(grouping: Grouping) -> Self {
        Self::showing(View {
            grouping,
            expanded: None,
            shown: [true; 5],
        })
    }

    fn showing(view: View) -> Self {
        let grouping = view.grouping;
        let workers = fleet::canned();
        let mut tree = UiTree::new();
        tree.set_theme(Theme::for_appearance(Appearance::Dark));

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

        let header = header(&mut tree, root, &workers, grouping, &view.shown);
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
        let mut panels = [None; 5];
        let now_shown = Panel::NOW.iter().any(|p| view.shown[*p as usize]);
        let rows = [
            (Panel::NOW.as_slice(), fill_remaining()),
            (Panel::HISTORY.as_slice(), history_row(now_shown)),
        ];
        for (members, style) in rows {
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
                panels[member as usize] = Some(id);
                match member {
                    Panel::Queue => {
                        queue_buttons = queue(&mut tree, id, &workers, view.expanded);
                    }
                    Panel::Running => running(&mut tree, id, &workers, grouping),
                    Panel::Done => done(&mut tree, id, &workers, grouping),
                    Panel::Timeline => timeline(&mut tree, id, &workers, grouping),
                    Panel::Graph => graph(&mut tree, id, &workers),
                }
            }
        }

        Self {
            tree,
            view,
            status: header.status,
            grouping_control: header.grouping_control,
            queue_buttons,
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

    fn handle(&mut self, message: Message, _text: &mut TextStore) {
        if let Some(ShortcutPressed(pressed)) = message.downcast_ref::<ShortcutPressed>() {
            if let Some(panel) = Panel::ALL.into_iter().find(|p| p.shortcut() == *pressed) {
                let mut shown = self.view.shown;
                shown[panel as usize] = !shown[panel as usize];
                *self = Self::showing(View { shown, ..self.view });
            }
            return;
        }
        if let Some(SelectionChanged(index)) = message.downcast_ref::<SelectionChanged>()
            && message.source == self.grouping_control
        {
            *self = Self::showing(View {
                grouping: Grouping::ALL[*index],
                ..self.view
            });
            return;
        }
        if message.downcast_ref::<ButtonClicked>().is_none() {
            return;
        }
        if let Some(label) = QueueButtons::worker(&self.queue_buttons.details, message.source) {
            *self = Self::showing(View {
                expanded: Some(label),
                ..self.view
            });
        } else if let Some(label) = QueueButtons::worker(&self.queue_buttons.open, message.source) {
            let status = format!("モック: {label} のペインへ移動します");
            self.tree
                .update::<Label, _>(self.status, |l, cx| l.set_text(cx, status));
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
            flex_shrink: 0.0,
            ..Default::default()
        }),
    );
    Panel::ALL.map(|panel| {
        let item = tree.insert(
            Some(strip),
            Container::row().align_items(AlignItems::CENTER).gap(3.0),
        );
        tree.insert(Some(item), Kbd::shortcut(panel.shortcut()));
        let label = Label::new(panel.title()).font_size(11.0);
        let label = if shown[panel as usize] {
            label.weight(700)
        } else {
            label.weight(400).color(Role::Muted)
        };
        tree.insert(Some(item), label);
        item
    })
}

fn header(
    tree: &mut UiTree,
    root: WidgetId,
    workers: &[Worker],
    grouping: Grouping,
    shown: &[bool; 5],
) -> Header {
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

    let waited: f64 = workers
        .iter()
        .map(|w| w.minutes_where(Phase::waits_on_a_person))
        .sum();
    tree.insert(
        Some(bar),
        Tag::new(format!("人待ち合計 {waited:.0} 分")).style(TagStyle::Outlined),
    );
    let longest = workers
        .iter()
        .filter(|w| w.need().is_some_and(|need| need != Need::Quiet))
        .max_by(|a, b| a.wait_minutes.total_cmp(&b.wait_minutes));
    if let Some(worker) = longest {
        tree.insert(
            Some(bar),
            Tag::new(format!("最長 {} {}", worker.label, worker.since))
                .style(TagStyle::Outlined)
                .tone(Tone::Warning),
        );
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
        Label::new(format!("ワーカー {} · ダミーデータ", workers.len()))
            .font_size(12.0)
            .color(Role::Muted)
            .truncation(Truncation::Tail),
    );
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

fn task_line(tree: &mut UiTree, parent: WidgetId, worker: &Worker) {
    tree.insert(
        Some(parent),
        Label::new(format!("依頼: {}", worker.task))
            .font_size(12.0)
            .color(Role::Muted)
            .wrap(true)
            .max_lines(2),
    );
}

fn queue(
    tree: &mut UiTree,
    rail: WidgetId,
    workers: &[Worker],
    expanded: Option<&'static str>,
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
    let expanded = expanded.or(waiting.first().map(|(worker, _)| worker.label));

    panel_title(tree, rail, "要対応", waiting.len(), Tone::Warning);
    let list = card_list(tree, rail);
    let mut buttons = QueueButtons::default();
    for (worker, need) in waiting {
        let card = queue_card(tree, list, worker, need);
        if expanded == Some(worker.label) {
            buttons
                .open
                .push((queue_card_body(tree, card, worker), worker.label));
        } else {
            buttons
                .details
                .push((queue_card_summary(tree, card, worker), worker.label));
        }
    }
    buttons
}

fn queue_card(tree: &mut UiTree, list: WidgetId, worker: &Worker, need: Need) -> WidgetId {
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
    tree.insert(Some(head), Label::new(worker.label).weight(700));
    tree.insert(Some(head), Tag::new(worker.kind).tone(Tone::Accent));
    spacer(tree, head);
    tree.insert(
        Some(head),
        Label::new(format!("{} {}", need.title(), worker.since))
            .font_size(12.0)
            .weight(700)
            .color(color),
    );
    card
}

fn queue_card_summary(tree: &mut UiTree, card: WidgetId, worker: &Worker) -> WidgetId {
    let row = tree.insert(
        Some(card),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let detail = tree.insert(Some(row), Container::new(fill_remaining()));
    tree.insert(
        Some(detail),
        Label::new(worker.detail)
            .font_size(12.0)
            .color(Role::Muted)
            .truncation(Truncation::Tail),
    );
    tree.insert(
        Some(row),
        Button::new("詳細").control_size(ControlSize::Small),
    )
}

fn queue_card_body(tree: &mut UiTree, card: WidgetId, worker: &Worker) -> WidgetId {
    task_line(tree, card, worker);

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
    for line in worker.screen_tail {
        tree.insert(
            Some(screen),
            Label::new(*line).font_size(11.0).font_family("monospace"),
        );
    }

    let actions = tree.insert(
        Some(card),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    tree.insert(
        Some(actions),
        Tag::new(worker.branch).style(TagStyle::Outlined),
    );
    tree.insert(Some(actions), Tag::new(worker.model_line()));
    spacer(tree, actions);
    caption(tree, actions, worker.orchestrator);
    tree.insert(
        Some(actions),
        Button::new("開く").control_size(ControlSize::Small),
    )
}

fn running(tree: &mut UiTree, column: WidgetId, workers: &[Worker], grouping: Grouping) {
    let members: Vec<&Worker> = workers
        .iter()
        .filter(|w| w.state == State::Running)
        .collect();
    panel_title(tree, column, "作業中", members.len(), Tone::Success);
    let scroll = card_list(tree, column);
    for (name, group) in grouping.split(members) {
        if let Some(name) = name {
            group_header(tree, scroll, name, group.len());
        }
        let grid = tree.insert(Some(scroll), Container::wrapping_row(8.0, 8.0));
        for worker in group {
            running_card(tree, grid, worker);
        }
    }
}

fn running_card(tree: &mut UiTree, grid: WidgetId, worker: &Worker) {
    let card = tree.insert(
        Some(grid),
        GroupBox::card(worker.label).width(RUNNING_CARD_WIDTH),
    );
    let tags = tree.insert(
        Some(card),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    tree.insert(Some(tags), Tag::new(worker.kind).tone(Tone::Accent));
    tree.insert(Some(tags), Tag::new(worker.model_line()));
    tree.insert(
        Some(tags),
        Tag::new(worker.branch).style(TagStyle::Outlined),
    );
    if worker.quiet {
        tree.insert(
            Some(tags),
            Tag::new(format!("静止 {:.0} 分", worker.wait_minutes)).tone(Tone::Critical),
        );
    }
    task_line(tree, card, worker);
    tree.insert(
        Some(card),
        Label::new(worker.detail).font_size(12.0).wrap(true),
    );

    let meters = tree.insert(
        Some(card),
        Container::row()
            .align_items(AlignItems::END)
            .justify_content(JustifyContent::SPACE_BETWEEN)
            .gap(12.0),
    );
    let output = tree.insert(Some(meters), Container::column().gap(2.0));
    caption(tree, output, "出力（行 / 間隔）");
    tree.insert(
        Some(output),
        Chart::line(vec![String::new(); worker.lines_per_tick.len()])
            .series(ChartSeries::new("行", worker.lines_per_tick.to_vec()))
            .sparkline()
            .width(170.0)
            .height(26.0),
    );
    let fresh = tree.insert(Some(meters), Container::column().gap(2.0));
    caption(tree, fresh, "鮮度");
    tree.insert(
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

    caption(
        tree,
        card,
        format!(
            "{:.0} 分前に開始 · 報告 {} 回 · {}",
            worker.total_minutes(),
            worker.reports,
            worker.since
        ),
    );
}

fn done(tree: &mut UiTree, column: WidgetId, workers: &[Worker], grouping: Grouping) {
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
            done_card(tree, list, worker);
        }
    }
}

fn done_card(tree: &mut UiTree, list: WidgetId, worker: &Worker) {
    let card = tree.insert(Some(list), GroupBox::card(worker.label).fill_width());
    let tags = tree.insert(
        Some(card),
        Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
    );
    tree.insert(Some(tags), Tag::new(worker.kind).tone(Tone::Accent));
    tree.insert(Some(tags), Tag::new(worker.model_line()));
    tree.insert(
        Some(tags),
        Tag::new(worker.branch).style(TagStyle::Outlined),
    );
    tree.insert(Some(tags), Tag::new("結果あり").tone(Tone::Success));
    task_line(tree, card, worker);
    tree.insert(
        Some(card),
        Label::new(worker.detail).font_size(12.0).wrap(true),
    );
    caption(
        tree,
        card,
        format!(
            "{} · 所要 {:.0} 分",
            worker.since,
            worker.minutes_where(|phase| phase != Phase::Ended)
        ),
    );
}

fn timeline(tree: &mut UiTree, section: WidgetId, workers: &[Worker], grouping: Grouping) {
    let span = workers
        .iter()
        .map(Worker::total_minutes)
        .fold(0.0, f64::max);

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
    caption(tree, title, format!("← {span:.0} 分前　　いま →"));

    let lanes = tree.insert(
        Some(section),
        ScrollView::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(5.0),
            ..fill_remaining()
        }),
    );
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
    tree.insert(Some(name), Label::new(worker.label).font_size(11.0));

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
    let segment = |minutes: f64| Style {
        flex_grow: minutes as f32,
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
    for (phase, minutes) in worker.phases {
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
    caption(tree, total, format!("{:.0} 分", worker.total_minutes()));
}

fn mark(worker: &Worker) -> (&'static str, Color) {
    match (worker.need(), worker.state) {
        (Some(need), _) => (need.symbol(), need_color(need)),
        (None, State::Finished) => ("✓", GRAY),
        (None, _) => ("●", GREEN),
    }
}

fn graph(tree: &mut UiTree, section: WidgetId, workers: &[Worker]) {
    tree.insert(
        Some(section),
        Label::new("関連（親 → ワーカー → 孫）")
            .font_size(12.0)
            .weight(700)
            .color(Role::Muted),
    );
    let roots = tree.insert(Some(section), Container::column().gap(12.0));
    for name in fleet::ORCHESTRATORS {
        let row = tree.insert(
            Some(roots),
            Container::row().align_items(AlignItems::CENTER),
        );
        node(tree, row, "◆", name, BLUE);
        let children: Vec<&Worker> = workers
            .iter()
            .filter(|w| w.orchestrator == name && w.parent.is_none())
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
        node(tree, child_row, symbol, child.label, color);
        let grandchildren: Vec<&Worker> = workers
            .iter()
            .filter(|w| w.parent == Some(child.label))
            .collect();
        connect(tree, child_row, workers, &grandchildren);
    }
}

#[cfg(test)]
mod tests {
    use noa_gpui_core::{Event, Modifiers, PointerButton};
    use noa_gpui_scene::{Point, Size};

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
            .map(|(_, label)| *label)
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
        let open: Vec<&str> = dashboard
            .queue_buttons
            .open
            .iter()
            .map(|(_, label)| *label)
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

    fn press(dashboard: &mut Dashboard, text: &mut TextStore, panel: Panel, modifiers: Modifiers) {
        let key = panel.shortcut().key;
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
            for panel in Panel::ALL {
                let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
                press(&mut dashboard, &mut text, panel, modifiers);
                for other in Panel::ALL {
                    let present = dashboard.panels[other as usize].is_some();
                    assert_eq!(present, other != panel, "{other:?} after hiding {panel:?}");
                }
                press(&mut dashboard, &mut text, panel, modifiers);
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
            for (panel, item) in Panel::ALL.into_iter().zip(items) {
                let parts = tree.children(*item);
                let kbd = tree.get::<Kbd>(parts[0]).unwrap();
                let keys = panel.shortcut().keys(tree.shortcut_glyphs());
                assert_eq!(kbd.keys(), keys.as_slice());
                let label = tree.get::<Label>(parts[1]).unwrap();
                assert_eq!(label.text(), panel.title());
            }
        }
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
            let width = rect(tree, strip)[2];
            assert!((299.5..=312.5).contains(&width), "strip is {width} px");
        }
    }
}
