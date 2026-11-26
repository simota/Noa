use crate::dashboard::Mark;
use noa_gpui_core::widgets::Label;

use super::*;

/// Presses J until `pane` is selected.
fn select(dashboard: &mut Dashboard, text: &mut TextStore, pane: u64) {
    for _ in 0..20 {
        if dashboard.view.selected == Some(pane) {
            return;
        }
        key(dashboard, text, Key::Char('j'));
    }
    panic!("J never selects {pane}");
}

fn walk(dashboard: &mut Dashboard, text: &mut TextStore, key_: Key, n: usize) -> Vec<u64> {
    (0..n)
        .map(|_| {
            key(dashboard, text, key_);
            dashboard.view.selected.unwrap()
        })
        .collect()
}

#[test]
fn j_and_k_walk_the_list_order() {
    // What needs attention (the approvals by wait, the question, the
    // failure), then what runs; nothing that ended is in 現在.
    let order = [14, 15, 16, 19, 11, 12, 13];
    let mut back: Vec<u64> = order.iter().rev().copied().skip(1).collect();
    back.push(14);
    for (next, prev) in [
        (Key::Char('j'), Key::Char('k')),
        (Key::ArrowDown, Key::ArrowUp),
    ] {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        assert_eq!(rows_of(&dashboard), order);
        assert_eq!(walk(&mut dashboard, &mut text, next, 7), order);
        assert_eq!(walk(&mut dashboard, &mut text, next, 1), [13], "no wrap");
        assert_eq!(walk(&mut dashboard, &mut text, prev, 7), back);
    }
}

/// Every mark shows whether its worker is `selected`; returns the kinds of
/// mark the selected worker has.
fn assert_marks(dashboard: &Dashboard, selected: u64) -> Vec<&'static str> {
    let theme = dashboard.tree.theme();
    let (accent, fill) = (theme.color(Role::Accent), theme.color(Role::Selection));
    let tree = &dashboard.tree;
    let mut kinds = Vec::new();
    for &(id, pane, mark) in &dashboard.registry.marks {
        let on = pane == selected;
        match mark {
            Mark::Bar => {
                assert_eq!(!tree.is_hidden(id), on, "bar of {pane}");
                if on {
                    let (color, _) = quad_at(dashboard, rect(tree, id)).unwrap();
                    assert_eq!(color, accent, "bar of {pane}");
                    kinds.push("bar");
                }
            }
            Mark::Lane => {
                let quad = quad_at(dashboard, rect(tree, id));
                if on {
                    assert_eq!(quad.map(|(c, _)| c), Some(fill), "lane of {pane}");
                    kinds.push("lane");
                } else {
                    assert!(quad.is_none_or(|(c, _)| c != fill), "lane of {pane}");
                }
            }
            Mark::Node { normal } => {
                let (_, border) = quad_at(dashboard, rect(tree, id)).unwrap();
                let expected = if on {
                    kinds.push("node");
                    Border {
                        width: 2.0,
                        color: accent,
                    }
                } else {
                    Border {
                        width: 1.0,
                        color: normal,
                    }
                };
                assert_eq!(border, expected, "node of {pane}");
            }
        }
    }
    kinds
}

#[test]
fn the_selection_shows_in_place_on_its_row() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    select(&mut dashboard, &mut text, 11);
    let row = row_of(&dashboard, 12);
    // A rebuild makes the status label again: the mark shows there was none
    // (a new tree hands out the same widget ids again).
    let status = dashboard.status;
    let mark = |d: &mut Dashboard| {
        d.tree
            .update::<Label, _>(status, |l, cx| l.set_text(cx, "mark"));
    };
    let marked = |d: &Dashboard| d.tree.get::<Label>(status).unwrap().text() == "mark";
    mark(&mut dashboard);
    key(&mut dashboard, &mut text, Key::Char('j'));
    assert_eq!(dashboard.view.selected, Some(12));
    assert!(marked(&dashboard), "the screen was rebuilt");
    assert_eq!(row_of(&dashboard, 12), row, "the screen was rebuilt");
    assert_eq!(assert_marks(&dashboard, 12), ["bar"]);
    key(&mut dashboard, &mut text, Key::Char('j'));
    assert_eq!(dashboard.view.selected, Some(13));
    assert!(marked(&dashboard), "the screen was rebuilt");
    assert_eq!(assert_marks(&dashboard, 13), ["bar"]);
}

