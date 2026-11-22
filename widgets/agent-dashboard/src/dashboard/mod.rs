use std::time::{Duration, Instant};

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, length, percent,
};
use noa_gpui_core::widgets::{
    ButtonClicked, Chart, Container, Label, LevelIndicator, SelectionChanged, TextChanged,
    TextField, Truncation,
};
use noa_gpui_core::{
    Appearance, Dirty, Edge, Event, Message, OverlayOptions, Placement, PointerButton, Role, Style,
    Theme, UiTree, WidgetId,
};
use noa_gpui_platform::Screen;
use noa_gpui_scene::{Color, Point, Size};
use noa_gpui_text::TextStore;

use crate::fleet::{self, Fleet, Grouping, Tier, Worker};
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
    /// Whether the queue shows its 参考情報 cards.
    info_open: bool,
    shown: [bool; 5],
    /// The selected worker (AC1).
    selected: Option<u64>,
    /// The worker in the details drawer (AC8).
    detail: Option<u64>,
    /// The worker whose result file the drawer shows, read when asked for.
    result: Option<u64>,
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
            info_open: false,
            shown: [true; 5],
            selected: None,
            detail: None,
            result: None,
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
    /// The server instance the shown pane ids belong to (the last
    /// `Snapshot::instance` that named one).
    instance: Option<String>,
    /// What the last action said (a pane opened, or why not), shown in the
    /// notice line at the window's foot for `NOTICE_FOR`.
    notice: Option<(String, Instant)>,
    channel: Option<Channel>,
    /// Workers were shown at some point (AC6: a lost link then says how
    /// old the data is, even when the fleet emptied before it was lost).
    had_data: bool,
}

const NOTICE_FOR: Duration = Duration::from_secs(8);
const NOTICE_HEIGHT: f32 = 24.0;
const NOTHING_SELECTED: &str = "ワーカーが選ばれていません（J / K で選ぶ）";
const DRAWER_WIDTH: f32 = 360.0;
const HELP_WIDTH: f32 = 460.0;
/// Where `cancel_press` lets go of a press: outside the window, over no
/// widget.
const OUTSIDE: Point = Point::new(-1.0, -1.0);

/// What becomes of a selected pane that leaves the order J / K walk
/// (`Dashboard::reselect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vanish {
    /// The user changed what is shown: the pane now at its place is selected.
    Neighbour,
    /// A live update took it: nothing is selected.
    Clear,
}

fn dropped_text(label: &str) -> String {
    format!(
        "{} が一覧から外れたため選択を解除しました",
        fleet::one_line(label)
    )
}

impl Feed {
    fn demo() -> Self {
        Feed {
            fleet: fleet::canned(),
            ..Feed::default()
        }
    }

    /// Takes a snapshot's link state and `fleet` (the snapshot's, or what
    /// `health::keep_on_down` kept). Its `synced_secs` count back from
    /// `received`, when it came in, not from now: one held back while an
    /// IME composed is as old as it was then plus the wait.
    fn accept(&mut self, fleet: Fleet, snapshot: Snapshot, received: Instant) {
        self.fleet = fleet;
        self.had_data |= !self.fleet.workers.is_empty();
        self.synced_at = self.synced_at_after(&snapshot, received);
        self.link = Some(snapshot.link);
        self.synced_secs = snapshot.synced_secs;
        self.can_focus = snapshot.can_focus;
        if snapshot.instance.is_some() {
            self.instance = snapshot.instance;
        }
    }

    /// When the server last answered once `snapshot`, which came at
    /// `received`, is taken in: a live one says; any other keeps what was
    /// known.
    fn synced_at_after(&self, snapshot: &Snapshot, received: Instant) -> Option<Instant> {
        (snapshot.link == Link::Live)
            .then(|| received.checked_sub(Duration::from_secs(snapshot.synced_secs)))
            .flatten()
            .or(self.synced_at)
    }

    fn structural(&self, snapshot: &Snapshot) -> bool {
        self.other_server(snapshot)
            || self.link.as_ref() != Some(&snapshot.link)
            || !self.fleet.same_structure(&snapshot.fleet)
    }

