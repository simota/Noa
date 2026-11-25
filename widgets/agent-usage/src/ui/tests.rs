use std::cell::RefCell;
use std::rc::Rc;

use noa_gpui_core::widgets::{Label, Tag};
use noa_gpui_core::{Event, Key, Modifiers};
use noa_gpui_scene::Size;

use agent_usage::model::{Agent, format};

use super::charts::{Dot, Gauge};
use super::style::CellText;
use super::*;
use crate::demo;

/// The demo's data, with the sources (and the report) a test wants,
/// recording refreshes.
struct TestFeed {
    sources: Vec<UsageSource>,
    report: Option<StatsReport>,
    pending: bool,
    calls: Rc<RefCell<Vec<&'static str>>>,
}

impl Feed for TestFeed {
    fn refresh(&mut self) {
        self.calls.borrow_mut().extend(["limits", "stats"]);
        self.pending = true;
    }

    fn drain(&mut self) -> Vec<UsageSource> {
        if std::mem::replace(&mut self.pending, false) {
            self.sources.clone()
        } else {
            Vec::new()
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
        force.then(|| {
            self.report
                .clone()
                .unwrap_or_else(|| demo::report(period, filter, now, tz))
        })
    }

    fn limit_series(&self, _range: (Unix, Unix)) -> Vec<Series> {
        demo::history(demo::now())
    }

    fn offer(&mut self, _source: &UsageSource, _now: Unix) {
        self.calls.borrow_mut().push("offer");
    }
}

fn window() -> Size {
    Size::new(1280.0, 860.0)
}

fn screen_with(
    sources: Vec<UsageSource>,
    prefs: Prefs,
    store: Option<PrefsStore>,
) -> (AgentUsage, Rc<RefCell<Vec<&'static str>>>, TextStore) {
    feed_screen(sources, None, prefs, store)
}

fn feed_screen(
    sources: Vec<UsageSource>,
    report: Option<StatsReport>,
    prefs: Prefs,
    store: Option<PrefsStore>,
) -> (AgentUsage, Rc<RefCell<Vec<&'static str>>>, TextStore) {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let feed = TestFeed {
        sources,
        report,
        pending: true,
        calls: calls.clone(),
    };
    let mut screen = AgentUsage::new(
        Box::new(feed),
        Clock::fixed(demo::now(), demo::tz()),
        prefs,
        store,
    );
    let mut text = TextStore::new(Default::default());
    screen.prepare(window(), &mut text);
    (screen, calls, text)
}

fn demo_screen() -> AgentUsage {
    screen_with(demo::sources(demo::now()), Prefs::default(), None).0
}

fn labels_under(tree: &UiTree, id: WidgetId) -> Vec<String> {
    let own = tree
        .get::<Label>(id)
        .map(|l| l.text().to_owned())
        .or_else(|| tree.get::<CellText>(id).map(|c| c.text().to_owned()))
        .or_else(|| tree.get::<Tag>(id).map(|t| t.text().to_owned()));
    own.into_iter()
        .chain(
            tree.children(id)
                .iter()
                .flat_map(|c| labels_under(tree, *c)),
        )
        .collect()
}

fn gauges_under(tree: &UiTree, id: WidgetId) -> Vec<(f64, Option<f64>, bool)> {
    let own = tree.get::<Gauge>(id).map(|g| (g.used, g.tick, g.dimmed));
    own.into_iter()
        .chain(
            tree.children(id)
                .iter()
                .flat_map(|c| gauges_under(tree, *c)),
        )
        .collect()
}

fn dot_under(tree: &UiTree, id: WidgetId) -> Option<bool> {
    tree.get::<Dot>(id)
        .map(|d| d.filled)
        .or_else(|| tree.children(id).iter().find_map(|c| dot_under(tree, *c)))
}

/// The limits tab's cards, in order (see `limits::build`).
fn cards(screen: &AgentUsage) -> Vec<WidgetId> {
    let row = screen.tree.children(screen.limits_host)[1];
    screen.tree.children(row).to_vec()
}

fn card_of(screen: &AgentUsage, agent: Agent) -> WidgetId {
    cards(screen)[agent.index()]
}

fn summary_of(screen: &AgentUsage) -> String {
    let row = screen.tree.children(screen.limits_host)[0];
    let label = screen.tree.children(row)[1];
    screen.tree.get::<Label>(label).unwrap().text().to_owned()
}

fn card_labels(screen: &AgentUsage, agent: Agent) -> Vec<String> {
    labels_under(&screen.tree, card_of(screen, agent))
}

fn has(labels: &[String], text: &str) -> bool {
    labels.iter().any(|l| l == text)
}

fn key(screen: &mut AgentUsage, text: &mut TextStore, c: char) {
    screen.dispatch(
        Event::KeyDown {
            key: Key::Char(c),
            modifiers: Modifiers::default(),
        },
        text,
    );
    screen.prepare(window(), text);
}

