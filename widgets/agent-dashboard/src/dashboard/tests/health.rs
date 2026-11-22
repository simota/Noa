//! AC6: the header's dot and status say how far the link can be trusted.
//! Every time here is injected (`apply_received`, `tick`): nothing reads
//! the clock or waits.

use std::time::{Duration, Instant};

use crate::dashboard::health::{self, Health, STALE_AFTER};
use crate::dashboard::style::{GRAY, GREEN, ORANGE, RED, YELLOW};

use super::*;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn down(reason: &str) -> Snapshot {
    Snapshot {
        fleet: Fleet::default(),
        link: Link::Down(reason.to_owned()),
        synced_secs: 0,
        can_focus: false,
        can_input: false,
        instance: None,
    }
}

fn status_text(dashboard: &Dashboard) -> String {
    dashboard
        .tree
        .get::<Label>(dashboard.status)
        .unwrap()
        .text()
        .to_owned()
}

/// The colour the dot is drawn in, after laying the frame out again.
fn dot_color(dashboard: &mut Dashboard, text: &mut TextStore) -> Color {
    dashboard.prepare(window(), text);
    let dot = dashboard.header.health.dot;
    let (color, _) = quad_at(dashboard, rect(&dashboard.tree, dot)).expect("the dot is drawn");
    color
}

/// A dashboard on its canned fleet that has just taken a live snapshot with
/// `synced_secs` at `base`, and its frame's tick.
fn live_at(base: Instant, synced_secs: u64) -> (Dashboard, TextStore) {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let snapshot = live(fleet::canned(), synced_secs);
    dashboard.apply_received(snapshot, base, &mut text);
    // A frame ticks right after taking a snapshot in.
    dashboard.tick(base);
    (dashboard, text)
}

#[test]
fn health_follows_link_and_age() {
    let down = Link::Down("切断".to_owned());
    let at = |link: Option<&Link>, age: f64, had_data: bool| {
        health::of(link, Duration::from_secs_f64(age), had_data)
    };
    // The canned fleet and a connecting link do not age.
    for age in [0.0, 9.9, 10.0, 60.0] {
        assert_eq!(at(None, age, true), Health::Demo, "demo at {age}");
        assert_eq!(
            at(Some(&Link::Connecting), age, false),
            Health::Connecting,
            "connecting at {age}"
        );
    }
    // A live link is stale from `STALE_AFTER` on, not a moment before.
    assert_eq!(STALE_AFTER, secs(10));
    assert_eq!(at(Some(&Link::Live), 0.0, true), Health::Live);
    assert_eq!(at(Some(&Link::Live), 9.9, true), Health::Live);
    assert_eq!(at(Some(&Link::Live), 10.0, true), Health::Stale);
    // A lost link is down at any age; whether data was shown only changes
    // its text.
    for age in [0.0, 9.9, 10.0] {
        assert_eq!(
            at(Some(&down), age, false),
            Health::Down { had_data: false }
        );
        assert_eq!(at(Some(&down), age, true), Health::Down { had_data: true });
    }
    // The dot's colour, and the label's (`None` is the muted text colour).
    let colors = [
        (Health::Demo, GRAY, None),
        (Health::Connecting, YELLOW, None),
        (Health::Live, GREEN, None),
        (Health::Stale, ORANGE, Some(ORANGE)),
        (Health::Down { had_data: false }, RED, Some(RED)),
        (Health::Down { had_data: true }, RED, Some(RED)),
    ];
    for (health, dot, text) in colors {
        assert_eq!(health.dot(), dot, "{health:?}");
        assert_eq!(health.text(), text, "{health:?}");
    }
}

#[test]
fn the_status_texts_follow_the_table() {
    let base = Instant::now();
    // Demo and connecting: unchanged.
    let (demo, _) = laid_out(Grouping::Orchestrator);
    assert_eq!(status_text(&demo), "ワーカー 9 · ダミーデータ");
    // Live, just under and at the limit.
    let (mut dashboard, _) = live_at(base, 0);
    dashboard.tick(base + Duration::from_secs_f64(9.9));
    assert_eq!(status_text(&dashboard), "ワーカー 9 · 同期 10 秒前");
    dashboard.tick(base + secs(10));
    assert_eq!(
        status_text(&dashboard),
        "古いデータ（同期 10 秒前）· ワーカー 9"
    );
    // Minutes once a minute has passed.
    dashboard.tick(base + secs(125));
    assert_eq!(
        status_text(&dashboard),
        "古いデータ（同期 2 分前）· ワーカー 9"
    );
    // Down with no data ever shown says only why.
    let mut empty = Dashboard::new(Grouping::Orchestrator);
    let mut text = TextStore::new(Default::default());
    empty.prepare(window(), &mut text);
    empty.apply_received(live(Fleet::default(), 0), base, &mut text);
    empty.apply_received(down("接続できません"), base, &mut text);
    empty.tick(base + secs(30));
    assert_eq!(status_text(&empty), "切断: 接続できません");
}

