//! Workers whose panes are gone, rebuilt from the journals: listed under
//! 終了 with 閉じた, their results readable, and nothing sent to their
//! panes.

use std::sync::mpsc::Receiver;

use crate::dashboard::filter::matches;

use super::*;

const BENCH: u64 = (1 << 63) | 1;

fn live_closed() -> (Dashboard, TextStore, Receiver<Command>) {
    let (channel, snapshots, _notices, commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots
        .send(live(fleet::canned_with_closed(), 0))
        .unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    (dashboard, text, commands)
}

fn sent(commands: &Receiver<Command>) -> Vec<Command> {
    commands
        .try_iter()
        .filter(|c| !matches!(c, Command::Watch(_)))
        .collect()
}

#[test]
fn closed_workers_are_ended_and_say_so() {
    let (mut dashboard, mut text, _commands) = live_closed();
    assert!(!rows_of(&dashboard).contains(&BENCH), "not in 現在");
    in_scope(&mut dashboard, &mut text, Scope::Ended);
    let closed_rows: Vec<Vec<String>> = rows_of(&dashboard)
        .into_iter()
        .map(|pane| texts_under(&dashboard.tree, row_of(&dashboard, pane)))
        .filter(|texts| texts.iter().any(|t| t.contains("閉じた")))
        .collect();
    assert_eq!(closed_rows.len(), 2, "{closed_rows:?}");
    for texts in &closed_rows {
        assert_eq!(texts[0], "終了", "{texts:?}");
    }
    let fleet = fleet::canned_with_closed();
    let closed: Vec<u64> = fleet
        .workers
        .iter()
        .filter(|w| matches(w, "state:closed"))
        .map(|w| w.pane)
        .collect();
    assert_eq!(closed, [BENCH, (1 << 63) | 2]);
    assert!(
        fleet
            .workers
            .iter()
            .filter(|w| matches(w, "-state:閉じた"))
            .all(|w| !w.is_closed())
    );
}

#[test]
fn nothing_reaches_the_pane_of_a_closed_worker() {
    let (mut dashboard, mut text, commands) = live_closed();
    dashboard.select(Some(BENCH), &mut text);
    key(&mut dashboard, &mut text, Key::Enter);
    assert_eq!(notice_of(&dashboard), drawer::CLOSED_PANE);
    stroke(&mut dashboard, &mut text, 's');
    assert_eq!(notice_of(&dashboard), drawer::CLOSED_SEND_GUARD);
    key(&mut dashboard, &mut text, Key::Char('x'));
    key(&mut dashboard, &mut text, Key::Char('x'));
    assert_eq!(notice_of(&dashboard), drawer::CLOSED_PANE);
    assert!(sent(&commands).is_empty(), "{:?}", sent(&commands));

    // The drawer: no 開く, no ペインを閉じる, no send field, and its pane's
    // screen is never asked for.
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.view.detail, Some(BENCH));
    assert!(dashboard.close_pane_button.is_none());
    assert!(dashboard.open_buttons.is_empty());
    assert!(dashboard.send_field.is_none());
    let watched: Vec<Vec<u64>> = commands
        .try_iter()
        .filter_map(|c| match c {
            Command::Watch(panes) => Some(panes),
            _ => None,
        })
        .collect();
    assert!(
        watched.iter().flatten().all(|pane| *pane != BENCH),
        "{watched:?}"
    );
    let drawer = dashboard.drawer.unwrap().0;
    let labels = labels_under(&dashboard.tree, drawer);
    assert!(
        labels.iter().any(|l| l == "ID 21 · 親 perf-wish"),
        "{labels:?}"
    );
    assert!(
        labels.iter().any(|l| l == drawer::CLOSED_PANE),
        "{labels:?}"
    );

    // A bulk close leaves them out: their panes are gone already.
    key(&mut dashboard, &mut text, Key::Escape);
    for _ in 0..2 {
        // Shift up between the presses (review #2).
        dashboard.dispatch(Event::ModifiersChanged(Modifiers::default()), &mut text);
        dashboard.dispatch(
            Event::KeyDown {
                key: Key::Char('x'),
                modifiers: Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
            },
            &mut text,
        );
        dashboard.dispatch(Event::Text("X".into()), &mut text);
    }
    let many: Vec<Vec<u64>> = sent(&commands)
        .into_iter()
        .filter_map(|c| match c {
            Command::CloseMany { targets, .. } => {
                Some(targets.iter().map(|t| t.pane).collect::<Vec<u64>>())
            }
            _ => None,
        })
        .collect();
    assert_eq!(many, [vec![17, 18, 19]]);
}

