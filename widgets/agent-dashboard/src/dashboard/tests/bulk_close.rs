//! ⇧X and the drawer's まとめて閉じる: every ended worker the filter shows,
//! closed after a second press.

use std::sync::mpsc::{Receiver, Sender};

use noa_gpui_core::widgets::Button;

use super::*;

/// ⇧X as the platform sends it: the key `x` with Shift held, then "X".
fn shift_x(dashboard: &mut Dashboard, text: &mut TextStore) {
    let modifiers = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    dashboard.dispatch(
        Event::KeyDown {
            key: Key::Char('x'),
            modifiers,
        },
        text,
    );
    dashboard.dispatch(Event::Text("X".into()), text);
    dashboard.prepare(window(), text);
}

fn filtered(dashboard: &mut Dashboard, text: &mut TextStore, query: &str) {
    typed(dashboard, text, "/");
    typed(dashboard, text, query);
    key(dashboard, text, Key::Enter);
}

fn live_canned() -> (Dashboard, TextStore, Sender<Snapshot>, Receiver<Command>) {
    let (channel, snapshots, _notices, commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots.send(live(fleet::canned(), 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    (dashboard, text, snapshots, commands)
}

/// The panes of each `CloseMany` sent, and whether any single close was.
fn closed_many(commands: &Receiver<Command>) -> Vec<Vec<u64>> {
    commands
        .try_iter()
        .filter_map(|c| match c {
            Command::CloseMany { instance, panes } => {
                assert_eq!(instance, INSTANCE);
                Some(panes)
            }
            Command::Close { .. } => panic!("a bulk close sends no single close"),
            _ => None,
        })
        .collect()
}

fn panes(dashboard: &Dashboard) -> Vec<u64> {
    dashboard
        .feed
        .fleet
        .workers
        .iter()
        .map(|w| w.pane)
        .collect()
}

#[test]
fn two_shift_x_close_the_ended_workers_the_filter_shows_and_no_other() {
    let (mut demo, mut text) = laid_out(Grouping::Orchestrator);
    filtered(&mut demo, &mut text, "state:done");
    shift_x(&mut demo, &mut text);
    assert_eq!(panes(&demo).len(), 9, "the first press closes nothing");
    let asked = notice_of(&demo);
    assert!(asked.contains("2 件"), "{asked}");
    assert!(asked.contains("lint-pass、changelog"), "{asked}");
    shift_x(&mut demo, &mut text);
    assert_eq!(panes(&demo), [11, 12, 13, 14, 15, 16, 19]);
    assert_eq!(notice_of(&demo), "2 件のペインを閉じました");

    // With no filter the failed one goes too; the running, the quiet and
    // those waiting on a person stay.
    key(&mut demo, &mut text, Key::Escape);
    shift_x(&mut demo, &mut text);
    shift_x(&mut demo, &mut text);
    assert_eq!(panes(&demo), [11, 12, 13, 14, 15, 16]);
}

#[test]
fn the_live_close_is_one_command_for_the_instance_shown() {
    let (mut dashboard, mut text, _snapshots, commands) = live_canned();
    shift_x(&mut dashboard, &mut text);
    assert!(closed_many(&commands).is_empty());
    shift_x(&mut dashboard, &mut text);
    assert_eq!(closed_many(&commands), [vec![17, 18, 19]]);
    assert_eq!(notice_of(&dashboard), "3 件のペインを閉じます");
}

#[test]
fn the_confirmation_holds_only_for_the_next_action_and_its_notice() {
    // Another action in between.
    let (mut dashboard, mut text, _snapshots, commands) = live_canned();
    shift_x(&mut dashboard, &mut text);
    key(&mut dashboard, &mut text, Key::Char('j'));
    shift_x(&mut dashboard, &mut text);
    assert!(closed_many(&commands).is_empty(), "J in between");

    // Esc says it was cancelled.
    let (mut dashboard, mut text, _snapshots, commands) = live_canned();
    shift_x(&mut dashboard, &mut text);
    key(&mut dashboard, &mut text, Key::Escape);
    assert_eq!(
        notice_of(&dashboard),
        "まとめて閉じるのをキャンセルしました"
    );
    shift_x(&mut dashboard, &mut text);
    assert!(closed_many(&commands).is_empty(), "Esc in between");

    // The notice ended.
    let (mut dashboard, mut text, _snapshots, commands) = live_canned();
    shift_x(&mut dashboard, &mut text);
    dashboard.frame(
        Instant::now() + NOTICE_FOR + Duration::from_secs(1),
        &mut text,
    );
    shift_x(&mut dashboard, &mut text);
    assert!(closed_many(&commands).is_empty(), "after the notice");

    // X on one worker in between takes the bulk close back.
    let (mut dashboard, mut text, _snapshots, commands) = live_canned();
    shift_x(&mut dashboard, &mut text);
    dashboard.select(Some(17), &mut text);
    key(&mut dashboard, &mut text, Key::Char('x'));
    shift_x(&mut dashboard, &mut text);
    assert!(closed_many(&commands).is_empty(), "X in between");
    assert_eq!(dashboard.view.confirm_close, None);
}

/// A live update between the presses changed which workers have ended:
/// the second press names the new ones instead of closing.
#[test]
fn other_ended_workers_at_the_second_press_are_named_again() {
    let (mut dashboard, mut text, snapshots, commands) = live_canned();
    shift_x(&mut dashboard, &mut text);
    let mut fleet = fleet::canned();
    fleet.workers.retain(|w| w.pane != 18);
    snapshots.send(live(fleet, 0)).unwrap();
    dashboard.before_frame(&mut text);
    shift_x(&mut dashboard, &mut text);
    assert!(closed_many(&commands).is_empty());
    assert!(
        notice_of(&dashboard).contains("2 件"),
        "{}",
        notice_of(&dashboard)
    );
    shift_x(&mut dashboard, &mut text);
    assert_eq!(closed_many(&commands), [vec![17, 19]]);
}

#[test]
fn nothing_ended_in_the_filter_is_said_and_closes_nothing() {
    let (mut dashboard, mut text, _snapshots, commands) = live_canned();
    filtered(&mut dashboard, &mut text, "state:running");
    shift_x(&mut dashboard, &mut text);
    shift_x(&mut dashboard, &mut text);
    assert!(closed_many(&commands).is_empty());
    assert_eq!(
        notice_of(&dashboard),
        "まとめて閉じる完了・失敗のワーカーはありません"
    );
}

#[test]
fn the_drawers_button_closes_them_after_a_second_click() {
    let (mut dashboard, mut text, _snapshots, commands) = live_canned();
    dashboard.select(Some(11), &mut text);
    key(&mut dashboard, &mut text, Key::Space);
    let (button, count) = dashboard.close_ended_button.expect("the drawer offers it");
    assert_eq!(count, 3);
    let pos = center(&dashboard.tree, button);
    click(&mut dashboard, &mut text, pos);
    assert!(closed_many(&commands).is_empty());
    assert!(dashboard.drawer.is_some(), "the drawer stays open");
    let (button, _) = dashboard.close_ended_button.unwrap();
    let armed = dashboard.tree.get::<Button>(button).unwrap();
    assert_eq!(armed.label(), "本当にまとめて閉じる（3 件）");
    assert!(armed.is_selected());
    let pos = center(&dashboard.tree, button);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(closed_many(&commands), [vec![17, 18, 19]]);

    // A filter with none ended: no such button.
    filtered(&mut dashboard, &mut text, "state:running");
    assert!(dashboard.view.detail.is_some());
    assert!(dashboard.close_ended_button.is_none());
}

#[test]
fn a_down_link_or_no_control_scope_closes_nothing() {
    let (mut dashboard, mut text, snapshots, commands) = live_canned();
    let mut snapshot = live(fleet::canned(), 0);
    snapshot.can_focus = false;
    snapshots.send(snapshot).unwrap();
    dashboard.before_frame(&mut text);
    shift_x(&mut dashboard, &mut text);
    shift_x(&mut dashboard, &mut text);
    assert!(closed_many(&commands).is_empty());
    assert!(
        notice_of(&dashboard).contains("control"),
        "{}",
        notice_of(&dashboard)
    );
}
