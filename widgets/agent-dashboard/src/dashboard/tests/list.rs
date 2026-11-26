//! dashboard-worker-list: the one list, attention first — what it holds
//! (AC1, AC2), how it counts (AC3), what a row says (AC4, AC6), its order
//! and its stability (AC5), attention outside the filter (AC7) and each
//! row's one action (AC8).

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use noa_gpui_core::accesskit::Action as Access;
use noa_gpui_core::widgets::{Button, Chart, LevelIndicator};

use crate::dashboard::list::{NO_TASK, ROW_HEIGHT, outside_text};
use crate::fleet::State;

use super::*;

/// What the row of `pane` says, column by column, depth first.
fn row_texts(dashboard: &Dashboard, pane: u64) -> Vec<String> {
    texts_under(&dashboard.tree, row_of(dashboard, pane))
}

/// The texts of the list's panel: its toolbar and its rows.
fn list_texts(dashboard: &Dashboard) -> Vec<String> {
    texts_under(&dashboard.tree, list_panel(dashboard))
}

fn list_panel(dashboard: &Dashboard) -> WidgetId {
    let list = dashboard.registry.list.as_ref().expect("the list").list;
    dashboard.tree.parent(list).expect("the list's panel")
}

fn of(fleet: Fleet) -> (Dashboard, TextStore) {
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    (dashboard, text)
}

fn with_filter(dashboard: &mut Dashboard, text: &mut TextStore, query: &str) {
    key(dashboard, text, Key::Char('/'));
    typed(dashboard, text, "/");
    typed(dashboard, text, query);
    key(dashboard, text, Key::Enter);
}

/// AC1: the default view is one list, one row per worker pane; no panel is
/// built.
#[test]
fn the_default_view_is_one_list() {
    let (dashboard, _text) = laid_out(Grouping::Orchestrator);
    assert!(dashboard.registry.list.is_some());
    let tree = &dashboard.tree;
    assert_eq!(tree.children(dashboard.body), [list_panel(&dashboard)]);
    let rows = rows_of(&dashboard);
    let mut panes = rows.clone();
    panes.dedup();
    assert_eq!(panes, rows, "one row per pane");
    let texts = texts_under(tree, tree.root().unwrap());
    for title in ["作業中", "完了", "状態の推移", "関連"] {
        assert!(!texts.iter().any(|t| t == title), "a panel titled {title}");
    }
}

/// AC2: 現在 / 要対応 / 終了, side by side with their counts; selecting one
/// shows its rows.
#[test]
fn the_switch_shows_each_scope_with_its_count() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    assert_eq!(scope_segments(&dashboard), ["現在 7", "要対応 4", "終了 2"]);
    // The middle of the switch is 要対応.
    let switch = dashboard.registry.scope_control.unwrap();
    let pos = center(&dashboard.tree, switch);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(dashboard.view.scope, Scope::Attention);
    assert_eq!(rows_of(&dashboard), [14, 15, 16, 19]);
    assert_eq!(scope_segments(&dashboard), ["現在 7", "要対応 4", "終了 2"]);
    in_scope(&mut dashboard, &mut text, Scope::Ended);
    assert_eq!(rows_of(&dashboard), [17, 18]);
}

/// AC2, AC5: 終了 lists the open workers that ended, then the closed ones,
/// each in the fleet's order; 現在 and 終了 split every worker.
#[test]
fn ended_lists_open_then_closed_workers() {
    let fleet = fleet::canned_with_closed();
    let all = fleet.workers.len();
    let (mut dashboard, mut text) = of(fleet);
    let current = rows_of(&dashboard).len();
    in_scope(&mut dashboard, &mut text, Scope::Ended);
    let ended = rows_of(&dashboard);
    assert_eq!(ended, [17, 18, (1 << 63) | 1, (1 << 63) | 2]);
    assert_eq!(current + ended.len(), all);
}

