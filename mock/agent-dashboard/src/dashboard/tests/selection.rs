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
fn j_and_k_walk_the_panel_order() {
    let order = [14, 19, 15, 16, 13, 11, 12, 17, 18];
    let mut back: Vec<u64> = order.iter().rev().copied().skip(1).collect();
    back.push(14);
    for (next, prev) in [
        (Key::Char('j'), Key::Char('k')),
        (Key::ArrowDown, Key::ArrowUp),
    ] {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        assert_eq!(walk(&mut dashboard, &mut text, next, 9), order);
        assert_eq!(walk(&mut dashboard, &mut text, next, 1), [18], "no wrap");
        assert_eq!(walk(&mut dashboard, &mut text, prev, 9), back);
    }
}

fn running_card(dashboard: &Dashboard, pane: u64) -> WidgetId {
    let list = dashboard.registry.lists[Panel::Running as usize]
        .as_ref()
        .unwrap();
    let (card, _) = list.items.iter().find(|(_, p)| *p == Some(pane)).unwrap();
    *card
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
fn the_selection_shows_in_card_lane_and_node() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    select(&mut dashboard, &mut text, 11);
    let card = running_card(&dashboard, 12);
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
    assert_eq!(running_card(&dashboard, 12), card, "the screen was rebuilt");
    assert_eq!(assert_marks(&dashboard, 12), ["bar", "lane", "node"]);
    key(&mut dashboard, &mut text, Key::Char('j'));
    assert_eq!(dashboard.view.selected, Some(17));
    assert!(marked(&dashboard), "the screen was rebuilt");
    assert_eq!(running_card(&dashboard, 12), card, "the screen was rebuilt");
    assert_eq!(assert_marks(&dashboard, 17), ["bar", "lane", "node"]);
}

#[test]
fn selecting_a_queue_worker_expands_its_card() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    select(&mut dashboard, &mut text, 19);
    assert_eq!(dashboard.view.expanded, Some(19));
    let open: Vec<u64> = dashboard
        .queue_buttons
        .open
        .iter()
        .map(|(_, pane)| *pane)
        .collect();
    assert_eq!(open, [19]);
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

fn status_of(dashboard: &Dashboard) -> String {
    let status = dashboard.tree.get::<Label>(dashboard.status).unwrap();
    status.text().to_owned()
}

