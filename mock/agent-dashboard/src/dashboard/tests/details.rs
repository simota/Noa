use noa_gpui_core::Role;
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

/// The drawer's root, if it is open.
fn drawer(dashboard: &Dashboard) -> Option<WidgetId> {
    dashboard.registry.drawer.map(|(root, _)| root)
}

/// The labels and tags in the drawer.
fn drawer_texts(dashboard: &Dashboard) -> Vec<String> {
    let root = drawer(dashboard).expect("the drawer is open");
    let mut texts = labels_under(&dashboard.tree, root);
    texts.extend(tags_under(&dashboard.tree, root));
    texts
}

fn shows(dashboard: &Dashboard, wanted: &str) -> bool {
    drawer_texts(dashboard).iter().any(|t| t == wanted)
}

/// A live dashboard showing `fleet`.
fn live_of(fleet: Fleet) -> (Dashboard, TextStore) {
    let (channel, snapshots, _notices, _commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots.send(live(fleet, 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    (dashboard, text)
}

#[test]
fn space_opens_the_drawer_for_any_worker() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    select(&mut dashboard, &mut text, 17);
    assert_eq!(dashboard.tree.overlays().count(), 0);
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.tree.overlays().count(), 1);
    assert_eq!(dashboard.view.detail, Some(17));
    for wanted in [
        "lint-pass",
        "依頼",
        "clippy の警告をすべて解消する",
        "所要 16 分",
        "結果あり",
    ] {
        assert!(
            shows(&dashboard, wanted),
            "{wanted}: {:?}",
            drawer_texts(&dashboard)
        );
    }
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.tree.overlays().count(), 0);
    assert_eq!(dashboard.view.detail, None);
    assert!(drawer(&dashboard).is_none());
}

#[test]
fn the_card_details_button_opens_the_drawer() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let (button, _) = *dashboard
        .registry
        .detail_buttons
        .iter()
        .find(|(_, pane)| *pane == 11)
        .expect("running card 11 has a 詳細 button");
    let pos = center(&dashboard.tree, button);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(dashboard.view.detail, Some(11));
    assert_eq!(dashboard.view.selected, Some(11));
    assert!(drawer(&dashboard).is_some());
}

#[test]
fn the_drawer_follows_the_selection() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    select(&mut dashboard, &mut text, 11);
    key(&mut dashboard, &mut text, Key::Space);
    assert!(shows(&dashboard, label_of(&dashboard, 11)));
    key(&mut dashboard, &mut text, Key::Char('j'));
    assert_eq!(dashboard.view.selected, Some(12));
    assert_eq!(dashboard.view.detail, Some(12));
    assert_eq!(dashboard.tree.overlays().count(), 1);
    assert!(shows(&dashboard, label_of(&dashboard, 12)));
    assert!(!shows(&dashboard, label_of(&dashboard, 11)));
}

/// The drawer's lines of screen, when it shows a tail: the labels inside
/// the box that holds them (the queue card's box: `Role::Background`).
fn tail_lines(dashboard: &Dashboard) -> Vec<String> {
    let root = drawer(dashboard).expect("the drawer is open");
    let tree = &dashboard.tree;
    let background = tree.theme().color(Role::Background);
    fn boxes(tree: &UiTree, id: WidgetId, found: &mut Vec<WidgetId>) {
        found.push(id);
        for child in tree.children(id) {
            boxes(tree, *child, found);
        }
    }
    let mut all = Vec::new();
    boxes(tree, root, &mut all);
    for id in all {
        let parts = labels_under(tree, id);
        let own = tree.get::<Label>(id).is_none();
        if own
            && !parts.is_empty()
            && quad_at(dashboard, rect(tree, id)).is_some_and(|(color, _)| color == background)
        {
            return parts;
        }
    }
    Vec::new()
}

#[test]
fn the_drawer_shows_the_screen_tail() {
    let mut fleet = fleet::canned();
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 12)
        .unwrap()
        .screen_tail = vec!["$ make".to_owned()];
    let (mut dashboard, mut text) = live_of(fleet);
    select(&mut dashboard, &mut text, 12);
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(tail_lines(&dashboard), ["$ make"]);
    assert!(!shows(&dashboard, "まだ取得していません"));

    let (mut dashboard, mut text) = live_of(fleet::canned());
    select(&mut dashboard, &mut text, 12);
    key(&mut dashboard, &mut text, Key::Space);
    assert!(tail_lines(&dashboard).is_empty());
    assert!(shows(&dashboard, "まだ取得していません"));

    // The demo has nothing to fetch.
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    select(&mut dashboard, &mut text, 12);
    key(&mut dashboard, &mut text, Key::Space);
    assert!(shows(&dashboard, "なし"));
}

