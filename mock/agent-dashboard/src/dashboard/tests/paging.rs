use noa_gpui_core::Key;

use super::*;

const SIZES: [Size; 3] = [
    Size::new(1100.0, 700.0),
    Size::new(1440.0, 900.0),
    Size::new(1920.0, 1200.0),
];

/// The canned fleet with six more running workers: more lanes and graph
/// children than the history row holds at 1100×700.
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

fn parts(dashboard: &Dashboard, panel: Panel) -> &ListParts {
    dashboard.registry.lists[panel as usize].as_ref().unwrap()
}

/// The indicator's text, `None` while it is hidden.
fn indicator(dashboard: &Dashboard, panel: Panel) -> Option<String> {
    let id = parts(dashboard, panel).indicator;
    let label = dashboard.tree.get::<Label>(id).unwrap();
    (!dashboard.tree.is_hidden(id)).then(|| label.text().to_owned())
}

/// Whether the item that shows `pane` as its own is shown.
fn shows(dashboard: &Dashboard, panel: Panel, pane: u64) -> bool {
    let parts = parts(dashboard, panel);
    let (id, _) = parts.items.iter().find(|(_, p)| *p == Some(pane)).unwrap();
    !dashboard.tree.is_hidden(*id)
}

fn wheel(dashboard: &mut Dashboard, text: &mut TextStore, pos: Point, dy: f32) {
    let delta = Point::new(0.0, dy);
    dashboard.dispatch(Event::Wheel { pos, delta }, text);
    dashboard.prepare(window(), text);
}

/// Every shown item of every list lies inside its list, but for the one
/// exception paging allows: a first item taller than its list, shown alone
/// (and clipped). A group header (or wrap grid) is shown exactly when one of
/// its items is.
fn assert_whole_items(dashboard: &Dashboard, what: &str) {
    let tree = &dashboard.tree;
    for panel in Panel::ALL {
        let Some(parts) = dashboard.registry.lists[panel as usize].as_ref() else {
            continue;
        };
        let list = rect(tree, parts.list);
        let shown: Vec<WidgetId> = visible_items(dashboard, panel);
        for id in &shown {
            let item = rect(tree, *id);
            let alone_and_taller = shown.len() == 1 && item[3] > list[3];
            assert!(
                inside(item, list) || alone_and_taller,
                "{what} {panel:?}: item {item:?} outside its list {list:?}"
            );
        }
        for (header, of) in &parts.headers {
            let any = of.iter().any(|i| !tree.is_hidden(parts.items[*i].0));
            assert_eq!(!tree.is_hidden(*header), any, "{what} {panel:?} header");
        }
    }
}

#[test]
fn no_card_is_drawn_partly() {
    for size in SIZES {
        for grouping in Grouping::ALL {
            let mut dashboard = Dashboard::new(grouping);
            let mut text = TextStore::new(Default::default());
            dashboard.prepare(size, &mut text);
            for panel in Panel::ALL {
                let list = rect(&dashboard.tree, parts(&dashboard, panel).list);
                for id in visible_items(&dashboard, panel) {
                    let item = rect(&dashboard.tree, id);
                    assert!(
                        inside(item, list),
                        "{size:?} {grouping:?} {panel:?}: {item:?} outside {list:?}"
                    );
                }
            }
            assert_whole_items(&dashboard, &format!("{size:?} {grouping:?}"));
        }
    }
}

#[test]
fn the_running_panel_says_what_it_hides() {
    let (dashboard, _text) = laid_out(Grouping::Orchestrator);
    assert!(
        !shows(&dashboard, Panel::Running, 13),
        "perf-probe is shown"
    );
    assert!(shows(&dashboard, Panel::Running, 11));
    assert!(shows(&dashboard, Panel::Running, 12));
    assert_eq!(
        indicator(&dashboard, Panel::Running).as_deref(),
        Some("下に 1 件")
    );
    assert_eq!(indicator(&dashboard, Panel::Queue), None);
    assert_eq!(visible_items(&dashboard, Panel::Queue).len(), 5);
}