#[test]
fn limits_view_shows_every_window_with_reset_and_percent() {
    let screen = demo_screen();
    let now = demo::now();
    let tz = demo::tz();
    assert_eq!(cards(&screen).len(), 3);
    for agent in Agent::ALL {
        // Fixed order: each card's first label is its agent's name.
        let labels = card_labels(&screen, agent);
        assert_eq!(labels[0], agent.display(), "fixed card order");
    }
    for source in demo::sources(now) {
        let labels = card_labels(&screen, source.agent);
        assert!(has(&labels, source.agent.display()), "{labels:?}");
        for w in &source.windows {
            assert!(has(&labels, &w.label), "{} in {labels:?}", w.label);
            assert!(has(&labels, &format::percent(w.used_percent)), "{labels:?}");
            assert!(
                has(&labels, &format::resets_in(w.resets_at, now, &tz)),
                "{labels:?}"
            );
        }
        for b in &source.buckets {
            assert!(has(&labels, &b.label), "{labels:?}");
        }
        if let Some(spend) = &source.spend {
            assert!(has(&labels, &spend.label) && has(&labels, &spend.detail));
        }
    }
    let claude = card_labels(&screen, Agent::Claude);
    for text in [
        "Max",
        "5h",
        "7d",
        "34%",
        "in 3h08m",
        "Sonnet 7d",
        "22%",
        "40秒前に取得",
    ] {
        assert!(has(&claude, text), "{text} in {claude:?}");
    }
    // Rows in order: 5h, 7d, the scoped bucket under 7d, then the spend.
    let pos = |t: &str| claude.iter().position(|l| l == t).unwrap();
    assert!(pos("5h") < pos("7d") && pos("7d") < pos("Sonnet 7d"));
    assert!(pos("Sonnet 7d") < pos("Extra usage"));
    let codex = card_labels(&screen, Agent::Codex);
    assert!(has(&codex, "in 48m") && has(&codex, "Pro") && has(&codex, "Credits balance: 42"));
    // The account e-mail is never shown: no label holds an "@".
    let all = labels_under(&screen.tree, screen.tree.root().unwrap());
    assert!(all.iter().all(|l| !l.contains('@')));
}

#[test]
fn pace_tick_and_over_pace_line_appear_when_ahead() {
    let screen = demo_screen();
    let now = demo::now();
    let sources = demo::sources(now);
    let codex = &sources[1].windows[1];
    let gauges = gauges_under(&screen.tree, card_of(&screen, Agent::Codex));
    // 5h and 7d both have a length and a reset: both carry the tick.
    assert_eq!(gauges.len(), 2);
    let tick = gauges[1].1.unwrap();
    assert!((tick - codex.elapsed_fraction(now).unwrap()).abs() < 1e-9);
    assert!((tick - 0.4).abs() < 0.01);
    let labels = card_labels(&screen, Agent::Codex);
    let line = labels
        .iter()
        .find(|l| l.contains("over pace"))
        .expect("over-pace line");
    assert!(line.starts_with("+16% over pace · maxes out "), "{line}");
    assert!(line.ends_with(" at this pace"), "{line}");
    // 5h is under pace: one over-pace line only.
    assert_eq!(labels.iter().filter(|l| l.contains("over pace")).count(), 1);
    // The scoped bucket and the spend carry no tick.
    let claude = gauges_under(&screen.tree, card_of(&screen, Agent::Claude));
    assert_eq!(claude.len(), 4);
    assert!(claude[0].1.is_some() && claude[1].1.is_some());
    assert!(claude[2].1.is_none() && claude[3].1.is_none());
    // A window without its length has no tick.
    let mut s = demo::sources(now);
    s[1].windows[1].window_minutes = None;
    let (screen, _, _) = screen_with(s, Prefs::default(), None);
    let gauges = gauges_under(&screen.tree, card_of(&screen, Agent::Codex));
    assert!(gauges[1].1.is_none());
    assert!(
        !card_labels(&screen, Agent::Codex)
            .iter()
            .any(|l| l.contains("over pace"))
    );
}

#[test]
fn stale_card_is_dimmed_with_age_and_reason() {
    let screen = demo_screen();
    let labels = card_labels(&screen, Agent::Agy);
    assert!(has(&labels, "古い値 · 12分前"), "{labels:?}");
    assert!(
        labels.iter().any(|l| l.contains("agy timed out after 12s")),
        "{labels:?}"
    );
    let gauges = gauges_under(&screen.tree, card_of(&screen, Agent::Agy));
    assert_eq!(gauges.len(), 3);
    assert!(gauges.iter().all(|g| g.2), "every gauge dimmed");
    assert_eq!(
        dot_under(&screen.tree, card_of(&screen, Agent::Agy)),
        Some(false)
    );
    // The current cards are not dimmed and have a filled mark.
    let claude = gauges_under(&screen.tree, card_of(&screen, Agent::Claude));
    assert!(claude.iter().all(|g| !g.2));
    assert_eq!(
        dot_under(&screen.tree, card_of(&screen, Agent::Claude)),
        Some(true)
    );
    // A value older than `stale_after` reads as stale too.
    let now = demo::now();
    let mut s = demo::sources(now);
    s[0].last_updated = Some(now - 1000);
    s[0].notes = vec!["No connection".into()];
    let (screen, _, _) = screen_with(s, Prefs::default(), None);
    let labels = card_labels(&screen, Agent::Claude);
    assert!(
        has(&labels, "古い値 · 16分前") && has(&labels, "No connection"),
        "{labels:?}"
    );
    // Values from the log fallback say so.
    let mut s = demo::sources(now);
    s[1].origin = Origin::Log;
    s[1].last_updated = Some(now - 3 * 3600);
    s[1].state = SourceState::Ok;
    let (screen, _, _) = screen_with(s, Prefs::default(), None);
    assert!(has(
        &card_labels(&screen, Agent::Codex),
        "ログから · 3時間前"
    ));
}