#[test]
fn the_status_ticks_without_a_snapshot() {
    let base = Instant::now();
    let (mut dashboard, mut text) = live_at(base, 1);
    assert_eq!(dot_color(&mut dashboard, &mut text), GREEN);

    dashboard.tick(base + secs(3));
    assert_eq!(status_text(&dashboard), "ワーカー 9 · 同期 4 秒前");
    assert_eq!(dot_color(&mut dashboard, &mut text), GREEN);

    dashboard.tick(base + secs(12));
    let stale = status_text(&dashboard);
    assert!(stale.starts_with("古いデータ"), "{stale}");
    assert_eq!(stale, "古いデータ（同期 13 秒前）· ワーカー 9");
    assert_eq!(dot_color(&mut dashboard, &mut text), ORANGE);

    // A fresh snapshot makes it live again.
    let later = base + secs(20);
    dashboard.apply_received(live(fleet::canned(), 0), later, &mut text);
    dashboard.tick(later);
    assert_eq!(status_text(&dashboard), "ワーカー 9 · 同期 0 秒前");
    assert_eq!(dot_color(&mut dashboard, &mut text), GREEN);
}

#[test]
fn a_lost_link_keeps_the_last_data_marked_stale() {
    let base = Instant::now();
    let (mut dashboard, mut text) = live_at(base, 0);
    let queue = |dashboard: &Dashboard| {
        dashboard.registry.lists[Panel::Queue as usize]
            .as_ref()
            .map(|parts| parts.items.len())
    };
    assert_eq!(queue(&dashboard), Some(4));

    let lost = base + secs(30);
    dashboard.apply_received(down("接続が切れました"), lost, &mut text);
    dashboard.tick(lost);
    dashboard.prepare(window(), &mut text);

    assert_eq!(queue(&dashboard), Some(4), "the last data stays");
    assert_eq!(dashboard.feed.fleet.workers.len(), 9);
    let status = status_text(&dashboard);
    assert!(status.starts_with("切断: "), "{status}");
    assert!(status.contains("古いデータ"), "{status}");
    assert_eq!(status, "切断: 古いデータ（30 秒前）· 接続が切れました");
    assert_eq!(dot_color(&mut dashboard, &mut text), RED);

    // The age keeps counting while the link stays down.
    dashboard.tick(base + secs(200));
    assert_eq!(
        status_text(&dashboard),
        "切断: 古いデータ（3 分前）· 接続が切れました"
    );

    // The server comes back: live data replaces it.
    let back = base + secs(210);
    dashboard.apply_received(live(fleet::canned(), 0), back, &mut text);
    dashboard.tick(back);
    assert_eq!(status_text(&dashboard), "ワーカー 9 · 同期 0 秒前");
    assert_eq!(dot_color(&mut dashboard, &mut text), GREEN);
}

#[test]
fn a_lost_link_with_nothing_shown_says_only_why() {
    let base = Instant::now();
    let mut dashboard = Dashboard::new(Grouping::Orchestrator);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    dashboard.apply_received(live(Fleet::default(), 0), base, &mut text);
    dashboard.apply_received(down("接続できません"), base + secs(1), &mut text);
    dashboard.tick(base + secs(40));
    assert!(dashboard.feed.fleet.workers.is_empty());
    assert_eq!(status_text(&dashboard), "切断: 接続できません");
    assert_eq!(dot_color(&mut dashboard, &mut text), RED);
}