    /// Whether the snapshot comes from another server instance than the
    /// panes shown (Noa restarted): its pane ids name other panes, even
    /// where they are the same numbers. A snapshot that names no instance
    /// (the link is down) says nothing either way.
    fn other_server(&self, snapshot: &Snapshot) -> bool {
        matches!(
            (&self.instance, &snapshot.instance),
            (Some(shown), Some(new)) if shown != new
        )
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
    /// The drawer's result 表示 / 隠す.
    result_toggle: Option<WidgetId>,
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

/// The line under the panels that shows what the last action said, cut at
/// the window's width. It keeps its height when empty, so a notice moves
/// nothing.
fn notice_line(tree: &mut UiTree, root: WidgetId, feed: &Feed) -> WidgetId {
    let line = tree.insert(
        Some(root),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::CENTER,
            size: LayoutSize {
                width: percent(1.0),
                height: length(NOTICE_HEIGHT),
            },
            padding: LayoutRect {
                left: length(GUTTER),
                right: length(GUTTER),
                top: length(0.0),
                bottom: length(0.0),
            },
            flex_shrink: 0.0,
            ..Default::default()
        }),
    );
    let slot = tree.insert(Some(line), Container::new(fill_remaining()));
    tree.insert(
        Some(slot),
        Label::new(health::initial_notice(feed))
            .font_size(12.0)
            .truncation(Truncation::Tail),
    )
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
    /// The notice line at the foot of the window (`health::tick`).
    notice: WidgetId,
    queue_buttons: QueueButtons,
    registry: Registry,
    /// The size the last `prepare` laid the screen out at. A rebuilt screen is
    /// laid out at it at once (see `rebuilt`).
    viewport: Option<Size>,
    /// The panes whose screen tails the feed was last asked for; `None`
    /// until the next `watch` asks, whatever it finds.
    watched: Option<Vec<u64>>,
    /// Wheel travel not yet turned into pages, per panel (the pager).
    wheel_accum: [f32; 5],
    /// A structural snapshot held back while an IME composes in the filter
    /// field (a rebuild would end the composition), with when it came.
    pending: Option<(Snapshot, Instant)>,
    /// The `now` of the last `tick`: a rebuild between frames shows the
    /// header's health as of it.
    ticked: Option<Instant>,
    /// The pointer buttons held down since presses the screen passed on,
    /// in the order pressed (`track_press`, `cancel_press`).
    held: Vec<PointerButton>,
    /// What the open help disabled under it, to enable again when it
    /// closes (`open_help`).
    shielded: Vec<WidgetId>,
    /// The filter field had the focus when the input being handled began:
    /// a rebuild that input causes focuses the new field. For a release,
    /// what counts is the press: a press moves the focus to what it hits
    /// before the release activates it (a grouping segment, a button).
    filter_had_focus: bool,
    pressed_in_filter: bool,
    #[cfg(test)]
    panels: [Option<WidgetId>; 5],
    /// Lets the tests drive `wheel` as if the pager took the event: U1's
    /// pager stub takes none (U3 fills `pager::wheel` in).
    #[cfg(test)]
    pager_takes_wheel: bool,
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
        let mut tree = UiTree::new();
        tree.set_theme(Theme::for_appearance(Appearance::Dark));
        let mut dashboard = Self::build(tree, view, feed, Filter::default(), false);
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

    /// The header shows the snapshot's age as of when it was taken in: the
    /// one time the build reads, not a later clock reading.
    fn from_snapshot(view: View, snapshot: Snapshot) -> Self {
        let mut feed = Feed::default();
        let fleet = snapshot.fleet.clone();
        let received = Instant::now();
        feed.accept(fleet, snapshot, received);
        let mut dashboard = Self::start(view, feed);
        dashboard.tick(received);
        dashboard
    }

    /// Takes a snapshot in that has just come, as a frame does.
    #[cfg(test)]
    fn apply(&mut self, snapshot: Snapshot, text: &mut TextStore) {
        let now = Instant::now();
        self.apply_received(snapshot, now, text);
        self.tick(now);
    }

