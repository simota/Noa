use std::time::{Duration, Instant};

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, length, percent,
};
use noa_gpui_core::widgets::{
    ButtonClicked, Chart, Container, LevelIndicator, SelectionChanged, TextChanged, TextField,
};
use noa_gpui_core::{
    Appearance, Dirty, Edge, Event, Message, OverlayOptions, Placement, Role, Style, Theme, UiTree,
    WidgetId,
};
use noa_gpui_platform::Screen;
use noa_gpui_scene::{Color, Size};
use noa_gpui_text::TextStore;

use crate::fleet::{self, Fleet, Grouping, Worker};
use crate::live::{Channel, Command, Link, Snapshot};

mod cards;
mod drawer;
mod filter;
mod graph;
mod header;
mod health;
mod help;
mod keymap;
mod pager;
mod panel;
mod queue;
mod select;
mod style;
mod timeline;
mod urgency;
use cards::{Cards, done, done_caption, quiet_text, running, running_caption};
use filter::Filter;
use graph::graph;
use header::{HeaderParts, longest, longest_text, match_text, waited_text};
use keymap::{Action, Mode, Route};
use pager::ListParts;
use panel::{Panel, empty, history_row, panel};
use queue::{QueueButtons, queue, queue_since};
use select::Step;
use style::{GUTTER, HEADER_HEIGHT, fill_remaining, gap, set_label, set_tag};
use timeline::{TimelineParts, fill_lanes, span_text, timeline, timeline_span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct View {
    grouping: Grouping,
    /// The queue card shown expanded.
    expanded: Option<u64>,
    shown: [bool; 5],
    /// The selected worker (AC1).
    selected: Option<u64>,
    /// The worker in the details drawer (AC8).
    detail: Option<u64>,
    /// Whether the keyboard help is open (AC5).
    help: bool,
    /// The first item each panel list shows (AC3, the pager).
    first: [usize; 5],
    /// The pane the pager must bring into view (AC3).
    reveal: Option<u64>,
}

impl View {
    fn of(grouping: Grouping) -> Self {
        View {
            grouping,
            expanded: None,
            shown: [true; 5],
            selected: None,
            detail: None,
            help: false,
            first: [0; 5],
            reveal: None,
        }
    }
}

/// Where the workers come from: the canned fleet (no link) or the live feed.
#[derive(Default)]
struct Feed {
    fleet: Fleet,
    link: Option<Link>,
    synced_secs: u64,
    /// When the server last answered, as of the last live snapshot.
    synced_at: Option<Instant>,
    /// The live feed's `Snapshot::can_focus`.
    can_focus: bool,
    /// What the last action said (a pane opened, or why not), shown in the
    /// header instead of the link state for `NOTICE_FOR`.
    notice: Option<(String, Instant)>,
    channel: Option<Channel>,
}

const NOTICE_FOR: Duration = Duration::from_secs(8);
const NOTHING_SELECTED: &str = "ワーカーが選ばれていません（J / K で選ぶ）";
const DRAWER_WIDTH: f32 = 360.0;
const HELP_WIDTH: f32 = 460.0;

impl Feed {
    fn demo() -> Self {
        Feed {
            fleet: fleet::canned(),
            ..Feed::default()
        }
    }

    /// Takes a snapshot's link state and `fleet` (the snapshot's, or what
    /// `health::keep_on_down` kept).
    fn accept(&mut self, fleet: Fleet, snapshot: Snapshot) {
        self.fleet = fleet;
        self.synced_at = (snapshot.link == Link::Live)
            .then(|| Instant::now().checked_sub(Duration::from_secs(snapshot.synced_secs)))
            .flatten()
            .or(self.synced_at);
        self.link = Some(snapshot.link);
        self.synced_secs = snapshot.synced_secs;
        self.can_focus = snapshot.can_focus;
    }

    fn structural(&self, snapshot: &Snapshot) -> bool {
        self.link.as_ref() != Some(&snapshot.link) || !self.fleet.same_structure(&snapshot.fleet)
    }
}

/// A widget whose content changes as time passes, updated in place by
/// `Dashboard::refresh`; the pane names the worker it shows.
#[derive(Debug, Clone, Copy)]
enum Pulse {
    Waited,
    Longest,
    QueueSince(u64),
    /// A queue card, whose look follows its urgency step (`queue::refresh_card`).
    QueueCard(u64),
    QuietTag(u64),
    Output(u64),
    Freshness(u64),
    RunningCaption(u64),
    DoneCaption(u64),
    /// A time label in the details drawer (`drawer::refresh`).
    Drawer(u64),
}

impl Pulse {
    /// Lives outside the body, so a body refill keeps it.
    fn outside_body(self) -> bool {
        matches!(self, Pulse::Longest | Pulse::Drawer(_))
    }
}

/// How a widget shows that its worker is selected (`Dashboard::paint_selection`).
#[derive(Debug, Clone, Copy)]
enum Mark {
    /// A card's accent bar: shown when selected.
    Bar,
    /// A timeline lane row: filled when selected.
    Lane,
    /// A graph node pill: a 2 px accent border when selected, else its own.
    Node { normal: Color },
}

/// What the builders register for later in-place updates and input.
#[derive(Default)]
pub struct Registry {
    pulses: Vec<(WidgetId, Pulse)>,
    /// The drawer's 開く (the queue's are in `Dashboard::queue_buttons`).
    open_buttons: Vec<(WidgetId, u64)>,
    /// Running and done cards' 詳細: they open the drawer.
    detail_buttons: Vec<(WidgetId, u64)>,
    marks: Vec<(WidgetId, u64, Mark)>,
    /// Each panel's list, by `Panel` index.
    lists: [Option<ListParts>; 5],
    timeline: Option<TimelineParts>,
    /// The drawer overlay and its 閉じる.
    drawer: Option<(WidgetId, WidgetId)>,
    /// The help overlay and its 閉じる.
    help: Option<(WidgetId, WidgetId)>,
}

impl Registry {
    /// Forgets what the body registered; the header and overlays stay.
    fn clear_body(&mut self) {
        self.pulses.retain(|(_, pulse)| pulse.outside_body());
        self.detail_buttons.clear();
        self.marks.clear();
        self.lists = Default::default();
        self.timeline = None;
    }
}

fn button_pane(buttons: &[(WidgetId, u64)], source: WidgetId) -> Option<u64> {
    QueueButtons::worker(buttons, source)
}

pub struct Dashboard {
    tree: UiTree,
    view: View,
    feed: Feed,
    filter: Filter,
    header: HeaderParts,
    /// The header's status label (`header.health.status`), as the tests
    /// name it.
    #[cfg(test)]
    status: WidgetId,
    grouping_control: WidgetId,
    /// The container the panels are built in; a filter change refills it.
    body: WidgetId,
    queue_buttons: QueueButtons,
    registry: Registry,
    /// The size the last `prepare` laid the screen out at. A rebuilt screen is
    /// laid out at it at once (see `rebuilt`).
    viewport: Option<Size>,
    /// The panes whose screen tails the feed was last asked for.
    watched: Option<Vec<u64>>,
    /// Wheel travel not yet turned into pages, per panel (the pager).
    wheel_accum: [f32; 5],
    /// A structural snapshot held back while an IME composes in the filter
    /// field: a rebuild would end the composition.
    pending: Option<Snapshot>,
    /// The filter field had the focus when the input being handled began:
    /// a rebuild that input causes focuses the new field. For a release,
    /// what counts is the press: a press moves the focus to what it hits
    /// before the release activates it (a grouping segment, a button).
    filter_had_focus: bool,
    pressed_in_filter: bool,
    #[cfg(test)]
    panels: [Option<WidgetId>; 5],
}

/// The tests read the registry's parts as the dashboard's own, as they read
/// `pulses` and `timeline` before the registry gathered them.
#[cfg(test)]
impl std::ops::Deref for Dashboard {
    type Target = Registry;

    fn deref(&self) -> &Registry {
        &self.registry
    }
}

impl Dashboard {
    /// The demo: the canned fleet.
    pub fn new(grouping: Grouping) -> Self {
        Self::showing(View::of(grouping))
    }

    fn showing(view: View) -> Self {
        Self::start(view, Feed::demo())
    }

    fn start(view: View, feed: Feed) -> Self {
        let theme = Theme::for_appearance(Appearance::Dark);
        let mut dashboard = Self::build(view, theme, feed, Filter::default(), false);
        dashboard.watch();
        dashboard
    }

    /// Fed by the live feed, connecting until its first snapshot arrives.
    pub fn live(grouping: Grouping, channel: Channel) -> Self {
        let feed = Feed {
            link: Some(Link::Connecting),
            channel: Some(channel),
            ..Feed::default()
        };
        Self::start(View::of(grouping), feed)
    }

    /// One live snapshot, with no feed behind it.
    pub fn of_snapshot(grouping: Grouping, snapshot: Snapshot) -> Self {
        Self::from_snapshot(View::of(grouping), snapshot)
    }

    fn from_snapshot(view: View, snapshot: Snapshot) -> Self {
        let mut feed = Feed::default();
        let fleet = snapshot.fleet.clone();
        feed.accept(fleet, snapshot);
        Self::start(view, feed)
    }

    /// Takes a snapshot in. One that changes only what time changes (texts,
    /// meters, lanes) updates those widgets in place, so the scroll positions
    /// and the hover survive; any other change rebuilds the screen.
    fn apply(&mut self, snapshot: Snapshot, text: &mut TextStore) {
        let old = self.order();
        let fleet = health::keep_on_down(&self.feed.fleet, snapshot.fleet.clone(), &snapshot.link);
        let structural = self.feed.link.as_ref() != Some(&snapshot.link)
            || !self.feed.fleet.same_structure(&fleet);
        self.feed.accept(fleet, snapshot);
        self.reselect(&old);
        if structural {
            self.rebuilt(self.view, text);
        } else {
            self.refresh(text);
        }
    }

    /// The order J / K walk now.
    fn order(&self) -> Vec<u64> {
        select::order(&self.feed.fleet, &self.view, &self.filter)
    }

    /// After the order changed from `old`: a selected pane that left it is
    /// replaced by its neighbour (expanding its queue card), and a drawer
    /// whose worker left the fleet or the filter closes (by the caller's
    /// rebuild or refill).
    fn reselect(&mut self, old: &[u64]) {
        let selected = select::reselect(old, &self.order(), self.view.selected);
        if selected != self.view.selected {
            self.view.selected = selected;
            self.view.reveal = selected;
            if let Some(pane) = selected.filter(|pane| self.waits(*pane)) {
                self.view.expanded = Some(pane);
            }
        }
        let gone = |pane: u64| {
            self.feed
                .fleet
                .worker(pane)
                .is_none_or(|w| !self.filter.matches(w))
        };
        if self.view.detail.is_some_and(gone) {
            self.view.detail = None;
        }
    }

    /// Whether the worker has a queue card (it waits on a person).
    fn waits(&self, pane: u64) -> bool {
        self.feed
            .fleet
            .worker(pane)
            .is_some_and(|w| w.need().is_some())
    }

    fn show_notice(&mut self, notice: String) {
        self.feed.notice = Some((notice, Instant::now()));
        self.tick(Instant::now());
    }

    /// Brings the link health in the header up to `now`; `before_frame`
    /// calls it on every frame (the live feed wakes the window every second).
    fn tick(&mut self, now: Instant) {
        health::tick(&mut self.tree, &self.header, &self.feed, now);
    }

    fn refresh(&mut self, text: &mut TextStore) {
        let (tree, fleet) = (&mut self.tree, &self.feed.fleet);
        for &(id, pulse) in &self.registry.pulses {
            let worker = |pane: u64| fleet.worker(pane).expect("the same structure");
            match pulse {
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
                Pulse::QueueCard(pane) => queue::refresh_card(tree, id, worker(pane)),
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
                Pulse::Drawer(pane) => drawer::refresh(tree, id, worker(pane)),
            }
        }
        if let Some(parts) = self.registry.timeline.take() {
            let matching: Vec<&Worker> = fleet
                .workers
                .iter()
                .filter(|w| self.filter.matches(w))
                .collect();
            let span = timeline_span(&matching);
            set_label(tree, parts.span, span_text(span));
            for lane in tree.children(parts.lanes).to_vec() {
                tree.remove(lane, text);
            }
            let registry = &mut self.registry;
            registry
                .marks
                .retain(|(_, _, mark)| !matches!(mark, Mark::Lane));
            let filtering = self.filter.active();
            fill_lanes(
                tree,
                &parts,
                &matching,
                self.view.grouping,
                filtering,
                span,
                registry,
            );
            registry.timeline = Some(parts);
        }
        self.paint_selection();
        self.tick(Instant::now());
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
    /// painted. The filter's text, caret and focus carry over to the new field.
    fn rebuilt(&mut self, view: View, text: &mut TextStore) {
        let field = self.header.filter_field;
        let refocus = self.filter_had_focus || self.tree.focused() == Some(field);
        if let Some(caret) = self.tree.get::<TextField>(field).map(TextField::caret) {
            self.filter.caret = caret;
        }
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
        let feed = std::mem::take(&mut self.feed);
        let filter = std::mem::take(&mut self.filter);
        let old = std::mem::replace(self, Self::build(view, theme, feed, filter, refocus));
        self.viewport = old.viewport;
        self.watched = old.watched;
        self.wheel_accum = old.wheel_accum;
        self.pending = old.pending;
        self.filter_had_focus = old.filter_had_focus;
        self.pressed_in_filter = old.pressed_in_filter;
        self.watch();
        self.lay_out_again(text);
    }

    /// Lays a changed tree out at the last viewport at once and marks it for
    /// compositing (see `rebuilt`).
    fn lay_out_again(&mut self, text: &mut TextStore) {
        if let Some(viewport) = self.viewport {
            self.prepare(viewport, text);
            if let Some(root) = self.tree.root() {
                self.tree.invalidate(root, Dirty::COMPOSITE);
            }
        }
    }

    fn build(view: View, theme: Theme, feed: Feed, filter: Filter, refocus: bool) -> Self {
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
        let mut registry = Registry::default();
        let header = header::header(&mut tree, root, &feed, &view, &filter, &mut registry);
        let body = tree.insert(
            Some(root),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(GUTTER),
                padding: LayoutRect::length(GUTTER),
                ..fill_remaining()
            }),
        );
        if refocus {
            tree.set_focus(Some(header.filter_field));
        }
        let mut dashboard = Self {
            tree,
            view,
            feed,
            filter,
            #[cfg(test)]
            status: header.health.status,
            grouping_control: header.grouping_control,
            header,
            body,
            queue_buttons: QueueButtons::default(),
            registry,
            viewport: None,
            watched: None,
            wheel_accum: [0.0; 5],
            pending: None,
            filter_had_focus: false,
            pressed_in_filter: false,
            #[cfg(test)]
            panels: [None; 5],
        };
        dashboard.fill_body();
        dashboard.open_drawer();
        if dashboard.view.help {
            dashboard.open_help();
        }
        dashboard
    }

    /// Builds the panels into the body for the view and the filter.
    fn fill_body(&mut self) {
        let Self {
            tree,
            view,
            feed,
            filter,
            registry,
            body,
            ..
        } = self;
        let (body, view) = (*body, *view);
        let workers = &feed.fleet.workers;
        let matching: Vec<&Worker> = workers.iter().filter(|w| filter.matches(w)).collect();
        let filtering = filter.active();
        let mut queue_buttons = QueueButtons::default();
        #[cfg(test)]
        let mut panels = [None; 5];
        let now_shown = Panel::NOW.iter().any(|p| view.shown[*p as usize]);
        let rows = [
            (Panel::NOW.as_slice(), fill_remaining()),
            (Panel::HISTORY.as_slice(), history_row(now_shown)),
        ];
        let nobody = feed.link.as_ref().filter(|_| workers.is_empty());
        if let Some(link) = nobody {
            empty(tree, body, link);
        }
        // The demo's numbers stand for lines; the live ones count preview
        // updates.
        let output_caption = if feed.link.is_some() {
            "出力の更新（回 / 5 秒）"
        } else {
            "出力（行 / 間隔）"
        };
        let cards = Cards {
            grouping: view.grouping,
            output_caption,
            filtering,
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
                let id = panel(tree, row, member.style(&view.shown));
                #[cfg(test)]
                {
                    panels[member as usize] = Some(id);
                }
                match member {
                    Panel::Queue => {
                        queue_buttons = queue(
                            tree,
                            id,
                            &matching,
                            workers,
                            view.expanded,
                            filtering,
                            registry,
                        );
                    }
                    Panel::Running => running(tree, id, &matching, cards, registry),
                    Panel::Done => done(tree, id, &matching, cards, registry),
                    Panel::Timeline => {
                        timeline(tree, id, &matching, view.grouping, filtering, registry);
                    }
                    Panel::Graph => graph(tree, id, &feed.fleet, filter, registry),
                }
            }
        }
        self.queue_buttons = queue_buttons;
        #[cfg(test)]
        {
            self.panels = panels;
        }
        self.paint_selection();
    }

    /// Builds the body again (a filter change): the header, and with it the
    /// filter field's focus, caret and IME state, stays.
    fn refill_body(&mut self, text: &mut TextStore) {
        for child in self.tree.children(self.body).to_vec() {
            self.tree.remove(child, text);
        }
        self.registry.clear_body();
        self.fill_body();
        self.lay_out_again(text);
    }

    /// Shows the selection on every widget registered for it, in place.
    fn paint_selection(&mut self) {
        let selected = self.view.selected;
        for &(id, pane, mark) in &self.registry.marks {
            let on = selected == Some(pane);
            match mark {
                Mark::Bar => self.tree.set_hidden(id, !on),
                Mark::Lane => {
                    let fill = on.then_some(Role::Selection.into());
                    self.tree
                        .update::<Container, _>(id, |c, cx| c.set_background(cx, fill));
                }
                Mark::Node { normal } => {
                    let border = if on {
                        (2.0, Role::Accent.into())
                    } else {
                        (1.0, normal.into())
                    };
                    self.tree
                        .update::<Container, _>(id, |c, cx| c.set_border(cx, Some(border)));
                }
            }
        }
    }

    /// Selects `pane` (or nothing). A worker with a queue card gets it
    /// expanded (a rebuild when another card was); an open drawer moves to
    /// the new worker.
    fn select(&mut self, pane: Option<u64>, text: &mut TextStore) {
        self.view.selected = pane;
        self.view.reveal = pane;
        let mut rebuild = false;
        if let Some(pane) = pane {
            if self.waits(pane) && self.view.shown[Panel::Queue as usize] {
                rebuild = self.queue_buttons.expanded != Some(pane);
                self.view.expanded = Some(pane);
            }
            if self.view.detail.is_some_and(|detail| detail != pane) {
                self.view.detail = Some(pane);
                if !rebuild {
                    self.reopen_drawer(text);
                }
            }
        }
        if rebuild {
            self.rebuilt(self.view, text);
        } else {
            self.paint_selection();
            self.watch();
        }
    }

    /// Moves the selection one step through the order.
    fn step(&mut self, step: Step, text: &mut TextStore) {
        let next = select::step(&self.order(), self.view.selected, step);
        if next.is_some() && next != self.view.selected {
            self.select(next, text);
        }
    }

    /// The existing 開く behaviour, for the buttons and Enter.
    fn open(&mut self, pane: u64) {
        let Some(worker) = self.feed.fleet.worker(pane) else {
            return;
        };
        let status = match &self.feed.channel {
            Some(_) if self.feed.link != Some(Link::Live) => {
                "ペインへ移動できません: Noa に接続していません".to_owned()
            }
            Some(_) if !self.feed.can_focus => {
                "ペインへ移動できません: Noa の server-scopes に control がありません".to_owned()
            }
            Some(channel) => {
                channel.send(Command::Focus(pane));
                format!("{} のペインへ移動します", worker.label)
            }
            None => format!("モック: {} のペインへ移動します", worker.label),
        };
        self.show_notice(status);
    }

    /// Opens the drawer on `view.detail`, if its worker is shown.
    fn open_drawer(&mut self) {
        let Some(pane) = self.view.detail else {
            return;
        };
        let Some(worker) = self
            .feed
            .fleet
            .worker(pane)
            .filter(|w| self.filter.matches(w))
        else {
            self.view.detail = None;
            return;
        };
        let root = self.tree.open_overlay(
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(10.0),
                padding: LayoutRect::length(14.0),
                size: LayoutSize {
                    width: length(DRAWER_WIDTH),
                    height: percent(1.0),
                },
                ..Default::default()
            })
            .background(Role::Surface)
            .border(1.0, Role::Separator),
            Placement::Drawer {
                edge: Edge::Right,
                top: HEADER_HEIGHT,
            },
            OverlayOptions::PASSIVE,
        );
        let (open, close) =
            drawer::build(&mut self.tree, root, worker, &self.feed, &mut self.registry);
        self.registry.open_buttons.push((open, pane));
        self.registry.drawer = Some((root, close));
    }

    fn close_drawer(&mut self, text: &mut TextStore) {
        if let Some((root, _)) = self.registry.drawer.take() {
            self.tree.close_overlay(root, text);
        }
        self.registry.open_buttons.clear();
        self.registry
            .pulses
            .retain(|(_, pulse)| !matches!(pulse, Pulse::Drawer(_)));
    }

    fn reopen_drawer(&mut self, text: &mut TextStore) {
        self.close_drawer(text);
        self.open_drawer();
        self.lay_out_again(text);
    }

    /// Shows the drawer on `pane` (selecting it), or closes it.
    fn set_detail(&mut self, pane: Option<u64>, text: &mut TextStore) {
        if pane.is_some() && pane != self.view.selected {
            self.select(pane, text);
        }
        self.view.detail = pane;
        self.reopen_drawer(text);
        self.watch();
    }

    fn open_help(&mut self) {
        let root = self.tree.open_overlay(
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(6.0),
                padding: LayoutRect::length(16.0),
                size: LayoutSize {
                    width: length(HELP_WIDTH),
                    height: noa_gpui_core::style::auto(),
                },
                ..Default::default()
            })
            .background(Role::Surface)
            .border(1.0, Role::Separator)
            .radius(10.0),
            Placement::Center,
            OverlayOptions::MODAL,
        );
        let close = help::build(&mut self.tree, root);
        self.registry.help = Some((root, close));
    }

    fn close_help(&mut self, text: &mut TextStore) {
        self.view.help = false;
        if let Some((root, _)) = self.registry.help.take() {
            self.tree.close_overlay(root, text);
        }
    }

    /// Asks the feed for the screen tails of the expanded queue card and the
    /// drawer's worker, when they changed.
    fn watch(&mut self) {
        let Some(channel) = &self.feed.channel else {
            return;
        };
        let mut panes = Vec::new();
        for pane in [self.queue_buttons.expanded, self.view.detail]
            .into_iter()
            .flatten()
        {
            if !panes.contains(&pane) {
                panes.push(pane);
            }
        }
        if self.watched.as_ref() != Some(&panes) {
            channel.send(Command::Watch(panes.clone()));
            self.watched = Some(panes);
        }
    }

    /// A changed query (and caret): the body is built again, the header and
    /// its field stay.
    fn set_filter(&mut self, query: String, caret: usize, text: &mut TextStore) {
        let old = self.order();
        let detail = self.view.detail;
        self.filter = Filter { query, caret };
        self.reselect(&old);
        if self.view.detail != detail {
            self.close_drawer(text);
        }
        self.refill_body(text);
        let count = self.header.match_count;
        set_label(
            &mut self.tree,
            count,
            match_text(&self.feed.fleet.workers, &self.filter),
        );
        self.tree.set_hidden(count, !self.filter.active());
        self.watch();
    }

    /// Clears the query, in the field too.
    fn clear_filter(&mut self, text: &mut TextStore) {
        let field = self.header.filter_field;
        self.tree
            .update::<TextField, _>(field, |f, cx| f.set_text(cx, ""));
        self.set_filter(String::new(), 0, text);
    }

    /// A view change that rebuilds the screen (panels, grouping).
    fn change_view(&mut self, view: View, text: &mut TextStore) {
        let old = self.order();
        self.view = view;
        self.reselect(&old);
        self.rebuilt(self.view, text);
    }

    fn mode(&self) -> Mode {
        if self.view.help {
            Mode::Help
        } else if self.tree.focused() == Some(self.header.filter_field) {
            Mode::Filter
        } else {
            Mode::Browse
        }
    }

    /// Whether an IME composition is in progress in the filter field.
    fn composing(&self) -> bool {
        self.tree
            .get::<TextField>(self.header.filter_field)
            .is_some_and(|f| !f.composition().is_empty())
    }

    fn act(&mut self, action: Action, text: &mut TextStore) {
        match action {
            Action::Next => self.step(Step::Next, text),
            Action::Prev => self.step(Step::Prev, text),
            Action::Open => match self.view.selected {
                Some(pane) => self.open(pane),
                None => self.show_notice(NOTHING_SELECTED.to_owned()),
            },
            Action::ToggleDetail => match self.view.selected {
                Some(pane) => {
                    let detail = (self.view.detail != Some(pane)).then_some(pane);
                    self.set_detail(detail, text);
                }
                None => self.show_notice(NOTHING_SELECTED.to_owned()),
            },
            Action::StartFilter => {
                let field = self.header.filter_field;
                self.tree.set_focus(Some(field));
                self.tree.update::<TextField, _>(field, |f, cx| {
                    let end = f.text().len();
                    f.select(cx, end..end);
                });
                self.filter.caret = self.filter.query.len();
            }
            Action::Back => {
                if self.view.detail.is_some() {
                    self.set_detail(None, text);
                } else if !self.filter.query.is_empty() {
                    self.clear_filter(text);
                } else if self.view.selected.is_some() {
                    self.select(None, text);
                }
            }
            Action::TogglePanel(panel) => {
                let mut view = self.view;
                view.shown[panel as usize] = !view.shown[panel as usize];
                self.change_view(view, text);
            }
            Action::OpenHelp => {
                self.view.help = true;
                self.open_help();
                self.lay_out_again(text);
            }
            Action::ClearFilter => self.clear_filter(text),
            Action::CommitFilter => {}
            Action::CloseHelp => self.close_help(text),
        }
    }

    /// Takes a wheel event over a panel list to page it (the pager).
    fn wheel(&mut self, event: &Event, text: &mut TextStore) -> bool {
        let taken = pager::wheel(
            &self.tree,
            &self.registry.lists,
            &mut self.view,
            &mut self.wheel_accum,
            event,
        );
        if taken && let Some(viewport) = self.viewport {
            let pages = pager::fit(
                &mut self.tree,
                text,
                viewport,
                &self.registry.lists,
                &mut self.view,
            );
            if pages {
                self.tree.prepare(viewport, text);
            }
        }
        taken
    }
}