#[test]
fn failed_source_does_not_hide_others() {
    let now = demo::now();
    let mut s = demo::sources(now);
    s[1] = UsageSource {
        state: SourceState::Error("codex not found in PATH".into()),
        ..UsageSource::placeholder(Agent::Codex)
    };
    let (screen, _, _) = screen_with(s, Prefs::default(), None);
    let codex = card_labels(&screen, Agent::Codex);
    assert!(has(&codex, "codex not found in PATH"), "{codex:?}");
    assert!(gauges_under(&screen.tree, card_of(&screen, Agent::Codex)).is_empty());
    assert_eq!(
        gauges_under(&screen.tree, card_of(&screen, Agent::Claude)).len(),
        4
    );
    assert_eq!(
        gauges_under(&screen.tree, card_of(&screen, Agent::Agy)).len(),
        3
    );
    assert!(has(&card_labels(&screen, Agent::Claude), "34%"));
    // Unconfigured: "未設定" and how to set it up.
    let mut s = demo::sources(now);
    s[2] = UsageSource::placeholder(Agent::Agy);
    let (screen, _, _) = screen_with(s, Prefs::default(), None);
    let agy = card_labels(&screen, Agent::Agy);
    assert!(
        has(&agy, "未設定") && agy.iter().any(|l| l.contains("agy")),
        "{agy:?}"
    );
}

#[test]
fn summary_names_the_binding_window() {
    let now = demo::now();
    let tz = demo::tz();
    let screen = demo_screen();
    let summary = summary_of(&screen);
    let codex = &demo::sources(now)[1].windows[1];
    let expected = format!(
        "Codex 7d 56% · {}",
        format::exhaustion(codex, now, &tz).unwrap()
    );
    assert_eq!(summary, expected);
    assert!(summary.starts_with("Codex 7d 56% · maxes out "));
    // Nothing on course to run out.
    let mut s = demo::sources(now);
    for src in &mut s {
        for w in &mut src.windows {
            w.used_percent = 1.0;
        }
    }
    assert_eq!(
        limits::summary_text(&s, now, &tz),
        "No limit is on course to run out"
    );
    // Nothing current.
    let stale: Vec<UsageSource> = demo::sources(now)
        .into_iter()
        .map(|mut src| {
            src.last_updated = Some(now - 3600);
            src
        })
        .collect();
    assert_eq!(limits::summary_text(&stale, now, &tz), "現在値なし");
}

#[test]
fn stats_view_shows_dash_for_unavailable_values() {
    let now = demo::now();
    let tz = demo::tz();
    let all = demo::report(Period::Days7, AgentFilter::All, now, &tz);
    let rows = stats::agent_rows(&all);
    let agy = rows.iter().find(|r| r[0] == "Antigravity").unwrap();
    // Sessions, turns and active time are known; messages, tools, tokens
    // and cost are not.
    assert_ne!(agy[1], stats::DASH);
    assert_ne!(agy[2], stats::DASH);
    for i in [3, 4, 6, 7, 8, 9, 10, 11, 12] {
        assert_eq!(agy[i], stats::DASH, "column {i}");
    }
    let codex = rows.iter().find(|r| r[0] == "Codex").unwrap();
    // Codex has no prices: a dash, and the tokens that leaves out.
    assert!(
        codex[12].starts_with(&format!("{} · 未計上 (", stats::DASH)),
        "{}",
        codex[12]
    );
    assert_ne!(codex[6], stats::DASH);
    let claude = rows.iter().find(|r| r[0] == "Claude Code").unwrap();
    assert!(claude[12].starts_with('$'));
    // The total is priced in part: the cost says what is left out.
    let kpis = stats::kpis(&all);
    let cost = kpis.iter().find(|k| k.0 == "推定コスト").unwrap();
    assert!(
        cost.1.starts_with('$') && cost.2.contains("一部未計上"),
        "{cost:?}"
    );
    // agy alone: no tokens, no messages, no tool calls, no cost.
    let only = demo::report(Period::Days7, AgentFilter::One(Agent::Agy), now, &tz);
    let kpis = stats::kpis(&only);
    for title in ["メッセージ", "ツール呼び出し", "総トークン", "推定コスト"]
    {
        let k = kpis.iter().find(|k| k.0 == title).unwrap();
        assert_eq!(k.1, stats::DASH, "{title}");
    }
    assert_ne!(
        kpis.iter().find(|k| k.0 == "ターン").unwrap().1,
        stats::DASH
    );
    // On screen.
    let (mut screen, _, mut text) = screen_with(
        demo::sources(now),
        Prefs {
            tab: Tab::Stats,
            agent: AgentFilter::One(Agent::Agy),
            ..Prefs::default()
        },
        None,
    );
    screen.prepare(window(), &mut text);
    let labels = labels_under(&screen.tree, screen.stats_host);
    assert!(labels.iter().filter(|l| *l == stats::DASH).count() >= 4);
    assert!(has(&labels, "agy はトークンを記録しません"));
}

