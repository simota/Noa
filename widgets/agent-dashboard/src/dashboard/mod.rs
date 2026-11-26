use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, length, percent,
};
use noa_gpui_core::widgets::{
    ButtonClicked, Container, ControlSize, Label, ScrollView, SegmentMode, SegmentedControl,
    SelectionChanged, TextChanged, TextField, Truncation,
};
use noa_gpui_core::{
    Appearance, Dirty, Edge, Event, Message, OverlayOptions, Placement, PointerButton, Role, Style,
    Theme, UiTree, WidgetId,
};
use noa_gpui_platform::Screen;
use noa_gpui_scene::{Color, Point, Size};
use noa_gpui_text::TextStore;

use crate::fleet::{self, Fleet, Grouping, Scope, State, Worker};
use crate::live::{Channel, CloseTarget, Command, Link, Snapshot, Unsent};
use crate::repo_browser::{self, Launcher};

mod analytics;
mod drawer;
mod filter;
mod gantt;
mod graph;
mod header;
mod health;
mod help;
mod keymap;
mod list;
mod mindmap;
mod pager;
mod panel;
mod prefs;
mod select;
mod style;
mod timeline;
use filter::Filter;
use gantt::TimeScale;
use header::{HeaderParts, match_text};
use keymap::{Action, Mode, Route};
use list::RowAction;
use mindmap::MapAxis;
use pager::ListParts;
use panel::empty;
pub use prefs::{Prefs, PrefsStore, default_path as default_prefs_path};
use select::Step;
use style::{GUTTER, HEADER_HEIGHT, fill_remaining, gap, set_label};

/// What a rebuild carries over to the drawer it builds: the unsent
/// messages (`Dashboard::drafts`, `Dashboard::send_caret`), so the drawer
/// shows its draft at once, and a bulk close waiting for its second press.
#[derive(Default)]
struct Drafts {
    text: HashMap<u64, String>,
    caret: Option<(u64, usize)>,
    /// The bulk close waiting for its second press (`Dashboard::close_ended`):
    /// the drawer's button says so from its first build.
    close_ended: Option<Vec<u64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Operations,
    Analytics,
    MindMap,
    Timeline,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Operations, Tab::Analytics, Tab::MindMap, Tab::Timeline];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Operations => "運用",
            Tab::Analytics => "統計",
            Tab::MindMap => "マップ",
            Tab::Timeline => "タイムライン",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "ops" | "operations" => Some(Tab::Operations),
            "analytics" | "stats" => Some(Tab::Analytics),
            "map" | "mindmap" => Some(Tab::MindMap),
            "timeline" | "gantt" => Some(Tab::Timeline),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct View {
    tab: Tab,
    map_axis: MapAxis,
    time_scale: TimeScale,
    grouping: Grouping,
    /// Which workers the list shows (dashboard-worker-list AC2).
    scope: Scope,
    /// The selected worker (AC1).
    selected: Option<u64>,
    /// The worker in the details drawer (AC8).
    detail: Option<u64>,
    /// The worker whose result file the drawer shows, read when asked for.
    result: Option<u64>,
    /// The worker awaiting a second confirmation to close its pane.
    confirm_close: Option<u64>,
    /// Whether the keyboard help is open (AC5).
    help: bool,
    /// The first row the list shows (AC3, the pager).
    first: usize,
    /// The pane of that row, as the pager last saw it: a list built again
    /// starts from it, so a row that comes above moves nothing the user is
    /// reading (dashboard-worker-list AC5). `None` starts at the top.
    anchor: Option<u64>,
    /// The pane the pager must bring into view (AC3): the user's own move.
    reveal: Option<u64>,
}

impl View {
    fn of(grouping: Grouping) -> Self {
        View {
            tab: Tab::Operations,
            map_axis: MapAxis::Orchestrator,
            time_scale: TimeScale::All,
            grouping,
            scope: Scope::Current,
            selected: None,
            detail: None,
            result: None,
            confirm_close: None,
            help: false,
            first: 0,
            anchor: None,
            reveal: None,
        }
    }

    /// The view a run starts with: what `prefs` keeps, nothing selected.
    fn from_prefs(prefs: &Prefs) -> Self {
        View {
            tab: prefs.tab,
            map_axis: prefs.map_axis,
            time_scale: prefs.time_scale,
            scope: prefs.scope,
            ..View::of(prefs.grouping)
        }
    }
}