    /// Takes a snapshot in that came at `received`. One that changes only
    /// what time changes (texts, meters, lanes, the queue's order) updates
    /// those widgets in place, so the scroll positions and the hover
    /// survive; any other change rebuilds the screen. A snapshot from
    /// another server instance rebuilds it without the panes chosen on the
    /// old one (`forget_panes`). The caller brings the header's time up to
    /// its own (`tick`).
    ///
    /// A live snapshot also tells the feed again, once, which panes this
    /// screen watches. The feed takes a `Watch` as naming the panes of the
    /// newest instance it had sent when the `Watch` came, and that can be
    /// one this screen never showed: a snapshot of another Noa held back
    /// while an IME composed, then passed over for a newer one of the Noa
    /// shown. The feed would then never fetch what this screen shows,
    /// since nothing it shows changed. Told again as each live snapshot is
    /// taken in, the feed names the list after the instance it is
    /// following by the next one at the latest; the same list again
    /// changes nothing there.
    fn apply_received(&mut self, snapshot: Snapshot, received: Instant, text: &mut TextStore) {
        let old = self.order();
        let old_labels: Vec<(u64, String)> = old
            .iter()
            .filter_map(|pane| Some((*pane, self.feed.fleet.worker(*pane)?.label.clone())))
            .collect();
        let other_server = self.feed.other_server(&snapshot);
        let fleet = health::keep_on_down(&self.feed.fleet, snapshot.fleet.clone(), &snapshot.link);
        let structural = other_server
            || self.feed.link.as_ref() != Some(&snapshot.link)
            || !self.feed.fleet.same_structure(&fleet);
        let live = snapshot.link == Link::Live;
        if live {
            // The rebuild or the in-place update below asks for what it
            // shows; else the `watch` at the end does.
            self.watched = None;
        }
        self.feed.accept(fleet, snapshot, received);
        if other_server {
            self.forget_panes();
        } else {
            // What the filter matches is structure: only a structural
            // snapshot can move the selection or the drawer, and its
            // rebuild shows that.
            let dropped = self.reselect(&old, Vanish::Clear);
            let label = dropped.and_then(|pane| old_labels.iter().find(|(p, _)| *p == pane));
            if let Some((_, label)) = label {
                self.feed.notice = Some((dropped_text(label), received));
            }
        }
        if structural {
            self.rebuilt(self.view, text);
        } else {
            self.refresh(text);
        }
        if live {
            self.watch();
        }
    }

    /// Drops what names a pane of the server instance the screen showed
    /// until now — the selection, the drawer, the expanded queue card —
    /// since the new instance may give those ids to other panes: Enter or
    /// the drawer's 開く would then reach a worker nobody chose. The rebuild
    /// that follows shows the new server's workers from the top, and asks
    /// for their tails as every live snapshot does (`apply_received`), even
    /// where the ids are the same: the ids asked for before were the old
    /// instance's panes.
    fn forget_panes(&mut self) {
        self.view.selected = None;
        self.view.reveal = None;
        self.view.detail = None;
        self.view.result = None;
        self.view.expanded = None;
    }

    /// Takes in what the feed sent since the last frame, as of `now`. A
    /// structural snapshot waits while an IME composes in the filter field
    /// (a rebuild would end the composition), keeping when it came.
    fn take_snapshots(&mut self, now: Instant, text: &mut TextStore) {
        let latest = self
            .feed
            .channel
            .as_ref()
            .and_then(|channel| channel.snapshots.try_iter().last());
        if let Some(snapshot) = latest {
            self.pending = Some((snapshot, now));
        }
        if let Some((snapshot, received)) = self.pending.take() {
            if self.composing() && self.feed.structural(&snapshot) {
                self.pending = Some((snapshot, received));
            } else {
                self.apply_received(snapshot, received, text);
            }
        }
    }

    /// One frame's work before it is drawn, all as of `now`: the snapshot
    /// taken in, the notice that came, and the header's link health. Each
    /// reads this one time, so what the header says does not depend on how
    /// long the frame took (a rebuild under load can take a second).
    fn frame(&mut self, now: Instant, text: &mut TextStore) {
        self.take_snapshots(now, text);
        let notice = self
            .feed
            .channel
            .as_ref()
            .and_then(|channel| channel.notices.try_iter().last());
        if let Some(notice) = notice {
            self.show_notice_at(notice, now);
        }
        self.tick(now);
    }