/// At 1440×900 the running list (512 px) holds one release-0.9.7 row and
/// not the perf-wish group below it: one step shows docs-sync alone in its
/// row (api-refactor above), the next shows perf-probe.
#[test]
fn the_wheel_pages_one_item() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let first = |d: &Dashboard| d.view.first[Panel::Running as usize];
    let over_list = center(&dashboard.tree, parts(&dashboard, Panel::Running).list);

    wheel(&mut dashboard, &mut text, over_list, -40.0);
    assert_eq!(first(&dashboard), 1);
    assert_eq!(
        indicator(&dashboard, Panel::Running).as_deref(),
        Some("上に 1 件 · 下に 1 件")
    );
    assert!(!shows(&dashboard, Panel::Running, 11));
    assert!(shows(&dashboard, Panel::Running, 12));

    wheel(&mut dashboard, &mut text, over_list, -40.0);
    assert_eq!(first(&dashboard), 2);
    assert_eq!(
        indicator(&dashboard, Panel::Running).as_deref(),
        Some("上に 2 件")
    );
    assert!(
        shows(&dashboard, Panel::Running, 13),
        "perf-probe is hidden"
    );
    assert_whole_items(&dashboard, "paged to the end");

    // Nothing is hidden below: the wheel goes no further, and the travel
    // past the end does not delay the way back.
    wheel(&mut dashboard, &mut text, over_list, -120.0);
    assert_eq!(first(&dashboard), 2);
    wheel(&mut dashboard, &mut text, over_list, 40.0);
    assert_eq!(first(&dashboard), 1);
    // Less than a step pages nothing until it adds up to one.
    wheel(&mut dashboard, &mut text, over_list, 25.0);
    assert_eq!(first(&dashboard), 1);
    wheel(&mut dashboard, &mut text, over_list, 15.0);
    assert_eq!(first(&dashboard), 0);
    assert_eq!(
        indicator(&dashboard, Panel::Running).as_deref(),
        Some("下に 1 件")
    );

    // A wheel over the header (or a panel's title row) pages nothing.
    let header = Point::new(window().width / 2.0, 20.0);
    let title = center(&dashboard.tree, parts(&dashboard, Panel::Running).indicator);
    for pos in [header, title] {
        let before = dashboard.view.first;
        wheel(&mut dashboard, &mut text, pos, -40.0);
        assert_eq!(dashboard.view.first, before, "{pos:?}");
        assert_eq!(
            indicator(&dashboard, Panel::Running).as_deref(),
            Some("下に 1 件")
        );
    }
}

#[test]
fn selecting_brings_the_card_into_view() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    assert!(!shows(&dashboard, Panel::Running, 13));
    // J walks the queue first: 14, 19, 15, 16, then perf-probe (13).
    for _ in 0..5 {
        key(&mut dashboard, &mut text, Key::Char('j'));
    }
    assert_eq!(dashboard.view.selected, Some(13));
    assert!(shows(&dashboard, Panel::Running, 13), "not in running");
    assert!(shows(&dashboard, Panel::Queue, 13), "not in the queue");
    assert!(
        shows(&dashboard, Panel::Timeline, 13),
        "not in the timeline"
    );
    assert_eq!(dashboard.view.reveal, None, "the reveal is done");
    assert_whole_items(&dashboard, "perf-probe selected");

    // On to a card above the running page: the page goes back to it.
    key(&mut dashboard, &mut text, Key::Char('j'));
    assert_eq!(dashboard.view.selected, Some(11));
    assert!(shows(&dashboard, Panel::Running, 11), "api-refactor");
    assert_eq!(dashboard.view.first[Panel::Running as usize], 0);
    assert_whole_items(&dashboard, "api-refactor selected");
}

#[test]
fn the_timeline_and_graph_page_too() {
    let size = Size::new(1100.0, 700.0);
    for grouping in Grouping::ALL {
        let mut dashboard = Dashboard::of_snapshot(grouping, live(busy_fleet(), 1));
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(size, &mut text);
        assert_whole_items(&dashboard, &format!("{grouping:?}"));
        for panel in [Panel::Timeline, Panel::Graph] {
            let total = parts(&dashboard, panel).items.len();
            let shown = visible_items(&dashboard, panel).len();
            assert!(
                shown < total,
                "{grouping:?} {panel:?} hides nothing at 1100×700"
            );
            assert!(shown > 0, "{grouping:?} {panel:?} shows nothing");
            assert_eq!(
                indicator(&dashboard, panel),
                Some(format!("下に {} 件", total - shown)),
                "{grouping:?} {panel:?}"
            );
        }
    }
}
