//! The window: a header (tabs, period, agent, refresh) over the limits tab
//! (one card per agent, the binding-window summary, the limit history) and
//! the statistics tab (KPIs, charts, tables).
//!
//! Data reaches the screen through a [`Feed`]: [`LiveFeed`] over the limits
//! sources, the stats index and the history store (units A and B), or
//! [`DemoFeed`] over `demo.rs`. On every wake the screen drains the limits,
//! offers each fresh source to the history, and asks for a new report when
//! the index, the period or the filter changed.

pub mod charts;
pub mod limits;
pub mod prefs;
pub mod stats;
pub mod style;

use std::path::PathBuf;
use std::sync::Arc;

use noa_gpui_core::style::{FlexDirection, Rect as LayoutRect, Size as LayoutSize, length};
use noa_gpui_core::widgets::{
    Button, ButtonClicked, Container, ControlSize, Label, ScrollView, SegmentMode,
    SegmentedControl, SelectionChanged,
};
use noa_gpui_core::{
    Appearance, Event, Key, Message, Role, Style, Theme, UiTree, Widget, WidgetId,
};
use noa_gpui_platform::Screen;
use noa_gpui_text::TextStore;

use agent_usage::history::{self, History, Series};
use agent_usage::limits::LimitsHandle;
use agent_usage::model::{Origin, SourceState, Unix, UsageSource};
use agent_usage::paths::{self, Roots};
use agent_usage::stats::types::{AgentFilter, Period, StatsReport};
use agent_usage::stats::{self as stats_index, Index, StatsHandle};
use agent_usage::time::{self, FixedTz, SystemTz, Tz};

use prefs::{FILTERS, PERIODS, Prefs, PrefsStore, Tab, cycle};
use style::{GUTTER, fill, full, gap};

/// Seconds between the limit polls.
const POLL_SECS: i64 = 300;
/// Samples further apart than two polls break the history line.
const MAX_GAP: i64 = 2 * POLL_SECS;

/// Where the screen's data comes from.
pub trait Feed {
    /// Ask every limits source now and rescan the logs.
    fn refresh(&mut self);
    /// Limit updates since the last call.
    fn drain(&mut self) -> Vec<UsageSource>;
    /// A new report when the data changed since the last one, or `force`.
    fn report(
        &mut self,
        period: Period,
        filter: AgentFilter,
        now: Unix,
        tz: &dyn Tz,
        force: bool,
    ) -> Option<StatsReport>;
    /// The limit history over `range`, one series per window.
    fn limit_series(&self, range: (Unix, Unix)) -> Vec<Series>;
    /// Offers a source to the history store.
    fn offer(&mut self, source: &UsageSource, now: Unix);
    /// Whether this is the canned demo.
    fn is_demo(&self) -> bool {
        false
    }
}

/// The real sources (units A and B).
pub struct LiveFeed {
    limits: LimitsHandle,
    stats: StatsHandle,
    history: Option<History>,
    last_index: Option<Arc<Index>>,
    last_minute: Unix,
    history_failed: bool,
}

impl LiveFeed {
    pub fn start(roots: Roots, wake: Arc<dyn Fn() + Send + Sync>) -> LiveFeed {
        let history = roots
            .own_dir
            .as_deref()
            .map(|own| History::open(paths::history_path(own)));
        LiveFeed {
            limits: agent_usage::limits::start(roots.clone(), wake.clone()),
            stats: stats_index::start(roots, wake),
            history,
            last_index: None,
            last_minute: 0,
            history_failed: false,
        }
    }
}

impl Feed for LiveFeed {
    fn refresh(&mut self) {
        self.limits.refresh();
        self.stats.rescan();
    }

    fn drain(&mut self) -> Vec<UsageSource> {
        self.limits.drain()
    }

    fn report(
        &mut self,
        period: Period,
        filter: AgentFilter,
        now: Unix,
        tz: &dyn Tz,
        force: bool,
    ) -> Option<StatsReport> {
        let index = self.stats.index();
        let same = self
            .last_index
            .as_ref()
            .is_some_and(|last| Arc::ptr_eq(last, &index));
        // A new minute moves "today" and the periods' ends.
        let minute = now / 60;
        if same && !force && minute == self.last_minute {
            return None;
        }
        self.last_minute = minute;
        let report = stats_index::report(&index, period, filter, now, tz);
        self.last_index = Some(index);
        Some(report)
    }