    /// The order J / K walk now.
    fn order(&self) -> Vec<u64> {
        select::order(&self.feed.fleet, &self.view, &self.filter)
    }

    /// After the order changed from `old`: a selected pane that left it is
    /// replaced by its neighbour (expanding its queue card) after the user's
    /// own change (`Vanish::Neighbour`), and dropped after a live update
    /// (`Vanish::Clear`): Enter then opens only a pane the user chose. The
    /// drawer closes when its worker left the fleet or the filter; else it
    /// follows a selection that moved, as it does on J / K. The caller's
    /// rebuild, refill or `reopen_drawer` shows that, and `watch` asks for
    /// its tail. Returns the pane a live update dropped.
    fn reselect(&mut self, old: &[u64], vanish: Vanish) -> Option<u64> {
        let order = self.order();
        let dropped = self
            .view
            .selected
            .filter(|pane| vanish == Vanish::Clear && !order.contains(pane));
        let selected = match dropped {
            Some(_) => None,
            None => select::reselect(old, &order, self.view.selected),
        };
        let moved = selected != self.view.selected;
        if moved {
            self.view.selected = selected;
            self.view.reveal = selected;
        }
        // The selected worker's queue card is the expanded one, also when
        // the queue comes back after the selection moved without it.
        if let Some(pane) = self.view.selected.filter(|pane| self.waits(*pane)) {
            self.view.expanded = Some(pane);
        }
        let gone = |pane: u64| {
            self.feed
                .fleet
                .worker(pane)
                .is_none_or(|w| !self.filter.matches(w))
        };
        if self.view.detail.is_some_and(gone) {
            self.view.detail = None;
        } else if moved && self.view.detail.is_some() {
            // With no selection left, the drawer closes.
            self.view.detail = selected;
        }
        dropped
    }

    /// Whether the worker has a queue card: it waits on a person, or it
    /// informs and the queue shows 参考情報.
    fn waits(&self, pane: u64) -> bool {
        let worker = self.feed.fleet.worker(pane);
        worker
            .and_then(Worker::need)
            .is_some_and(|need| self.view.info_open || need.tier() != Tier::Info)
    }

    fn show_notice(&mut self, notice: String) {
        self.show_notice_at(notice, Instant::now());
    }

    fn show_notice_at(&mut self, notice: String, now: Instant) {
        self.feed.notice = Some((notice, now));
        self.tick(now);
    }

    /// Brings the link health in the header up to `now`; `before_frame`
    /// calls it on every frame (the live feed wakes the window every second,
    /// and a notice asks for the frame that ends it). A snapshot held back
    /// while an IME composes already shows its link state there: a lost
    /// link must not wait for the composition to end.
    fn tick(&mut self, now: Instant) {
        self.ticked = Some(now);
        let held = self.pending.as_ref().map(|(snapshot, at)| (snapshot, *at));
        health::tick(
            &mut self.tree,
            &self.header,
            self.notice,
            &self.feed,
            held,
            now,
        );
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
        self.reorder_queue(text);
        self.paint_selection();
    }