#[test]
fn a_rebuild_keeps_the_drawer_open() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    select(&mut dashboard, &mut text, 11);
    key(&mut dashboard, &mut text, Key::Space);
    key(&mut dashboard, &mut text, Key::Char('5'));
    assert!(!dashboard.view.shown[Panel::Graph as usize]);
    assert_eq!(dashboard.view.detail, Some(11));
    assert_eq!(dashboard.tree.overlays().count(), 1);
    assert!(shows(&dashboard, label_of(&dashboard, 11)));
}

#[test]
fn the_time_rows_follow_the_worker_in_place() {
    let (mut dashboard, mut text) = live_of(fleet::canned());
    select(&mut dashboard, &mut text, 11);
    key(&mut dashboard, &mut text, Key::Space);
    assert!(shows(&dashboard, "人待ち合計 3 分"));
    let mut fleet = dashboard.feed.fleet.clone();
    let worker = fleet.workers.iter_mut().find(|w| w.pane == 11).unwrap();
    worker.phases.push((fleet::Phase::Input, 5.0));
    worker.started_minutes = Some(30.0);
    worker.since = "1 分前に出力".to_owned();
    let before = drawer(&dashboard);
    dashboard.apply(live(fleet, 0), &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(drawer(&dashboard), before, "no rebuild");
    for wanted in [
        "人待ち合計 8 分",
        "開始 30 分前",
        "1 分前に出力",
        "入力待ち 5 分",
    ] {
        assert!(
            shows(&dashboard, wanted),
            "{wanted}: {:?}",
            drawer_texts(&dashboard)
        );
    }
}

#[test]
fn the_drawer_stays_inside_the_window() {
    for (width, height) in [(1100.0, 700.0), (1440.0, 900.0), (1920.0, 1200.0)] {
        let size = Size::new(width, height);
        let mut dashboard = Dashboard::new(Grouping::Orchestrator);
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(size, &mut text);
        for pane in [13, 11, 17] {
            select(&mut dashboard, &mut text, pane);
            dashboard.dispatch(
                Event::KeyDown {
                    key: Key::Space,
                    modifiers: Modifiers::default(),
                },
                &mut text,
            );
            dashboard.prepare(size, &mut text);
            let root = drawer(&dashboard).unwrap();
            let drawer = rect(&dashboard.tree, root);
            assert!(
                inside(drawer, [0.0, 0.0, width, height]),
                "{drawer:?} in {width}x{height}"
            );
            let (open, close) = (
                dashboard.registry.open_buttons.last().unwrap().0,
                dashboard.registry.drawer.unwrap().1,
            );
            for button in [open, close] {
                assert!(inside(rect(&dashboard.tree, button), drawer));
            }
            dashboard.dispatch(
                Event::KeyDown {
                    key: Key::Space,
                    modifiers: Modifiers::default(),
                },
                &mut text,
            );
        }
    }
}

/// `GroupBox::card` made the running cards 247 / 231 / 255 px and the done
/// cards 198 px tall (measured before U1). The caption line (15 px) became a
/// row with a small 詳細 button, 26.32 px tall: every card grew by 11.32 px.
#[test]
fn the_running_cards_keep_their_height() {
    const ROW_GROWTH: f32 = 26.32 - 15.0;
    let (dashboard, _text) = laid_out(Grouping::Orchestrator);
    let height_of = |panel: Panel, pane: u64| {
        let (card, _) = dashboard.registry.lists[panel as usize]
            .as_ref()
            .unwrap()
            .items
            .iter()
            .find(|(_, p)| *p == Some(pane))
            .copied()
            .unwrap();
        rect(&dashboard.tree, card)[3]
    };
    for (panel, pane, group_box) in [
        (Panel::Running, 11, 247.0),
        (Panel::Running, 12, 231.0),
        (Panel::Running, 13, 255.0),
        (Panel::Done, 17, 198.0),
        (Panel::Done, 18, 198.0),
    ] {
        let height = height_of(panel, pane);
        assert!(
            (height - (group_box + ROW_GROWTH)).abs() <= 0.5,
            "card {pane}: {height} vs {}",
            group_box + ROW_GROWTH
        );
    }
}