fn focused_panes(commands: &std::sync::mpsc::Receiver<Command>) -> Vec<u64> {
    commands
        .try_iter()
        .filter_map(|c| match c {
            Command::Focus(pane) => Some(pane),
            Command::Watch(_) => None,
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
        "ペインへ移動できません: Noa の server-scopes に control がありません"
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
    select(&mut dashboard, &mut text, 13);
    let mut fleet = fleet::canned();
    fleet.workers.retain(|w| w.pane != 13);
    dashboard.apply(live(fleet, 0), &mut text);
    dashboard.prepare(window(), &mut text);
    // perf-probe was fifth: 14, 19, 15, 16, then api-refactor.
    assert_eq!(dashboard.view.selected, Some(11));
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
    // Hiding the queue takes fix-tests out of the order: the selection
    // moves to the pane now at its place, and the drawer with it.
    key(&mut dashboard, &mut text, Key::Char('1'));
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
    assert_eq!(status, format!("モック: {label} のペインへ移動します"));
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
    assert_eq!(dashboard.view.selected, Some(19), "fix-tests' place");
}

#[test]
fn a_live_drawer_watches_the_worker_it_moved_to() {
    let (mut dashboard, mut text, commands) = live_with(live(fleet::canned(), 0));
    key(&mut dashboard, &mut text, Key::Char('j'));
    key(&mut dashboard, &mut text, Key::Space);
    commands.try_iter().for_each(drop);
    key(&mut dashboard, &mut text, Key::Char('1'));
    let selected = dashboard.view.selected.unwrap();
    let watched: Vec<Command> = commands.try_iter().collect();
    assert_eq!(watched.last(), Some(&Command::Watch(vec![selected])));
}

/// The queue's cards from top to bottom on screen.
fn queue_on_screen(dashboard: &Dashboard) -> Vec<u64> {
    let list = dashboard.registry.lists[Panel::Queue as usize]
        .as_ref()
        .unwrap();
    let mut cards: Vec<(f32, u64)> = list
        .items
        .iter()
        .map(|(id, pane)| (rect(&dashboard.tree, *id)[1], pane.unwrap()))
        .collect();
    cards.sort_by(|a, b| a.0.total_cmp(&b.0));
    cards.into_iter().map(|(_, pane)| pane).collect()
}

#[test]
fn the_queue_cards_keep_the_order_j_walks() {
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet::canned(), 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    assert_eq!(queue_on_screen(&dashboard), [14, 19, 15, 16, 13]);
    let card = |d: &Dashboard, pane: u64| {
        let list = d.registry.lists[Panel::Queue as usize].as_ref().unwrap();
        list.items.iter().find(|(_, p)| *p == Some(pane)).unwrap().0
    };
    let smoke = card(&dashboard, 19);

    // Only e2e-smoke's wait grows, past fix-tests' (the same need rank).
    let mut fleet = fleet::canned();
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 19)
        .unwrap()
        .wait_minutes = 3.0;
    dashboard.apply(live(fleet, 0), &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(card(&dashboard, 19), smoke, "updated in place");
    let shown = queue_on_screen(&dashboard);
    assert_eq!(shown, [19, 14, 15, 16, 13]);
    assert_eq!(dashboard.order()[..5], shown);
    key(&mut dashboard, &mut text, Key::Char('j'));
    assert_eq!(
        dashboard.view.selected,
        Some(shown[0]),
        "J takes the top card"
    );
}

#[test]
fn every_graph_node_names_its_root_row() {
    let (dashboard, _text) = laid_out(Grouping::Orchestrator);
    let tree = &dashboard.tree;
    let graph = dashboard.registry.lists[Panel::Graph as usize]
        .as_ref()
        .unwrap();
    for worker in &dashboard.feed.fleet.workers {
        let item = graph.item_of(worker.pane).unwrap_or_else(|| {
            panic!("no root row shows {}", worker.label);
        });
        let (row, _) = graph.items[item];
        let (pill, _, _) = dashboard
            .registry
            .marks
            .iter()
            .find(|(_, pane, mark)| *pane == worker.pane && matches!(mark, Mark::Node { .. }))
            .unwrap();
        let mut up = Some(*pill);
        while up.is_some() && up != Some(row) {
            up = up.and_then(|id| tree.parent(id));
        }
        assert_eq!(up, Some(row), "{}'s node is not in its row", worker.label);
    }
    // perf-wish has no pane of its own: its row holds perf-probe (a child)
    // and e2e-smoke (a grandchild).
    let perf_wish = graph.item_of(13).unwrap();
    assert_eq!(graph.items[perf_wish].1, None);
    assert_eq!(graph.item_of(19), Some(perf_wish));
}

fn last_watch(commands: &std::sync::mpsc::Receiver<Command>) -> Option<Vec<u64>> {
    commands
        .try_iter()
        .filter_map(|c| match c {
            Command::Watch(panes) => Some(panes),
            Command::Focus(_) => None,
        })
        .last()
}

fn open_panes(dashboard: &Dashboard) -> Vec<u64> {
    let open = dashboard.queue_buttons.open.iter();
    open.map(|(_, pane)| *pane).collect()
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

    // Hiding the queue leaves fix-tests nowhere to be selected.
    key(&mut dashboard, &mut text, Key::Char('1'));
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
fn the_queue_comes_back_with_the_selected_card_expanded() {
    let (mut dashboard, mut text, commands) = live_with(live(fleet::canned(), 0));
    key(&mut dashboard, &mut text, Key::Char('1'));
    select(&mut dashboard, &mut text, 13);
    commands.try_iter().for_each(drop);

    key(&mut dashboard, &mut text, Key::Char('1'));
    assert_eq!(dashboard.view.selected, Some(13));
    assert_eq!(dashboard.queue_buttons.expanded, Some(13));
    assert_eq!(open_panes(&dashboard), [13], "開く is on the selected card");
    assert_eq!(last_watch(&commands), Some(vec![13]));
}

/// The queue's card widgets, by pane.
fn queue_cards(dashboard: &Dashboard) -> Vec<(WidgetId, Option<u64>)> {
    let list = dashboard.registry.lists[Panel::Queue as usize].as_ref();
    let mut cards = list.unwrap().items.clone();
    cards.sort_by_key(|(_, pane)| *pane);
    cards
}

#[test]
fn a_new_top_card_is_expanded_in_place() {
    let (mut dashboard, mut text, commands) = live_with(live(fleet::canned(), 0));
    assert_eq!(dashboard.queue_buttons.expanded, Some(14));
    let cards = queue_cards(&dashboard);
    // Only e2e-smoke's wait grows, past fix-tests': it is the top card now.
    let mut fleet = fleet::canned();
    let smoke = fleet.workers.iter_mut().find(|w| w.pane == 19).unwrap();
    smoke.wait_minutes = 3.0;
    dashboard.apply(live(fleet.clone(), 0), &mut text);
    dashboard.prepare(window(), &mut text);

    assert_eq!(queue_cards(&dashboard), cards, "no rebuild");
    assert_eq!(queue_on_screen(&dashboard), [19, 14, 15, 16, 13]);
    assert_eq!(dashboard.queue_buttons.expanded, Some(19));
    assert_eq!(open_panes(&dashboard), [19]);
    let details: Vec<u64> = dashboard
        .queue_buttons
        .details
        .iter()
        .map(|b| b.1)
        .collect();
    assert_eq!(details, [14, 15, 16, 13]);
    assert_eq!(last_watch(&commands), Some(vec![19]));
    // What a screen built for this fleet shows (and owns).
    let mut fresh = Dashboard::live(Grouping::Orchestrator, Channel::detached().0);
    let mut fresh_text = TextStore::new(Default::default());
    fresh.prepare(window(), &mut fresh_text);
    fresh.apply(live(fleet, 0), &mut fresh_text);
    fresh.prepare(window(), &mut fresh_text);
    assert_eq!(fresh.queue_buttons.expanded, Some(19));
    assert_eq!(
        frame_of(dashboard.display_list(), &text),
        frame_of(fresh.display_list(), &fresh_text)
    );
    assert_eq!(text.layouts().len(), fresh_text.layouts().len());

    // A rebuild for something else keeps it.
    key(&mut dashboard, &mut text, Key::Char('5'));
    assert_eq!(dashboard.queue_buttons.expanded, Some(19));
    assert_eq!(last_watch(&commands), None, "the watch did not change");
}

#[test]
fn running_and_done_cards_are_named_groups() {
    let (dashboard, _text) = laid_out(Grouping::Orchestrator);
    let update = dashboard.tree.accessibility_update(1.0);
    let groups: Vec<&str> = update
        .nodes
        .iter()
        .filter(|(_, node)| node.role() == noa_gpui_core::accesskit::Role::Group)
        .filter_map(|(_, node)| node.label())
        .collect();
    let carded = dashboard
        .feed
        .fleet
        .workers
        .iter()
        .filter(|w| matches!(w.state, fleet::State::Running | fleet::State::Finished));
    let mut count = 0;
    for worker in carded {
        count += 1;
        let label = worker.label.as_str();
        assert!(
            groups.contains(&label),
            "no group for {label} in {groups:?}"
        );
    }
    assert_eq!(count, 5, "three running cards and two done cards");
}