#[test]
fn period_and_agent_keys_cycle_and_persist() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("view.json");
    let (mut screen, _, mut text) = screen_with(
        demo::sources(demo::now()),
        Prefs::default(),
        Some(PrefsStore::at(path.clone())),
    );
    assert_eq!(screen.prefs.period, Period::Days7);
    let periods: Vec<Period> = (0..4)
        .map(|_| {
            key(&mut screen, &mut text, 'p');
            screen.prefs.period
        })
        .collect();
    assert_eq!(
        periods,
        vec![Period::Days30, Period::All, Period::Today, Period::Days7]
    );
    let agents: Vec<AgentFilter> = (0..4)
        .map(|_| {
            key(&mut screen, &mut text, 'a');
            screen.prefs.agent
        })
        .collect();
    assert_eq!(
        agents,
        vec![
            AgentFilter::One(Agent::Claude),
            AgentFilter::One(Agent::Codex),
            AgentFilter::One(Agent::Agy),
            AgentFilter::All
        ]
    );
    key(&mut screen, &mut text, 'p');
    key(&mut screen, &mut text, 'a');
    key(&mut screen, &mut text, '2');
    assert_eq!(screen.prefs.tab, Tab::Stats);
    assert!(screen.tree.get::<ScrollView>(screen.stats_host).is_some());
    let saved = PrefsStore::at(path.clone()).load();
    assert_eq!(
        saved,
        Prefs {
            tab: Tab::Stats,
            period: Period::Days30,
            agent: AgentFilter::One(Agent::Claude),
        }
    );
    // The segmented controls follow the keys.
    let selected = |id| {
        screen
            .tree
            .get::<SegmentedControl>(id)
            .unwrap()
            .selected()
            .to_vec()
    };
    assert_eq!(selected(screen.period), vec![false, false, true, false]);
    assert_eq!(selected(screen.agent), vec![false, true, false, false]);
    assert_eq!(selected(screen.tabs), vec![false, true]);
    // T toggles back; 1 is the limits tab.
    key(&mut screen, &mut text, 't');
    assert_eq!(screen.prefs.tab, Tab::Limits);
    key(&mut screen, &mut text, '2');
    key(&mut screen, &mut text, '1');
    assert_eq!(PrefsStore::at(path).load().tab, Tab::Limits);
    // Keys with a modifier are not shortcuts.
    screen.dispatch(
        Event::KeyDown {
            key: Key::Char('p'),
            modifiers: Modifiers {
                command: true,
                ..Modifiers::default()
            },
        },
        &mut text,
    );
    assert_eq!(screen.prefs.period, Period::Days30);
}

#[test]
fn refresh_key_calls_every_source() {
    let (mut screen, calls, mut text) =
        screen_with(demo::sources(demo::now()), Prefs::default(), None);
    // The first drain offered the two current, live sources to the history.
    assert_eq!(*calls.borrow(), vec!["offer", "offer"]);
    calls.borrow_mut().clear();
    key(&mut screen, &mut text, 'r');
    let after: Vec<&str> = calls.borrow().clone();
    assert_eq!(&after[..2], &["limits", "stats"]);
    // The refreshed values were drained and offered again.
    assert_eq!(after.iter().filter(|c| **c == "offer").count(), 2);
    // The button does the same.
    calls.borrow_mut().clear();
    let origin = screen.tree.window_origin(screen.refresh).unwrap();
    let rect = screen.tree.layout(screen.refresh).unwrap().rect;
    let pos = noa_gpui_scene::Point::new(origin.x + rect.width / 2.0, origin.y + rect.height / 2.0);
    let button = noa_gpui_core::PointerButton::Primary;
    screen.dispatch(Event::PointerMove { pos }, &mut text);
    screen.dispatch(Event::PointerDown { pos, button }, &mut text);
    screen.dispatch(Event::PointerUp { pos, button }, &mut text);
    assert_eq!(&calls.borrow()[..2], &["limits", "stats"]);
}

/// The screen at the window's minimum size.
const MIN_WINDOW: Size = Size::new(960.0, 640.0);

fn screen_at(
    sources: Vec<UsageSource>,
    prefs: Prefs,
    size: Size,
) -> (AgentUsage, Rc<RefCell<Vec<&'static str>>>, TextStore) {
    let (mut screen, calls, mut text) = screen_with(sources, prefs, None);
    screen.prepare(size, &mut text);
    (screen, calls, text)
}

fn window_rect(tree: &UiTree, id: WidgetId) -> noa_gpui_scene::Rect {
    let origin = tree.window_origin(id).unwrap();
    let size = tree.layout(id).unwrap().rect;
    noa_gpui_scene::Rect::new(origin.x, origin.y, size.width, size.height)
}

/// Widgets under `root` that stick out of their parent sideways (so are cut
/// off), except the content of the views in `scrolls`, which scroll sideways.
fn cut_off(tree: &UiTree, root: WidgetId, scrolls: &[WidgetId]) -> Vec<String> {
    let mut out = Vec::new();
    let parent = window_rect(tree, root);
    for child in tree.children(root) {
        let r = window_rect(tree, *child);
        if !scrolls.contains(&root) && (r.x < parent.x - 0.5 || r.right() > parent.right() + 0.5) {
            let what = labels_under(tree, *child);
            out.push(format!("{what:?} {r:?} in {parent:?}"));
        }
        out.extend(cut_off(tree, *child, scrolls));
    }
    out
}

fn scroll_of(screen: &AgentUsage, name: &str) -> WidgetId {
    screen
        .stats_scrolls
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, id)| *id)
        .unwrap()
}

fn wheel(screen: &mut AgentUsage, text: &mut TextStore, at: WidgetId, dx: f32, dy: f32) {
    let r = window_rect(&screen.tree, at);
    let pos = noa_gpui_scene::Point::new(r.x + r.width / 2.0, r.y + r.height / 2.0);
    screen.dispatch(Event::PointerMove { pos }, text);
    screen.dispatch(
        Event::Wheel {
            pos,
            delta: noa_gpui_scene::Point::new(dx, dy),
        },
        text,
    );
    screen.prepare(MIN_WINDOW, text);
}