impl Prefs {
    /// The command line's values over the saved ones: each one given wins.
    pub fn overridden(
        self,
        tab: Option<Tab>,
        grouping: Option<Grouping>,
        filter: Option<String>,
    ) -> Prefs {
        Prefs {
            tab: tab.unwrap_or(self.tab),
            grouping: grouping.unwrap_or(self.grouping),
            filter: filter.unwrap_or(self.filter),
            ..self
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
    /// The live feed's `Snapshot::can_input`.
    can_input: bool,
    /// The live feed's `Snapshot::safe_send`.
    safe_send: bool,
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
/// What 差分を見る says where nothing is ever started.
const DEMO_NO_LAUNCH: &str = "[DEMO] デモでは Repo Browser を起動しません";
const SNAPSHOT_NO_LAUNCH: &str = "スナップショットでは Repo Browser を起動しません";
const NOTHING_SELECTED: &str = "ワーカーが選ばれていません（J / K で選ぶ）";
const DRAWER_WIDTH: f32 = 384.0;
const HELP_WIDTH: f32 = 520.0;
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

/// What the notice says of a selection a live update took: where its
/// worker went, when it is still there under the filter (`moved_to`).
fn dropped_text(label: &str, moved_to: Option<Scope>) -> String {
    let label = fleet::one_line(label);
    match moved_to {
        Some(scope) => format!(
            "{label} が一覧から外れたため選択を解除しました（{label} は {} に移りました）",
            scope.title()
        ),
        None => format!("{label} が一覧から外れたため選択を解除しました"),
    }
}

/// What the notice says of workers a live update put in need of attention.
fn arrived_text(arrived: &[&Worker]) -> Option<String> {
    match arrived {
        [] => None,
        [worker] => Some(format!(
            "要対応が増えました: {}（{}）",
            fleet::one_line(&worker.label),
            worker.status().title()
        )),
        more => Some(format!("要対応が {} 件増えました", more.len())),
    }
}

impl Feed {
    #[cfg(test)]
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
        self.can_input = snapshot.can_input;
        self.safe_send = snapshot.safe_send;
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
    /// A row's wait or last update (`list::time_text`).
    RowTime(u64),
    /// A quiet row's qualifiers, which say how long its output stood still
    /// (`list::qualifiers`).
    RowQuiet(u64),
    /// A time label in the details drawer (`drawer::refresh`).
    Drawer(u64),
}

impl Pulse {
    /// Lives outside the body, so a body refill keeps it.
    fn outside_body(self) -> bool {
        matches!(self, Pulse::Drawer(_))
    }
}

/// How a widget shows that its worker is selected (`Dashboard::paint_selection`).
#[derive(Debug, Clone, Copy)]
enum Mark {
    /// A row's accent bar: shown when selected.
    Bar,
    /// A timeline lane row: filled when selected.
    Lane,
    /// A マップ (mindmap) node pill: a 2 px accent border when selected,
    /// else its own.
    Node { normal: Color },
}

/// What the builders register for later in-place updates and input.
#[derive(Default)]
pub struct Registry {
    pulses: Vec<(WidgetId, Pulse)>,
    /// The drawer's 開く.
    open_buttons: Vec<(WidgetId, u64)>,
    marks: Vec<(WidgetId, u64, Mark)>,
    /// The worker list's rows, which the pager pages.
    list: Option<ListParts>,
    /// Each row, for a click that selects its worker.
    rows: Vec<(WidgetId, u64)>,
    /// Each row's button and what it does (AC8).
    row_actions: Vec<(WidgetId, u64, RowAction)>,
    /// The list's 現在 / 要対応 / 終了 switch.
    scope_control: Option<WidgetId>,
    /// The toolbar's 「絞り込み外に要対応N件」 and its 絞り込みを解除 (AC7).
    outside_note: Option<WidgetId>,
    clear_filter: Option<WidgetId>,
    /// The drawer overlay and its 閉じる.
    drawer: Option<(WidgetId, WidgetId)>,
    /// The drawer's ペインを閉じる / 本当に閉じる button.
    close_pane_button: Option<(WidgetId, u64)>,
    /// The drawer's まとめて閉じる button, with the count it names.
    close_ended_button: Option<(WidgetId, usize)>,
    /// The drawer's 差分を見る button.
    diff_button: Option<(WidgetId, u64)>,
    /// The drawer's result 表示 / 隠す.
    result_toggle: Option<WidgetId>,
    /// The drawer's result 再読込 button.
    result_reload: Option<WidgetId>,
    /// The drawer's result コピー button.
    result_copy: Option<WidgetId>,
    /// The drawer's screen tail コピー button.
    tail_copy: Option<WidgetId>,
    /// The drawer's send input field and 送信 button.
    send_field: Option<(WidgetId, u64)>,
    send_button: Option<(WidgetId, u64)>,
    /// Tab-local segmented controls (map axis, timeline scale).
    axis_control: Option<WidgetId>,
    scale_control: Option<WidgetId>,
    /// The help overlay and its 閉じる.
    help: Option<(WidgetId, WidgetId)>,
}

impl Registry {
    /// Forgets what the body registered; the header and overlays stay.
    fn clear_body(&mut self) {
        self.pulses.retain(|(_, pulse)| pulse.outside_body());
        self.marks.clear();
        self.list = None;
        self.rows.clear();
        self.row_actions.clear();
        self.scope_control = None;
        self.outside_note = None;
        self.clear_filter = None;
        self.axis_control = None;
        self.scale_control = None;
    }
}

/// The footer bar under the list: left notice text (cut at available width)
/// and right tab switcher (`運用 / 統計 / マップ / タイムライン`). Keeps its
/// fixed height (`NOTICE_HEIGHT`) so switching notices moves nothing.
fn notice_line(tree: &mut UiTree, root: WidgetId, feed: &Feed, tab: Tab) -> (WidgetId, WidgetId) {
    let line = tree.insert(
        Some(root),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::CENTER,
            gap: gap(10.0),
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
        })
        .background(Role::Surface)
        .border(1.0, Role::Separator),
    );
    let slot = tree.insert(Some(line), Container::new(fill_remaining()));
    let notice = tree.insert(
        Some(slot),
        Label::new(health::initial_notice(feed))
            .font_size(12.0)
            .truncation(Truncation::Tail),
    );
    let selected = Tab::ALL.iter().position(|t| *t == tab).unwrap_or(0);
    let tab_control = tree.insert(
        Some(line),
        SegmentedControl::new(Tab::ALL.map(Tab::title), SegmentMode::SelectOne)
            .with_selected(&[selected])
            .control_size(ControlSize::Small),
    );
    (notice, tab_control)
}

/// The scroll views under `id`, depth first.
fn scroll_views(tree: &UiTree, id: WidgetId) -> Vec<WidgetId> {
    let own = tree.get::<ScrollView>(id).map(|_| id);
    own.into_iter()
        .chain(
            tree.children(id)
                .iter()
                .flat_map(|child| scroll_views(tree, *child)),
        )
        .collect()
}

fn button_pane(buttons: &[(WidgetId, u64)], source: WidgetId) -> Option<u64> {
    buttons
        .iter()
        .find(|(id, _)| *id == source)
        .map(|(_, pane)| *pane)
}

/// How many workers a bulk close names by label before it counts the rest.
const BULK_NAMED: usize = 5;

/// The labels of `panes` for a bulk close's confirmation: the first
/// `BULK_NAMED`, then how many more.
fn bulk_names(fleet: &Fleet, panes: &[u64]) -> String {
    let labels: Vec<String> = panes
        .iter()
        .filter_map(|pane| fleet.worker(*pane))
        .take(BULK_NAMED)
        .map(|w| fleet::one_line(&w.label))
        .collect();
    let rest = panes.len().saturating_sub(labels.len());
    let named = labels.join("、");
    if rest > 0 {
        format!("{named} ほか {rest} 件")
    } else {
        named
    }
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
    /// The container the list is built in; a filter change refills it.
    body: WidgetId,
    /// The notice line at the foot of the window (`health::tick`).
    notice: WidgetId,
    /// The tab switcher in the footer bar.
    tab_control: WidgetId,
    /// The result file `view.result` shows, as read when 表示 was pressed:
    /// a rebuild shows it again without reading the file.
    preview: Option<(u64, PathBuf, drawer::Preview)>,
    /// Unsent message drafts per worker pane, kept when switching workers.
    drafts: HashMap<u64, String>,
    /// The send field's caret across rebuilds, with the pane whose draft it
    /// is in: another worker's field puts its caret at its draft's end.
    send_caret: Option<(u64, usize)>,
    registry: Registry,
    /// The size the last `prepare` laid the screen out at. A rebuilt screen is
    /// laid out at it at once (see `rebuilt`).
    viewport: Option<Size>,
    /// The panes whose screen tails the feed was last asked for; `None`
    /// until the next `watch` asks, whatever it finds.
    watched: Option<Vec<u64>>,
    /// Wheel travel not yet turned into pages (the pager).
    wheel_accum: f32,
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
    /// When the close `view.confirm_close` (or `close_ended`) waits for
    /// ends unconfirmed: as its notice does (`NOTICE_FOR`).
    close_until: Option<Instant>,
    /// The ended workers a first ⇧X (or まとめて閉じる) named, waiting for
    /// the second press to close them all (`request_close_ended`).
    close_ended: Option<Vec<u64>>,
    /// Shift came up since `close_ended` was armed. The platform marks no
    /// key repeat and sends no key release, so ⇧X held down sends ⇧X again
    /// and again: only one after Shift came up is a second press.
    shift_up: bool,
    /// The filter field had the focus when the input being handled began:
    /// a rebuild that input causes focuses the new field. For a release,
    /// what counts is the press: a press moves the focus to what it hits
    /// before the release activates it (a grouping segment, a button).
    filter_had_focus: bool,
    pressed_in_filter: bool,
    /// Where the view is saved when it changes (`after_input`) and when
    /// the window closes; `None` keeps nothing (the tests, a snapshot).
    prefs_store: Option<PrefsStore>,
    /// Starts the Repo Browser for 差分を見る.
    repo_browser: Box<dyn Launcher>,
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
    /// The demo: the canned fleet. Only the tests start so; the app starts
    /// from the saved view (`with_prefs`).
    #[cfg(test)]
    pub fn new(grouping: Grouping) -> Self {
        Self::showing(View::of(grouping))
    }

    #[cfg(test)]
    fn showing(view: View) -> Self {
        Self::start(view, Feed::demo())
    }

    fn start(view: View, feed: Feed) -> Self {
        let mut tree = UiTree::new();
        tree.set_theme(Theme::for_appearance(Appearance::Dark));
        let mut dashboard = Self::build(
            tree,
            view,
            feed,
            Filter::default(),
            false,
            None,
            Drafts::default(),
        );
        dashboard.watch();
        dashboard
    }

    /// Fed by the live feed, connecting until its first snapshot arrives.
    /// As `new`, for the tests (`live_with_prefs`).
    #[cfg(test)]
    pub fn live(grouping: Grouping, channel: Channel) -> Self {
        let feed = Feed {
            link: Some(Link::Connecting),
            channel: Some(channel),
            ..Feed::default()
        };
        Self::start(View::of(grouping), feed)
    }

    /// The demo of `fleet` (`fleet::canned`), starting from `prefs`.
    pub fn with_prefs(prefs: &Prefs, fleet: Fleet) -> Self {
        let feed = Feed {
            fleet,
            ..Feed::default()
        };
        Self::start(View::from_prefs(prefs), feed)
            .offline(DEMO_NO_LAUNCH)
            .with_filter(prefs.filter.clone())
    }

    /// `live`, starting from `prefs`.
    pub fn live_with_prefs(prefs: &Prefs, channel: Channel) -> Self {
        let feed = Feed {
            link: Some(Link::Connecting),
            channel: Some(channel),
            ..Feed::default()
        };
        Self::start(View::from_prefs(prefs), feed).with_filter(prefs.filter.clone())
    }

    /// `of_snapshot`, starting from `prefs`.
    pub fn of_snapshot_with_prefs(prefs: &Prefs, snapshot: Snapshot) -> Self {
        Self::from_snapshot(View::from_prefs(prefs), snapshot)
            .offline(SNAPSHOT_NO_LAUNCH)
            .with_filter(prefs.filter.clone())
    }

    /// Saves the view in `store` from now on, whenever it changes and when
    /// the window closes. The view shown now is taken as saved: the
    /// command line's values are kept only once the user changes the view.
    pub fn persisting(mut self, mut store: PrefsStore) -> Self {
        store.start_from(self.prefs());
        self.prefs_store = Some(store);
        self
    }

    /// The view as it is kept across runs.
    fn prefs(&self) -> Prefs {
        Prefs {
            tab: self.view.tab,
            grouping: self.view.grouping,
            scope: self.view.scope,
            map_axis: self.view.map_axis,
            time_scale: self.view.time_scale,
            filter: self.filter.query.clone(),
        }
    }

    fn save_prefs(&mut self) {
        let prefs = self.prefs();
        if let Some(store) = &mut self.prefs_store {
            store.save(&prefs);
        }
    }

    /// Switches to an initial tab before the first frame. The app's
    /// `--tab` goes through `Prefs::overridden`.
    #[cfg(test)]
    pub fn with_tab(mut self, tab: Tab) -> Self {
        if self.view.tab == tab {
            return self;
        }
        let mut text = TextStore::new(Default::default());
        let view = View { tab, ..self.view };
        self.change_view(view, &mut text);
        self
    }

    /// Applies an initial filter query (the saved one, or `--filter`) before
    /// the first frame.
    pub fn with_filter(mut self, query: String) -> Self {
        let query = fleet::one_line(&query);
        if query.is_empty() {
            return self;
        }
        let caret = query.len();
        let field = self.header.filter_field;
        self.tree.update::<TextField, _>(field, |f, cx| {
            f.set_text(cx, query.as_str());
            f.select(cx, caret..caret);
        });
        let mut text = TextStore::new(Default::default());
        self.set_filter(query, caret, &mut text);
        self
    }

    /// One live snapshot, with no feed behind it. As `new`, for the tests
    /// (`of_snapshot_with_prefs`).
    #[cfg(test)]
    pub fn of_snapshot(grouping: Grouping, snapshot: Snapshot) -> Self {
        Self::from_snapshot(View::of(grouping), snapshot).offline(SNAPSHOT_NO_LAUNCH)
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
    /// what time changes (the rows' waits and updates, the drawer's times)
    /// updates those widgets in place and moves no row, so the page, the
    /// order and the hover survive; any other change rebuilds the screen,
    /// and the list starts at the row its page started at (`View::anchor`).
    /// The notice says which workers came to need attention, and where a
    /// selected worker that left the list went. A snapshot from
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
        // Only the selection can be dropped, and it may be outside the
        // scope's rows (chosen in another tab): its label is taken from
        // the fleet before the snapshot replaces it.
        let selected_label = self
            .view
            .selected
            .and_then(|pane| Some((pane, self.feed.fleet.worker(pane)?.label.clone())));
        let other_server = self.feed.other_server(&snapshot);
        // Announced only once workers were shown: the first snapshot's
        // attention rows are the list itself, not news.
        let announce = self.feed.had_data && !other_server;
        let attention_before: Vec<u64> = self
            .feed
            .fleet
            .workers
            .iter()
            .filter(|w| w.needs_attention())
            .map(|w| w.pane)
            .collect();
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
            let label = dropped.and(selected_label.as_ref());
            let dropped = label.map(|(pane, label)| {
                let moved_to = self
                    .feed
                    .fleet
                    .worker(*pane)
                    .filter(|w| self.filter.matches(w))
                    .map(|w| {
                        if w.in_scope(Scope::Current) {
                            Scope::Current
                        } else {
                            Scope::Ended
                        }
                    });
                dropped_text(label, moved_to)
            });
            let arrived: Vec<&Worker> = self
                .feed
                .fleet
                .workers
                .iter()
                .filter(|w| w.needs_attention() && !attention_before.contains(&w.pane))
                .collect();
            let arrived = arrived_text(&arrived).filter(|_| announce);
            let notice = [dropped, arrived].into_iter().flatten().collect::<Vec<_>>();
            if !notice.is_empty() {
                self.feed.notice = Some((notice.join(" · "), received));
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
    /// until now — the selection, the drawer, the list's anchor, the unsent
    /// drafts — since the new instance may give those ids to other
    /// panes: Enter, the drawer's 開く or 送信 would then reach a worker
    /// nobody chose. The rebuild that follows shows the new server's
    /// workers from the top, and asks
    /// for their tails as every live snapshot does (`apply_received`), even
    /// where the ids are the same: the ids asked for before were the old
    /// instance's panes.
    fn forget_panes(&mut self) {
        self.view.selected = None;
        self.view.reveal = None;
        self.view.detail = None;
        self.view.result = None;
        self.view.confirm_close = None;
        self.close_ended = None;
        self.view.anchor = None;
        self.view.first = 0;
        self.drafts.clear();
        self.send_caret = None;
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
        if self.close_until.is_some_and(|until| now >= until) {
            self.disarm_close(text);
        }
        let notice = self
            .feed
            .channel
            .as_ref()
            .and_then(|channel| channel.notices.try_iter().last());
        if let Some(notice) = notice {
            self.show_notice_at(notice, now);
        }
        self.take_unsent(text);
        self.tick(now);
    }

    /// Makes each message the feed gave back undelivered its pane's draft
    /// again, before what was typed for that pane since; the drawer shows
    /// it at once when it is on that pane. One for a pane of another server
    /// instance than the one shown is dropped: here its id names another
    /// pane (`forget_panes`).
    fn take_unsent(&mut self, text: &mut TextStore) {
        let unsent: Vec<Unsent> = self
            .feed
            .channel
            .as_ref()
            .map(|channel| channel.unsent.try_iter().collect())
            .unwrap_or_default();
        for Unsent {
            instance,
            pane,
            text: message,
        } in unsent
        {
            if self.feed.instance.as_ref() != Some(&instance) {
                continue;
            }
            let draft = match self.drafts.remove(&pane) {
                Some(newer) => format!("{message} {newer}"),
                None => message,
            };
            self.send_caret = Some((pane, draft.len()));
            self.drafts.insert(pane, draft);
            if self.registry.send_field.is_some_and(|(_, p)| p == pane) {
                let focused = self
                    .registry
                    .send_field
                    .is_some_and(|(id, _)| self.tree.focused() == Some(id));
                self.reopen_drawer(text);
                if focused && let Some((id, _)) = self.registry.send_field {
                    self.tree.set_focus(Some(id));
                }
            }
        }
    }

    /// The rows of the scope shown, in the order a list built now has.
    fn order(&self) -> Vec<u64> {
        select::order(&self.feed.fleet, &self.view, &self.filter)
    }

    /// The order J / K and N walk: the rows as the list shows them, which
    /// time does not reorder (`refresh` moves no row), else `order`.
    fn walk_order(&self) -> Vec<u64> {
        match &self.registry.list {
            Some(parts) => parts.panes(),
            None => self.order(),
        }
    }

    /// After the order changed from `old`: a selected pane that left it is
    /// replaced by its neighbour after the user's own change
    /// (`Vanish::Neighbour`), and dropped after a live update
    /// (`Vanish::Clear`): Enter then opens only a pane the user chose. A
    /// worker selected outside the list (another tab, a drawer opened on
    /// it) is kept while the filter shows it. The drawer closes when its
    /// worker left the fleet or the filter; else it follows a selection
    /// the user moved, as it does on J / K, and stays where it is when a
    /// live update dropped the selection. The caller's rebuild, refill or
    /// `reopen_drawer` shows that, and `watch` asks for its tail. Returns
    /// the pane a live update dropped.
    fn reselect(&mut self, old: &[u64], vanish: Vanish) -> Option<u64> {
        let order = self.order();
        // A worker chosen outside the list (in another tab, or the drawer
        // opened on it) stays chosen while it is not in the list, as long
        // as the filter shows it.
        let shown = |pane: u64| {
            let worker = self.feed.fleet.worker(pane);
            worker.is_some_and(|w| self.filter.matches(w))
        };
        let outside = self
            .view
            .selected
            .is_some_and(|pane| !old.contains(&pane) && !order.contains(&pane) && shown(pane));
        let dropped = self
            .view
            .selected
            .filter(|pane| !outside && vanish == Vanish::Clear && !order.contains(pane));
        let selected = match dropped {
            Some(_) => None,
            None if outside => self.view.selected,
            None => select::reselect(old, &order, self.view.selected),
        };
        let moved = selected != self.view.selected;
        if moved {
            // The caller's rebuild shows the drawer of the new selection.
            self.view.confirm_close = None;
            self.view.selected = selected;
            self.view.reveal = selected;
        }
        let gone = |pane: u64| {
            self.feed
                .fleet
                .worker(pane)
                .is_none_or(|w| !self.filter.matches(w))
        };
        if self.view.detail.is_some_and(gone) {
            self.view.detail = None;
        } else if moved && dropped.is_none() && self.view.detail.is_some() {
            // With no selection left, the drawer closes; one a live update
            // took out of the scope keeps its drawer while the filter
            // shows it.
            self.view.detail = selected;
        }
        dropped
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

    /// Brings what time changes up to date in place: the rows' times and a
    /// quiet row's qualifiers, the drawer's times. No row moves.
    fn refresh(&mut self, text: &mut TextStore) {
        let (tree, fleet) = (&mut self.tree, &self.feed.fleet);
        for &(id, pulse) in &self.registry.pulses {
            let worker = |pane: u64| fleet.worker(pane).expect("the same structure");
            match pulse {
                Pulse::RowTime(pane) => set_label(tree, id, list::time_text(worker(pane))),
                Pulse::RowQuiet(pane) => set_label(tree, id, list::qualifiers(worker(pane))),
                Pulse::Drawer(pane) => drawer::refresh(tree, id, worker(pane)),
            }
        }
        self.paint_selection();
        if self.view.tab != Tab::Operations {
            // The other tabs draw what time changes (totals, charts, bars,
            // waits) with no widget registered to update in place.
            self.refill_keeping_scroll(text);
        }
    }

    /// `refill_body`, each scroll view in the body scrolled back to where
    /// it was: the refill is the same structure, so they pair in order.
    fn refill_keeping_scroll(&mut self, text: &mut TextStore) {
        let positions: Vec<Point> = scroll_views(&self.tree, self.body)
            .into_iter()
            .filter_map(|id| self.tree.get::<ScrollView>(id))
            .map(ScrollView::scroll_position)
            .collect();
        self.refill_body(text);
        for (id, position) in scroll_views(&self.tree, self.body)
            .into_iter()
            .zip(positions)
        {
            self.tree
                .update::<ScrollView, _>(id, |s, cx| s.set_scroll_position(cx, position));
        }
        self.lay_out_again(text);
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
        let refocus_send = !refocus
            && self
                .registry
                .send_field
                .is_some_and(|(id, _)| self.tree.focused() == Some(id));
        if let Some(caret) = self.tree.get::<TextField>(field).map(TextField::caret) {
            self.filter.caret = caret;
        }
        if let Some((id, pane)) = self.registry.send_field
            && let Some(caret) = self.tree.get::<TextField>(id).map(TextField::caret)
        {
            self.send_caret = Some((pane, caret));
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
        let preview = self.preview.take();
        let drafts = Drafts {
            text: std::mem::take(&mut self.drafts),
            caret: self.send_caret,
            close_ended: self.close_ended.take(),
        };
        let mut built = Self::build(tree, view, feed, filter, refocus, preview, drafts);
        if refocus_send && let Some((id, _)) = built.registry.send_field {
            built.tree.set_focus(Some(id));
        }
        let old = std::mem::replace(self, built);
        self.viewport = old.viewport;
        self.watched = old.watched;
        self.wheel_accum = old.wheel_accum;
        self.pending = old.pending;
        if let Some(now) = old.ticked {
            self.tick(now);
        }
        self.held = old.held;
        self.close_until = old.close_until;
        self.shift_up = old.shift_up;
        self.filter_had_focus = old.filter_had_focus;
        self.pressed_in_filter = old.pressed_in_filter;
        self.prefs_store = old.prefs_store;
        self.repo_browser = old.repo_browser;
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
    fn build(
        mut tree: UiTree,
        view: View,
        feed: Feed,
        filter: Filter,
        refocus: bool,
        preview: Option<(u64, PathBuf, drawer::Preview)>,
        drafts: Drafts,
    ) -> Self {
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
        let registry = Registry::default();
        let header = header::header(&mut tree, root, &feed, &view, &filter);
        let body = tree.insert(
            Some(root),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(GUTTER),
                padding: LayoutRect::length(GUTTER),
                ..fill_remaining()
            }),
        );
        let (notice, tab_control) = notice_line(&mut tree, root, &feed, view.tab);
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
            tab_control,
            preview,
            drafts: drafts.text,
            send_caret: drafts.caret,
            registry,
            viewport: None,
            watched: None,
            wheel_accum: 0.0,
            pending: None,
            ticked: None,
            held: Vec::new(),
            shielded: Vec::new(),
            close_until: None,
            close_ended: drafts.close_ended,
            shift_up: false,
            filter_had_focus: false,
            pressed_in_filter: false,
            prefs_store: None,
            repo_browser: Box::new(repo_browser::System),
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

    /// Builds the body for the view and the filter: the worker list, or
    /// the tab's analysis view.
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
        let nobody = feed.link.as_ref().filter(|_| workers.is_empty());
        // The demo's numbers stand for lines; the live ones count preview
        // updates.
        let output_caption = if feed.link.is_some() {
            "出力の更新（回 / 5 秒）"
        } else {
            "出力（行 / 間隔）"
        };
        match (nobody, view.tab) {
            (Some(link), _) => empty(tree, body, link),
            (None, Tab::Operations) => {
                list::build(tree, body, &feed.fleet, &view, filter, registry)
            }
            (None, Tab::Analytics) => {
                analytics::build(tree, body, &matching, output_caption, filtering, registry);
            }
            (None, Tab::MindMap) => {
                registry.axis_control = Some(mindmap::build(
                    tree,
                    body,
                    &feed.fleet,
                    filter,
                    view.map_axis,
                    registry,
                ));
            }
            (None, Tab::Timeline) => {
                registry.scale_control = Some(gantt::build(
                    tree,
                    body,
                    &matching,
                    view.grouping,
                    view.time_scale,
                    registry,
                ));
            }
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

    /// Selects `pane` (or nothing), in place; an open drawer moves to the
    /// new worker.
    fn select(&mut self, pane: Option<u64>, text: &mut TextStore) {
        if self.view.confirm_close != pane {
            self.view.confirm_close = None;
        }
        self.view.selected = pane;
        self.view.reveal = pane;
        if let Some(pane) = pane
            && self.view.detail.is_some_and(|detail| detail != pane)
        {
            self.view.detail = Some(pane);
            self.reopen_drawer(text);
        }
        self.paint_selection();
        self.watch();
    }

    /// Moves the selection one step through the rows as shown.
    fn step(&mut self, step: Step, text: &mut TextStore) {
        let next = select::step(&self.walk_order(), self.view.selected, step);
        if next.is_some() && next != self.view.selected {
            self.select(next, text);
        }
    }

    /// The existing 開く behaviour, for the buttons and Enter.
    fn open(&mut self, pane: u64) {
        let Some(worker) = self.feed.fleet.worker(pane) else {
            return;
        };
        if worker.is_closed() {
            self.show_notice(drawer::CLOSED_PANE.to_owned());
            return;
        }
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
        if self.preview.as_ref().map(|(p, _, _)| *p) != self.view.result {
            self.preview = None;
        }
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
        // A result gone, or another file since (the worker ran again), is
        // read only when asked for again.
        let same_file =
            |(_, path, _): &(u64, PathBuf, drawer::Preview)| worker.result.as_ref() == Some(path);
        if self
            .preview
            .as_ref()
            .is_some_and(|cached| !same_file(cached))
            || (self.view.result == Some(pane) && worker.result.is_none())
        {
            self.preview = None;
            self.view.result = None;
        }
        if self.preview.is_none()
            && self.view.result == Some(pane)
            && let Some(path) = worker.result.as_deref()
        {
            self.preview = Some((pane, path.to_owned(), drawer::read_result(path)));
        }
        let preview = self.preview.as_ref().map(|(_, _, preview)| preview);
        let draft = self.drafts.get(&pane).map_or("", String::as_str);
        let confirm_close = self.view.confirm_close == Some(pane);
        let ended_workers = self.ended_workers();
        let ended = ended_workers.len();
        // Armed only for the workers it named: for others the next click
        // names them again (`request_close_ended`), and the button says so.
        let armed = self.close_ended.as_ref() == Some(&ended_workers);
        let close_ended = (ended > 0).then_some((ended, armed));
        let buttons = drawer::build(
            &mut self.tree,
            root,
            drawer::DrawerInput {
                worker,
                feed: &self.feed,
                preview,
                draft,
                send_caret: match self.send_caret {
                    Some((of, caret)) if of == pane => caret,
                    _ => draft.len(),
                },
                confirm_close,
                close_ended,
            },
            &mut self.registry,
        );
        self.registry
            .open_buttons
            .extend(buttons.open.map(|id| (id, pane)));
        self.registry.close_pane_button = buttons.close_pane.map(|id| (id, pane));
        self.registry.close_ended_button = buttons.close_ended.map(|id| (id, ended));
        self.registry.diff_button = buttons.diff.map(|id| (id, pane));
        self.registry.drawer = Some((root, buttons.close));
        self.registry.result_toggle = buttons.result;
        self.registry.result_reload = buttons.result_reload;
        self.registry.result_copy = buttons.result_copy;
        self.registry.tail_copy = buttons.tail_copy;
        self.registry.send_field = buttons.send_field.map(|id| (id, pane));
        self.registry.send_button = buttons.send_button.map(|id| (id, pane));
    }

    fn close_drawer(&mut self, text: &mut TextStore) {
        if let Some((root, _)) = self.registry.drawer.take() {
            self.tree.close_overlay(root, text);
        }
        self.registry.open_buttons.clear();
        self.registry.close_pane_button = None;
        self.registry.close_ended_button = None;
        self.registry.diff_button = None;
        self.registry.result_toggle = None;
        self.registry.result_reload = None;
        self.registry.result_copy = None;
        self.registry.tail_copy = None;
        self.registry.send_field = None;
        self.registry.send_button = None;
        self.registry
            .pulses
            .retain(|(_, pulse)| !matches!(pulse, Pulse::Drawer(_)));
    }

    /// Builds the drawer again. Under an open help (a live update or a
    /// message given back while it is open) the help is opened again over
    /// it, so the new drawer lies under the help and is disabled as the
    /// old one was (`open_help`).
    fn reopen_drawer(&mut self, text: &mut TextStore) {
        let help = self.view.help;
        if help {
            self.close_help(text);
        }
        self.close_drawer(text);
        self.open_drawer();
        if help {
            self.view.help = true;
            self.open_help();
        }
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
                // 5, not more: every row then fits the smallest window
                // (1100 x 700) with the ⇧X row too.
                gap: gap(5.0),
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

    /// Asks the feed for the screen tail of the drawer's worker, when it
    /// changed.
    fn watch(&mut self) {
        let Some(channel) = &self.feed.channel else {
            return;
        };
        // A closed worker has no pane to fetch a screen from.
        let panes: Vec<u64> = self
            .view
            .detail
            .filter(|pane| !self.feed.fleet.worker(*pane).is_some_and(Worker::is_closed))
            .into_iter()
            .collect();
        if self.watched.as_ref() != Some(&panes) {
            channel.send(Command::Watch(panes.clone()));
            self.watched = Some(panes);
        }
    }

    /// A changed query (and caret): the body is built again, from the top
    /// of the list, the header and its field stay.
    fn set_filter(&mut self, query: String, caret: usize, text: &mut TextStore) {
        let old = self.order();
        let detail = self.view.detail;
        self.filter = Filter { query, caret };
        self.view.anchor = None;
        self.view.first = 0;
        self.reselect(&old, Vanish::Neighbour);
        // Other workers to close at once: a close armed for the old ones is
        // taken back, and the drawer's button names the new count.
        let ended = self.ended_workers();
        let disarmed = self.close_ended.take_if(|armed| *armed != ended).is_some();
        let stale = self
            .registry
            .close_ended_button
            .map_or(!ended.is_empty(), |(_, count)| count != ended.len());
        if self.view.detail != detail || (self.view.detail.is_some() && (disarmed || stale)) {
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

    /// A view change that rebuilds the screen (scope, tab, grouping).
    fn change_view(&mut self, view: View, text: &mut TextStore) {
        let old = self.order();
        self.view = view;
        self.reselect(&old, Vanish::Neighbour);
        self.rebuilt(self.view, text);
    }

    /// Shows the list of `scope`, from its top (keys 1–3, the switch).
    fn set_scope(&mut self, scope: Scope, text: &mut TextStore) {
        let view = View {
            scope,
            first: 0,
            anchor: None,
            ..self.view
        };
        self.change_view(view, text);
    }

    fn mode(&self) -> Mode {
        if self.view.help {
            Mode::Help
        } else if self.tree.focused() == Some(self.header.filter_field) {
            Mode::Filter
        } else if self
            .registry
            .send_field
            .is_some_and(|(id, _)| self.tree.focused() == Some(id))
        {
            Mode::Send
        } else {
            Mode::Browse
        }
    }

    /// Whether an IME composition is in progress in the filter or send field.
    fn composing(&self) -> bool {
        let in_filter = self
            .tree
            .get::<TextField>(self.header.filter_field)
            .is_some_and(|f| !f.composition().is_empty());
        let in_send = self.registry.send_field.is_some_and(|(id, _)| {
            self.tree
                .get::<TextField>(id)
                .is_some_and(|f| !f.composition().is_empty())
        });
        in_filter || in_send
    }

    /// Sends the current draft to `pane`. Refused here, the draft stays; a
    /// message the feed then does not deliver comes back as the draft
    /// (`Channel::unsent`, `take_unsent`).
    fn submit_send(&mut self, pane: u64, text: &mut TextStore) {
        let Some(worker) = self.feed.fleet.worker(pane) else {
            return;
        };
        let label = worker.label.clone();
        if worker.is_closed() {
            self.show_notice(drawer::CLOSED_SEND_GUARD.to_owned());
            return;
        }
        if worker.state == State::Permission {
            self.show_notice(drawer::PERMISSION_SEND_GUARD.to_owned());
            return;
        }
        let message = self
            .drafts
            .get(&pane)
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        if message.is_empty() {
            self.show_notice("送信するメッセージを入力してください".to_owned());
            return;
        }
        match (&self.feed.channel, &self.feed.instance) {
            (Some(_), _) if self.feed.link != Some(Link::Live) => {
                self.show_notice("送信できません: Noa に接続していません".to_owned());
                return;
            }
            (Some(_), _) if !self.feed.can_input => {
                self.show_notice(format!("送信できません: {}", drawer::NEEDS_INPUT));
                return;
            }
            (Some(_), _) if !self.feed.safe_send => {
                self.show_notice(crate::live::UNSAFE_SEND.to_owned());
                return;
            }
            (Some(channel), Some(instance)) => {
                channel.send(Command::Send {
                    instance: instance.clone(),
                    pane,
                    text: message,
                });
            }
            // A live link always names its instance (`Snapshot::instance`).
            (Some(_), None) | (None, _) => {}
        }
        self.drafts.remove(&pane);
        self.send_caret = None;
        if let Some((field, field_pane)) = self.registry.send_field
            && field_pane == pane
        {
            self.tree.update::<TextField, _>(field, |f, cx| {
                f.set_text(cx, "");
                f.select(cx, 0..0);
            });
        }
        self.tree.set_focus(None);
        self.show_notice(format!("{label} へ送信しました"));
        self.lay_out_again(text);
    }

    /// Two-step confirmation to close `pane` safely.
    fn request_close_pane(&mut self, pane: u64, text: &mut TextStore) {
        let Some(worker) = self.feed.fleet.worker(pane) else {
            return;
        };
        if worker.is_closed() {
            self.show_notice(drawer::CLOSED_PANE.to_owned());
            return;
        }
        let label = worker.label.clone();
        if self.close_ended.take().is_some()
            && self.view.detail.is_some_and(|detail| detail != pane)
        {
            // Its drawer no longer says 本当にまとめて閉じる.
            self.reopen_drawer(text);
        }
        if self.view.confirm_close != Some(pane) {
            self.view.confirm_close = Some(pane);
            self.close_until = Some(Instant::now() + NOTICE_FOR);
            if self.view.detail == Some(pane) {
                self.reopen_drawer(text);
            }
            self.show_notice(format!(
                "{label} のペインを閉じますか？ もう一度 X または「本当に閉じる」で実行（Esc でキャンセル）"
            ));
            return;
        }
        self.view.confirm_close = None;
        match &self.feed.channel {
            Some(_) if self.feed.link != Some(Link::Live) => {
                if self.view.detail == Some(pane) {
                    self.reopen_drawer(text);
                }
                self.show_notice("ペインを閉じられません: Noa に接続していません".to_owned());
            }
            Some(_) if !self.feed.can_focus => {
                if self.view.detail == Some(pane) {
                    self.reopen_drawer(text);
                }
                self.show_notice(
                    "ペインを閉じられません: Noa の server-scopes に control がありません（例: server-scopes = read,control）"
                        .to_owned(),
                );
            }
            Some(channel) => {
                if let Some(instance) = &self.feed.instance {
                    channel.send(Command::Close {
                        instance: instance.clone(),
                        pane,
                    });
                }
                if self.view.detail == Some(pane) {
                    self.reopen_drawer(text);
                }
                self.show_notice(format!("{label} のペインを閉じます"));
            }
            None => {
                let old = self.order();
                self.feed.fleet.workers.retain(|w| w.pane != pane);
                self.drafts.remove(&pane);
                self.reselect(&old, Vanish::Neighbour);
                self.rebuilt(self.view, text);
                self.show_notice(format!("{label} のペインを閉じました"));
            }
        }
    }

    /// The workers ⇧X closes: those the filter shows that have ended
    /// (finished or failed), in the fleet's order. One running or waiting
    /// on a person is never among them.
    fn ended_workers(&self) -> Vec<u64> {
        self.feed
            .fleet
            .workers
            .iter()
            .filter(|w| w.can_close_unseen() && self.filter.matches(w))
            .map(|w| w.pane)
            .collect()
    }

    /// Two-step confirmation to close every ended worker the filter shows
    /// (`ended_workers`). The first press names them; the second closes
    /// them when they are still the same, and else names the new ones.
    ///
    /// `pressed_again`: whether this is a second press, not ⇧X held down
    /// (`shift_up`); a click always is.
    fn request_close_ended(&mut self, pressed_again: bool, text: &mut TextStore) {
        let ended = self.ended_workers();
        let single = self.view.confirm_close.take();
        if ended.is_empty() {
            self.close_ended = None;
            self.close_until = None;
            if single.is_some() && single == self.view.detail {
                self.reopen_drawer(text);
            }
            self.show_notice("まとめて閉じる完了・失敗のワーカーはありません".to_owned());
            return;
        }
        if self.close_ended.as_ref() == Some(&ended) && !pressed_again {
            // The key repeating while held: still the first press.
            return;
        }
        if self.close_ended.as_ref() != Some(&ended) {
            let names = bulk_names(&self.feed.fleet, &ended);
            let count = ended.len();
            self.close_ended = Some(ended);
            self.shift_up = false;
            self.close_until = Some(Instant::now() + NOTICE_FOR);
            if self.view.detail.is_some() {
                self.reopen_drawer(text);
            }
            self.show_notice(format!(
                "完了・失敗の {count} 件を閉じますか？ {names} — もう一度 ⇧X または「本当にまとめて閉じる」で実行（⇧X は Shift を離してから。Esc でキャンセル）"
            ));
            return;
        }
        self.close_ended = None;
        self.close_until = None;
        let count = ended.len();
        match &self.feed.channel {
            Some(_) if self.feed.link != Some(Link::Live) => {
                self.show_notice("ペインを閉じられません: Noa に接続していません".to_owned());
            }
            Some(_) if !self.feed.can_focus => {
                self.show_notice(
                    "ペインを閉じられません: Noa の server-scopes に control がありません（例: server-scopes = read,control）"
                        .to_owned(),
                );
            }
            Some(channel) => {
                // Each as shown now: the feed leaves one that changed since.
                let targets = ended
                    .iter()
                    .filter_map(|pane| self.feed.fleet.worker(*pane))
                    .map(|worker| CloseTarget {
                        pane: worker.pane,
                        state: worker.state,
                        seq: worker.reports,
                    })
                    .collect();
                if let Some(instance) = &self.feed.instance {
                    channel.send(Command::CloseMany {
                        instance: instance.clone(),
                        targets,
                    });
                }
                self.show_notice(format!("{count} 件のペインを閉じます"));
            }
            None => {
                let old = self.order();
                self.feed.fleet.workers.retain(|w| !ended.contains(&w.pane));
                for pane in &ended {
                    self.drafts.remove(pane);
                }
                self.reselect(&old, Vanish::Neighbour);
                self.rebuilt(self.view, text);
                self.show_notice(format!("{count} 件のペインを閉じました"));
                return;
            }
        }
        if self.view.detail.is_some() {
            self.reopen_drawer(text);
        }
    }

    /// 差分を見る: the Repo Browser on the worktree holding the worker's
    /// cwd, open or closed. The notice says it started, or why not.
    fn view_diff(&mut self, pane: u64) {
        let Some(worker) = self.feed.fleet.worker(pane) else {
            return;
        };
        let label = worker.label.clone();
        if worker.cwd.is_empty() {
            self.show_notice(format!("{label} の作業ディレクトリがわかりません"));
            return;
        }
        let cwd = worker.cwd.clone();
        let notice = match self.repo_browser.launch(&cwd) {
            Ok(()) => format!("{label} の作業ツリーを Repo Browser で開きます"),
            Err(why) => why,
        };
        self.show_notice(notice);
    }

    /// Starts no other program from now on: the demo and a snapshot show
    /// what the app does without doing it. `notice` is said instead.
    fn offline(mut self, notice: &'static str) -> Self {
        self.repo_browser = Box::new(repo_browser::Offline(notice));
        self
    }

    /// Lets a test start a Repo Browser of its own.
    #[cfg(test)]
    fn set_launcher(&mut self, launcher: impl Launcher + 'static) {
        self.repo_browser = Box::new(launcher);
    }

    fn copy_screen_tail(&mut self) {
        let Some(pane) = self.view.detail else {
            return;
        };
        let Some(worker) = self.feed.fleet.worker(pane) else {
            return;
        };
        if worker.screen_tail.is_empty() {
            return;
        }
        let lines = worker.screen_tail.len();
        let joined = worker.screen_tail.join("\n");
        self.tree.clipboard().write_text(&joined);
        self.show_notice(format!("画面の末尾をコピーしました（{lines} 行）"));
    }

    fn copy_result(&mut self) {
        let Some((_, _, Ok((body, _)))) = &self.preview else {
            return;
        };
        let body = body.clone();
        self.tree.clipboard().write_text(&body);
        self.show_notice("結果をコピーしました".to_owned());
    }

    /// Handles a direct click on a list row (not its button), a timeline
    /// lane or a マップ (mindmap) node without changing any widget layout.
    fn try_direct_click(&mut self, pos: Point, text: &mut TextStore) -> bool {
        if self.view.help {
            return false;
        }
        let Some(hit) = self.tree.hit_test(pos) else {
            return false;
        };
        // Ignore clicks inside an open drawer overlay.
        if let Some((drawer_root, _)) = self.registry.drawer
            && self.tree.ancestors(hit).any(|id| id == drawer_root)
        {
            return false;
        }
        let under = |id: WidgetId| self.tree.ancestors(hit).any(|anc| anc == id);
        // A row's button acts as a button (`click`).
        if self
            .registry
            .row_actions
            .iter()
            .any(|(id, _, _)| under(*id))
        {
            return false;
        }
        let row = self.registry.rows.iter().find(|(id, _)| under(*id));
        let clicked_pane = row.map(|(_, pane)| *pane).or_else(|| {
            self.registry.marks.iter().find_map(|&(id, pane, mark)| {
                matches!(mark, Mark::Lane | Mark::Node { .. })
                    .then(|| under(id).then_some(pane))
                    .flatten()
            })
        });
        if let Some(pane) = clicked_pane {
            // Focus goes as on a press on space that takes none (the tree
            // clears it there, in `UiTree::dispatch`, which this press never
            // reaches): out of the filter or send field, so the next key
            // acts on the selected row. Cleared before selecting, so nothing
            // rebuilt on the way puts it back.
            self.tree.set_focus(None);
            self.filter_had_focus = false;
            self.pressed_in_filter = false;
            self.disarm_close(text);
            if self.view.tab == Tab::Operations {
                self.select(Some(pane), text);
            } else {
                self.set_detail(Some(pane), text);
            }
            return true;
        }
        false
    }

    /// Takes back a close that waits for its second press: the next action
    /// is not that press, or its notice ended. The drawer's button says
    /// ペインを閉じる again.
    fn disarm_close(&mut self, text: &mut TextStore) {
        self.close_until = None;
        let bulk = self.close_ended.take().is_some();
        let single = self.view.confirm_close.take();
        if self.view.detail.is_some() && (bulk || single == self.view.detail) {
            self.reopen_drawer(text);
        }
    }

    fn act(&mut self, action: Action, text: &mut TextStore) {
        // Only the second X confirms a close (Esc says it was cancelled).
        if !matches!(
            action,
            Action::ClosePane | Action::CloseEnded | Action::Back
        ) {
            self.disarm_close(text);
        }
        match action {
            Action::Next => self.step(Step::Next, text),
            Action::Prev => self.step(Step::Prev, text),
            Action::NextWaiting => {
                let order = self.walk_order();
                match select::next_waiting(&self.feed.fleet, &order, self.view.selected) {
                    Some(pane) => self.select(Some(pane), text),
                    None => self.show_notice("要対応のワーカーはありません".to_owned()),
                }
            }
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
            Action::StartSend => match self.view.selected {
                Some(pane) => self.start_send(pane, text),
                None => self.show_notice(NOTHING_SELECTED.to_owned()),
            },
            Action::SubmitSend => {
                if let Some((_, pane)) = self.registry.send_field {
                    self.submit_send(pane, text);
                }
            }
            Action::CancelSend => {
                self.tree.set_focus(None);
            }
            Action::ClosePane => match self.view.selected {
                Some(pane) => self.request_close_pane(pane, text),
                None => self.show_notice(NOTHING_SELECTED.to_owned()),
            },
            Action::CloseEnded => self.request_close_ended(self.shift_up, text),
            Action::ViewDiff => match self.view.selected {
                Some(pane) => self.view_diff(pane),
                None => self.show_notice(NOTHING_SELECTED.to_owned()),
            },
            Action::CycleGrouping => {
                let current = Grouping::ALL
                    .iter()
                    .position(|g| *g == self.view.grouping)
                    .unwrap_or(0);
                let next = Grouping::ALL[(current + 1) % Grouping::ALL.len()];
                let view = View {
                    grouping: next,
                    ..self.view
                };
                self.change_view(view, text);
                self.show_notice(format!("グループ: {}", next.title()));
            }
            Action::NextTab => {
                let current = Tab::ALL
                    .iter()
                    .position(|t| *t == self.view.tab)
                    .unwrap_or(0);
                let next = Tab::ALL[(current + 1) % Tab::ALL.len()];
                let view = View {
                    tab: next,
                    ..self.view
                };
                self.change_view(view, text);
                self.show_notice(format!("表示: {}", next.title()));
            }
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
                if self.close_ended.take().is_some() {
                    self.close_until = None;
                    if self.view.detail.is_some() {
                        self.reopen_drawer(text);
                    }
                    self.show_notice("まとめて閉じるのをキャンセルしました".to_owned());
                } else if let Some(pane) = self.view.confirm_close.take() {
                    if self.view.detail == Some(pane) {
                        self.reopen_drawer(text);
                    }
                    self.show_notice("ペイン終了をキャンセルしました".to_owned());
                } else if self.view.detail.is_some() {
                    self.set_detail(None, text);
                } else if self.filter.active() {
                    self.clear_filter(text);
                } else if self.view.selected.is_some() {
                    self.select(None, text);
                }
            }
            Action::SetScope(scope) => self.set_scope(scope, text),
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

    /// Opens the drawer on `pane` (selecting it) with its send field
    /// focused, the caret at the draft's end: S, and an input row's 入力する.
    /// Refused for a closed worker and an approval, as sending is.
    fn start_send(&mut self, pane: u64, text: &mut TextStore) {
        let worker = self.feed.fleet.worker(pane);
        if worker.is_some_and(Worker::is_closed) {
            self.show_notice(drawer::CLOSED_SEND_GUARD.to_owned());
            return;
        }
        if worker.is_some_and(|w| w.state == State::Permission) {
            self.show_notice(drawer::PERMISSION_SEND_GUARD.to_owned());
            return;
        }
        if self.view.detail != Some(pane) {
            self.set_detail(Some(pane), text);
        } else if self.view.selected != Some(pane) {
            // The drawer stayed on it while a live update took it out of
            // the list (`reselect`): it is selected again all the same.
            self.select(Some(pane), text);
        }
        if let Some((field, _)) = self.registry.send_field {
            self.tree.set_focus(Some(field));
            self.tree.update::<TextField, _>(field, |f, cx| {
                let end = f.text().len();
                f.select(cx, end..end);
            });
            let end = self.drafts.get(&pane).map_or(0, String::len);
            self.send_caret = Some((pane, end));
        }
    }

    /// Takes a wheel event over the list to page it (the pager).
    fn wheel(&mut self, event: &Event, text: &mut TextStore) -> bool {
        let taken = pager::wheel(
            &self.tree,
            self.registry.list.as_ref(),
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
    /// switches the scope.
    fn dispatch(&mut self, event: Event, text: &mut TextStore) {
        let Some(event) = self.track_press(event) else {
            return;
        };
        if let Event::ModifiersChanged(modifiers) = &event
            && !modifiers.shift
        {
            self.shift_up = true;
        }
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
                let typing_on = matches!(action, Action::StartFilter | Action::StartSend)
                    || (mode == Mode::Filter && matches!(action, Action::Next | Action::Prev));
                if !typing_on {
                    self.tree.set_focus(None);
                }
            }
            Route::Swallow => {}
            Route::Tree => {
                if let Event::PointerDown {
                    pos,
                    button: PointerButton::Primary,
                } = event
                    && self.try_direct_click(pos, text)
                {
                    self.after_input();
                    return;
                }
                let event = if matches!(mode, Mode::Filter | Mode::Send) {
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
            self.registry.list.as_ref(),
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

    /// Every view change comes from input: the view is saved after each.
    fn after_input(&mut self) {
        self.save_prefs();
    }

    fn close_requested(&mut self) {
        self.save_prefs();
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
        if let Some(TextChanged(value)) = message.downcast_ref::<TextChanged>()
            && let Some((field, pane)) = self.registry.send_field
            && message.source == field
        {
            let caret = self
                .tree
                .get::<TextField>(field)
                .map_or(value.len(), TextField::caret);
            let (value, caret) = match filter::on_one_line(value, caret) {
                Some((line, caret)) => {
                    self.tree.update::<TextField, _>(field, |f, cx| {
                        f.set_text(cx, line.as_str());
                        f.select(cx, caret..caret);
                    });
                    (line, caret)
                }
                None => (value.clone(), caret),
            };
            if value.is_empty() {
                self.drafts.remove(&pane);
            } else {
                self.drafts.insert(pane, value);
            }
            self.send_caret = Some((pane, caret));
            return;
        }
        if let Some(SelectionChanged(index)) = message.downcast_ref::<SelectionChanged>() {
            // A switch is not the confirming press: it takes back a close
            // waiting for one, as keys 1–3 do (`act`). None of the
            // switches is in the drawer, which this relabels.
            self.disarm_close(text);
            if message.source == self.grouping_control {
                let view = View {
                    grouping: Grouping::ALL[*index],
                    ..self.view
                };
                self.change_view(view, text);
                return;
            }
            if self.registry.scope_control == Some(message.source) {
                self.set_scope(Scope::ALL[*index], text);
                return;
            }
            if message.source == self.tab_control {
                let view = View {
                    tab: Tab::ALL[*index],
                    ..self.view
                };
                self.change_view(view, text);
                return;
            }
            if self.registry.axis_control == Some(message.source) {
                let view = View {
                    map_axis: MapAxis::ALL[*index],
                    ..self.view
                };
                self.change_view(view, text);
                return;
            }
            if self.registry.scale_control == Some(message.source) {
                let view = View {
                    time_scale: TimeScale::ALL[*index],
                    ..self.view
                };
                self.change_view(view, text);
                return;
            }
        }
        if message.downcast_ref::<ButtonClicked>().is_none() {
            return;
        }
        let source = message.source;
        // Any other button takes back a close waiting for its confirmation;
        // the drawer's button is relabelled once this click is handled
        // (relabelling first would give the drawer's buttons new ids). A
        // row's button is in the list, which relabelling leaves alone: there
        // the drawer is relabelled first, as a key does (`act`), so what the
        // row's button opens is not built again under it — 入力する's
        // focused send field would lose its focus.
        let in_row = self
            .registry
            .row_actions
            .iter()
            .any(|(id, _, _)| *id == source);
        let confirms = self
            .registry
            .close_pane_button
            .is_some_and(|(id, _)| id == source)
            || self
                .registry
                .close_ended_button
                .is_some_and(|(id, _)| id == source);
        let (disarmed, bulk) = if confirms {
            (None, false)
        } else {
            self.close_until = None;
            (
                self.view.confirm_close.take(),
                self.close_ended.take().is_some(),
            )
        };
        let relabels = |detail: Option<u64>| {
            (disarmed.is_some() && disarmed == detail) || (bulk && detail.is_some())
        };
        if in_row && relabels(self.view.detail) {
            self.reopen_drawer(text);
        }
        self.click(source, text);
        if !in_row && relabels(self.view.detail) {
            self.reopen_drawer(text);
        }
    }
}

impl Dashboard {
    /// What a clicked button does (`Screen::handle`).
    fn click(&mut self, source: WidgetId, text: &mut TextStore) {
        let mut row_actions = self.registry.row_actions.iter();
        let row_action = row_actions.find(|(id, _, _)| *id == source);
        if let Some(&(_, pane, action)) = row_action {
            self.row_action(pane, action, text);
        } else if self.registry.clear_filter == Some(source) {
            self.clear_filter(text);
        } else if let Some(pane) = button_pane(&self.registry.open_buttons, source) {
            self.open(pane);
            if self.view.selected != Some(pane) {
                self.select(Some(pane), text);
            }
        } else if self.registry.result_toggle == Some(source) {
            let shown = self.view.result.is_some() && self.view.result == self.view.detail;
            self.view.result = if shown { None } else { self.view.detail };
            self.preview = None;
            self.reopen_drawer(text);
        } else if self.registry.result_reload == Some(source) {
            self.preview = None;
            self.reopen_drawer(text);
        } else if self.registry.result_copy == Some(source) {
            self.copy_result();
        } else if self.registry.tail_copy == Some(source) {
            self.copy_screen_tail();
        } else if let Some((_, pane)) = self.registry.send_button.filter(|(id, _)| *id == source) {
            self.submit_send(pane, text);
        } else if let Some((_, pane)) = self
            .registry
            .close_pane_button
            .filter(|(id, _)| *id == source)
        {
            self.request_close_pane(pane, text);
        } else if self
            .registry
            .close_ended_button
            .is_some_and(|(id, _)| id == source)
        {
            self.request_close_ended(true, text);
        } else if let Some((_, pane)) = self.registry.diff_button.filter(|(id, _)| *id == source) {
            self.view_diff(pane);
        } else if self
            .registry
            .drawer
            .is_some_and(|(_, close)| close == source)
        {
            self.view.confirm_close = None;
            self.set_detail(None, text);
        } else if self.registry.help.is_some_and(|(_, close)| close == source) {
            self.close_help(text);
        }
    }

    /// What a row's button does (AC8).
    fn row_action(&mut self, pane: u64, action: RowAction, text: &mut TextStore) {
        match action {
            RowAction::OpenPane => {
                self.open(pane);
                if self.view.selected != Some(pane) {
                    self.select(Some(pane), text);
                }
            }
            RowAction::StartSend => self.start_send(pane, text),
            RowAction::Detail => self.set_detail(Some(pane), text),
            RowAction::Result => {
                // As the drawer's 表示 does: the file is read for this view.
                self.view.result = Some(pane);
                self.preview = None;
                self.set_detail(Some(pane), text);
            }
        }
    }
}

#[cfg(test)]
mod tests;