#[test]
fn only_a_down_link_keeps_the_old_workers() {
    let old = fleet::canned();
    let kept = |new: Fleet, link: Link| health::keep_on_down(&old, new, &link);
    let down = Link::Down("x".to_owned());
    assert_eq!(kept(Fleet::default(), down.clone()), old);
    // A Down snapshot that brings workers shows them.
    let one = Fleet {
        workers: vec![old.workers[0].clone()],
        ..Fleet::default()
    };
    assert_eq!(kept(one.clone(), down.clone()), one);
    // Other links show what they bring, an empty fleet included.
    assert_eq!(kept(Fleet::default(), Link::Live), Fleet::default());
    assert_eq!(kept(Fleet::default(), Link::Connecting), Fleet::default());
    // With nothing before, there is nothing to keep.
    assert_eq!(
        health::keep_on_down(&Fleet::default(), Fleet::default(), &down),
        Fleet::default()
    );
}

#[test]
fn a_notice_leaves_the_link_state_readable() {
    let base = Instant::now();
    let (mut dashboard, mut text) = live_at(base, 0);
    let lost = base + secs(30);
    dashboard.apply_received(down("接続が切れました"), lost, &mut text);
    dashboard.show_notice_at("通知です".to_owned(), lost);
    assert_eq!(notice_of(&dashboard), "通知です");
    assert_eq!(
        status_text(&dashboard),
        "切断: 古いデータ（30 秒前）· 接続が切れました"
    );
    assert_eq!(dot_color(&mut dashboard, &mut text), RED);
    // After `NOTICE_FOR` the notice line is empty again.
    dashboard.tick(lost + NOTICE_FOR);
    assert_eq!(notice_of(&dashboard), "");
    assert!(status_text(&dashboard).starts_with("切断: "));
}

#[test]
fn a_rebuilt_header_starts_on_the_text_tick_will_set() {
    let base = Instant::now();
    for (synced, whole) in [
        (2, "ワーカー 9 · 同期 2 秒前"),
        (14, "古いデータ（同期 14 秒前）· ワーカー 9"),
        (130, "古いデータ（同期 2 分前）· ワーカー 9"),
    ] {
        let (dashboard, _) = live_at(base, synced);
        let feed = &dashboard.feed;
        // What the builder writes reads no clock: it is what `status` says
        // when the snapshot came, which is when the frame's `tick` is.
        let initial = health::initial_status(feed);
        assert_eq!(initial, health::status(feed, base), "{synced}");
        assert_eq!(initial, whole, "{synced}");
    }
    // The header a rebuild made, before any tick, carries that text.
    let (mut dashboard, mut text) = live_at(base, 2);
    dashboard.rebuilt(dashboard.view, &mut text);
    assert_eq!(status_text(&dashboard), "ワーカー 9 · 同期 2 秒前");
    // Down: the last data and the age as of the snapshot.
    let lost = base + secs(30);
    dashboard.apply_received(down("切れた"), lost, &mut text);
    dashboard.show_notice_at("通知".to_owned(), lost);
    dashboard.rebuilt(dashboard.view, &mut text);
    assert_eq!(notice_of(&dashboard), "通知", "a fresh notice stays");
    assert!(status_text(&dashboard).starts_with("切断: "));
}

/// The colour the status label's text is drawn in.
fn text_color(dashboard: &mut Dashboard, text: &mut TextStore) -> Color {
    dashboard.prepare(window(), text);
    let [x, y, _, h] = rect(&dashboard.tree, dashboard.status);
    dashboard
        .display_list()
        .commands()
        .iter()
        .rev()
        .find_map(|command| match command {
            DrawCommand::Text { rect, color, .. }
                if (rect.x - x).abs() <= 0.5 && rect.y >= y - 0.5 && rect.y <= y + h =>
            {
                Some(*color)
            }
            _ => None,
        })
        .expect("the status is drawn")
}

/// The status as drawn: its text, its colour and the dot's.
fn drawn(dashboard: &mut Dashboard, text: &mut TextStore) -> (String, Color, Color) {
    let color = text_color(dashboard, text);
    (status_text(dashboard), color, dot_color(dashboard, text))
}