#[test]
fn a_closed_workers_result_is_read_in_the_drawer() {
    let path = std::env::temp_dir().join(format!(
        "agent-dashboard-closed-result-{}.md",
        std::process::id()
    ));
    let long: String = (0..300).map(|n| format!("line {n}\n")).collect();
    std::fs::write(&path, &long).unwrap();
    let mut fleet = fleet::canned_with_closed();
    let worker = fleet.workers.iter_mut().find(|w| w.pane == BENCH).unwrap();
    worker.result = Some(path.clone());
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    dashboard.select(Some(BENCH), &mut text);
    key(&mut dashboard, &mut text, Key::Space);
    let toggle = dashboard.result_toggle.expect("a 表示 button");
    let pos = center(&dashboard.tree, toggle);
    click(&mut dashboard, &mut text, pos);
    std::fs::remove_file(&path).unwrap();
    let drawer = dashboard.drawer.unwrap().0;
    let labels = labels_under(&dashboard.tree, drawer);
    assert!(labels.iter().any(|l| l == "line 0"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "line 199"));
    assert!(!labels.iter().any(|l| l == "line 200"), "200 lines at most");
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("先頭だけを表示しています"))
    );
}

/// Their spawn and close feed the statistics: counted done, their time
/// worked in the average and the phase totals.
#[test]
fn closed_workers_count_in_the_statistics() {
    let canned = fleet::canned();
    let with = fleet::canned_with_closed();
    let before = analytics::compute_metrics(&canned.workers.iter().collect::<Vec<_>>());
    let after = analytics::compute_metrics(&with.workers.iter().collect::<Vec<_>>());
    assert_eq!(after.finished, before.finished + 2);
    let running = |m: &analytics::FleetMetrics| m.phase_minutes[0].1;
    assert_eq!(running(&after), running(&before) + 18.0);
    assert_ne!(after.avg_took_minutes, before.avg_took_minutes);
}

/// Review #8: a closed failure stays failed — in the statistics and the
/// state filter — yet is listed under 終了 (失敗 · 閉じた), never 要対応.
#[test]
fn a_closed_failure_is_ended_and_counted_failed() {
    let mut fleet = fleet::canned_with_closed();
    let bench = fleet.workers.iter_mut().find(|w| w.pane == BENCH).unwrap();
    bench.state = State::Error;
    let mut dashboard = Dashboard::of_snapshot(Grouping::Flat, live(fleet.clone(), 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    assert!(!rows_of(&dashboard).contains(&BENCH), "not in 現在");
    in_scope(&mut dashboard, &mut text, Scope::Ended);
    let texts = texts_under(&dashboard.tree, row_of(&dashboard, BENCH));
    assert_eq!(texts[0], "失敗", "{texts:?}");
    assert!(texts[1].contains("閉じた"), "{texts:?}");
    assert!(dashboard.order().contains(&BENCH), "J / K reach it");
    let metrics = analytics::compute_metrics(&fleet.workers.iter().collect::<Vec<_>>());
    assert_eq!(metrics.error, 2, "e2e-smoke and bench-ascii");
    assert!(matches(fleet.worker(BENCH).unwrap(), "state:error"));
}
