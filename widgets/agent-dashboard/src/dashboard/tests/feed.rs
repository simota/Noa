use std::time::Duration;

use crate::fleet::{Phase, State};
use noa_gpui_core::widgets::Label;

use super::*;

/// The canned fleet with six more running workers, so that the list pages
/// at the default window size.
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

#[test]
fn a_volatile_snapshot_updates_in_place() {
    let fleet = busy_fleet();
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet.clone(), 1));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    // One wheel step down over the list, then the next frame.
    let page_down = |dashboard: &mut Dashboard, text: &mut TextStore| {
        let pos = center(&dashboard.tree, list_of(dashboard));
        let delta = Point::new(0.0, -40.0);
        dashboard.dispatch(Event::Wheel { pos, delta }, text);
        dashboard.prepare(window(), text);
    };
    page_down(&mut dashboard, &mut text);
    let first = |d: &Dashboard| d.view.first;
    assert_eq!(first(&dashboard), 1, "the list pages");
    let rows = rows_of(&dashboard);
    let running = pulse_id(&dashboard, |p| matches!(p, Pulse::RowTime(11)));
    let since = pulse_id(&dashboard, |p| matches!(p, Pulse::RowTime(14)));

    let later = later(&fleet);
    dashboard.apply(live(later.clone(), 4), &mut text);
    dashboard.prepare(window(), &mut text);

    assert_eq!(first(&dashboard), 1, "the list was rebuilt");
    assert_eq!(rows_of(&dashboard), rows, "no row moved");
    let running = dashboard.tree.get::<Label>(running).unwrap();
    assert_eq!(running.text(), "later 11");
    let since = dashboard.tree.get::<Label>(since).unwrap();
    assert_eq!(since.text(), "待ち later 14");
    let status = dashboard.tree.get::<Label>(dashboard.status).unwrap();
    assert!(status.text() == "同期済み", "{}", status.text());
    // The screen shows and owns what one built for the later fleet, paged
    // the same way, does.
    let mut fresh = Dashboard::of_snapshot(Grouping::Orchestrator, live(later, 4));
    let mut fresh_text = TextStore::new(Default::default());
    fresh.prepare(window(), &mut fresh_text);
    page_down(&mut fresh, &mut fresh_text);
    assert_eq!(first(&fresh), 1);
    assert_eq!(
        frame_of(dashboard.display_list(), &text),
        frame_of(fresh.display_list(), &fresh_text)
    );
    assert_eq!(text.layouts().len(), fresh_text.layouts().len());
}

fn list_of(dashboard: &Dashboard) -> WidgetId {
    dashboard.registry.list.as_ref().unwrap().list
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
    // An approval now: among the approvals, by its wait (none yet).
    assert_eq!(rows_of(&dashboard)[..3], [14, 15, 12]);
    let button = row_button(&dashboard, 12);
    let button = dashboard.tree.get::<noa_gpui_core::widgets::Button>(button);
    assert_eq!(button.unwrap().label(), "承認画面へ");
    assert_eq!(
        notice_of(&dashboard),
        "要対応が増えました: docs-sync（承認待ち）"
    );
    // The notice aside, the screen is one built for the new fleet.
    dashboard.feed.notice = None;
    dashboard.tick(Instant::now());
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
        can_focus: false,
        can_input: false,
        safe_send: true,
        instance: None,
    };
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, down);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    assert!(dashboard.registry.list.is_none(), "no list without workers");
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

#[test]
fn a_live_screen_follows_its_feed() {
    let (channel, snapshots, _notices, commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    assert_eq!(commands.try_recv(), Ok(Command::Watch(vec![])));
    let status = dashboard.tree.get::<Label>(dashboard.status).unwrap();
    assert_eq!(status.text(), "接続中…");

    snapshots.send(live(fleet::canned(), 0)).unwrap();
    snapshots.send(live(fleet::canned(), 2)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    // Each live snapshot tells the feed again what is watched: no drawer,
    // no tail.
    assert_eq!(commands.try_recv(), Ok(Command::Watch(vec![])));
    let status = dashboard.tree.get::<Label>(dashboard.status).unwrap();
    assert_eq!(status.text(), "同期済み");

    // The click comes a second later: the link state ages, the notice does
    // not replace it.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    // The top row, fix-tests, waits for an approval: 承認画面へ.
    let pane = rows_of(&dashboard)[0];
    let pos = center(&dashboard.tree, row_button(&dashboard, pane));
    click(&mut dashboard, &mut text, pos);
    assert_eq!(commands.try_recv(), Ok(Command::Focus(pane)));
    assert_eq!(notice_of(&dashboard), "fix-tests のペインへ移動します");
    let status = dashboard.tree.get::<Label>(dashboard.status).unwrap();
    assert!(status.text() == "同期済み", "{}", status.text());
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
        text_of(&dashboard, |p| matches!(p, Pulse::RowTime(11))),
        "8 秒前に出力"
    );
    in_scope(&mut dashboard, &mut text, Scope::Ended);
    assert_eq!(
        text_of(&dashboard, |p| matches!(p, Pulse::RowTime(18))),
        "—"
    );
    assert_eq!(
        text_of(&dashboard, |p| matches!(p, Pulse::RowTime(17))),
        "4 分前に完了"
    );
    let tree = &dashboard.tree;
    let texts = texts_under(tree, tree.root().unwrap());
    assert!(!texts.iter().any(|t| t.contains("結果あり")), "{texts:?}");
}

#[test]
fn the_demo_ended_rows_keep_their_times_and_results() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    in_scope(&mut dashboard, &mut text, Scope::Ended);
    assert_eq!(
        text_of(&dashboard, |p| matches!(p, Pulse::RowTime(17))),
        "4 分前に完了"
    );
    let rows: Vec<Vec<String>> = rows_of(&dashboard)
        .into_iter()
        .map(|pane| texts_under(&dashboard.tree, row_of(&dashboard, pane)))
        .collect();
    let with_result = rows.iter().filter(|texts| texts[1] == "結果あり");
    assert_eq!(with_result.count(), 2, "{rows:?}");
}