    fn limit_series(&self, range: (Unix, Unix)) -> Vec<Series> {
        match &self.history {
            Some(h) => history::series(h.samples(range.0), range, MAX_GAP),
            None => Vec::new(),
        }
    }

    fn offer(&mut self, source: &UsageSource, now: Unix) {
        let Some(history) = &mut self.history else {
            return;
        };
        if let Err(e) = history.offer(source, now)
            && !self.history_failed
        {
            // Once: the window works without a history file.
            eprintln!("agent-usage: limit history not written: {e}");
            self.history_failed = true;
        }
    }
}

/// The canned data of `--demo`.
#[derive(Default)]
pub struct DemoFeed {
    drained: bool,
}

impl Feed for DemoFeed {
    fn refresh(&mut self) {
        self.drained = false;
    }

    fn drain(&mut self) -> Vec<UsageSource> {
        if std::mem::replace(&mut self.drained, true) {
            Vec::new()
        } else {
            crate::demo::sources(crate::demo::now())
        }
    }

    fn report(
        &mut self,
        period: Period,
        filter: AgentFilter,
        now: Unix,
        tz: &dyn Tz,
        force: bool,
    ) -> Option<StatsReport> {
        force.then(|| crate::demo::report(period, filter, now, tz))
    }

    fn limit_series(&self, range: (Unix, Unix)) -> Vec<Series> {
        crate::demo::history(crate::demo::now())
            .into_iter()
            .map(|s| Series {
                label: s.label,
                points: s
                    .points
                    .into_iter()
                    .filter(|(t, _)| *t >= range.0 && *t <= range.1)
                    .collect(),
            })
            .collect()
    }

    fn offer(&mut self, _source: &UsageSource, _now: Unix) {}

    fn is_demo(&self) -> bool {
        true
    }
}

/// The time the screen shows: the system's, or the demo's fixed clock.
pub struct Clock {
    fixed: Option<Unix>,
    tz: Box<dyn Tz>,
}

impl Clock {
    pub fn live() -> Clock {
        Clock {
            fixed: None,
            tz: Box::new(SystemTz),
        }
    }

    pub fn fixed(now: Unix, tz: FixedTz) -> Clock {
        Clock {
            fixed: Some(now),
            tz: Box::new(tz),
        }
    }

    pub fn now(&self) -> Unix {
        self.fixed.unwrap_or_else(time::now)
    }

    pub fn tz(&self) -> &dyn Tz {
        self.tz.as_ref()
    }
}

pub const PERIOD_LABELS: [&str; 4] = ["今日", "7日", "30日", "全期間"];
pub const FILTER_LABELS: [&str; 4] = ["全体", "Claude", "Codex", "agy"];

pub struct AgentUsage {
    tree: UiTree,
    feed: Box<dyn Feed>,
    clock: Clock,
    prefs: Prefs,
    store: Option<PrefsStore>,
    /// The latest of each source, in `Agent::ALL` order; `None` until its first
    /// answer, which the card shows as being fetched.
    sources: [Option<UsageSource>; 3],
    report: StatsReport,
    tabs: WidgetId,
    period: WidgetId,
    agent: WidgetId,
    refresh: WidgetId,
    status: WidgetId,
    limits_host: WidgetId,
    stats_host: WidgetId,
    /// The minute the shown tab was built for (ages and "today" move).
    built_minute: Option<Unix>,
    /// Each source's state, aged, when the tab was built: a value going stale
    /// (its age or a reset) rebuilds the cards within the minute.
    built_states: Vec<Option<SourceState>>,
    /// The statistics tables' sideways scroll views, kept across rebuilds.
    stats_scrolls: stats::Scrolls,
}

