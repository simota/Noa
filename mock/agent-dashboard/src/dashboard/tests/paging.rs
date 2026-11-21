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
    wheel_at(dashboard, text, window(), pos, dy);
}

/// A wheel event, then the next frame's layout at `size`.
fn wheel_at(dashboard: &mut Dashboard, text: &mut TextStore, size: Size, pos: Point, dy: f32) {
    let delta = Point::new(0.0, dy);
    dashboard.dispatch(Event::Wheel { pos, delta }, text);
    dashboard.prepare(size, text);
}

/// The centre of a panel's list, where a wheel pages it.
fn over_list(dashboard: &Dashboard, panel: Panel) -> Point {
    center(&dashboard.tree, parts(dashboard, panel).list)
}

fn first(dashboard: &Dashboard, panel: Panel) -> usize {
    dashboard.view.first[panel as usize]
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
            if alone_and_taller {
                // It starts at the list's top, and the list cuts what is
                // drawn of it at its bottom.
                assert!((item[1] - list[1]).abs() <= 0.5, "{what} {panel:?}");
                let seen = tree.visible_rect(*id).expect("a tall item is drawn");
                let seen = [seen.x, seen.y, seen.width, seen.height];
                assert!(
                    inside(seen, list),
                    "{what} {panel:?}: {seen:?} drawn outside its list {list:?}"
                );
                let bottom = |r: [f32; 4]| r[1] + r[3];
                assert!((bottom(seen) - bottom(list)).abs() <= 0.5, "{what}");
            }
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

/// How far the wheel pages depends on how far it travelled, not on how the
/// platform cut the travel into events: at 1440×900 one step still leaves
/// perf-probe hidden below (docs-sync's row and the perf-wish group do not
/// fit together), so 80 px pages twice, in one event or in two.
#[test]
fn one_long_wheel_event_pages_as_far_as_short_ones() {
    let (mut long, mut text) = laid_out(Grouping::Orchestrator);
    let pos = over_list(&long, Panel::Running);
    wheel(&mut long, &mut text, pos, -80.0);
    let (mut short, mut short_text) = laid_out(Grouping::Orchestrator);
    wheel(&mut short, &mut short_text, pos, -40.0);
    wheel(&mut short, &mut short_text, pos, -40.0);

    for (dashboard, what) in [(&long, "one -80 event"), (&short, "two -40 events")] {
        assert_eq!(first(dashboard, Panel::Running), 2, "{what}");
        assert!(shows(dashboard, Panel::Running, 13), "{what}: perf-probe");
        assert_eq!(
            indicator(dashboard, Panel::Running).as_deref(),
            Some("上に 2 件"),
            "{what}"
        );
    }
    // Far past the end pages no further than the last page, and the way
    // back starts at once.
    wheel(&mut long, &mut text, pos, -400.0);
    assert_eq!(first(&long, Panel::Running), 2);
    wheel(&mut long, &mut text, pos, 40.0);
    assert_eq!(first(&long, Panel::Running), 1);
}

/// A list built again (the filter cleared, a panel shown again) shows the
/// worker that is still selected.
#[test]
fn clearing_the_filter_keeps_the_selected_card_in_view() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    key(&mut dashboard, &mut text, Key::Char('/'));
    typed(&mut dashboard, &mut text, "/");
    typed(&mut dashboard, &mut text, "perf");
    key(&mut dashboard, &mut text, Key::Enter);
    key(&mut dashboard, &mut text, Key::Char('j'));
    assert_eq!(dashboard.view.selected, Some(13));

    key(&mut dashboard, &mut text, Key::Escape);
    assert_eq!(dashboard.filter.query, "");
    assert_eq!(dashboard.view.selected, Some(13));
    assert!(
        shows(&dashboard, Panel::Running, 13),
        "perf-probe is hidden"
    );
    assert!(shows(&dashboard, Panel::Queue, 13));
    assert_whole_items(&dashboard, "filter cleared");
}

#[test]
fn showing_a_panel_again_shows_the_selected_card() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    press(&mut dashboard, &mut text, '2', Modifiers::default());
    assert!(dashboard.registry.lists[Panel::Running as usize].is_none());
    // J walks the queue: 14, 19, 15, 16, then perf-probe (13).
    for _ in 0..5 {
        key(&mut dashboard, &mut text, Key::Char('j'));
    }
    assert_eq!(dashboard.view.selected, Some(13));

    press(&mut dashboard, &mut text, '2', Modifiers::default());
    assert!(
        shows(&dashboard, Panel::Running, 13),
        "perf-probe is hidden"
    );
    assert_whole_items(&dashboard, "running shown again");
}