/// The タイムライン tab's lane of `pane`.
fn lane(dashboard: &Dashboard, pane: u64) -> WidgetId {
    let mut marks = dashboard.registry.marks.iter();
    let found = marks.find(|(_, p, mark)| *p == pane && matches!(mark, Mark::Lane));
    found.unwrap_or_else(|| panic!("no lane for {pane}")).0
}

/// A click on a lane of the タイムライン tab selects its worker and opens
/// its drawer, as a node of the マップ does; the lane shows the selection,
/// and another lane's click moves it there.
#[test]
fn clicking_a_timeline_lane_selects_its_worker() {
    let mut dashboard = Dashboard::new(Grouping::Orchestrator).with_tab(Tab::Timeline);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    for pane in [12, 13] {
        let pos = center(&dashboard.tree, lane(&dashboard, pane));
        click(&mut dashboard, &mut text, pos);
        assert_eq!(dashboard.view.selected, Some(pane));
        assert_eq!(dashboard.view.detail, Some(pane));
        assert_eq!(assert_marks(&dashboard, pane), ["lane"], "{pane}");
    }
}

fn live_with(snapshot: Snapshot) -> (Dashboard, TextStore, std::sync::mpsc::Receiver<Command>) {
    let (channel, snapshots, _notices, commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots.send(snapshot).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    // What the screen asked for until now (the screen tails to fetch).
    commands.try_iter().for_each(drop);
    (dashboard, text, commands)
}

/// What the last action said: the notice line.
fn status_of(dashboard: &Dashboard) -> String {
    notice_of(dashboard)
}

fn focused_panes(commands: &std::sync::mpsc::Receiver<Command>) -> Vec<u64> {
    commands
        .try_iter()
        .filter_map(|c| match c {
            Command::Focus(pane) => Some(pane),
            Command::Watch(_)
            | Command::Send { .. }
            | Command::Close { .. }
            | Command::CloseMany { .. } => None,
        })
        .collect()
}

#[test]
fn enter_opens_the_selected_worker_like_the_open_button() {
    let (mut dashboard, mut text, commands) = live_with(live(fleet::canned(), 0));
    key(&mut dashboard, &mut text, Key::Char('j'));
    key(&mut dashboard, &mut text, Key::Enter);
    assert_eq!(focused_panes(&commands), [14]);
    assert_eq!(status_of(&dashboard), "fix-tests のペインへ移動します");

    let read_only = Snapshot {
        can_focus: false,
        ..live(fleet::canned(), 0)
    };
    let (mut dashboard, mut text, commands) = live_with(read_only);
    key(&mut dashboard, &mut text, Key::Char('j'));
    key(&mut dashboard, &mut text, Key::Enter);
    assert!(focused_panes(&commands).is_empty());
    assert_eq!(
        status_of(&dashboard),
        "ペインへ移動できません: Noa の server-scopes に control がありません（例: server-scopes = read,control）"
    );

    let (mut dashboard, mut text, commands) = live_with(live(fleet::canned(), 0));
    key(&mut dashboard, &mut text, Key::Enter);
    assert_eq!(commands.try_iter().count(), 0, "something was sent");
    assert_eq!(
        status_of(&dashboard),
        "ワーカーが選ばれていません（J / K で選ぶ）"
    );
}

#[test]
fn a_vanished_selection_moves_to_its_neighbour() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    select(&mut dashboard, &mut text, 11);
    // Showing 終了 is the user's own change: the selection moves to what
    // is now at api-refactor's place (the fifth row), clamped to the last
    // of 終了's two.
    key(&mut dashboard, &mut text, Key::Char('3'));
    assert_eq!(dashboard.view.selected, Some(18));
}

#[test]
fn a_live_update_that_takes_the_selection_selects_nothing() {
    let (mut dashboard, mut text, commands) = live_with(live(fleet::canned(), 0));
    select(&mut dashboard, &mut text, 15);
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.view.detail, Some(15));
    commands.try_iter().for_each(drop);
    let mut fleet = fleet::canned();
    fleet.workers.retain(|w| w.pane != 15);
    dashboard.apply(live(fleet, 0), &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(dashboard.view.selected, None);
    assert_eq!(dashboard.view.detail, None);
    assert!(drawer_labels(&dashboard).is_none());
    let label = label_of_canned(15);
    assert_eq!(status_of(&dashboard), dropped_text(&label, None));
    // Enter, once or again, reaches no pane.
    key(&mut dashboard, &mut text, Key::Enter);
    key(&mut dashboard, &mut text, Key::Enter);
    assert!(focused_panes(&commands).is_empty());
    assert_eq!(status_of(&dashboard), NOTHING_SELECTED);
}