#[test]
fn a_rebuilt_header_draws_the_health_it_had() {
    let base = Instant::now();
    // A snapshot that came already stale; the frame ticks at its arrival.
    let (mut stale, mut text) = live_at(base, 14);
    let before = drawn(&mut stale, &mut text);
    assert_eq!(
        before,
        (
            "古いデータ（同期 14 秒前）· ワーカー 9".to_owned(),
            ORANGE,
            ORANGE
        )
    );
    // A rebuild between frames (a click, an accessibility action) draws
    // what the last tick did, with no tick of its own.
    stale.rebuilt(stale.view, &mut text);
    assert_eq!(drawn(&mut stale, &mut text), before, "stale");

    // A lost link with its data: red text and dot after the rebuild too.
    let (mut lost, mut text) = live_at(base, 0);
    let at = base + secs(30);
    lost.apply_received(down("接続が切れました"), at, &mut text);
    lost.tick(at);
    let (_, color, dot) = drawn(&mut lost, &mut text);
    assert_eq!((color, dot), (RED, RED));
    lost.rebuilt(lost.view, &mut text);
    let (status, color, dot) = drawn(&mut lost, &mut text);
    assert_eq!((color, dot), (RED, RED), "down");
    assert!(status.starts_with("切断: 古いデータ"), "{status}");
}

#[test]
fn a_fleet_that_emptied_still_had_data() {
    let base = Instant::now();
    let (mut dashboard, mut text) = live_at(base, 0);
    // Every worker closes, then the link is lost.
    let emptied = base + secs(5);
    dashboard.apply_received(live(Fleet::default(), 0), emptied, &mut text);
    dashboard.tick(emptied);
    let lost = base + secs(35);
    dashboard.apply_received(down("終了"), lost, &mut text);
    dashboard.tick(lost);
    assert!(
        dashboard.feed.fleet.workers.is_empty(),
        "the list stays empty"
    );
    assert_eq!(status_text(&dashboard), "切断: 古いデータ（30 秒前）· 終了");
    // A rebuild does not forget it.
    dashboard.rebuilt(dashboard.view, &mut text);
    assert_eq!(status_text(&dashboard), "切断: 古いデータ（30 秒前）· 終了");
    assert_eq!(dot_color(&mut dashboard, &mut text), RED);
}

#[test]
fn a_rebuild_between_frames_keeps_the_age() {
    let base = Instant::now();
    // Live, 30 s after the last answer: stale, as the last frame drew it.
    let (mut dashboard, mut text) = live_at(base, 0);
    dashboard.tick(base + secs(30));
    let before = drawn(&mut dashboard, &mut text);
    assert_eq!(
        before,
        (
            "古いデータ（同期 30 秒前）· ワーカー 9".to_owned(),
            ORANGE,
            ORANGE
        )
    );
    dashboard.rebuilt(dashboard.view, &mut text);
    assert_eq!(drawn(&mut dashboard, &mut text), before, "stale");

    // Down with its data, a minute after the link was lost.
    let lost = base + secs(40);
    dashboard.apply_received(down("接続が切れました"), lost, &mut text);
    dashboard.tick(lost + secs(60));
    let before = drawn(&mut dashboard, &mut text);
    assert_eq!(before.0, "切断: 古いデータ（2 分前）· 接続が切れました");
    dashboard.rebuilt(dashboard.view, &mut text);
    assert_eq!(drawn(&mut dashboard, &mut text), before, "down");
}

#[test]
fn a_reason_with_line_breaks_shows_on_one_line() {
    let base = Instant::now();
    let (mut dashboard, mut text) = live_at(base, 0);
    let lost = base + secs(30);
    dashboard.apply_received(down("A\nB\r\nC\tD\u{2028}E\nF"), lost, &mut text);
    dashboard.tick(lost);
    assert_eq!(
        status_text(&dashboard),
        "切断: 古いデータ（30 秒前）· A B  C D E F"
    );
    // Drawn as one 12 px line, inside the 48 px header.
    dashboard.prepare(window(), &mut text);
    let [_, y, _, h] = rect(&dashboard.tree, dashboard.status);
    let [_, bar_y, _, bar_h] = rect(&dashboard.tree, dashboard.header.bar);
    assert!(h < 24.0, "{h} px tall");
    assert!(
        y >= bar_y && y + h <= bar_y + bar_h,
        "{y} + {h} leaves the bar"
    );

    // With no data ever shown, the reason alone is on one line too.
    let mut empty = Dashboard::new(Grouping::Orchestrator);
    empty.prepare(window(), &mut text);
    empty.apply_received(live(Fleet::default(), 0), base, &mut text);
    empty.apply_received(down("接続\nできません"), base, &mut text);
    empty.tick(base);
    assert_eq!(status_text(&empty), "切断: 接続 できません");
}