impl Screen for Dashboard {
    fn tree(&self) -> &UiTree {
        &self.tree
    }

    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }

    /// Every key goes through the key map first (`keymap::route`): the tree
    /// has no shortcuts, so a digit typed into the filter field never
    /// toggles a panel.
    fn dispatch(&mut self, event: Event, text: &mut TextStore) {
        let mode = self.mode();
        let in_filter = mode == Mode::Filter;
        let (press, release) = match event {
            Event::PointerDown { .. } => (true, false),
            Event::PointerUp { .. } => (false, true),
            _ => (false, false),
        };
        if press {
            self.pressed_in_filter = in_filter;
        }
        self.filter_had_focus = in_filter || (release && self.pressed_in_filter);
        match keymap::route(mode, self.composing(), &event) {
            Route::Act(action) => {
                self.act(action, text);
                let typing_on = action == Action::StartFilter
                    || (mode == Mode::Filter && matches!(action, Action::Next | Action::Prev));
                if !typing_on {
                    self.tree.set_focus(None);
                }
            }
            Route::Swallow => {}
            Route::Tree => {
                if !self.wheel(&event, text) {
                    self.tree.dispatch(event, text);
                    for message in self.tree.take_messages() {
                        self.handle(message, text);
                    }
                }
            }
        }
        self.filter_had_focus = false;
        if release {
            self.pressed_in_filter = false;
        }
        self.after_input();
    }

    fn prepare(&mut self, viewport: Size, text: &mut TextStore) -> bool {
        self.viewport = Some(viewport);
        let mut changed = self.tree.prepare(viewport, text).is_some();
        let header = header::fit(&mut self.tree, text, viewport, &self.header);
        let pages = pager::fit(
            &mut self.tree,
            text,
            viewport,
            &self.registry.lists,
            &mut self.view,
        );
        if header || pages {
            changed |= self.tree.prepare(viewport, text).is_some();
        }
        changed
    }

    fn before_frame(&mut self, text: &mut TextStore) {
        let latest = self
            .feed
            .channel
            .as_ref()
            .and_then(|channel| channel.snapshots.try_iter().last());
        if latest.is_some() {
            self.pending = latest;
        }
        if let Some(snapshot) = self.pending.take() {
            if self.composing() && self.feed.structural(&snapshot) {
                self.pending = Some(snapshot);
            } else {
                self.apply(snapshot, text);
            }
        }
        let notice = self
            .feed
            .channel
            .as_ref()
            .and_then(|channel| channel.notices.try_iter().last());
        if let Some(notice) = notice {
            self.show_notice(notice);
        }
        self.tick(Instant::now());
    }

    fn handle(&mut self, message: Message, text: &mut TextStore) {
        if let Some(TextChanged(query)) = message.downcast_ref::<TextChanged>()
            && message.source == self.header.filter_field
        {
            let caret = self
                .tree
                .get::<TextField>(message.source)
                .map_or(query.len(), TextField::caret);
            self.set_filter(query.clone(), caret, text);
            return;
        }
        if let Some(SelectionChanged(index)) = message.downcast_ref::<SelectionChanged>()
            && message.source == self.grouping_control
        {
            let view = View {
                grouping: Grouping::ALL[*index],
                ..self.view
            };
            self.change_view(view, text);
            return;
        }
        if message.downcast_ref::<ButtonClicked>().is_none() {
            return;
        }
        let source = message.source;
        let queue = &self.queue_buttons;
        if let Some(pane) = button_pane(&queue.details, source) {
            self.select(Some(pane), text);
        } else if let Some(pane) = button_pane(&queue.open, source)
            .or_else(|| button_pane(&self.registry.open_buttons, source))
        {
            self.open(pane);
            if self.view.selected != Some(pane) {
                self.select(Some(pane), text);
            }
        } else if let Some(pane) = button_pane(&self.registry.detail_buttons, source) {
            let detail = (self.view.detail != Some(pane)).then_some(pane);
            self.set_detail(detail, text);
        } else if self
            .registry
            .drawer
            .is_some_and(|(_, close)| close == source)
        {
            self.set_detail(None, text);
        } else if self.registry.help.is_some_and(|(_, close)| close == source) {
            self.close_help(text);
        }
    }
}

#[cfg(test)]
mod tests;