#[test]
fn a_live_update_that_only_reorders_keeps_the_selection() {
    let (mut dashboard, mut text, commands) = live_with(live(fleet::canned(), 0));
    select(&mut dashboard, &mut text, 15);
    commands.try_iter().for_each(drop);
    let mut fleet = fleet::canned();
    let fix = fleet.workers.iter_mut().find(|w| w.pane == 15).unwrap();
    fix.wait_minutes = 3.0;
    dashboard.apply(live(fleet, 0), &mut text);
    assert_eq!(dashboard.view.selected, Some(15));
    key(&mut dashboard, &mut text, Key::Enter);
    assert_eq!(focused_panes(&commands), [15]);
}

/// dashboard-worker-list AC5: a selected worker that finished while 現在
/// is shown leaves it, and the notice says where it went; an open drawer
/// stays on it.
#[test]
fn a_selection_that_leaves_the_scope_says_where_it_went() {
    let (mut dashboard, mut text, _commands) = live_with(live(fleet::canned(), 0));
    select(&mut dashboard, &mut text, 11);
    key(&mut dashboard, &mut text, Key::Space);
    let mut fleet = fleet::canned();
    let api = fleet.workers.iter_mut().find(|w| w.pane == 11).unwrap();
    api.state = fleet::State::Finished;
    dashboard.apply(live(fleet, 0), &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(dashboard.view.selected, None);
    assert!(!rows_of(&dashboard).contains(&11));
    assert_eq!(
        status_of(&dashboard),
        dropped_text("api-refactor", Some(Scope::Ended))
    );
    assert!(status_of(&dashboard).contains("api-refactor は 終了 に移りました"));
    assert_eq!(dashboard.view.detail, Some(11), "the drawer stays");
}

/// dashboard-worker-list AC5: a worker selected outside the scope's rows
/// (lint-pass, finished, chosen on its タイムライン lane while 現在 is the
/// scope) that a live update takes out of the fleet or out of the filter
/// is no longer selected, and the notice names it.
#[test]
fn a_selection_outside_the_list_that_leaves_says_so() {
    for case in ["the fleet", "the filter"] {
        let (mut dashboard, mut text, _commands) = live_with(live(fleet::canned(), 0));
        if case == "the filter" {
            typed(&mut dashboard, &mut text, "/");
            typed(&mut dashboard, &mut text, "branch:chore");
            key(&mut dashboard, &mut text, Key::Enter);
        }
        super::tabs::show_tab(&mut dashboard, &mut text, Tab::Timeline);
        let pos = center(&dashboard.tree, lane(&dashboard, 17));
        click(&mut dashboard, &mut text, pos);
        super::tabs::show_tab(&mut dashboard, &mut text, Tab::Operations);
        assert_eq!(dashboard.view.scope, Scope::Current, "{case}");
        assert!(!rows_of(&dashboard).contains(&17), "{case}: 17 is in 現在");
        assert_eq!(dashboard.view.selected, Some(17), "{case}");

        let mut fleet = fleet::canned();
        if case == "the fleet" {
            fleet.workers.retain(|w| w.pane != 17);
        } else {
            let lint = fleet.workers.iter_mut().find(|w| w.pane == 17).unwrap();
            lint.branch = "fix/clippy".into();
        }
        dashboard.apply(live(fleet, 0), &mut text);
        dashboard.prepare(window(), &mut text);
        assert_eq!(dashboard.view.selected, None, "{case}");
        assert_eq!(
            status_of(&dashboard),
            dropped_text("lint-pass", None),
            "{case}"
        );
    }
}

fn label_of_canned(pane: u64) -> String {
    let fleet = fleet::canned();
    fleet.worker(pane).unwrap().label.clone()
}

#[test]
fn escape_backs_out_one_step_at_a_time() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    key(&mut dashboard, &mut text, Key::Char('j'));
    key(&mut dashboard, &mut text, Key::Char('/'));
    typed(&mut dashboard, &mut text, "/");
    typed(&mut dashboard, &mut text, "a");
    key(&mut dashboard, &mut text, Key::Enter);
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.view.detail, Some(14));
    assert_eq!(dashboard.tree.overlays().count(), 1);
    assert_eq!(dashboard.filter.query, "a");

    key(&mut dashboard, &mut text, Key::Escape);
    assert_eq!(dashboard.view.detail, None);
    assert_eq!(dashboard.tree.overlays().count(), 0);
    assert_eq!(dashboard.filter.query, "a");
    key(&mut dashboard, &mut text, Key::Escape);
    assert_eq!(dashboard.filter.query, "");
    assert_eq!(dashboard.view.selected, Some(14));
    key(&mut dashboard, &mut text, Key::Escape);
    assert_eq!(dashboard.view.selected, None);
}