/// AC3: the 要対応 count is the number of rows its view lays out, under
/// any filter, and its rows are 現在's attention rows in the same order.
#[test]
fn the_attention_count_is_the_rows_it_lays_out() {
    for query in ["", "parent:release", "agent:codex", "-state:input"] {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        if !query.is_empty() {
            with_filter(&mut dashboard, &mut text, query);
        }
        let count = segment_count(&dashboard, Scope::Attention);
        let fleet = &dashboard.feed.fleet;
        let current: Vec<u64> = rows_of(&dashboard)
            .into_iter()
            .filter(|pane| fleet.worker(*pane).unwrap().needs_attention())
            .collect();
        assert_eq!(
            segment_count(&dashboard, Scope::Current),
            rows_of(&dashboard).len()
        );
        in_scope(&mut dashboard, &mut text, Scope::Attention);
        let attention = rows_of(&dashboard);
        assert_eq!(attention.len(), count, "{query:?}");
        assert_eq!(attention, current, "{query:?}: 現在's attention rows");
        assert_eq!(current.first(), attention.first(), "{query:?}: the top");
    }
}

/// AC4: a row is status (and its qualifiers), task, report and parent,
/// kind and label, the wait, one button — nothing else.
#[test]
fn a_row_shows_its_columns_and_nothing_else() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let fleet = dashboard.feed.fleet.clone();
    let worker = fleet.worker(14).unwrap();
    assert_eq!(
        row_texts(&dashboard, 14),
        [
            "承認待ち".to_owned(),
            worker.task.clone(),
            format!("{} · 親 api-refactor", worker.detail),
            worker.kind.clone(),
            worker.label.clone(),
            "待ち 2 分".to_owned(),
            "承認画面へ".to_owned(),
        ]
    );
    for scope in Scope::ALL {
        in_scope(&mut dashboard, &mut text, scope);
        let tree = &dashboard.tree;
        let mut under = vec![list_panel(&dashboard)];
        while let Some(id) = under.pop() {
            assert!(tree.get::<Chart>(id).is_none(), "a chart in the list");
            assert!(tree.get::<LevelIndicator>(id).is_none(), "a gauge");
            under.extend(tree.children(id));
        }
        for pane in rows_of(&dashboard) {
            let worker = fleet.worker(pane).unwrap();
            let texts = row_texts(&dashboard, pane).join(" / ");
            for hidden in [&worker.model, &worker.effort, &worker.branch, &worker.cwd] {
                assert!(
                    hidden.is_empty() || !texts.contains(hidden.as_str()),
                    "{pane}: {hidden:?} in {texts}"
                );
            }
            assert!(!texts.contains("報告"), "{pane}: {texts}");
            assert!(!texts.contains("最長"), "{pane}: {texts}");
        }
    }
}

/// AC4: every row has the same height, whatever its texts.
#[test]
fn every_row_is_row_height() {
    let mut fleet = fleet::canned();
    fleet.workers[0].task = "とても長い作業内容 ".repeat(40);
    fleet.workers[0].detail = "とても長い報告 ".repeat(40);
    fleet.workers[1].task.clear();
    fleet.workers[1].detail.clear();
    let (mut dashboard, mut text) = of(fleet);
    for scope in [Scope::Current, Scope::Ended] {
        in_scope(&mut dashboard, &mut text, scope);
        for pane in rows_of(&dashboard) {
            let height = rect(&dashboard.tree, row_of(&dashboard, pane))[3];
            assert!(
                (height - ROW_HEIGHT).abs() <= 0.5,
                "{scope:?} {pane}: {height}"
            );
        }
    }
}

/// AC4, AC5: rows at the smallest and the default window stay inside the
/// list, whole.
#[test]
fn rows_stay_inside_the_list_at_every_window() {
    for size in [Size::new(1100.0, 700.0), Size::new(1440.0, 900.0)] {
        let mut dashboard = Dashboard::new(Grouping::Orchestrator);
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(size, &mut text);
        let tree = &dashboard.tree;
        let list = rect(tree, dashboard.registry.list.as_ref().unwrap().list);
        let shown = visible_rows(&dashboard);
        assert_eq!(shown.len(), 7, "{size:?}: 現在's seven rows fit");
        for id in shown {
            let row = rect(tree, id);
            assert!(inside(row, list), "{size:?}: {row:?} outside {list:?}");
        }
    }
}