impl AgentUsage {
    /// `store` is where the view is saved (`None`: never read or written).
    pub fn new(feed: Box<dyn Feed>, clock: Clock, prefs: Prefs, store: Option<PrefsStore>) -> Self {
        let mut tree = UiTree::new();
        crate::appearance::install(&mut tree, Theme::for_appearance(Appearance::Dark));
        let root = tree.insert(
            None,
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                size: full(),
                ..Default::default()
            })
            .background(Role::Background),
        );
        let header = tree.insert(
            Some(root),
            Container::new(Style {
                padding: LayoutRect {
                    left: length(GUTTER),
                    right: length(GUTTER),
                    top: length(10.0),
                    bottom: length(10.0),
                },
                ..style::row(14.0)
            })
            .background(Role::Surface),
        );
        tree.insert(
            Some(header),
            Label::new("エージェント使用量")
                .font_size(crate::appearance::font(15.0))
                .weight(700),
        );
        let tabs = tree.insert(
            Some(header),
            SegmentedControl::new(["制限", "統計"], SegmentMode::SelectOne)
                .with_selected(&[prefs.tab.index()]),
        );
        let period = tree.insert(
            Some(header),
            SegmentedControl::new(PERIOD_LABELS, SegmentMode::SelectOne)
                .control_size(ControlSize::Small)
                .with_selected(&[period_index(prefs.period)]),
        );
        let agent = tree.insert(
            Some(header),
            SegmentedControl::new(FILTER_LABELS, SegmentMode::SelectOne)
                .control_size(ControlSize::Small)
                .with_selected(&[filter_index(prefs.agent)]),
        );
        style::spacer(&mut tree, header);
        let status = tree.insert(
            Some(header),
            Label::new("")
                .font_size(crate::appearance::font(11.0))
                .color(Role::Muted),
        );
        let refresh = tree.insert(Some(header), Button::new("更新 (R)"));
        tree.insert(
            Some(root),
            Container::new(Style {
                size: LayoutSize {
                    width: noa_gpui_core::style::percent(1.0),
                    height: length(1.0),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .background(Role::Separator),
        );
        let body = tree.insert(Some(root), Container::new(fill()));
        let scroll_style = || Style {
            flex_direction: FlexDirection::Column,
            padding: LayoutRect::length(GUTTER),
            gap: gap(GUTTER),
            ..fill()
        };
        let limits_host = tree.insert(Some(body), ScrollView::new(scroll_style()));
        let stats_host = tree.insert(Some(body), ScrollView::new(scroll_style()));

        let now = clock.now();
        let mut screen = AgentUsage {
            tree,
            feed,
            clock,
            prefs,
            store,
            sources: Default::default(),
            report: StatsReport::empty((now, now)),
            tabs,
            period,
            agent,
            refresh,
            status,
            limits_host,
            stats_host,
            built_minute: None,
            built_states: Vec::new(),
            stats_scrolls: Vec::new(),
        };
        // Nothing is laid out yet: any store serves the first build.
        screen.poll(true, &mut TextStore::new(Default::default()));
        screen
    }

    /// The demo window: canned data at the demo's fixed clock.
    pub fn demo(prefs: Prefs, store: Option<PrefsStore>) -> Self {
        AgentUsage::new(
            Box::<DemoFeed>::default(),
            Clock::fixed(crate::demo::now(), crate::demo::tz()),
            prefs,
            store,
        )
    }

    /// Takes in what the feed has, then rebuilds what changed.
    fn poll(&mut self, force_report: bool, text: &mut TextStore) {
        let now = self.clock.now();
        let mut changed = force_report;
        for source in self.feed.drain() {
            if source.state == SourceState::Ok && source.origin == Origin::Live {
                self.feed.offer(&source, now);
            }
            let at = source.agent.index();
            self.sources[at] = Some(source);
            changed = true;
        }
        if let Some(report) = self.feed.report(
            self.prefs.period,
            self.prefs.agent,
            now,
            self.clock.tz(),
            force_report,
        ) {
            self.report = report;
            changed = true;
        }
        let minute = now / 60;
        if changed
            || self.built_minute != Some(minute)
            || self.built_states != self.aged_states(now)
        {
            self.rebuild(text);
        }
    }

    fn aged_states(&self, now: Unix) -> Vec<Option<SourceState>> {
        self.sources
            .iter()
            .map(|s| s.as_ref().map(|s| s.aged_state(now)))
            .collect()
    }

    fn period_range(&self) -> (Unix, Unix) {
        let now = self.clock.now();
        let tz = self.clock.tz();
        let today = time::local(now, tz).date;
        let first = match self.prefs.period {
            Period::Today => today,
            Period::Days7 => time::add_days(today, -6),
            Period::Days30 => time::add_days(today, -29),
            // The history keeps 400 days.
            Period::All => time::add_days(today, -399),
        };
        let start = time::day_start(first, tz);
        // "All": from the first sample on record.
        if self.prefs.period == Period::All {
            let first_sample = self
                .feed
                .limit_series((start, now))
                .iter()
                .filter_map(|s| s.points.first().map(|p| p.0))
                .min();
            if let Some(t) = first_sample {
                return (time::day_start(time::local(t, tz).date, tz), now);
            }
        }
        (start, now)
    }

    /// `text` must be the store the tree was laid out with: removed widgets
    /// release their text layouts in it.
    fn rebuild(&mut self, text: &mut TextStore) {
        let now = self.clock.now();
        self.built_minute = Some(now / 60);
        self.built_states = self.aged_states(now);
        let shown = self.prefs.tab;
        self.tree.set_hidden(self.limits_host, shown != Tab::Limits);
        self.tree.set_hidden(self.stats_host, shown != Tab::Stats);
        let status = self.status_text();
        self.tree
            .update::<Label, _>(self.status, |l, cx| l.set_text(cx, status));
        match shown {
            Tab::Limits => {
                clear(&mut self.tree, self.limits_host, text);
                let range = self.period_range();
                let series: Vec<Series> = self
                    .feed
                    .limit_series(range)
                    .into_iter()
                    .filter(|s| series_matches(&s.label, self.prefs.agent))
                    .collect();
                limits::build(
                    &mut self.tree,
                    self.limits_host,
                    &limits::Input {
                        sources: &self.sources,
                        series: &series,
                        range,
                        period: self.prefs.period,
                        filter: self.prefs.agent,
                        now,
                        tz: self.clock.tz(),
                    },
                );
            }
            Tab::Stats => {
                // The tables are rebuilt every minute: keep where each one
                // was scrolled sideways.
                let saved: Vec<(&str, String)> = self
                    .stats_scrolls
                    .iter()
                    .filter_map(|(name, id)| {
                        let view = self.tree.get::<ScrollView>(*id)?;
                        Some((*name, view.save_state()?))
                    })
                    .collect();
                clear(&mut self.tree, self.stats_host, text);
                self.stats_scrolls = stats::build(
                    &mut self.tree,
                    self.stats_host,
                    &self.report,
                    self.prefs.period,
                    self.prefs.agent,
                );
                for (name, id) in &self.stats_scrolls {
                    if let Some((_, state)) = saved.iter().find(|(n, _)| n == name) {
                        self.tree
                            .update::<ScrollView, _>(*id, |view, cx| view.restore_state(cx, state));
                    }
                }
            }
        }
    }

    fn status_text(&self) -> String {
        let c = &self.report.coverage;
        let files = if c.running || c.files_done < c.files_total {
            format!("集計中 {} / {} ファイル", c.files_done, c.files_total)
        } else {
            format!("ログ {} ファイル", c.files_total)
        };
        if self.feed.is_demo() {
            format!("デモ表示 · {files}")
        } else {
            files
        }
    }

    fn set_prefs(&mut self, prefs: Prefs, text: &mut TextStore) {
        if prefs == self.prefs {
            return;
        }
        let report_changed = prefs.period != self.prefs.period || prefs.agent != self.prefs.agent;
        self.prefs = prefs;
        if let Some(store) = &mut self.store {
            store.save(&prefs);
        }
        let (tab, period, agent) = (
            prefs.tab.index(),
            period_index(prefs.period),
            filter_index(prefs.agent),
        );
        let select = |tree: &mut UiTree, id: WidgetId, i: usize| {
            tree.update::<SegmentedControl, _>(id, |s, cx| {
                if s.selected().get(i) != Some(&true) {
                    s.set_selected(cx, &[i]);
                }
            });
        };
        select(&mut self.tree, self.tabs, tab);
        select(&mut self.tree, self.period, period);
        select(&mut self.tree, self.agent, agent);
        if report_changed {
            self.poll(true, text);
        } else {
            self.rebuild(text);
        }
    }

    pub fn press(&mut self, c: char, text: &mut TextStore) -> bool {
        let p = self.prefs;
        match c {
            '1' => self.set_prefs(
                Prefs {
                    tab: Tab::Limits,
                    ..p
                },
                text,
            ),
            '2' => self.set_prefs(
                Prefs {
                    tab: Tab::Stats,
                    ..p
                },
                text,
            ),
            't' => self.set_prefs(
                Prefs {
                    tab: p.tab.other(),
                    ..p
                },
                text,
            ),
            'p' => self.set_prefs(
                Prefs {
                    period: cycle(&PERIODS, p.period),
                    ..p
                },
                text,
            ),
            'a' => self.set_prefs(
                Prefs {
                    agent: cycle(&FILTERS, p.agent),
                    ..p
                },
                text,
            ),
            'r' => self.refresh_all(text),
            _ => return false,
        }
        true
    }

    fn refresh_all(&mut self, text: &mut TextStore) {
        self.feed.refresh();
        self.poll(true, text);
    }
}

/// Whether a history series (labelled "Claude 5h", "Codex 7d", "agy …")
/// belongs to the agents the filter shows.
fn series_matches(label: &str, filter: AgentFilter) -> bool {
    match filter {
        AgentFilter::All => true,
        AgentFilter::One(agent) => {
            let lower = label.to_ascii_lowercase();
            lower.starts_with(agent.id())
                || lower.starts_with(&agent.display().to_ascii_lowercase())
        }
    }
}

fn period_index(period: Period) -> usize {
    PERIODS.iter().position(|p| *p == period).unwrap_or(0)
}

fn filter_index(filter: AgentFilter) -> usize {
    FILTERS.iter().position(|f| *f == filter).unwrap_or(0)
}

fn clear(tree: &mut UiTree, parent: WidgetId, text: &mut TextStore) {
    for child in tree.children(parent).to_vec() {
        tree.remove(child, text);
    }
}

impl Screen for AgentUsage {
    fn set_system_appearance(&mut self, appearance: noa_gpui_core::Appearance) {
        crate::appearance::system(&mut self.tree, appearance);
    }
    fn tree(&self) -> &UiTree {
        &self.tree
    }

    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }

    fn woken(&mut self, text: &mut TextStore) {
        self.poll(false, text);
    }

    fn dispatch(&mut self, event: Event, text: &mut TextStore) {
        if let Event::KeyDown {
            key: Key::Char(c),
            modifiers,
        } = &event
            && !(modifiers.ctrl || modifiers.alt || modifiers.meta || modifiers.command)
            && self.press(c.to_ascii_lowercase(), text)
        {
            return;
        }
        self.tree.dispatch(event, text);
        for message in self.tree.take_messages() {
            self.handle(message, text);
        }
    }

    fn handle(&mut self, message: Message, text: &mut TextStore) {
        let p = self.prefs;
        if let Some(SelectionChanged(i)) = message.downcast_ref::<SelectionChanged>() {
            let i = *i;
            if message.source == self.tabs {
                let tab = if i == 0 { Tab::Limits } else { Tab::Stats };
                self.set_prefs(Prefs { tab, ..p }, text);
            } else if message.source == self.period
                && let Some(period) = PERIODS.get(i)
            {
                self.set_prefs(
                    Prefs {
                        period: *period,
                        ..p
                    },
                    text,
                );
            } else if message.source == self.agent
                && let Some(agent) = FILTERS.get(i)
            {
                self.set_prefs(Prefs { agent: *agent, ..p }, text);
            }
        } else if message.source == self.refresh
            && message.downcast_ref::<ButtonClicked>().is_some()
        {
            self.refresh_all(text);
        }
    }
}

/// The prefs file: `--prefs`, else `<data dir>/noa/agent-usage/view.json`.
pub fn default_prefs_path() -> Option<PathBuf> {
    Roots::system().own_dir.map(|own| paths::prefs_path(&own))
}

#[cfg(test)]
mod tests;