/// Paged away from the selected card, the page stays where the wheel put
/// it, also when the screen is laid out again or a snapshot updates it in
/// place; a new selection brings the page back to it.
#[test]
fn the_wheel_does_not_fight_the_selection() {
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet::canned(), 1));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    key(&mut dashboard, &mut text, Key::Char('j'));
    while dashboard.view.selected != Some(11) {
        key(&mut dashboard, &mut text, Key::Char('j'));
    }
    assert_eq!(first(&dashboard, Panel::Running), 0);
    let pos = over_list(&dashboard, Panel::Running);
    wheel(&mut dashboard, &mut text, pos, -80.0);
    assert_eq!(first(&dashboard, Panel::Running), 2);
    assert!(!shows(&dashboard, Panel::Running, 11), "api-refactor");

    dashboard.prepare(window(), &mut text);
    let mut later = fleet::canned();
    for worker in &mut later.workers {
        worker.lines_per_tick.reverse();
    }
    dashboard.apply(live(later, 2), &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(dashboard.view.selected, Some(11));
    assert_eq!(first(&dashboard, Panel::Running), 2, "the page went back");

    key(&mut dashboard, &mut text, Key::Char('k'));
    key(&mut dashboard, &mut text, Key::Char('j'));
    assert_eq!(dashboard.view.selected, Some(11));
    assert!(shows(&dashboard, Panel::Running, 11), "api-refactor");
}

/// A window made larger shows what now fits above the page: at 1100×700 the
/// queue hides cards and can be paged; at 1440×900 all five fit again.
#[test]
fn a_larger_window_pages_back_what_fits() {
    let small = Size::new(1100.0, 700.0);
    let mut dashboard = Dashboard::new(Grouping::Orchestrator);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(small, &mut text);
    assert!(visible_items(&dashboard, Panel::Queue).len() < 5);
    let pos = over_list(&dashboard, Panel::Queue);
    wheel_at(&mut dashboard, &mut text, small, pos, -40.0);
    assert_eq!(first(&dashboard, Panel::Queue), 1);

    dashboard.prepare(window(), &mut text);
    assert_eq!(first(&dashboard, Panel::Queue), 0);
    assert_eq!(visible_items(&dashboard, Panel::Queue).len(), 5);
    assert_eq!(indicator(&dashboard, Panel::Queue), None);
    assert_whole_items(&dashboard, "1100×700 → 1440×900");
}

/// Another server instance's workers start at the top of every list, as
/// the panes chosen on the old one are forgotten.
#[test]
fn another_server_starts_the_lists_at_the_top() {
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet::canned(), 1));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let pos = over_list(&dashboard, Panel::Running);
    wheel(&mut dashboard, &mut text, pos, -40.0);
    wheel(&mut dashboard, &mut text, pos, -40.0);
    assert_eq!(first(&dashboard, Panel::Running), 2);

    let restarted = Snapshot {
        instance: Some("restarted".into()),
        ..live(fleet::canned(), 1)
    };
    dashboard.apply(restarted, &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(first(&dashboard, Panel::Running), 0);
    assert!(
        shows(&dashboard, Panel::Running, 11),
        "the new server's first"
    );
    assert_eq!(
        indicator(&dashboard, Panel::Running).as_deref(),
        Some("下に 1 件")
    );
}

/// A frame in which nothing changed — no input, no data, the same size —
/// lays nothing out again and has nothing to present (the live window
/// wakes every second).
#[test]
fn an_unchanged_frame_presents_nothing() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    assert!(!dashboard.prepare(window(), &mut text), "after the build");
    assert!(!dashboard.tree.needs_update(window()));
    assert!(!shows(&dashboard, Panel::Running, 13));

    let pos = over_list(&dashboard, Panel::Running);
    wheel(&mut dashboard, &mut text, pos, -40.0);
    assert!(!dashboard.prepare(window(), &mut text), "after a page");
    assert_eq!(first(&dashboard, Panel::Running), 1);

    let small = Size::new(1100.0, 700.0);
    assert!(dashboard.prepare(small, &mut text), "a new size");
    assert!(!dashboard.prepare(small, &mut text), "after the new size");
    assert_whole_items(&dashboard, "1100×700");
}

/// Whatever other panels are shown, the demo's queue shows all five cards
/// at 1440×900 and hides none.
#[test]
fn the_whole_queue_shows_with_any_other_panels() {
    let mut text = TextStore::new(Default::default());
    for others in 0..16_usize {
        let mut shown = [true; 5];
        for (bit, panel) in Panel::ALL[1..].iter().enumerate() {
            shown[*panel as usize] = others & (1 << bit) != 0;
        }
        let mut dashboard = Dashboard::showing(View {
            shown,
            ..View::of(Grouping::Orchestrator)
        });
        dashboard.prepare(window(), &mut text);
        let what = format!("shown {shown:?}");
        assert_eq!(visible_items(&dashboard, Panel::Queue).len(), 5, "{what}");
        assert_eq!(indicator(&dashboard, Panel::Queue), None, "{what}");
        assert_whole_items(&dashboard, &what);
    }
}