/// The labels in the drawer, if it is open.
fn drawer_labels(dashboard: &Dashboard) -> Option<Vec<String>> {
    let (root, _) = dashboard.registry.drawer?;
    Some(labels_under(&dashboard.tree, root))
}

#[test]
fn the_drawer_follows_any_reselection() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    key(&mut dashboard, &mut text, Key::Char('j'));
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.view.detail, Some(14));
    // 終了 takes fix-tests out of the order: the selection moves to the
    // pane now at its place, and the drawer with it.
    key(&mut dashboard, &mut text, Key::Char('3'));
    let selected = dashboard.view.selected.unwrap();
    assert_ne!(selected, 14);
    assert_eq!(dashboard.view.detail, Some(selected));
    let label = label_of(&dashboard, selected).to_owned();
    let labels = drawer_labels(&dashboard).expect("the drawer is open");
    assert!(labels.contains(&label), "{label} not in {labels:?}");
    assert!(!labels.iter().any(|l| l == "fix-tests"), "{labels:?}");
    let (_, pane) = dashboard.registry.open_buttons[0];
    assert_eq!(pane, selected, "the drawer's 開く");
    key(&mut dashboard, &mut text, Key::Enter);
    let status = status_of(&dashboard);
    assert_eq!(status, format!("{label} のペインへ移動します"));
}

#[test]
fn a_filtered_out_drawer_worker_closes_the_drawer() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    key(&mut dashboard, &mut text, Key::Char('j'));
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.view.detail, Some(14));
    key(&mut dashboard, &mut text, Key::Char('/'));
    typed(&mut dashboard, &mut text, "/");
    typed(&mut dashboard, &mut text, "codex");
    assert_eq!(dashboard.view.detail, None);
    assert!(drawer_labels(&dashboard).is_none());
    assert_eq!(dashboard.tree.overlays().count(), 0);
    assert_eq!(dashboard.view.selected, Some(15), "fix-tests' place");
}

#[test]
fn a_live_drawer_watches_the_worker_it_moved_to() {
    let (mut dashboard, mut text, commands) = live_with(live(fleet::canned(), 0));
    key(&mut dashboard, &mut text, Key::Char('j'));
    key(&mut dashboard, &mut text, Key::Space);
    commands.try_iter().for_each(drop);
    key(&mut dashboard, &mut text, Key::Char('3'));
    let selected = dashboard.view.selected.unwrap();
    let watched: Vec<Command> = commands.try_iter().collect();
    assert_eq!(watched.last(), Some(&Command::Watch(vec![selected])));
}

/// The list's rows from top to bottom on screen.
fn rows_on_screen(dashboard: &Dashboard) -> Vec<u64> {
    let list = dashboard.registry.list.as_ref().unwrap();
    let mut rows: Vec<(f32, u64)> = list
        .items
        .iter()
        .map(|(id, pane)| (rect(&dashboard.tree, *id)[1], *pane))
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows.into_iter().map(|(_, pane)| pane).collect()
}

/// dashboard-worker-list AC5: time passing moves no row, whatever the
/// waits say now; J walks the rows as shown.
#[test]
fn the_rows_keep_their_order_as_time_passes() {
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet::canned(), 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    assert_eq!(rows_on_screen(&dashboard)[..4], [14, 15, 16, 19]);
    let second = row_of(&dashboard, 15);

    // Only the second approval's wait grows, past fix-tests' (the same
    // need).
    let mut fleet = fleet::canned();
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 15)
        .unwrap()
        .wait_minutes = 3.0;
    dashboard.apply(live(fleet, 0), &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(row_of(&dashboard, 15), second, "updated in place");
    let shown = rows_on_screen(&dashboard);
    assert_eq!(shown[..4], [14, 15, 16, 19], "no row moved");
    assert_eq!(rows_of(&dashboard), shown);
    key(&mut dashboard, &mut text, Key::Char('j'));
    assert_eq!(
        dashboard.view.selected,
        Some(shown[0]),
        "J takes the top row"
    );
}

