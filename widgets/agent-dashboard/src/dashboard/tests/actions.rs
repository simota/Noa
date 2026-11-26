use std::collections::BTreeSet;

use noa_gpui_core::clipboard::MemoryClipboard;
use noa_gpui_core::widgets::{Label, TextField};

use crate::dashboard::filter::matches;

use super::*;

fn click_id(dashboard: &mut Dashboard, text: &mut TextStore, id: WidgetId) {
    let pos = center(&dashboard.tree, id);
    click(dashboard, text, pos);
}

fn matching(query: &str) -> BTreeSet<u64> {
    fleet::canned()
        .workers
        .iter()
        .filter(|w| matches(w, query))
        .map(|w| w.pane)
        .collect()
}

#[test]
fn structured_filter_supports_qualifiers_and_negation() {
    let set = |panes: &[u64]| panes.iter().copied().collect::<BTreeSet<u64>>();
    assert_eq!(matching("state:done"), set(&[17, 18]));
    assert_eq!(matching("state:完了"), set(&[17, 18]));
    assert_eq!(matching("state:running"), set(&[11, 12, 13]));
    assert_eq!(matching("state:permission"), set(&[14, 15]));
    assert_eq!(matching("state:input"), set(&[16]));
    assert_eq!(matching("state:error"), set(&[19]));
    assert_eq!(matching("state:quiet"), set(&[13]));
    assert_eq!(matching("state:waiting"), set(&[14, 15, 16, 19]));

    assert_eq!(matching("need:approval"), set(&[14, 15]));
    assert_eq!(matching("need:承認"), set(&[14, 15]));
    assert_eq!(matching("need:question"), set(&[16]));
    assert_eq!(matching("need:failure"), set(&[19]));
    assert_eq!(matching("need:decision"), set(&[14, 15, 16]));

    assert_eq!(matching("parent:perf-wish"), set(&[13, 15, 19]));
    assert_eq!(
        matching("parent:release -state:done"),
        set(&[11, 12, 14, 16])
    );
    assert_eq!(matching("-agent:codex state:running"), set(&[11, 12]));
    assert_eq!(matching("result:yes"), set(&[17, 18]));
    assert_eq!(matching("result:no parent:perf"), set(&[13, 15, 19]));
    // Unknown `key:value` falls back to plain substring matching.
    assert!(matching("unknown:xyz").is_empty());
}

#[test]
fn n_key_cycles_through_waiting_workers() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    // Queue order in canned fleet (info folded): 14, 15, 16, 19.
    for expected in [14, 15, 16, 19, 14] {
        key(&mut dashboard, &mut text, Key::Char('n'));
        assert_eq!(dashboard.view.selected, Some(expected));
    }
    // When filter excludes all waiting workers, N reports it in the notice line.
    let mut filtered = Dashboard::new(Grouping::Orchestrator).with_filter("state:done".into());
    filtered.prepare(window(), &mut text);
    key(&mut filtered, &mut text, Key::Char('n'));
    assert_eq!(notice_of(&filtered), "要対応のワーカーはありません");
}

#[test]
fn g_key_cycles_grouping() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    for expected in [
        Grouping::Agent,
        Grouping::Model,
        Grouping::Flat,
        Grouping::Orchestrator,
    ] {
        key(&mut dashboard, &mut text, Key::Char('g'));
        assert_eq!(dashboard.view.grouping, expected);
        assert_eq!(
            notice_of(&dashboard),
            format!("グループ: {}", expected.title())
        );
    }
}