/// An orchestrator with more workers than its graph row can show at
/// 1100×700: the row is shown alone and the list cuts it at its bottom.
#[test]
fn a_row_taller_than_its_list_is_cut_at_the_list() {
    let mut fleet = fleet::canned();
    for n in 0..24 {
        let mut worker = fleet.workers[0].clone();
        worker.pane = 100 + n;
        worker.label = format!("extra-{n}");
        fleet.workers.push(worker);
    }
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 1));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(Size::new(1100.0, 700.0), &mut text);
    let parts = parts(&dashboard, Panel::Graph);
    let list = rect(&dashboard.tree, parts.list);
    let tall = parts.items.iter().map(|(id, _)| *id).find(|id| {
        let row = rect(&dashboard.tree, *id);
        row[3] > list[3] + 1.0
    });
    let tall = tall.expect("a graph row taller than the list");
    let tall_index = parts.items.iter().position(|(id, _)| *id == tall).unwrap();
    // Page to the tall row: it is shown alone, from the list's top.
    let pos = over_list(&dashboard, Panel::Graph);
    let size = Size::new(1100.0, 700.0);
    for _ in 0..tall_index {
        wheel_at(&mut dashboard, &mut text, size, pos, -40.0);
    }
    assert_eq!(visible_items(&dashboard, Panel::Graph), [tall]);
    let row = rect(&dashboard.tree, tall);
    assert!(
        row[1] + row[3] > list[1] + list[3] + 1.0,
        "{row:?} {list:?}"
    );
    assert_whole_items(&dashboard, "a tall graph row");
}

/// A fleet of running workers only, panes 1, 2, …, ungrouped by
/// `Grouping::Flat`: a card's caption gains one 15 px line per `extra[n]`
/// (a running card is 247 px tall at 1440 wide and 1100 wide, its caption
/// keeps the worker's `since`, which a volatile snapshot changes in place).
fn running_fleet(extra: &[usize]) -> Fleet {
    let mut fleet = fleet::canned();
    let base = fleet.workers[0].clone();
    fleet.orchestrators.clear();
    fleet.workers = extra
        .iter()
        .zip(1..)
        .map(|(&lines, pane)| Worker {
            pane,
            label: format!("worker-{pane}"),
            orchestrator: String::new(),
            since: tall_since(lines),
            ..base.clone()
        })
        .collect();
    fleet
}