#[test]
fn a_notice_with_line_breaks_shows_on_one_line() {
    let base = Instant::now();
    let (mut dashboard, _text) = live_at(base, 0);
    dashboard.show_notice_at("移動できません:\nno such pane".to_owned(), base);
    assert_eq!(notice_of(&dashboard), "移動できません: no such pane");
}

/// A live screen fed by the test, laid out, that has taken in the canned
/// fleet at `base`, with the filter field composing "けん".
fn composing_live_at(base: Instant) -> (Dashboard, TextStore, std::sync::mpsc::Sender<Snapshot>) {
    let (channel, snapshots, _notices, _commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots.send(live(fleet::canned(), 0)).unwrap();
    dashboard.frame(base, &mut text);
    typed(&mut dashboard, &mut text, "/");
    let composing = Event::Composition {
        text: "けん".into(),
        cursor: None,
    };
    dashboard.dispatch(composing, &mut text);
    dashboard.prepare(window(), &mut text);
    assert!(dashboard.composing());
    (dashboard, text, snapshots)
}

#[test]
fn a_lost_link_shows_while_a_composition_holds_the_snapshot() {
    let base = Instant::now();
    let (mut dashboard, mut text, snapshots) = composing_live_at(base);
    let field = dashboard.header.filter_field;
    let lost = base + secs(30);
    snapshots.send(down("接続が切れました")).unwrap();
    dashboard.frame(lost, &mut text);
    dashboard.prepare(window(), &mut text);

    // The body and the field wait for the composition ...
    assert!(dashboard.pending.is_some(), "held");
    assert_eq!(dashboard.feed.link, Some(Link::Live));
    assert_eq!(dashboard.header.filter_field, field);
    assert!(dashboard.composing(), "the composition goes on");
    // ... the header does not: down, red, with the age of the last data.
    let down_text = "切断: 古いデータ（30 秒前）· 接続が切れました";
    assert_eq!(
        drawn(&mut dashboard, &mut text),
        (down_text.into(), RED, RED)
    );
    // It stays down as time passes: not "stale" orange.
    dashboard.tick(base + secs(40));
    let (status, color, dot) = drawn(&mut dashboard, &mut text);
    assert_eq!(status, "切断: 古いデータ（40 秒前）· 接続が切れました");
    assert_eq!((color, dot), (RED, RED));

    // The composition ends: the snapshot is taken in, the header unchanged.
    typed(&mut dashboard, &mut text, "検");
    dashboard.frame(base + secs(41), &mut text);
    assert!(dashboard.pending.is_none());
    assert!(matches!(dashboard.feed.link, Some(Link::Down(_))));
    assert_eq!(dashboard.feed.fleet.workers.len(), 9, "the last data stays");
    let (status, color, dot) = drawn(&mut dashboard, &mut text);
    assert_eq!(status, "切断: 古いデータ（41 秒前）· 接続が切れました");
    assert_eq!((color, dot), (RED, RED));
}

#[test]
fn a_held_snapshot_from_a_link_back_up_shows_it_live() {
    let base = Instant::now();
    let (mut dashboard, mut text, snapshots) = composing_live_at(base);
    snapshots.send(down("切れた")).unwrap();
    // A composition already under way holds what comes next as well.
    typed(&mut dashboard, &mut text, "検");
    dashboard.frame(base + secs(5), &mut text);
    let composing = Event::Composition {
        text: "けん".into(),
        cursor: None,
    };
    dashboard.dispatch(composing, &mut text);
    let back = base + secs(60);
    let mut fewer = fleet::canned();
    fewer.workers.truncate(4);
    snapshots.send(live(fewer, 1)).unwrap();
    dashboard.frame(back, &mut text);
    assert!(dashboard.pending.is_some(), "held");
    assert_eq!(dashboard.feed.fleet.workers.len(), 9, "the body waits");
    // The header counts the workers the snapshot brings, from when it came.
    let live_text = "ワーカー 4 · 同期 1 秒前".to_owned();
    let muted = dashboard.tree.theme().color(Role::Muted);
    assert_eq!(drawn(&mut dashboard, &mut text), (live_text, muted, GREEN));
}

#[test]
fn a_notice_asks_for_the_frame_that_ends_it() {
    // The demo: no feed wakes the window, only what the screen asks for.
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    assert_eq!(dashboard.tree.next_timer(), None);
    key(&mut dashboard, &mut text, Key::Enter);
    assert_eq!(notice_of(&dashboard), NOTHING_SELECTED);
    let (_, at) = dashboard.feed.notice.clone().expect("a notice");
    // The tests' tree clock stands at 0: the frame is asked for at the
    // moment the notice ends.
    assert_eq!(dashboard.tree.now(), 0.0);
    assert_eq!(dashboard.tree.next_timer(), Some(NOTICE_FOR.as_secs_f64()));

    // A rebuild (a panel toggled) asks again, on its new header.
    key(&mut dashboard, &mut text, Key::Char('5'));
    assert_eq!(dashboard.tree.next_timer(), Some(NOTICE_FOR.as_secs_f64()));
    key(&mut dashboard, &mut text, Key::Char('5'));

    // A frame that comes a little early asks for what is left.
    let early = at + NOTICE_FOR - Duration::from_millis(250);
    dashboard.frame(early, &mut text);
    assert_eq!(notice_of(&dashboard), NOTHING_SELECTED);
    assert_eq!(dashboard.tree.next_timer(), Some(0.25));

    // The frame the timer brings ends the notice.
    dashboard.frame(at + NOTICE_FOR, &mut text);
    assert_eq!(notice_of(&dashboard), "");
    assert_eq!(status_text(&dashboard), "ワーカー 9 · ダミーデータ");
}

#[test]
fn the_empty_body_gives_the_reason_on_one_line() {
    let down = down("A\nB\u{2029}\u{5D0}\tC");
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, down);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    assert!(dashboard.panels.iter().all(Option::is_none));
    let tree = &dashboard.tree;
    let texts = labels_under(tree, tree.root().unwrap());
    let reason = "Noa に接続できません: A B \u{5D0} C";
    assert!(texts.iter().any(|t| t == reason), "{texts:?}");
    assert!(
        texts.iter().all(|t| !t.contains(['\n', '\t', '\u{2029}'])),
        "{texts:?}"
    );
    assert_eq!(status_text(&dashboard), "切断: A B \u{5D0} C");
}