    /// Puts the queue's cards (and its pager items) in the queue's order
    /// again, which waits change: the cards show the order J / K walk. A
    /// card keeps to its section (a change of need is structural), so the
    /// cards of each section trade places among the list's slots for that
    /// section, and the section headers and a caption stay where they are.
    /// The expanded card follows as a build picks it — `view.expanded`
    /// while the queue has it, else the top card outside 参考情報 — so a new
    /// top card is expanded in place (and watched), as a fresh build of the
    /// same state shows it.
    fn reorder_queue(&mut self, text: &mut TextStore) {
        let Some(parts) = self.registry.lists[Panel::Queue as usize].as_mut() else {
            return;
        };
        let matching = self
            .feed
            .fleet
            .workers
            .iter()
            .filter(|w| self.filter.matches(w));
        let cards = queue::cards(matching, self.view.info_open);
        let order: Vec<u64> = cards.iter().map(|(w, _)| w.pane).collect();
        let tier_of = |pane: u64| {
            cards
                .iter()
                .find(|(w, _)| w.pane == pane)
                .map(|(_, n)| n.tier())
        };
        let card_pane = |id: WidgetId| {
            parts
                .items
                .iter()
                .find(|(card, _)| *card == id)
                .and_then(|(_, pane)| *pane)
        };
        let children = self.tree.children(parts.list).to_vec();
        let mut sorted: Vec<(WidgetId, u64)> = children
            .iter()
            .filter_map(|id| Some((*id, card_pane(*id)?)))
            .collect();
        let at = |pane: u64| order.iter().position(|p| *p == pane);
        sorted.sort_by_key(|(_, pane)| at(*pane));
        let mut placed = Vec::with_capacity(children.len());
        for id in children {
            let Some(pane) = card_pane(id) else {
                placed.push(id);
                continue;
            };
            let tier = tier_of(pane);
            let next = sorted
                .iter()
                .position(|(_, other)| tier_of(*other) == tier)
                .expect("a card of the same section");
            placed.push(sorted.remove(next).0);
        }
        for (index, id) in placed.iter().enumerate() {
            self.tree.move_child(*id, index);
        }
        let position = |id: &WidgetId| placed.iter().position(|p| p == id);
        parts.items.sort_by_key(|(id, _)| position(id));
        let expanded = self
            .view
            .expanded
            .filter(|pane| order.contains(pane))
            .or(cards
                .iter()
                .find(|(_, need)| need.tier() != Tier::Info)
                .map(|(w, _)| w.pane));
        if expanded != self.queue_buttons.expanded {
            self.expand_in_place(expanded, &order, text);
        }
    }

    /// Makes `pane`'s queue card the expanded one without a rebuild: the
    /// card expanded so far collapses, and the buttons are registered as
    /// `queue` registers them (in the queue's `order`).
    fn expand_in_place(&mut self, pane: Option<u64>, order: &[u64], text: &mut TextStore) {
        let Some(parts) = self.registry.lists[Panel::Queue as usize].as_ref() else {
            return;
        };
        let card_of = |pane: u64| {
            let item = parts.items.iter().find(|(_, own)| *own == Some(pane));
            item.map(|(card, _)| *card)
        };
        let fleet = &self.feed.fleet;
        let buttons = &mut self.queue_buttons;
        for (pane, expanded) in [(buttons.expanded, false), (pane, true)] {
            let Some((pane, card)) = pane.and_then(|pane| Some((pane, card_of(pane)?))) else {
                continue;
            };
            let worker = fleet.worker(pane).expect("a queue card's worker");
            let button = queue::set_expanded(&mut self.tree, card, worker, expanded, text);
            buttons.open.retain(|(_, own)| *own != pane);
            buttons.details.retain(|(_, own)| *own != pane);
            if expanded {
                buttons.open.push((button, pane));
            } else {
                buttons.details.push((button, pane));
            }
        }
        buttons.expanded = pane;
        let at = |pane: u64| order.iter().position(|p| *p == pane);
        buttons.details.sort_by_key(|(_, pane)| at(*pane));
        self.watch();
    }

    /// Builds the same screen again for a changed view, in the same `UiTree`:
    /// what the app installed on it stays — the system clipboard (⌘V into
    /// the filter field), a stylesheet, the theme (the appearance the OS last
    /// set), input recording — where a new tree would start from its
    /// defaults (an empty in-app clipboard). The old widgets go through
    /// `UiTree::remove` first: that is what releases their text layouts in
    /// the `TextStore`. The new widgets are laid out at the old size
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
        let roots: Vec<WidgetId> = self
            .tree
            .root()
            .into_iter()
            .chain(self.tree.overlays())
            .collect();
        for root in roots {
            self.tree.remove(root, text);
        }
        let tree = std::mem::take(&mut self.tree);
        let feed = std::mem::take(&mut self.feed);
        let filter = std::mem::take(&mut self.filter);
        let old = std::mem::replace(self, Self::build(tree, view, feed, filter, refocus));
        self.viewport = old.viewport;
        self.watched = old.watched;
        self.wheel_accum = old.wheel_accum;
        self.pending = old.pending;
        if let Some(now) = old.ticked {
            self.tick(now);
        }
        self.held = old.held;
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