#[test]
fn opening_without_the_control_scope_says_why_and_sends_nothing() {
    let (channel, snapshots, _notices, commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let read_only = Snapshot {
        can_focus: false,
        ..live(fleet::canned(), 0)
    };
    snapshots.send(read_only).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    let open = row_button(&dashboard, rows_of(&dashboard)[0]);
    let pos = center(&dashboard.tree, open);
    click(&mut dashboard, &mut text, pos);
    assert!(
        commands.try_iter().all(|c| !matches!(c, Command::Focus(_))),
        "a focus the server will refuse is not sent"
    );
    assert_eq!(
        notice_of(&dashboard),
        "ペインへ移動できません: Noa の server-scopes に control がありません（例: server-scopes = read,control）"
    );

    // The next snapshot keeps the reason up, beside the link state.
    snapshots
        .send(Snapshot {
            can_focus: false,
            ..live(fleet::canned(), 3)
        })
        .unwrap();
    dashboard.before_frame(&mut text);
    assert!(notice_of(&dashboard).contains("control"));
    let status = dashboard.tree.get::<Label>(dashboard.status).unwrap();
    assert_eq!(status.text(), "同期済み");
}

#[test]
fn a_focus_the_server_refused_replaces_the_promise_in_the_header() {
    let (channel, snapshots, notices, commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots.send(live(fleet::canned(), 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    let pane = rows_of(&dashboard)[0];
    let pos = center(&dashboard.tree, row_button(&dashboard, pane));
    click(&mut dashboard, &mut text, pos);
    assert!(commands.try_iter().any(|c| c == Command::Focus(pane)));
    notices
        .send("ペインへ移動できません: no such pane".into())
        .unwrap();
    dashboard.before_frame(&mut text);
    assert_eq!(
        notice_of(&dashboard),
        "ペインへ移動できません: no such pane"
    );
}

#[test]
fn the_drawer_worker_is_watched() {
    let (channel, snapshots, _notices, commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots.send(live(fleet::canned(), 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    // No drawer, no tail: a selection alone asks for nothing.
    dashboard.select(Some(12), &mut text);
    assert_eq!(commands.try_iter().last(), Some(Command::Watch(vec![])));
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.view.detail, Some(12));
    let last = commands.try_iter().last();
    assert_eq!(last, Some(Command::Watch(vec![12])));
}

#[test]
fn a_frame_reads_the_time_once() {
    let (channel, snapshots, notices, _commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let status = |d: &Dashboard| d.tree.get::<Label>(d.status).unwrap().text().to_owned();
    // Frames at times far from the system clock's: a part of the frame
    // that read the clock itself would be minutes off.
    let first = Instant::now() + Duration::from_secs(600);

    // The first snapshot rebuilds the screen.
    snapshots.send(live(fleet::canned(), 2)).unwrap();
    dashboard.frame(first, &mut text);
    assert_eq!(status(&dashboard), "同期済み");

    // A later one changes only times: in place.
    let second = first + Duration::from_secs(60);
    snapshots.send(live(later(&fleet::canned()), 3)).unwrap();
    dashboard.frame(second, &mut text);
    assert_eq!(status(&dashboard), "同期済み");

    // A notice that came shows for NOTICE_FOR from this frame on.
    let third = second + Duration::from_secs(60);
    notices
        .send("ペインへ移動できません: no such pane".into())
        .unwrap();
    dashboard.frame(third, &mut text);
    assert_eq!(
        notice_of(&dashboard),
        "ペインへ移動できません: no such pane"
    );
    assert_eq!(status(&dashboard), "最後の受信から 1 分（古いデータ）");
    dashboard.frame(third + NOTICE_FOR, &mut text);
    assert_eq!(notice_of(&dashboard), "");
    // 71 s without an update is stale (AC6).
    assert_eq!(status(&dashboard), "最後の受信から 1 分（古いデータ）");
}
