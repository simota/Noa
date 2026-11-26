//! ⇧X and the drawer's まとめて閉じる: every ended worker the filter shows,
//! closed after a second press.

use std::sync::mpsc::{Receiver, Sender};

use noa_gpui_core::widgets::Button;

use super::*;

/// ⇧X as the platform sends it: Shift goes down, the key `x` with Shift
/// held and its text "X", then Shift comes up.
pub(super) fn shift_x(dashboard: &mut Dashboard, text: &mut TextStore) {
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    dashboard.dispatch(Event::ModifiersChanged(shift), text);
    repeat_x(dashboard, text);
    dashboard.dispatch(Event::ModifiersChanged(Modifiers::default()), text);
    dashboard.prepare(window(), text);
}

/// What ⇧X held down sends again and again: the key and its text, with no
/// change of the modifiers in between.
fn repeat_x(dashboard: &mut Dashboard, text: &mut TextStore) {
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
            Command::CloseMany { instance, targets } => {
                assert_eq!(instance, INSTANCE);
                // Each as the dashboard showed it (review #1).
                let canned = fleet::canned();
                for target in &targets {
                    if let Some(shown) = canned.worker(target.pane) {
                        assert_eq!((target.state, target.seq), (shown.state, shown.reports));
                    }
                }
                Some(targets.iter().map(|t| t.pane).collect())
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

/// Review #5: a rebuild while a bulk close waits (a worker came, the ended
/// ones are the same) keeps the drawer's button saying what its next click
/// does.
#[test]
fn a_rebuild_keeps_the_armed_button_armed() {
    let (mut dashboard, mut text, snapshots, commands) = live_canned();
    dashboard.select(Some(11), &mut text);
    key(&mut dashboard, &mut text, Key::Space);
    let (button, _) = dashboard.close_ended_button.unwrap();
    let pos = center(&dashboard.tree, button);
    click(&mut dashboard, &mut text, pos);
    let mut fleet = fleet::canned();
    let mut newcomer = fleet.workers[0].clone();
    newcomer.pane = 30;
    newcomer.label = "newcomer".into();
    fleet.workers.push(newcomer);
    snapshots.send(live(fleet, 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    let (button, _) = dashboard.close_ended_button.expect("still offered");
    let armed = dashboard.tree.get::<Button>(button).unwrap();
    assert_eq!(armed.label(), "本当にまとめて閉じる（3 件）");
    assert!(closed_many(&commands).is_empty());

    // Other ended workers since: the button asks again.
    let mut fleet = fleet::canned();
    fleet.workers.retain(|w| w.pane != 18);
    snapshots.send(live(fleet, 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    let (button, _) = dashboard.close_ended_button.unwrap();
    let label = dashboard
        .tree
        .get::<Button>(button)
        .unwrap()
        .label()
        .to_owned();
    assert_eq!(label, "完了・失敗をまとめて閉じる（2 件）");
}

/// Review #15: a link that went down after the workers were shown: two
/// presses close nothing and say why.
#[test]
fn a_bulk_close_while_the_link_is_down_closes_nothing() {
    let (mut dashboard, mut text, snapshots, commands) = live_canned();
    snapshots
        .send(Snapshot {
            fleet: Fleet::default(),
            link: Link::Down("gone".into()),
            synced_secs: 0,
            can_focus: false,
            can_input: false,
            safe_send: false,
            instance: None,
        })
        .unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    assert!(
        dashboard.feed.fleet.worker(17).is_some(),
        "the workers stay shown"
    );
    shift_x(&mut dashboard, &mut text);
    shift_x(&mut dashboard, &mut text);
    assert!(closed_many(&commands).is_empty());
    assert_eq!(
        notice_of(&dashboard),
        "ペインを閉じられません: Noa に接続していません"
    );
}

/// Review #2: holding ⇧X down (key repeat) never confirms: the platform
/// marks no repeat, so a second press counts only after Shift came up.
#[test]
fn holding_shift_x_down_closes_nothing() {
    let (mut dashboard, mut text, _snapshots, commands) = live_canned();
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    dashboard.dispatch(Event::ModifiersChanged(shift), &mut text);
    for _ in 0..20 {
        repeat_x(&mut dashboard, &mut text);
    }
    assert!(closed_many(&commands).is_empty());
    assert!(dashboard.close_ended.is_some(), "still waiting for a press");
    // Shift up, then a press: that one confirms.
    dashboard.dispatch(Event::ModifiersChanged(Modifiers::default()), &mut text);
    shift_x(&mut dashboard, &mut text);
    assert_eq!(closed_many(&commands), [vec![17, 18, 19]]);
}