    /// Builds the screen into `tree`, which has no root or overlay.
    fn build(mut tree: UiTree, view: View, feed: Feed, filter: Filter, refocus: bool) -> Self {
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
        let notice = notice_line(&mut tree, root, &feed);
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
            notice,
            queue_buttons: QueueButtons::default(),
            registry,
            viewport: None,
            watched: None,
            wheel_accum: [0.0; 5],
            pending: None,
            ticked: None,
            held: Vec::new(),
            shielded: Vec::new(),
            filter_had_focus: false,
            pressed_in_filter: false,
            #[cfg(test)]
            panels: [None; 5],
            #[cfg(test)]
            pager_takes_wheel: false,
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
            empty(tree, body, link, &view.shown);
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
                        queue_buttons =
                            queue(tree, id, &matching, workers, &view, filtering, registry);
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
                "ペインへ移動できません: Noa の server-scopes に control がありません（例: server-scopes = read,control）".to_owned()
            }
            Some(channel) => {
                channel.send(Command::Focus(pane));
                format!("{} のペインへ移動します", worker.label)
            }
            None => format!("{} のペインへ移動します", worker.label),
        };
        self.show_notice(status);
    }

    /// Opens the drawer on `view.detail`, if its worker is shown. A result
    /// read for another worker, or before the drawer closed, is not read
    /// again unasked.
    fn open_drawer(&mut self) {
        self.view.result = self.view.result.filter(|p| Some(*p) == self.view.detail);
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
        let preview = worker
            .result
            .as_deref()
            .filter(|_| self.view.result == Some(pane))
            .map(drawer::read_result);
        let buttons = drawer::build(
            &mut self.tree,
            root,
            worker,
            &self.feed,
            preview.as_ref(),
            &mut self.registry,
        );
        self.registry.open_buttons.push((buttons.open, pane));
        self.registry.drawer = Some((root, buttons.close));
        self.registry.result_toggle = buttons.result;
    }

    fn close_drawer(&mut self, text: &mut TextStore) {
        if let Some((root, _)) = self.registry.drawer.take() {
            self.tree.close_overlay(root, text);
        }
        self.registry.open_buttons.clear();
        self.registry.result_toggle = None;
        self.registry
            .pulses
            .retain(|(_, pulse)| !matches!(pulse, Pulse::Drawer(_)));
    }

    fn reopen_drawer(&mut self, text: &mut TextStore) {
        self.close_drawer(text);
        self.open_drawer();
        self.lay_out_again(text);
        self.watch();
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
        // What lies under the help — the screen and the drawer — is
        // disabled while it is open. The modal overlay keeps pointer and
        // keys from it, but assistive technology acts on a widget directly
        // (`UiTree::accessibility_action`, not `dispatch`), and the tree
        // refuses that only for a disabled widget, which it also offers as
        // disabled. Disabled controls are drawn dimmed under the help. Only
        // what is enabled now is disabled (`shielded`), so closing the help
        // leaves the rest as it was.
        let drawer = self.registry.drawer.map(|(root, _)| root);
        for root in self.tree.root().into_iter().chain(drawer) {
            if self.tree.is_enabled(root) {
                self.tree.set_enabled(root, false);
                self.shielded.push(root);
            }
        }
    }

    /// Enables what the help disabled before it closes the overlay: the
    /// tree then gives the focus back to where it was when the help opened
    /// (a 閉じる clicked took it), and a disabled widget would refuse it.
    fn close_help(&mut self, text: &mut TextStore) {
        self.view.help = false;
        for root in std::mem::take(&mut self.shielded) {
            self.tree.set_enabled(root, true);
        }
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
        self.reselect(&old, Vanish::Neighbour);
        if self.view.detail != detail {
            self.close_drawer(text);
            self.open_drawer();
        }
        // The count first: shown or hidden, it moves the field, and the
        // refill lays the screen out at once for input that comes before
        // the next frame (a click aimed at the field where it now is).
        let count = self.header.match_count;
        set_label(
            &mut self.tree,
            count,
            match_text(&self.feed.fleet.workers, &self.filter),
        );
        self.tree.set_hidden(count, !self.filter.active());
        self.refill_body(text);
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
        self.reselect(&old, Vanish::Neighbour);
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
                } else if self.filter.active() {
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
                self.cancel_press(text);
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
        #[cfg(test)]
        let taken = taken || (self.pager_takes_wheel && matches!(event, Event::Wheel { .. }));
        if taken {
            self.paged(text);
        }
        taken
    }

    /// After the wheel changed a page: pages and lays out at once (input
    /// may follow before the next frame), and leaves the root marked for
    /// compositing, since that layout is not a frame the window presented
    /// and would otherwise take the change from it (see `rebuilt`).
    fn paged(&mut self, text: &mut TextStore) {
        self.lay_out_again(text);
    }

    /// Ends the pointer presses in progress without their releases taking
    /// effect. A release goes to the widget that took the press (it holds
    /// the pointer), so a modal opened in between would let a held 開く fire
    /// underneath it. noa-gpui has no call to drop that hold; a release
    /// outside the window does it the way the widgets expect — a button
    /// lets go without acting on a release that is not over it. The left
    /// button's release goes first: it is the one a button acts on, and
    /// the tree ends its hold on the pointer at any button's release, after
    /// which the left one would reach nothing. The real releases that
    /// follow reach nothing (`track_press`). The tree uses the pointer
    /// position only for the cursor, which the next pointer event sets
    /// again.
    fn cancel_press(&mut self, text: &mut TextStore) {
        let mut held = std::mem::take(&mut self.held);
        held.sort_by_key(|button| *button != PointerButton::Primary);
        for button in held {
            let pos = OUTSIDE;
            self.tree.dispatch(Event::PointerUp { pos, button }, text);
        }
        // A release away from what was pressed activates nothing: anything
        // said about it is part of the cancelled press.
        drop(self.tree.take_messages());
    }

    /// Keeps `held` up to date with `event`, and drops the release of a
    /// press that is not held — `cancel_press` ended it and sent its
    /// release already. Passed on, the tree would give it to whatever holds
    /// the pointer now, a press begun since (which any button's release
    /// ends: a segmented control lets go of its segment), or else to what
    /// is under it (a button whose press lost the pointer's hold before the
    /// cancel still counts itself pressed, and would fire).
    fn track_press(&mut self, event: Event) -> Option<Event> {
        match event {
            Event::PointerDown { button, .. } => {
                if !self.held.contains(&button) {
                    self.held.push(button);
                }
            }
            Event::PointerUp { button, .. } => {
                let at = self.held.iter().position(|held| *held == button)?;
                self.held.remove(at);
            }
            _ => {}
        }
        Some(event)
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
        let Some(event) = self.track_press(event) else {
            return;
        };
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
                let event = if in_filter {
                    filter::one_line_input(event)
                } else {
                    event
                };
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
        self.frame(Instant::now(), text);
    }

    fn handle(&mut self, message: Message, text: &mut TextStore) {
        if let Some(TextChanged(query)) = message.downcast_ref::<TextChanged>()
            && message.source == self.header.filter_field
        {
            let field = message.source;
            let caret = self
                .tree
                .get::<TextField>(field)
                .map_or(query.len(), TextField::caret);
            // A paste puts the clipboard's text in as it is: the field
            // gets it on one line before anything lays it out.
            let (query, caret) = match filter::on_one_line(query, caret) {
                Some((line, caret)) => {
                    self.tree.update::<TextField, _>(field, |f, cx| {
                        f.set_text(cx, line.as_str());
                        f.select(cx, caret..caret);
                    });
                    (line, caret)
                }
                None => (query.clone(), caret),
            };
            self.set_filter(query, caret, text);
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
        if self.queue_buttons.info_toggle == Some(source) {
            let view = View {
                info_open: !self.view.info_open,
                ..self.view
            };
            self.change_view(view, text);
            return;
        }
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
        } else if self.registry.result_toggle == Some(source) {
            let shown = self.view.result.is_some() && self.view.result == self.view.detail;
            self.view.result = if shown { None } else { self.view.detail };
            self.reopen_drawer(text);
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