#[test]
fn every_part_of_both_tabs_is_reachable_at_minimum_width() {
    let stats_prefs = Prefs {
        tab: Tab::Stats,
        agent: AgentFilter::All,
        ..Prefs::default()
    };
    let (mut screen, _, mut text) = screen_at(demo::sources(demo::now()), stats_prefs, MIN_WINDOW);
    let scrolls: Vec<WidgetId> = screen.stats_scrolls.iter().map(|(_, id)| *id).collect();
    assert_eq!(scrolls.len(), 6, "every table scrolls sideways");
    // Nothing outside a table is cut off: the KPIs and the share legend wrap.
    let cut = cut_off(&screen.tree, screen.stats_host, &scrolls);
    assert!(cut.is_empty(), "{cut:#?}");
    for kpi in ["キャッシュ読取", "推定コスト"] {
        let labels = labels_under(&screen.tree, screen.stats_host);
        assert!(labels.iter().any(|l| l.contains(kpi)), "{kpi}");
    }

    // The agent table is wider than its card: a sideways wheel brings its
    // last columns (reasoning, cost, workers) into view.
    let agents = scroll_of(&screen, "agents");
    // Down the page with the wheel until the table is in the window.
    let host = screen.stats_host;
    let below = window_rect(&screen.tree, agents).y - 200.0;
    wheel(&mut screen, &mut text, host, 0.0, -below);
    let view = window_rect(&screen.tree, agents);
    assert!(
        view.y > 0.0 && view.bottom() < MIN_WINDOW.height,
        "{view:?}"
    );
    let last_cell = |screen: &AgentUsage| {
        let body = screen.tree.children(agents)[0];
        let header = screen.tree.children(body)[0];
        let cells = screen.tree.children(header);
        window_rect(&screen.tree, cells[cells.len() - 1])
    };
    assert!(
        last_cell(&screen).right() > view.right() + 1.0,
        "starts cut off"
    );
    let page_before = screen
        .tree
        .get::<ScrollView>(screen.stats_host)
        .unwrap()
        .scroll_position();
    wheel(&mut screen, &mut text, agents, -2000.0, 0.0);
    let x = screen
        .tree
        .get::<ScrollView>(agents)
        .unwrap()
        .scroll_position()
        .x;
    assert!(x > 0.0, "the table moved sideways");
    assert!(
        last_cell(&screen).right() <= view.right() + 0.5,
        "{:?} in {view:?}",
        last_cell(&screen)
    );
    let table = labels_under(&screen.tree, agents);
    for column in ["推論", "推定コスト", "ワーカー"] {
        assert!(has(&table, column), "{column}");
    }
    // A plain wheel over the table still scrolls the page, not the table.
    wheel(&mut screen, &mut text, agents, 0.0, -120.0);
    let page_after = screen
        .tree
        .get::<ScrollView>(screen.stats_host)
        .unwrap()
        .scroll_position();
    assert!(
        page_after.y > page_before.y,
        "{page_before:?} → {page_after:?}"
    );
    assert_eq!(
        screen
            .tree
            .get::<ScrollView>(agents)
            .unwrap()
            .scroll_position()
            .x,
        x
    );
    // The tab is rebuilt every minute: the table stays where it was scrolled.
    screen.press('r', &mut text);
    screen.prepare(MIN_WINDOW, &mut text);
    let rebuilt = scroll_of(&screen, "agents");
    assert_ne!(rebuilt, agents);
    let kept = screen
        .tree
        .get::<ScrollView>(rebuilt)
        .unwrap()
        .scroll_position()
        .x;
    assert!(kept > 0.0, "sideways position kept across the rebuild");

    // The limits tab has no sideways scrolling: nothing in it is cut off.
    let (screen, _, _) = screen_at(demo::sources(demo::now()), Prefs::default(), MIN_WINDOW);
    let cut = cut_off(&screen.tree, screen.limits_host, &[]);
    assert!(cut.is_empty(), "{cut:#?}");
}

/// The Codex-only report of a machine without Codex logs: the ctl journals
/// still counted two workers.
fn codex_without_logs() -> StatsReport {
    use agent_usage::stats::types::{Availability, Counts};
    let now = demo::now();
    let mut report = StatsReport::empty((now - 86_400, now));
    let counts = Counts {
        workers: 2,
        ..Counts::empty()
    };
    report.total = counts.clone();
    report.per_agent = vec![(
        Agent::Codex,
        counts,
        Availability::Missing("no logs found".into()),
    )];
    report
}

#[test]
fn missing_logs_show_dashes_but_journal_workers_still_count() {
    let report = codex_without_logs();
    let kpis = stats::kpis(&report);
    for title in [
        "セッション",
        "ターン",
        "メッセージ",
        "ツール呼び出し",
        "稼働時間",
        "総トークン",
        "推定コスト",
    ] {
        let k = kpis.iter().find(|k| k.0 == title).unwrap();
        assert_eq!(k.1, stats::DASH, "{title}: {k:?}");
    }
    let tokens = kpis.iter().find(|k| k.0 == "総トークン").unwrap();
    assert_eq!(tokens.2, "Codex: no logs found");
    let rows = stats::agent_rows(&report);
    assert_eq!(rows[0][1], stats::DASH, "sessions");
    assert_eq!(rows[0][5], stats::DASH, "active time");
    assert_eq!(rows[0][13], "2", "workers come from the journals");
    // agy alone still says it records no tokens.
    let now = demo::now();
    let agy = demo::report(
        Period::Days7,
        AgentFilter::One(Agent::Agy),
        now,
        &demo::tz(),
    );
    assert_eq!(
        stats::unavailable_reason(&agy, "トークン"),
        "agy はトークンを記録しません"
    );
    assert!(stats::logs_known(&agy));
}