#[test]
fn a_header_built_before_any_tick_draws_the_feeds_health() {
    let base = Instant::now();
    let built = |feed: Feed| {
        let mut dashboard = Dashboard::start(View::of(Grouping::Orchestrator), feed);
        let mut text = TextStore::new(Default::default());
        assert_eq!(dashboard.ticked, None, "no frame has ticked");
        drawn(&mut dashboard, &mut text)
    };
    let accepted = |snapshots: Vec<(Snapshot, Instant)>| {
        let mut feed = Feed::default();
        for (snapshot, at) in snapshots {
            let fleet = health::keep_on_down(&feed.fleet, snapshot.fleet.clone(), &snapshot.link);
            feed.accept(fleet, snapshot, at);
        }
        feed
    };
    let muted = Dashboard::new(Grouping::Orchestrator)
        .tree
        .theme()
        .color(Role::Muted);

    let fresh = accepted(vec![(live(fleet::canned(), 2), base)]);
    assert_eq!(
        built(fresh),
        ("ワーカー 9 · 同期 2 秒前".into(), muted, GREEN)
    );
    let stale = accepted(vec![(live(fleet::canned(), 14), base)]);
    assert_eq!(
        built(stale),
        (
            "古いデータ（同期 14 秒前）· ワーカー 9".into(),
            ORANGE,
            ORANGE
        )
    );
    let lost = accepted(vec![
        (live(fleet::canned(), 0), base),
        (down("切れた"), base + secs(30)),
    ]);
    let (status, color, dot) = built(lost);
    assert!(status.starts_with("切断: 古いデータ"), "{status}");
    assert!(status.ends_with("· 切れた"), "{status}");
    assert_eq!((color, dot), (RED, RED));
    let never = accepted(vec![(down("接続できません"), base)]);
    assert_eq!(built(never), ("切断: 接続できません".into(), RED, RED));
    let connecting = Feed {
        link: Some(Link::Connecting),
        ..Feed::default()
    };
    assert_eq!(built(connecting), ("接続中…".into(), muted, YELLOW));
    let demo = Feed::demo();
    assert_eq!(
        built(demo),
        ("ワーカー 9 · ダミーデータ".into(), muted, GRAY)
    );
}