/// A `since` that adds `lines` lines to a running card's caption.
fn tall_since(lines: usize) -> String {
    std::iter::once("8 秒前に出力")
        .chain(std::iter::repeat_n("…", lines))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The running fleet as a live dashboard laid out at `size`.
fn running_at(extra: &[usize], size: Size) -> (Dashboard, TextStore) {
    let mut dashboard = Dashboard::of_snapshot(Grouping::Flat, live(running_fleet(extra), 1));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(size, &mut text);
    (dashboard, text)
}

/// Shown panes of a panel's items, in order.
fn shown_panes(dashboard: &Dashboard, panel: Panel) -> Vec<u64> {
    let parts = parts(dashboard, panel);
    let shown = parts
        .items
        .iter()
        .filter(|(id, _)| !dashboard.tree.is_hidden(*id));
    shown.filter_map(|(_, pane)| *pane).collect()
}

/// Travel that turns no page does not linger: at the demo's last running
/// page, 10 px more forward is dropped (the end refuses it), so 40 px back
/// turns one page back whether the forward travel came as one event or as
/// two.
#[test]
fn travel_past_the_end_does_not_eat_the_way_back() {
    let at_the_end = || {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        let pos = over_list(&dashboard, Panel::Running);
        wheel(&mut dashboard, &mut text, pos, -40.0);
        wheel(&mut dashboard, &mut text, pos, -40.0);
        assert_eq!(first(&dashboard, Panel::Running), 2);
        assert_eq!(
            indicator(&dashboard, Panel::Running).as_deref(),
            Some("上に 2 件")
        );
        (dashboard, text, pos)
    };
    for split in [&[-50.0, 40.0][..], &[-40.0, -10.0, 40.0], &[-10.0, 40.0]] {
        let (mut dashboard, mut text, pos) = at_the_end();
        for dy in split {
            wheel(&mut dashboard, &mut text, pos, *dy);
        }
        assert_eq!(first(&dashboard, Panel::Running), 1, "{split:?}");
        assert_eq!(
            indicator(&dashboard, Panel::Running).as_deref(),
            Some("上に 1 件 · 下に 1 件"),
            "{split:?}"
        );
    }
    // Turning back drops the remainder of the way forward too.
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let pos = over_list(&dashboard, Panel::Running);
    for dy in [-30.0, 30.0, -30.0] {
        wheel(&mut dashboard, &mut text, pos, dy);
    }
    assert_eq!(first(&dashboard, Panel::Running), 0);
}

/// Two columns of cards of two heights, low (247 px) and high (307 px):
/// L L L H H. At 1440×980 the running list (592 px) holds a low row and a
/// high row but not two high rows. From the top (rows LL, LH, H) one step
/// shows LL, HH: everything left, so that is the last page — a further
/// step would wrap the cards as LH, H and hide the last again. One long
/// wheel event stops on the same page as short ones.
#[test]
fn a_long_wheel_event_stops_at_the_last_page() {
    let size = Size::new(1440.0, 980.0);
    let extra = [0, 0, 0, 4, 4];
    let (dashboard, _) = running_at(&extra, size);
    let list = rect(&dashboard.tree, parts(&dashboard, Panel::Running).list);
    assert_eq!(list[3], 592.0, "the running list's height");
    assert_eq!(shown_panes(&dashboard, Panel::Running), [1, 2, 3, 4]);

    for split in [
        &[-40.0, -40.0][..],
        &[-80.0],
        &[-120.0],
        &[-40.0, -40.0, -40.0],
    ] {
        let (mut dashboard, mut text) = running_at(&extra, size);
        let pos = over_list(&dashboard, Panel::Running);
        for dy in split {
            wheel_at(&mut dashboard, &mut text, size, pos, *dy);
        }
        let what = format!("{split:?}");
        assert_eq!(first(&dashboard, Panel::Running), 1, "{what}");
        assert_eq!(
            shown_panes(&dashboard, Panel::Running),
            [2, 3, 4, 5],
            "{what}"
        );
        assert_eq!(
            indicator(&dashboard, Panel::Running).as_deref(),
            Some("上に 1 件"),
            "{what}"
        );
        assert_whole_items(&dashboard, &what);
        // And the way back is one step.
        wheel_at(&mut dashboard, &mut text, size, pos, 40.0);
        assert_eq!(first(&dashboard, Panel::Running), 0, "{what}");
    }
}

/// One column (1100×1200: the running list is 812 px tall) of a tall card
/// (607 px) and a low one: on the last page the low card shows alone. When
/// the tall card, hidden above, shrinks in place to fit with it, the page
/// takes it back and the indicator says nothing is hidden.
#[test]
fn an_item_above_that_shrinks_comes_back() {
    let size = Size::new(1100.0, 1200.0);
    let (mut dashboard, mut text) = running_at(&[24, 0], size);
    let list = rect(&dashboard.tree, parts(&dashboard, Panel::Running).list);
    assert_eq!(list[3], 812.0, "the running list's height");
    let pos = over_list(&dashboard, Panel::Running);
    wheel_at(&mut dashboard, &mut text, size, pos, -40.0);
    assert_eq!(shown_panes(&dashboard, Panel::Running), [2]);
    assert_eq!(
        indicator(&dashboard, Panel::Running).as_deref(),
        Some("上に 1 件")
    );

    dashboard.apply(live(running_fleet(&[0, 0]), 2), &mut text);
    dashboard.prepare(size, &mut text);
    assert_eq!(shown_panes(&dashboard, Panel::Running), [1, 2]);
    assert_eq!(first(&dashboard, Panel::Running), 0);
    assert_eq!(indicator(&dashboard, Panel::Running), None);
    assert_whole_items(&dashboard, "the card above shrank");
    assert!(!dashboard.prepare(size, &mut text), "a frame after");
}

/// The same column with two low cards, the lower one selected: when the
/// upper card grows in place so that both no longer fit, the page follows
/// the selection (no wheel moved it away) instead of hiding it.
#[test]
fn a_shown_selection_stays_shown_when_a_card_grows() {
    let size = Size::new(1100.0, 1200.0);
    let (mut dashboard, mut text) = running_at(&[0, 0], size);
    while dashboard.view.selected != Some(2) {
        dashboard.dispatch(
            Event::KeyDown {
                key: Key::Char('j'),
                modifiers: Modifiers::default(),
            },
            &mut text,
        );
        dashboard.prepare(size, &mut text);
    }
    assert_eq!(shown_panes(&dashboard, Panel::Running), [1, 2]);

    dashboard.apply(live(running_fleet(&[30, 0]), 2), &mut text);
    dashboard.prepare(size, &mut text);
    assert_eq!(dashboard.view.selected, Some(2));
    assert_eq!(shown_panes(&dashboard, Panel::Running), [2]);
    assert_eq!(first(&dashboard, Panel::Running), 1);
    assert_eq!(
        indicator(&dashboard, Panel::Running).as_deref(),
        Some("上に 1 件")
    );
    assert_whole_items(&dashboard, "the card above grew");
    assert!(!dashboard.prepare(size, &mut text), "a frame after");
}