#[test]
fn send_key_opens_drawer_and_submits_message_with_draft_preservation() {
    let (channel, snapshots, _notices, commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots.send(live(fleet::canned(), 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);

    // Select running worker 11 (api-refactor).
    dashboard.select(Some(11), &mut text);
    dashboard.prepare(window(), &mut text);
    stroke(&mut dashboard, &mut text, 's');
    assert_eq!(dashboard.view.detail, Some(11));
    let (field, pane) = dashboard.registry.send_field.expect("send field");
    assert_eq!(pane, 11);
    assert_eq!(dashboard.tree.focused(), Some(field));

    typed(&mut dashboard, &mut text, "進捗を教えて");
    assert_eq!(
        dashboard.drafts.get(&11).map(String::as_str),
        Some("進捗を教えて")
    );

    // Switching away and back keeps the draft.
    key(&mut dashboard, &mut text, Key::Escape);
    assert_eq!(dashboard.tree.focused(), None);
    dashboard.select(Some(12), &mut text);
    dashboard.select(Some(11), &mut text);
    dashboard.prepare(window(), &mut text);
    let (field, _) = dashboard.registry.send_field.expect("send field");
    assert_eq!(
        dashboard.tree.get::<TextField>(field).unwrap().text(),
        "進捗を教えて"
    );

    // Focus again and press Enter to send.
    stroke(&mut dashboard, &mut text, 's');
    key(&mut dashboard, &mut text, Key::Enter);
    let sent: Vec<Command> = commands
        .try_iter()
        .filter(|c| matches!(c, Command::Send { .. }))
        .collect();
    assert_eq!(
        sent,
        vec![Command::Send {
            instance: INSTANCE.to_owned(),
            pane: 11,
            text: "進捗を教えて".to_owned(),
        }]
    );
    assert!(!dashboard.drafts.contains_key(&11));
    assert_eq!(notice_of(&dashboard), "api-refactor へ送信しました");
}

#[test]
fn permission_worker_cannot_be_sent_messages_and_missing_scope_keeps_draft() {
    let (channel, snapshots, _notices, commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots
        .send(Snapshot {
            can_input: false,
            safe_send: true,
            ..live(fleet::canned(), 0)
        })
        .unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);

    // Permission worker 14 is blocked by safety guard.
    dashboard.select(Some(14), &mut text);
    stroke(&mut dashboard, &mut text, 's');
    assert_eq!(notice_of(&dashboard), drawer::PERMISSION_SEND_GUARD);

    // Worker 11 without `input` scope refuses sending and preserves draft.
    dashboard.select(Some(11), &mut text);
    stroke(&mut dashboard, &mut text, 's');
    typed(&mut dashboard, &mut text, "hello");
    key(&mut dashboard, &mut text, Key::Enter);
    assert!(
        commands
            .try_iter()
            .all(|c| !matches!(c, Command::Send { .. }))
    );
    assert!(notice_of(&dashboard).contains("input"));
    assert_eq!(dashboard.drafts.get(&11).map(String::as_str), Some("hello"));
}

#[test]
fn close_pane_requires_two_step_confirmation_and_supports_cancel() {
    // Live mode: two presses send Command::Close; Esc cancels first press.
    let (channel, snapshots, _notices, commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots.send(live(fleet::canned(), 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);

    dashboard.select(Some(17), &mut text);
    key(&mut dashboard, &mut text, Key::Char('x'));
    assert_eq!(dashboard.view.confirm_close, Some(17));
    assert!(
        commands
            .try_iter()
            .all(|c| !matches!(c, Command::Close { .. }))
    );

    // Esc cancels confirmation.
    key(&mut dashboard, &mut text, Key::Escape);
    assert_eq!(dashboard.view.confirm_close, None);
    assert_eq!(notice_of(&dashboard), "ペイン終了をキャンセルしました");

    // Two X presses send Command::Close(17).
    key(&mut dashboard, &mut text, Key::Char('x'));
    key(&mut dashboard, &mut text, Key::Char('x'));
    let closed: Vec<u64> = commands
        .try_iter()
        .filter_map(|c| match c {
            Command::Close { pane, .. } => Some(pane),
            _ => None,
        })
        .collect();
    assert_eq!(closed, vec![17]);

    // Demo mode: two presses remove the worker from the fleet.
    let (mut demo, mut text) = laid_out(Grouping::Orchestrator);
    demo.select(Some(17), &mut text);
    key(&mut demo, &mut text, Key::Char('x'));
    key(&mut demo, &mut text, Key::Char('x'));
    assert!(demo.feed.fleet.worker(17).is_none());
    assert_eq!(notice_of(&demo), "lint-pass のペインを閉じました");
}

#[test]
fn drawer_copies_screen_tail_and_reloads_and_copies_result() {
    let path = std::env::temp_dir().join(format!(
        "agent-dashboard-actions-result-{}.md",
        std::process::id()
    ));
    std::fs::write(&path, b"first version\n").unwrap();
    let mut fleet = fleet::canned();
    let worker = fleet.workers.iter_mut().find(|w| w.pane == 17).unwrap();
    worker.result = Some(path.clone());
    worker.screen_tail = vec!["line A".into(), "line B".into()];
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    dashboard
        .tree_mut()
        .set_clipboard(Box::new(MemoryClipboard::default()));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    dashboard.set_detail(Some(17), &mut text);
    dashboard.prepare(window(), &mut text);

    // Copy screen tail.
    let tail_copy = dashboard.registry.tail_copy.expect("tail copy button");
    let pos = center(&dashboard.tree, tail_copy);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(
        dashboard.tree_mut().clipboard().read_text().as_deref(),
        Some("line A\nline B")
    );

    // Show result, copy it, update file on disk, then reload it.
    let toggle = dashboard.registry.result_toggle.expect("result toggle");
    click_id(&mut dashboard, &mut text, toggle);
    let copy_btn = dashboard.registry.result_copy.expect("result copy button");
    click_id(&mut dashboard, &mut text, copy_btn);
    assert_eq!(
        dashboard.tree_mut().clipboard().read_text().as_deref(),
        Some("first version")
    );

    std::fs::write(&path, b"second version\n").unwrap();
    let reload_btn = dashboard
        .registry
        .result_reload
        .expect("result reload button");
    click_id(&mut dashboard, &mut text, reload_btn);
    let (drawer_root, _) = dashboard.registry.drawer.unwrap();
    let labels = labels_under(&dashboard.tree, drawer_root);
    assert!(labels.contains(&"second version".to_owned()), "{labels:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn clicking_urgency_count_toggles_filter_and_clicking_lane_or_node_selects_worker() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    // Click "失敗 1" in header.
    let failure_label = dashboard
        .header
        .urgency_labels
        .iter()
        .find(|(_, need)| *need == fleet::Need::Failure)
        .unwrap()
        .0;
    click_id(&mut dashboard, &mut text, failure_label);
    assert_eq!(dashboard.filter.query, "need:failure");
    let count_text = dashboard
        .tree
        .get::<Label>(dashboard.header.match_count)
        .unwrap()
        .text();
    assert_eq!(count_text, "1 / 9 件");

    // Clicking the same urgency count again clears the filter.
    let failure_label = dashboard
        .header
        .urgency_labels
        .iter()
        .find(|(_, need)| *need == fleet::Need::Failure)
        .unwrap()
        .0;
    click_id(&mut dashboard, &mut text, failure_label);
    assert_eq!(dashboard.filter.query, "");

    // Click a timeline lane (pane 12) to select it.
    let lane_12 = dashboard
        .registry
        .marks
        .iter()
        .find(|(_, pane, mark)| *pane == 12 && matches!(mark, Mark::Lane))
        .unwrap()
        .0;
    click_id(&mut dashboard, &mut text, lane_12);
    assert_eq!(dashboard.view.selected, Some(12));

    // Click a graph node (pane 17) to select it.
    let node_17 = dashboard
        .registry
        .marks
        .iter()
        .find(|(_, pane, mark)| *pane == 17 && matches!(mark, Mark::Node { .. }))
        .unwrap()
        .0;
    click_id(&mut dashboard, &mut text, node_17);
    assert_eq!(dashboard.view.selected, Some(17));
}

#[test]
fn with_filter_applies_initial_query_for_cli() {
    let mut dashboard =
        Dashboard::new(Grouping::Orchestrator).with_filter("parent:perf -state:error".into());
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    assert_eq!(dashboard.filter.query, "parent:perf -state:error");
    assert_eq!(
        dashboard
            .feed
            .fleet
            .workers
            .iter()
            .filter(|w| dashboard.filter.matches(w))
            .map(|w| w.pane)
            .collect::<Vec<_>>(),
        vec![13, 15]
    );
}

/// A live dashboard that took in the canned fleet from `INSTANCE`.
fn live_dashboard() -> (
    Dashboard,
    TextStore,
    std::sync::mpsc::Sender<Snapshot>,
    std::sync::mpsc::Receiver<Command>,
) {
    let (channel, snapshots, _notices, commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots.send(live(fleet::canned(), 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    (dashboard, text, snapshots, commands)
}

fn send_field_text(dashboard: &Dashboard) -> String {
    let (field, _) = dashboard.registry.send_field.expect("send field");
    dashboard
        .tree
        .get::<TextField>(field)
        .unwrap()
        .text()
        .to_owned()
}

fn closed(commands: &std::sync::mpsc::Receiver<Command>) -> Vec<u64> {
    commands
        .try_iter()
        .filter_map(|c| match c {
            Command::Close { pane, .. } => Some(pane),
            _ => None,
        })
        .collect()
}

/// S as the platform sends it (`KeyDown`, then `Text("s")`) opens the send
/// field empty: the key's own text does not land in it.
#[test]
fn s_opens_the_send_field_without_typing_an_s_into_it() {
    let (mut dashboard, mut text, _snapshots, _commands) = live_dashboard();
    dashboard.select(Some(11), &mut text);
    stroke(&mut dashboard, &mut text, 's');
    let (field, _) = dashboard.registry.send_field.expect("send field");
    assert_eq!(dashboard.tree.focused(), Some(field));
    assert_eq!(send_field_text(&dashboard), "");
    assert!(!dashboard.drafts.contains_key(&11));
}

/// Pane ids of a restarted Noa name other panes: a draft written for the
/// old pane 11 must not wait in the new pane 11's send field.
#[test]
fn a_new_server_instance_drops_the_drafts() {
    let (mut dashboard, mut text, snapshots, _commands) = live_dashboard();
    dashboard.select(Some(11), &mut text);
    stroke(&mut dashboard, &mut text, 's');
    typed(&mut dashboard, &mut text, "古い指示");
    key(&mut dashboard, &mut text, Key::Escape);
    assert!(dashboard.drafts.contains_key(&11));

    snapshots
        .send(Snapshot {
            instance: Some("restarted".to_owned()),
            ..live(fleet::canned(), 0)
        })
        .unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    assert!(dashboard.drafts.is_empty(), "{:?}", dashboard.drafts);
    dashboard.set_detail(Some(11), &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(send_field_text(&dashboard), "");
}

/// A live rebuild while the help is open keeps a drawer with a draft under
/// the help, disabled, as it keeps one without.
#[test]
fn a_rebuild_under_the_help_keeps_a_drafted_drawer_shielded() {
    let (mut dashboard, mut text, snapshots, _commands) = live_dashboard();
    dashboard.select(Some(11), &mut text);
    stroke(&mut dashboard, &mut text, 's');
    typed(&mut dashboard, &mut text, "draft");
    key(&mut dashboard, &mut text, Key::Escape);
    typed(&mut dashboard, &mut text, "?");
    assert!(dashboard.view.help);

    let mut fleet = fleet::canned();
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 12)
        .unwrap()
        .label = "docs-sync-2".into();
    snapshots.send(live(fleet, 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);

    let (drawer, _) = dashboard.registry.drawer.expect("drawer");
    let (help, _) = dashboard.registry.help.expect("help");
    assert!(
        !dashboard.tree.is_enabled(drawer),
        "drawer under help is disabled"
    );
    assert_eq!(dashboard.tree.overlays().last(), Some(help), "help on top");
    assert_eq!(send_field_text(&dashboard), "draft");
}

/// The close confirmation holds only for the very next action, and only
/// while its notice shows.
#[test]
fn the_close_confirmation_does_not_outlive_the_next_action_or_its_notice() {
    // Another action in between.
    let (mut dashboard, mut text, _snapshots, commands) = live_dashboard();
    dashboard.select(Some(17), &mut text);
    key(&mut dashboard, &mut text, Key::Char('x'));
    key(&mut dashboard, &mut text, Key::Char('g'));
    key(&mut dashboard, &mut text, Key::Char('x'));
    assert!(
        closed(&commands).is_empty(),
        "G in between: X only asks again"
    );
    assert_eq!(dashboard.view.confirm_close, Some(17));

    // The selection moved away by a filter and came back.
    let (mut dashboard, mut text, _snapshots, commands) = live_dashboard();
    dashboard.select(Some(17), &mut text);
    key(&mut dashboard, &mut text, Key::Char('x'));
    typed(&mut dashboard, &mut text, "/");
    typed(&mut dashboard, &mut text, "docs-sync");
    assert_ne!(dashboard.view.selected, Some(17));
    key(&mut dashboard, &mut text, Key::Escape);
    dashboard.select(Some(17), &mut text);
    key(&mut dashboard, &mut text, Key::Char('x'));
    assert!(
        closed(&commands).is_empty(),
        "back on 17: X only asks again"
    );

    // The notice ended.
    let (mut dashboard, mut text, _snapshots, commands) = live_dashboard();
    dashboard.select(Some(17), &mut text);
    key(&mut dashboard, &mut text, Key::Char('x'));
    dashboard.frame(
        Instant::now() + NOTICE_FOR + Duration::from_secs(1),
        &mut text,
    );
    assert_eq!(dashboard.view.confirm_close, None);
    key(&mut dashboard, &mut text, Key::Char('x'));
    assert!(
        closed(&commands).is_empty(),
        "after the notice: X only asks again"
    );
}

/// Each worker's send field puts the caret at the end of its own draft,
/// never at a byte offset carried over from another worker's.
#[test]
fn the_send_caret_does_not_carry_over_to_another_workers_draft() {
    let (mut dashboard, mut text, _snapshots, _commands) = live_dashboard();
    dashboard.select(Some(12), &mut text);
    stroke(&mut dashboard, &mut text, 's');
    typed(&mut dashboard, &mut text, "あいう");
    key(&mut dashboard, &mut text, Key::Escape);
    dashboard.select(Some(11), &mut text);
    stroke(&mut dashboard, &mut text, 's');
    typed(&mut dashboard, &mut text, "abcd");
    key(&mut dashboard, &mut text, Key::Escape);

    dashboard.select(Some(12), &mut text);
    dashboard.prepare(window(), &mut text);
    let (field, pane) = dashboard.registry.send_field.expect("send field");
    assert_eq!(pane, 12);
    let field = dashboard.tree.get::<TextField>(field).unwrap();
    assert_eq!(field.text(), "あいう");
    assert_eq!(field.caret(), "あいう".len());
}

/// A message the feed gives back while the help is open becomes the draft
/// under the help: the drawer stays below it and disabled.
#[test]
fn a_message_given_back_under_the_help_keeps_the_drawer_shielded() {
    let (channel, snapshots, _notices, unsent, _commands) = Channel::detached_with_unsent();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots.send(live(fleet::canned(), 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    dashboard.select(Some(11), &mut text);
    stroke(&mut dashboard, &mut text, 's');
    typed(&mut dashboard, &mut text, "msg");
    key(&mut dashboard, &mut text, Key::Enter);
    typed(&mut dashboard, &mut text, "?");
    assert!(dashboard.view.help);

    unsent
        .send(crate::live::Unsent {
            instance: INSTANCE.to_owned(),
            pane: 11,
            text: "msg".to_owned(),
        })
        .unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);

    let (drawer, _) = dashboard.registry.drawer.expect("drawer");
    let (help, _) = dashboard.registry.help.expect("help");
    assert!(
        !dashboard.tree.is_enabled(drawer),
        "drawer under help is disabled"
    );
    assert_eq!(dashboard.tree.overlays().last(), Some(help), "help on top");
    assert_eq!(send_field_text(&dashboard), "msg");
}

/// A message given back for a pane of another server instance is nobody's
/// draft here: the same id names another pane.
#[test]
fn a_message_given_back_for_another_instance_is_dropped() {
    let (channel, snapshots, _notices, unsent, _commands) = Channel::detached_with_unsent();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots.send(live(fleet::canned(), 0)).unwrap();
    dashboard.before_frame(&mut text);
    unsent
        .send(crate::live::Unsent {
            instance: "before-restart".to_owned(),
            pane: 11,
            text: "old".to_owned(),
        })
        .unwrap();
    dashboard.before_frame(&mut text);
    assert!(dashboard.drafts.is_empty(), "{:?}", dashboard.drafts);
}