#[test]
fn project_cost_says_what_it_leaves_out() {
    let now = demo::now();
    let tz = demo::tz();
    // Claude (priced) and Codex (unpriced) share every project.
    let all = demo::report(Period::Days7, AgentFilter::All, now, &tz);
    let rows = stats::project_rows(&all);
    assert!(!rows.is_empty());
    for row in &rows {
        assert!(
            row[5].starts_with('$') && row[5].contains("一部未計上"),
            "{row:?}"
        );
    }
    // Claude alone is priced in full.
    let claude = demo::report(Period::Days7, AgentFilter::One(Agent::Claude), now, &tz);
    for row in stats::project_rows(&claude) {
        assert!(!row[5].contains("未計上"), "{row:?}");
    }
}

#[test]
fn values_going_stale_say_why_and_redraw_without_new_data() {
    let now = demo::now();
    // Only Claude is current; its 5h window resets 20 s from now.
    let mut s = demo::sources(now);
    s[0].windows[0].resets_at = Some(now + 20);
    s[1] = UsageSource::placeholder(Agent::Codex);
    s[2] = UsageSource::placeholder(Agent::Agy);
    let (mut screen, _, mut text) = screen_with(s.clone(), Prefs::default(), None);
    assert!(summary_of(&screen).starts_with("Claude Code 7d 61%"));
    // Half a minute later, within the same minute, a tick with no new data.
    screen.clock = Clock::fixed(now + 30, demo::tz());
    screen.woken(&mut text);
    screen.prepare(window(), &mut text);
    assert_eq!(summary_of(&screen), "現在値なし");
    let labels = card_labels(&screen, Agent::Claude);
    assert!(has(&labels, "古い値: 5h がリセット済み"), "{labels:?}");
    assert!(
        gauges_under(&screen.tree, card_of(&screen, Agent::Claude))
            .iter()
            .all(|g| g.2)
    );

    // Older than `stale_after` (900 s) with no note: the age is the reason.
    let mut s = demo::sources(now);
    s[0].last_updated = Some(now - 890);
    let (mut screen, _, mut text) = screen_with(s, Prefs::default(), None);
    assert!(
        card_labels(&screen, Agent::Claude)
            .iter()
            .all(|l| !l.contains("古い値"))
    );
    screen.clock = Clock::fixed(now + 30, demo::tz());
    screen.woken(&mut text);
    screen.prepare(window(), &mut text);
    let labels = card_labels(&screen, Agent::Claude);
    assert!(has(&labels, "古い値 · 15分前"), "{labels:?}");
    assert!(has(&labels, "古い値: 取得から15分経過"), "{labels:?}");
}

#[test]
fn notes_and_buckets_show_without_windows() {
    let now = demo::now();
    // Codex: no window, no spend, only the credit balance.
    let mut s = demo::sources(now);
    s[1].windows.clear();
    s[1].notes = vec!["Credit balance 42".into()];
    // Claude: only the scoped bucket.
    s[0].windows.clear();
    s[0].spend = None;
    let (screen, _, _) = screen_with(s, Prefs::default(), None);
    let codex = card_labels(&screen, Agent::Codex);
    assert!(has(&codex, "Credit balance 42"), "{codex:?}");
    let claude = card_labels(&screen, Agent::Claude);
    assert!(
        has(&claude, "Sonnet 7d") && has(&claude, "22%"),
        "{claude:?}"
    );
    assert_eq!(
        gauges_under(&screen.tree, card_of(&screen, Agent::Claude)).len(),
        1
    );
    // A first fetch that failed, then a refresh the source deferred.
    let mut s = demo::sources(now);
    s[0] = UsageSource {
        state: SourceState::Error("Token expired (launch Claude Code to renew)".into()),
        notes: vec!["Refresh deferred (next request in 2m)".into()],
        ..UsageSource::placeholder(Agent::Claude)
    };
    let (screen, _, _) = screen_with(s, Prefs::default(), None);
    let claude = card_labels(&screen, Agent::Claude);
    assert!(
        has(&claude, "Token expired (launch Claude Code to renew)")
            && has(&claude, "Refresh deferred (next request in 2m)"),
        "{claude:?}"
    );
}

fn gauge_ids(tree: &UiTree, id: WidgetId) -> Vec<WidgetId> {
    let own = tree.get::<Gauge>(id).map(|_| id);
    own.into_iter()
        .chain(tree.children(id).iter().flat_map(|c| gauge_ids(tree, *c)))
        .collect()
}