fn last_watch(commands: &std::sync::mpsc::Receiver<Command>) -> Option<Vec<u64>> {
    commands
        .try_iter()
        .filter_map(|c| match c {
            Command::Watch(panes) => Some(panes),
            Command::Focus(_)
            | Command::Send { .. }
            | Command::Close { .. }
            | Command::CloseMany { .. } => None,
        })
        .last()
}

#[test]
fn a_reselection_to_nobody_closes_the_drawer() {
    let (mut dashboard, mut text, commands) = live_with(live(fleet::canned(), 0));
    key(&mut dashboard, &mut text, Key::Char('/'));
    typed(&mut dashboard, &mut text, "/");
    typed(&mut dashboard, &mut text, "fix-tests");
    key(&mut dashboard, &mut text, Key::Enter);
    key(&mut dashboard, &mut text, Key::Char('j'));
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.view.detail, Some(14));
    commands.try_iter().for_each(drop);

    // 終了 leaves fix-tests nowhere to be selected.
    key(&mut dashboard, &mut text, Key::Char('3'));
    assert_eq!(dashboard.view.selected, None);
    assert_eq!(dashboard.view.detail, None);
    assert!(drawer_labels(&dashboard).is_none());
    assert_eq!(dashboard.tree.overlays().count(), 0);
    assert!(
        dashboard.registry.open_buttons.is_empty(),
        "the drawer's 開く"
    );
    assert_eq!(last_watch(&commands), Some(vec![]), "14 is not watched");
    key(&mut dashboard, &mut text, Key::Enter);
    assert!(focused_panes(&commands).is_empty());
    assert_eq!(status_of(&dashboard), NOTHING_SELECTED);
}

#[test]
fn rows_are_named_groups() {
    // A row the pager hides is not exposed to assistive technology: at
    // 1440×900 every row of either scope is on its page.
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    for scope in [Scope::Current, Scope::Ended] {
        in_scope(&mut dashboard, &mut text, scope);
        assert_eq!(
            visible_rows(&dashboard).len(),
            rows_of(&dashboard).len(),
            "{scope:?} pages rows out at 1440×900"
        );
        let update = dashboard.tree.accessibility_update(1.0);
        let groups: Vec<&str> = update
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == noa_gpui_core::accesskit::Role::Group)
            .filter_map(|(_, node)| node.label())
            .collect();
        for pane in rows_of(&dashboard) {
            let label = label_of(&dashboard, pane);
            assert!(
                groups.contains(&label),
                "no group for {label} in {groups:?}"
            );
        }
    }
}

#[test]
fn another_server_instance_forgets_the_chosen_panes() {
    let (channel, snapshots, _notices, commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots.send(live(fleet::canned(), 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    select(&mut dashboard, &mut text, 15);
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.view.detail, Some(15));
    commands.try_iter().for_each(drop);

    // Noa restarts and gives every pane id out again. The link went down
    // and came back before the next frame, which takes in only the last
    // snapshot: nothing but the instance says the panes are others.
    let down = Snapshot {
        fleet: Fleet::default(),
        link: Link::Down("the connection closed".into()),
        synced_secs: 0,
        can_focus: false,
        can_input: false,
        safe_send: true,
        instance: None,
    };
    snapshots.send(down).unwrap();
    let restarted = Snapshot {
        instance: Some("restarted".into()),
        ..live(fleet::canned(), 0)
    };
    snapshots.send(restarted).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);

    assert_eq!(dashboard.view.selected, None);
    assert_eq!(dashboard.view.detail, None);
    assert!(drawer_labels(&dashboard).is_none());
    assert_eq!(dashboard.tree.overlays().count(), 0);
    assert!(
        dashboard.registry.open_buttons.is_empty(),
        "the drawer's 開く"
    );
    assert_eq!(dashboard.view.anchor, Some(14), "the list from its top");
    key(&mut dashboard, &mut text, Key::Enter);
    assert!(
        focused_panes(&commands).is_empty(),
        "the old 15 is not reached"
    );
    assert_eq!(status_of(&dashboard), NOTHING_SELECTED);

    // Snapshots from the new instance keep what is chosen on it.
    select(&mut dashboard, &mut text, 15);
    let again = Snapshot {
        instance: Some("restarted".into()),
        ..live(fleet::canned(), 1)
    };
    snapshots.send(again).unwrap();
    dashboard.before_frame(&mut text);
    assert_eq!(dashboard.view.selected, Some(15));
}