/// A worker of the canned fleet made `state`, with `wait` and `started`.
fn crafted(
    pane: u64,
    state: State,
    orchestrator: &str,
    wait: f64,
    started: Option<f64>,
) -> crate::fleet::Worker {
    let mut worker = fleet::canned().workers[0].clone();
    worker.pane = pane;
    worker.label = format!("w-{pane}");
    worker.state = state;
    worker.orchestrator = orchestrator.into();
    worker.parent = None;
    worker.wait_minutes = wait;
    worker.started_minutes = started;
    worker.quiet = false;
    worker
}

/// Two approvals (waits 2 and 9), an input, a failure, and three running
/// workers under two parents, in a scrambled fleet order.
fn crafted_fleet() -> Fleet {
    let mut fleet = fleet::canned();
    fleet.workers = vec![
        crafted(1, State::Running, "perf-wish", 0.0, Some(5.0)),
        crafted(2, State::Permission, "perf-wish", 2.0, None),
        crafted(3, State::Running, "release-0.9.7", 0.0, Some(1.0)),
        crafted(4, State::Error, "release-0.9.7", 4.0, None),
        crafted(5, State::Input, "perf-wish", 1.0, None),
        crafted(6, State::Running, "release-0.9.7", 0.0, Some(30.0)),
        crafted(7, State::Permission, "release-0.9.7", 9.0, None),
    ];
    fleet
}

/// AC5: approval, input, failure, then running; the longest wait first
/// within a kind; running by parent (in the fleet's orchestrator order),
/// then the one started first.
#[test]
fn the_order_is_attention_first_then_running_by_parent() {
    let (dashboard, _text) = of(crafted_fleet());
    assert_eq!(rows_of(&dashboard), [7, 2, 5, 4, 6, 3, 1]);
}

