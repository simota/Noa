use std::cell::RefCell;
use std::rc::Rc;

use noa_gpui_core::widgets::{Label, Tag};
use noa_gpui_core::{Event, Key, Modifiers};
use noa_gpui_scene::Size;

use agent_usage::model::format;

use super::charts::{Dot, Gauge};
use super::*;
use crate::demo;

/// The demo's data, with the sources a test wants, recording refreshes.
struct TestFeed {
    sources: Vec<UsageSource>,
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
        force.then(|| demo::report(period, filter, now, tz))
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
    let calls = Rc::new(RefCell::new(Vec::new()));
    let feed = TestFeed {
        sources,
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
    assert_eq!(codex[12], stats::DASH, "Codex has no prices");
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