/// The alpha of everything drawn inside `rect`.
fn alphas_in(tree: &UiTree, rect: noa_gpui_scene::Rect) -> Vec<f32> {
    use noa_gpui_scene::DrawCommand;
    let inside = |r: &noa_gpui_scene::Rect| {
        r.x >= rect.x - 0.5
            && r.y >= rect.y - 0.5
            && r.right() <= rect.right() + 0.5
            && r.bottom() <= rect.bottom() + 0.5
    };
    tree.display_list()
        .commands()
        .iter()
        .filter_map(|c| match c {
            DrawCommand::Quad { rect, color, .. } | DrawCommand::Text { rect, color, .. }
                if inside(rect) =>
            {
                Some(color.a)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn stale_rows_and_gauges_are_drawn_at_half_opacity() {
    let (screen, _, _) = screen_at(demo::sources(demo::now()), Prefs::default(), window());
    let tree = &screen.tree;
    for (agent, stale) in [(Agent::Agy, true), (Agent::Claude, false)] {
        for gauge in gauge_ids(tree, card_of(&screen, agent)) {
            // The gauge's row: label, gauge, percent and reset.
            let row = tree.parent(gauge).unwrap();
            let alphas = alphas_in(tree, window_rect(tree, row));
            assert!(alphas.len() >= 4, "{agent:?}: {alphas:?}");
            if stale {
                assert!(alphas.iter().all(|a| *a <= 0.5 + 1e-3), "{alphas:?}");
            } else {
                assert!(alphas.iter().any(|a| *a > 0.99), "{alphas:?}");
            }
        }
    }
}

/// The cells' texts under `id`, in order.
fn cell_texts(tree: &UiTree, id: WidgetId) -> Vec<WidgetId> {
    let own = tree.get::<CellText>(id).map(|_| id);
    own.into_iter()
        .chain(tree.children(id).iter().flat_map(|c| cell_texts(tree, *c)))
        .collect()
}

#[test]
fn long_names_are_cut_in_their_column_and_whole_on_hover() {
    use noa_gpui_core::widgets::Tooltip;
    use noa_gpui_scene::{DrawCommand, Point};
    let now = demo::now();
    let tz = demo::tz();
    // The demo's long project path and plugin skill, a model and an MCP
    // tool: each longer than its column.
    let path = demo::PROJECTS[1];
    let skill = "example-plugin:spreadsheet-builder";
    let model = "claude-sonnet-5-5-20261001-extended-context-preview";
    let tool = "mcp__github__create_pull_request_review_comment";
    let mut report = demo::report(Period::Days7, AgentFilter::All, now, &tz);
    report.per_model[0].model = model.to_owned();
    report.tools[0].0 = tool.to_owned();
    let prefs = Prefs {
        tab: Tab::Stats,
        agent: AgentFilter::All,
        ..Prefs::default()
    };
    // Tall enough that every table is drawn.
    let size = Size::new(1280.0, 3200.0);
    let (mut screen, _, mut text) = feed_screen(demo::sources(now), Some(report), prefs, None);
    screen.prepare(size, &mut text);
    let tree = &screen.tree;
    let drawn: Vec<noa_gpui_scene::Rect> = tree
        .display_list()
        .commands()
        .iter()
        .filter_map(|c| match c {
            DrawCommand::Text { rect, .. } => Some(*rect),
            _ => None,
        })
        .collect();
    let mut cut = Vec::new();
    for id in cell_texts(tree, screen.stats_host) {
        let cell = tree.get::<CellText>(id).unwrap();
        let column = window_rect(tree, tree.parent(id).unwrap());
        // The text drawn for a cell ends inside it, before the next column.
        let own: Vec<_> = drawn
            .iter()
            .filter(|r| column.contains(Point::new(r.x, r.y)))
            .collect();
        assert_eq!(own.len(), 1, "{:?} in {column:?}", cell.text());
        assert!(
            own[0].right() <= column.right() + 0.5,
            "{:?} drawn to {} past its column {column:?}",
            cell.text(),
            own[0].right()
        );
        if cell.shown_text() != cell.text() {
            assert!(cell.shown_text().contains('…'), "{:?}", cell.shown_text());
            cut.push(cell.text().to_owned());
        }
    }
    // The long names are cut (the model in its table and in the share
    // legend), and nothing else is.
    cut.sort();
    let mut expected = vec![model, model, path, skill, tool];
    expected.sort();
    assert_eq!(cut, expected);
    let shown = |name: &str| {
        let id = cell_texts(tree, screen.stats_host)
            .into_iter()
            .find(|id| tree.get::<CellText>(*id).unwrap().text() == name)
            .unwrap();
        (
            id,
            tree.get::<CellText>(id).unwrap().shown_text().to_owned(),
        )
    };
    // Cut in the middle, the path keeps its start and its end.
    let (path_cell, short) = shown(path);
    assert!(
        short.starts_with("~/repos/") && short.ends_with("components"),
        "{short}"
    );
    let (alpha, _) = shown("~/work/alpha");

    // Hovering the cut path shows it whole; a name that fits has no tooltip.
    let hover = |screen: &mut AgentUsage, text: &mut TextStore, id: WidgetId| {
        let r = window_rect(&screen.tree, id);
        let pos = Point::new(r.x + r.width / 2.0, r.y + r.height / 2.0);
        screen.dispatch(Event::PointerMove { pos }, text);
        let later = screen.tree.now() + 1.0;
        screen.tree.animate(later, text);
        screen
            .tree
            .tooltip_overlay()
            .and_then(|o| screen.tree.get::<Tooltip>(o))
            .map(|t| t.text().to_owned())
    };
    assert_eq!(
        hover(&mut screen, &mut text, path_cell).as_deref(),
        Some(path)
    );
    assert_eq!(hover(&mut screen, &mut text, alpha), None);
}

/// Limit sources that answer one batch per wake.
struct Trickle(Vec<Vec<UsageSource>>);

impl Feed for Trickle {
    fn refresh(&mut self) {}

    fn drain(&mut self) -> Vec<UsageSource> {
        if self.0.is_empty() {
            Vec::new()
        } else {
            self.0.remove(0)
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
        force.then(|| demo::report(period, filter, now, tz))
    }

    fn limit_series(&self, _range: (Unix, Unix)) -> Vec<Series> {
        Vec::new()
    }

    fn offer(&mut self, _source: &UsageSource, _now: Unix) {}
}

#[test]
fn a_source_not_heard_from_yet_is_fetching_not_unconfigured() {
    let now = demo::now();
    let claude = demo::sources(now).remove(0);
    // Claude answers at once; Codex later, without its CLI; agy not yet.
    let feed = Trickle(vec![
        vec![claude],
        vec![UsageSource::placeholder(Agent::Codex)],
    ]);
    let mut screen = AgentUsage::new(
        Box::new(feed),
        Clock::fixed(now, demo::tz()),
        Prefs::default(),
        None,
    );
    let mut text = TextStore::new(Default::default());
    screen.prepare(window(), &mut text);
    assert_eq!(cards(&screen).len(), 3);
    for agent in [Agent::Codex, Agent::Agy] {
        assert_eq!(
            card_labels(&screen, agent),
            vec![agent.display(), limits::FETCHING],
            "{agent:?}"
        );
        assert_eq!(
            dot_under(&screen.tree, card_of(&screen, agent)),
            Some(false)
        );
    }
    assert!(has(&card_labels(&screen, Agent::Claude), "34%"));
    assert!(summary_of(&screen).starts_with("Claude Code "));

    // Codex's first answer: unconfigured, and how to set it up.
    screen.woken(&mut text);
    screen.prepare(window(), &mut text);
    let codex = card_labels(&screen, Agent::Codex);
    assert!(
        has(&codex, "未設定") && codex.iter().any(|l| l.contains("codex login")),
        "{codex:?}"
    );
    assert!(!has(&codex, limits::FETCHING), "{codex:?}");
    assert_eq!(
        card_labels(&screen, Agent::Agy),
        vec!["Antigravity", limits::FETCHING]
    );
}

#[test]
fn a_wholly_unpriced_row_says_how_many_tokens_it_leaves_out() {
    use agent_usage::stats::types::Counts;
    // A model missing from the price table: 12,345 tokens, no cost.
    let mut c = Counts::empty();
    c.c.tokens.input = 12_345;
    c.unpriced_tokens = 12_345;
    assert_eq!(stats::cost_text(&c, true), "— · 未計上 (12.3K tokens)");
    // Tokens the agent does not record, or none used: a dash alone.
    assert_eq!(stats::cost_text(&c, false), stats::DASH);
    assert_eq!(stats::cost_text(&Counts::empty(), true), stats::DASH);

    // Codex has no prices: every table says what its cost leaves out.
    let now = demo::now();
    let codex = demo::report(
        Period::Days7,
        AgentFilter::One(Agent::Codex),
        now,
        &demo::tz(),
    );
    let unpriced = |s: &str| s.starts_with("— · 未計上 (") && s.ends_with(" tokens)");
    assert!(
        unpriced(&stats::agent_rows(&codex)[0][12]),
        "{:?}",
        stats::agent_rows(&codex)[0]
    );
    for row in stats::project_rows(&codex) {
        assert!(unpriced(&row[5]), "{row:?}");
    }
    let (screen, _, _) = screen_with(
        demo::sources(now),
        Prefs {
            tab: Tab::Stats,
            agent: AgentFilter::One(Agent::Codex),
            ..Prefs::default()
        },
        None,
    );
    let models = labels_under(&screen.tree, scroll_of(&screen, "models"));
    assert_eq!(
        models.iter().filter(|l| unpriced(l)).count(),
        codex.per_model.len(),
        "{models:?}"
    );
}

#[test]
fn a_first_failure_shows_its_reason_once() {
    let now = demo::now();
    let reason = "Token expired (launch Claude Code to renew)";
    // The source puts the reason in its state and among its notes, with no
    // earlier value and with one.
    let failed = |earlier: Option<UsageSource>| UsageSource {
        state: SourceState::Error(reason.into()),
        notes: vec![reason.into()],
        ..earlier.unwrap_or_else(|| UsageSource::placeholder(Agent::Claude))
    };
    for earlier in [None, Some(demo::sources(now).remove(0))] {
        let mut s = demo::sources(now);
        s[0] = failed(earlier);
        let (screen, _, _) = screen_with(s, Prefs::default(), None);
        let claude = card_labels(&screen, Agent::Claude);
        assert_eq!(
            claude.iter().filter(|l| *l == reason).count(),
            1,
            "{claude:?}"
        );
    }
}

#[test]
fn skill_table_lists_uses_by_agent_and_agy_has_none() {
    let now = demo::now();
    let tz = demo::tz();
    let all = demo::report(Period::Days7, AgentFilter::All, now, &tz);
    let rows = stats::skill_rows(&all);
    assert!(!rows.is_empty());
    // Skill, agent, uses, sessions, projects, last use.
    assert_eq!(rows[0].len(), 6);
    assert!(rows.iter().any(|r| r[1] == "Codex") && rows.iter().any(|r| r[1] == "Claude Code"));
    assert!(rows.iter().all(|r| r[5].starts_with("11/24 ")), "{rows:?}");
    assert!(
        stats::project_rows(&all)
            .iter()
            .all(|r| r[3] != stats::DASH)
    );
    let agy = demo::report(Period::Days7, AgentFilter::One(Agent::Agy), now, &tz);
    assert!(stats::skill_rows(&agy).is_empty());
    assert!(!stats::skills_known(&agy));
    assert_eq!(
        stats::unavailable_reason(&agy, "スキル使用"),
        "agy はスキル使用を記録しません"
    );
}

#[test]
fn an_empty_skill_table_still_says_agy_records_none() {
    let now = demo::now();
    // Claude and Codex logs were read but hold no skill use in the period;
    // agy is selected too.
    let mut report = demo::report(Period::Days7, AgentFilter::All, now, &demo::tz());
    report.skills.clear();
    assert!(stats::skills_known(&report));
    let (mut screen, _, mut text) = feed_screen(
        demo::sources(now),
        Some(report),
        Prefs {
            tab: Tab::Stats,
            ..Prefs::default()
        },
        None,
    );
    screen.prepare(window(), &mut text);
    let labels = labels_under(&screen.tree, screen.stats_host);
    assert!(
        labels
            .iter()
            .any(|l| l.contains("スキル使用の記録がありません")
                && l.contains("agy はスキル使用を記録しません")),
        "{labels:?}"
    );
}