/// AC5: a snapshot that changes only times moves no row and keeps the
/// page, though the waits now say otherwise; a rebuild then keeps the
/// order as long as the waits within a kind keep theirs.
#[test]
fn a_clock_advance_does_not_reorder() {
    let (mut dashboard, mut text) = of(crafted_fleet());
    let rows = rows_of(&dashboard);
    let first = dashboard.view.first;
    let mut later = crafted_fleet();
    for worker in &mut later.workers {
        worker.wait_minutes = match worker.pane {
            2 => 9.0,
            7 => 2.0,
            _ => worker.wait_minutes + 1.0,
        };
        worker.started_minutes = worker.started_minutes.map(|m| 60.0 - m);
        worker.since = format!("later {}", worker.pane);
    }
    dashboard.apply(live(later, 0), &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(rows_of(&dashboard), rows, "no row moved");
    assert_eq!(dashboard.view.first, first);
    let time = pulse_id(&dashboard, |p| matches!(p, Pulse::RowTime(2)));
    assert_eq!(
        dashboard.tree.get::<Label>(time).unwrap().text(),
        "待ち later 2"
    );

    // Time passing as it does, every wait one minute longer: a rebuild
    // gives the order the list had.
    let mut aged = crafted_fleet();
    for worker in &mut aged.workers {
        worker.wait_minutes += 1.0;
        worker.started_minutes = worker.started_minutes.map(|m| m + 1.0);
    }
    let (fresh, _text) = of(aged);
    assert_eq!(rows_of(&fresh), rows);
}

/// The canned fleet with twelve more running workers: the list pages at
/// 1100×700.
fn busy() -> (Dashboard, TextStore, Receiver<Command>, Fleet) {
    let mut fleet = fleet::canned();
    for n in 0..12 {
        let mut worker = fleet.workers[0].clone();
        worker.pane = 100 + n;
        worker.label = format!("extra-{n}");
        fleet.workers.push(worker);
    }
    let (channel, snapshots, _notices, commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(Size::new(1100.0, 700.0), &mut text);
    snapshots.send(live(fleet.clone(), 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(Size::new(1100.0, 700.0), &mut text);
    (dashboard, text, commands, fleet)
}

/// AC5: a selected running worker that comes to wait for an approval is
/// still selected (by its pane), the page still starts at the same row,
/// and the notice names it.
#[test]
fn a_selected_worker_that_needs_attention_stays_selected() {
    let small = Size::new(1100.0, 700.0);
    let (mut dashboard, mut text, _commands, mut fleet) = busy();
    for _ in 0..3 {
        let pos = center(
            &dashboard.tree,
            dashboard.registry.list.as_ref().unwrap().list,
        );
        let delta = Point::new(0.0, -40.0);
        dashboard.dispatch(Event::Wheel { pos, delta }, &mut text);
        dashboard.prepare(small, &mut text);
    }
    // A running worker on the page, not its first row.
    let pane = rows_of(&dashboard)[dashboard.view.first + 2];
    dashboard.select(Some(pane), &mut text);
    dashboard.prepare(small, &mut text);
    let top = rows_of(&dashboard)[dashboard.view.first];
    let label = label_of(&dashboard, pane).to_owned();

    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == pane)
        .unwrap()
        .state = State::Permission;
    dashboard.apply(live(fleet, 0), &mut text);
    dashboard.prepare(small, &mut text);
    assert_eq!(dashboard.view.selected, Some(pane), "still selected");
    assert_eq!(rows_of(&dashboard)[dashboard.view.first], top, "the page");
    assert_eq!(
        notice_of(&dashboard),
        format!("要対応が増えました: {label}（承認待ち）")
    );
}

/// AC5: new attention while another row is selected leaves the selection,
/// and says so; more than one is counted.
#[test]
fn new_attention_keeps_the_selection_and_says_so() {
    let (mut dashboard, mut text, _commands, mut fleet) = busy();
    dashboard.select(Some(12), &mut text);
    for pane in [100, 101] {
        let worker = fleet.workers.iter_mut().find(|w| w.pane == pane).unwrap();
        worker.state = State::Input;
    }
    dashboard.apply(live(fleet, 0), &mut text);
    assert_eq!(dashboard.view.selected, Some(12));
    assert_eq!(notice_of(&dashboard), "要対応が 2 件増えました");
}

/// AC6: quiet is 実行中 with how long its output stood still; a closed
/// worker that finished has ended, closed, with its result; no status
/// says 成功 or 完了.
#[test]
fn a_row_says_what_the_worker_does_in_the_row_words() {
    let (mut dashboard, mut text) = of(fleet::canned_with_closed());
    let quiet = row_texts(&dashboard, 13);
    assert_eq!(quiet[..2], ["実行中", "出力未変化 3 分"], "{quiet:?}");
    assert!(
        !quiet
            .iter()
            .any(|t| t.contains("失敗") || t.contains("停止"))
    );
    in_scope(&mut dashboard, &mut text, Scope::Ended);
    let bench = row_texts(&dashboard, (1 << 63) | 1);
    assert_eq!(bench[..2], ["終了", "閉じた · 結果あり"], "{bench:?}");
    for word in ["成功", "完了"] {
        assert!(
            !bench.iter().any(|t| t.contains(word)),
            "{word} in {bench:?}"
        );
        // No status or qualifier says it either (the time column quotes
        // the feed: "4 分前に完了").
        for pane in rows_of(&dashboard) {
            let texts = row_texts(&dashboard, pane);
            let words = &texts[..2];
            assert!(
                !words.iter().any(|t| t.contains(word)),
                "{word} in {texts:?}"
            );
        }
    }
}

/// AC6: a worker with no task says 作業内容未取得, never its label in its
/// place: the label is in its own column only.
#[test]
fn a_missing_task_is_not_the_label() {
    let mut fleet = fleet::canned();
    fleet.workers[0].task.clear();
    fleet.workers[0].label = "x".into();
    let (dashboard, _text) = of(fleet);
    let texts = row_texts(&dashboard, 11);
    assert_eq!(texts[1], NO_TASK, "{texts:?}");
    assert_eq!(texts.iter().filter(|t| *t == "x").count(), 1, "{texts:?}");
    assert_eq!(texts[4], "x", "the エージェント column: {texts:?}");
}

/// AC7: what needs attention outside the filter is said, with a way to
/// clear the filter, only while there is some.
#[test]
fn attention_outside_the_filter_is_announced() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    assert!(dashboard.registry.clear_filter.is_none(), "no filter");
    with_filter(&mut dashboard, &mut text, "label:docs");
    assert!(list_texts(&dashboard).contains(&outside_text(4)));
    let clear = dashboard.registry.clear_filter.expect("絞り込みを解除");
    let pos = center(&dashboard.tree, clear);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(dashboard.filter.query, "");
    assert!(dashboard.registry.outside_note.is_none());
    assert!(
        !list_texts(&dashboard)
            .iter()
            .any(|t| t.contains("絞り込み外"))
    );

    // A filter that keeps every worker needing attention hides none.
    with_filter(&mut dashboard, &mut text, "state:waiting");
    assert_eq!(rows_of(&dashboard), [14, 15, 16, 19]);
    assert!(dashboard.registry.outside_note.is_none());
}

fn button_label(dashboard: &Dashboard, pane: u64) -> String {
    let button = row_button(dashboard, pane);
    dashboard
        .tree
        .get::<Button>(button)
        .unwrap()
        .label()
        .to_owned()
}

/// AC8: one action per status.
#[test]
fn each_status_has_one_action() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    for (pane, label) in [
        (14, "承認画面へ"),
        (16, "入力する"),
        (19, "確認する"),
        (11, "詳細"),
        (13, "詳細"),
    ] {
        assert_eq!(button_label(&dashboard, pane), label, "{pane}");
    }
    in_scope(&mut dashboard, &mut text, Scope::Ended);
    // The demo's ended workers have a result, but no file to read.
    assert_eq!(button_label(&dashboard, 17), "詳細");
}

fn live_canned() -> (Dashboard, TextStore, Receiver<Command>) {
    let (channel, snapshots, _notices, commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots.send(live(fleet::canned(), 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    commands.try_iter().for_each(drop);
    (dashboard, text, commands)
}

fn press_row_button(dashboard: &mut Dashboard, text: &mut TextStore, pane: u64) {
    let pos = center(&dashboard.tree, row_button(dashboard, pane));
    click(dashboard, text, pos);
}

/// AC8: 承認画面へ focuses the pane and selects it; nothing is approved.
#[test]
fn the_approval_button_focuses_the_pane() {
    let (mut dashboard, mut text, commands) = live_canned();
    press_row_button(&mut dashboard, &mut text, 14);
    let sent: Vec<Command> = commands
        .try_iter()
        .filter(|c| !matches!(c, Command::Watch(_)))
        .collect();
    assert_eq!(sent, [Command::Focus(14)]);
    assert_eq!(dashboard.view.selected, Some(14));
    assert_eq!(dashboard.view.detail, None);
}

/// AC8: 入力する opens the drawer with its send field focused.
#[test]
fn the_input_button_focuses_the_send_field() {
    let (mut dashboard, mut text, _commands) = live_canned();
    press_row_button(&mut dashboard, &mut text, 16);
    assert_eq!(dashboard.view.selected, Some(16));
    assert_eq!(dashboard.view.detail, Some(16));
    let (field, pane) = dashboard.registry.send_field.expect("a send field");
    assert_eq!(pane, 16);
    assert_eq!(dashboard.tree.focused(), Some(field));
}

/// AC8: after 入力する, a click on the row itself (away from its button)
/// takes the focus out of the send field, as a click on space that takes
/// none does: J then moves the selection and types no draft.
#[test]
fn clicking_the_row_leaves_the_send_field() {
    let (mut dashboard, mut text, _commands) = live_canned();
    press_row_button(&mut dashboard, &mut text, 16);
    let (field, _) = dashboard.registry.send_field.expect("a send field");
    assert_eq!(dashboard.tree.focused(), Some(field));

    let pos = row_body(&dashboard, 16);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(dashboard.view.selected, Some(16));
    assert_eq!(dashboard.view.detail, Some(16), "the drawer stays");
    assert_eq!(dashboard.tree.focused(), None);
    assert_eq!(dashboard.mode(), Mode::Browse);
    stroke(&mut dashboard, &mut text, 'j');
    assert_ne!(dashboard.view.selected, Some(16), "J moves the selection");
    assert_eq!(dashboard.drafts.get(&16).map(String::as_str), None);
}

/// `pane`'s row button pressed by assistive technology, which acts on the
/// button directly: an open drawer covers the rows' 操作 column from the
/// pointer.
fn press_row_button_for_assistive(dashboard: &mut Dashboard, text: &mut TextStore, pane: u64) {
    let node = noa_gpui_core::access_id(row_button(dashboard, pane)).0;
    super::hardening::assistive(dashboard, text, node, Access::Click);
}

/// AC8: 入力する while a close waits for its second press (X on the
/// selected worker, or ⇧X) takes the close back and leaves the send field
/// focused, as S does: what is typed next is the message, not a key. Also
/// with the drawer already open on the worker, its button relabelled.
#[test]
fn the_input_button_focuses_the_send_field_while_a_close_waits() {
    for arm in ["X", "⇧X", "X with the drawer open"] {
        let (mut dashboard, mut text, _commands) = live_canned();
        let drawer_open = arm == "X with the drawer open";
        if arm == "⇧X" {
            super::bulk_close::shift_x(&mut dashboard, &mut text);
            assert!(dashboard.close_ended.is_some(), "{arm}");
        } else {
            dashboard.select(Some(16), &mut text);
            if drawer_open {
                key(&mut dashboard, &mut text, Key::Space);
            }
            key(&mut dashboard, &mut text, Key::Char('x'));
            assert_eq!(dashboard.view.confirm_close, Some(16), "{arm}");
        }
        assert_eq!(dashboard.view.detail.is_some(), drawer_open, "{arm}");

        if drawer_open {
            press_row_button_for_assistive(&mut dashboard, &mut text, 16);
        } else {
            press_row_button(&mut dashboard, &mut text, 16);
        }
        assert_eq!(dashboard.view.confirm_close, None, "{arm}");
        assert!(dashboard.close_ended.is_none(), "{arm}");
        assert_eq!(dashboard.view.detail, Some(16), "{arm}");
        let (root, _) = dashboard.registry.drawer.expect("the drawer");
        let drawer = texts_under(&dashboard.tree, root);
        assert!(
            !drawer.iter().any(|t| t.starts_with("本当に")),
            "{arm}: the drawer still asks to confirm: {drawer:?}"
        );
        let (field, pane) = dashboard.registry.send_field.expect("a send field");
        assert_eq!(pane, 16, "{arm}");
        assert_eq!(dashboard.tree.focused(), Some(field), "{arm}: focus");
        stroke(&mut dashboard, &mut text, 'j');
        assert_eq!(dashboard.view.selected, Some(16), "{arm}: J moved on");
        assert_eq!(
            dashboard.drafts.get(&16).map(String::as_str),
            Some("j"),
            "{arm}"
        );
    }
}

/// AC8: 入力する selects its row also when the drawer is already on it: a
/// live update took the worker out of 要対応 (the selection with it) and
/// brought it back while its drawer stayed open.
#[test]
fn the_input_button_selects_a_row_whose_drawer_stayed_open() {
    let (mut dashboard, mut text, _commands) = live_canned();
    in_scope(&mut dashboard, &mut text, Scope::Attention);
    press_row_button(&mut dashboard, &mut text, 16);
    key(&mut dashboard, &mut text, Key::Escape);
    let mut running = fleet::canned();
    let worker = running.workers.iter_mut().find(|w| w.pane == 16).unwrap();
    worker.state = State::Running;
    dashboard.apply(live(running, 0), &mut text);
    dashboard.prepare(window(), &mut text);
    assert!(!rows_of(&dashboard).contains(&16));
    assert_eq!(dashboard.view.selected, None);
    assert_eq!(dashboard.view.detail, Some(16), "the drawer stays");
    dashboard.apply(live(fleet::canned(), 0), &mut text);
    dashboard.prepare(window(), &mut text);
    assert!(rows_of(&dashboard).contains(&16));
    assert_eq!(dashboard.view.selected, None);
    assert_eq!(dashboard.view.detail, Some(16));

    press_row_button_for_assistive(&mut dashboard, &mut text, 16);
    assert_eq!(dashboard.view.selected, Some(16));
    assert_eq!(dashboard.view.detail, Some(16));
    let mut marks = dashboard.registry.marks.iter();
    let (bar, _, _) = marks
        .find(|(_, pane, mark)| *pane == 16 && matches!(mark, Mark::Bar))
        .expect("the row's selection bar");
    assert!(
        !dashboard.tree.is_hidden(*bar),
        "the row shows its selection"
    );
    let (field, pane) = dashboard.registry.send_field.expect("a send field");
    assert_eq!(pane, 16);
    assert_eq!(dashboard.tree.focused(), Some(field));
}

/// AC8: 確認する and 詳細 open the drawer, in any scope.
#[test]
fn the_other_buttons_open_the_drawer() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    for pane in [19, 11] {
        press_row_button(&mut dashboard, &mut text, pane);
        assert_eq!(dashboard.view.detail, Some(pane));
        assert_eq!(dashboard.view.selected, Some(pane));
        // Pressed again it stays open.
        press_row_button(&mut dashboard, &mut text, pane);
        assert_eq!(dashboard.view.detail, Some(pane));
        key(&mut dashboard, &mut text, Key::Escape);
    }
    in_scope(&mut dashboard, &mut text, Scope::Ended);
    press_row_button(&mut dashboard, &mut text, 18);
    assert_eq!(dashboard.view.detail, Some(18));
    assert_eq!(dashboard.view.result, None, "no file to read");
}

/// AC8: an ended worker with a result file shows 結果を見る, which opens
/// the drawer with the file read.
#[test]
fn an_ended_result_is_shown_from_its_row() {
    let path: PathBuf = std::env::temp_dir().join(format!(
        "agent-dashboard-list-result-{}.md",
        std::process::id()
    ));
    std::fs::write(&path, "all green\n").unwrap();
    let mut fleet = fleet::canned();
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 17)
        .unwrap()
        .result = Some(path.clone());
    let (mut dashboard, mut text) = of(fleet);
    in_scope(&mut dashboard, &mut text, Scope::Ended);
    assert_eq!(button_label(&dashboard, 17), "結果を見る");
    press_row_button(&mut dashboard, &mut text, 17);
    assert_eq!(dashboard.view.detail, Some(17));
    assert_eq!(dashboard.view.result, Some(17));
    let (root, _) = dashboard.registry.drawer.unwrap();
    let texts = labels_under(&dashboard.tree, root);
    assert!(texts.contains(&"all green".to_owned()), "{texts:?}");
    let _ = std::fs::remove_file(&path);
}

/// AC4: a long label or task is cut inside its column: the wait and the
/// button stay inside the row.
#[test]
fn a_long_label_leaves_the_wait_and_the_button_inside_the_row() {
    let mut fleet = fleet::canned();
    for worker in &mut fleet.workers {
        worker.label = LONG_LABEL.into();
        worker.task = format!("{LONG_LABEL} {LONG_LABEL} {LONG_LABEL}");
    }
    for size in [Size::new(1100.0, 700.0), Size::new(1440.0, 900.0)] {
        let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet.clone(), 0));
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(size, &mut text);
        let tree = &dashboard.tree;
        for pane in rows_of(&dashboard) {
            let row = rect(tree, row_of(&dashboard, pane));
            let time = pulse_id(&dashboard, |p| matches!(p, Pulse::RowTime(p) if p == pane));
            let button = row_button(&dashboard, pane);
            for (what, id) in [("the wait", time), ("the button", button)] {
                let at = rect(tree, id);
                assert!(
                    inside(at, row),
                    "{size:?} {pane}: {what} {at:?} outside {row:?}"
                );
            }
            let shown: Vec<&Label> = texts_ids(tree, row_of(&dashboard, pane))
                .into_iter()
                .filter_map(|id| tree.get::<Label>(id))
                .filter(|l| l.text() == LONG_LABEL)
                .collect();
            assert_eq!(shown.len(), 1, "{pane}: the label once");
            assert!(shown[0].shown_text().ends_with('…'), "{pane}: cut");
        }
    }
}

/// The widgets under `id`, itself included.
fn texts_ids(tree: &UiTree, id: WidgetId) -> Vec<WidgetId> {
    std::iter::once(id)
        .chain(tree.children(id).iter().flat_map(|c| texts_ids(tree, *c)))
        .collect()
}
