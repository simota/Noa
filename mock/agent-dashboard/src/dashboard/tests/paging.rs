use noa_gpui_core::Key;

use super::super::pager::WHEEL_STEP;

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

/// Presses J, laying out at `size` after each, until `pane` is selected:
/// at most once per worker and once more, so that a J that does not move
/// the selection fails here instead of pressing forever.
fn select_by_j(dashboard: &mut Dashboard, text: &mut TextStore, size: Size, pane: u64) {
    let presses = dashboard.feed.fleet.workers.len() + 1;
    for _ in 0..presses {
        if dashboard.view.selected == Some(pane) {
            break;
        }
        let (key, modifiers) = (Key::Char('j'), Modifiers::default());
        dashboard.dispatch(Event::KeyDown { key, modifiers }, text);
        dashboard.prepare(size, text);
    }
    assert_eq!(
        dashboard.view.selected,
        Some(pane),
        "J never selected {pane}"
    );
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
/// its items is, but for a title left out above a page's first item that
/// would not fit below it: that is measured by showing the title again for
/// one layout, and the page is then laid out as it was.
fn assert_whole_items(dashboard: &mut Dashboard, text: &mut TextStore, what: &str) {
    let mut left_out_titles = Vec::new();
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
        // A group's title is shown when one of its items is, but for one
        // case: above a page's first item that does not fit below it, the
        // title is left out and the item starts at the list's top. A wrap
        // grid is shown exactly when one of its items is.
        let first_shown = parts.items.iter().position(|(id, _)| !tree.is_hidden(*id));
        for (header, of) in &parts.headers {
            let any = of.iter().any(|i| !tree.is_hidden(parts.items[*i].0));
            let grid = of
                .iter()
                .any(|i| tree.ancestors(parts.items[*i].0).any(|id| id == *header));
            let left_out = any && tree.is_hidden(*header);
            if grid || !left_out {
                assert_eq!(!tree.is_hidden(*header), any, "{what} {panel:?} header");
                continue;
            }
            let first = first_shown.expect("an item is shown");
            assert!(of.contains(&first), "{what} {panel:?}: a title left out");
            let item = rect(tree, parts.items[first].0);
            assert!(
                (item[1] - list[1]).abs() <= 0.5,
                "{what} {panel:?}: {item:?} below a title left out, in {list:?}"
            );
            left_out_titles.push((panel, *header, parts.items[first].0, list));
        }
    }
    let viewport = dashboard.viewport.expect("the dashboard is laid out");
    for (panel, title, item, list) in left_out_titles {
        dashboard.tree.set_hidden(title, false);
        dashboard.tree.prepare(viewport, text);
        let below_title = rect(&dashboard.tree, item);
        dashboard.tree.set_hidden(title, true);
        dashboard.tree.prepare(viewport, text);
        assert!(
            below_title[1] + below_title[3] > list[1] + list[3] + 0.5,
            "{what} {panel:?}: a title left out above {below_title:?}, which fits below it in {list:?}"
        );
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
            assert_whole_items(&mut dashboard, &mut text, &format!("{size:?} {grouping:?}"));
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
    assert_whole_items(&mut dashboard, &mut text, "paged to the end");

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
    assert_whole_items(&mut dashboard, &mut text, "perf-probe selected");

    // On to a card above the running page: the page goes back to it.
    key(&mut dashboard, &mut text, Key::Char('j'));
    assert_eq!(dashboard.view.selected, Some(11));
    assert!(shows(&dashboard, Panel::Running, 11), "api-refactor");
    assert_eq!(dashboard.view.first[Panel::Running as usize], 0);
    assert_whole_items(&mut dashboard, &mut text, "api-refactor selected");
}

#[test]
fn the_timeline_and_graph_page_too() {
    let size = Size::new(1100.0, 700.0);
    for grouping in Grouping::ALL {
        let mut dashboard = Dashboard::of_snapshot(grouping, live(busy_fleet(), 1));
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(size, &mut text);
        assert_whole_items(&mut dashboard, &mut text, &format!("{grouping:?}"));
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
    assert_whole_items(&mut dashboard, &mut text, "filter cleared");
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
    assert_whole_items(&mut dashboard, &mut text, "running shown again");
}

/// Paged away from the selected card, the page stays where the wheel put
/// it, also when the screen is laid out again or a snapshot updates it in
/// place; a new selection brings the page back to it.
#[test]
fn the_wheel_does_not_fight_the_selection() {
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet::canned(), 1));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    select_by_j(&mut dashboard, &mut text, window(), 11);
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
    assert_whole_items(&mut dashboard, &mut text, "1100×700 → 1440×900");
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
    assert_whole_items(&mut dashboard, &mut text, "1100×700");
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
        assert_whole_items(&mut dashboard, &mut text, &what);
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
    assert_whole_items(&mut dashboard, &mut text, "a tall graph row");
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
        assert_whole_items(&mut dashboard, &mut text, &what);
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
    assert_whole_items(&mut dashboard, &mut text, "the card above shrank");
    assert!(!dashboard.prepare(size, &mut text), "a frame after");
}

/// The same column with two low cards, the lower one selected: when the
/// upper card grows in place so that both no longer fit, the page follows
/// the selection (no wheel moved it away) instead of hiding it.
#[test]
fn a_shown_selection_stays_shown_when_a_card_grows() {
    let size = Size::new(1100.0, 1200.0);
    let (mut dashboard, mut text) = running_at(&[0, 0], size);
    select_by_j(&mut dashboard, &mut text, size, 2);
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
    assert_whole_items(&mut dashboard, &mut text, "the card above grew");
    assert!(!dashboard.prepare(size, &mut text), "a frame after");
}

/// Pages every list of `dashboard` to its end with the wheel, a step at a
/// time, checking after each step that every shown item is whole.
fn page_every_list_to_its_end(
    dashboard: &mut Dashboard,
    text: &mut TextStore,
    size: Size,
    what: &str,
) {
    assert_whole_items(dashboard, text, what);
    for panel in Panel::ALL {
        let Some(parts) = dashboard.registry.lists[panel as usize].as_ref() else {
            continue;
        };
        let steps = parts.items.len();
        let pos = over_list(dashboard, panel);
        let hides_below = |d: &Dashboard| indicator(d, panel).is_some_and(|t| t.contains("下に"));
        for step in 0..steps {
            if !hides_below(dashboard) {
                break;
            }
            wheel_at(dashboard, text, size, pos, -WHEEL_STEP);
            assert_whole_items(
                dashboard,
                text,
                &format!("{what} {panel:?} step {}", step + 1),
            );
        }
        assert!(
            !hides_below(dashboard),
            "{what} {panel:?}: never reached the end"
        );
    }
}

/// On every page of every list, at every tested size and grouping, every
/// shown item is whole unless it is taller than its list. The fleets: the
/// demo, the busy fleet, and running cards of mixed heights — one nearly
/// as tall as a list at 1100×700, one taller than any list but the one at
/// 1920×1200 — so that a group's title can push a page's first item out.
#[test]
fn every_page_shows_whole_items() {
    let mut text = TextStore::new(Default::default());
    let fleets = [busy_fleet(), running_fleet(&[0, 4, 30, 0, 4, 8, 2])];
    for size in SIZES {
        for grouping in Grouping::ALL {
            let demo = Dashboard::new(grouping);
            let live_ones = fleets
                .iter()
                .map(|fleet| Dashboard::of_snapshot(grouping, live(fleet.clone(), 1)));
            for (n, mut dashboard) in std::iter::once(demo).chain(live_ones).enumerate() {
                dashboard.prepare(size, &mut text);
                let what = format!("{size:?} {grouping:?} fleet {n}");
                page_every_list_to_its_end(&mut dashboard, &mut text, size, &what);
            }
        }
    }
}

/// D4: at 1100×700, grouped by agent, one running card as tall as the
/// running list can hold, but not with its group's title above it. The
/// title is left out and the card is shown whole from the list's top; with
/// room for both the title is back.
#[test]
fn a_card_its_list_can_hold_is_not_pushed_out_by_its_title() {
    let size = Size::new(1100.0, 700.0);
    let mut text = TextStore::new(Default::default());
    let mut grouped = |extra: &[usize]| {
        let snapshot = live(running_fleet(extra), 1);
        let mut dashboard = Dashboard::of_snapshot(Grouping::Agent, snapshot);
        dashboard.prepare(size, &mut text);
        dashboard
    };
    let card = |d: &Dashboard| rect(&d.tree, parts(d, Panel::Running).items[0].0);
    let list = |d: &Dashboard| rect(&d.tree, parts(d, Panel::Running).list);
    let title = |d: &Dashboard| {
        let parts = parts(d, Panel::Running);
        let card = parts.items[0].0;
        let titles = parts.headers.iter().map(|(id, _)| *id);
        let mut titles = titles.filter(|id| !d.tree.ancestors(card).any(|a| a == *id));
        titles.next().expect("the agent's group has a title")
    };
    let low = grouped(&[0]);
    // The title, and the gap below it, above the card.
    let above = card(&low)[1] - rect(&low.tree, title(&low))[1];
    let room = list(&low)[3];
    // The most caption lines whose card the list holds, each card measured:
    // a card does not grow by one line per line, as the first lines only
    // fill the caption row up to its 詳細 button's height. A card that
    // never outgrows the list within `MOST` lines cannot make the case.
    const MOST: usize = 200;
    let lines = (0..MOST)
        .take_while(|&n| card(&grouped(&[n + 1]))[3] <= room)
        .count();
    assert!(
        lines < MOST,
        "a card with {MOST} caption lines still fits the list of {room}"
    );

    let mut dashboard = grouped(&[lines]);
    let height = card(&dashboard)[3];
    assert!(
        height <= room && height + above > room + 0.5,
        "card {height} with {above} above it in a list of {room}"
    );
    assert!(
        inside(card(&dashboard), list(&dashboard)),
        "{:?} cut by {:?}",
        card(&dashboard),
        list(&dashboard)
    );
    assert!(
        dashboard.tree.is_hidden(title(&dashboard)),
        "the title stays"
    );
    assert_eq!(indicator(&dashboard, Panel::Running), None);
    assert_whole_items(&mut dashboard, &mut text, "a card as tall as its list");
    assert!(!dashboard.prepare(size, &mut text), "a frame after");

    let taller = Size::new(1100.0, 900.0);
    dashboard.prepare(taller, &mut text);
    assert!(!dashboard.tree.is_hidden(title(&dashboard)), "no title");
    assert!(inside(card(&dashboard), list(&dashboard)));
    assert_whole_items(&mut dashboard, &mut text, "room for the title");
}

/// D1: a page the wheel turned away from the selection keeps its first
/// item when the window changes size, as long as it still holds it.
#[test]
fn a_new_size_keeps_a_page_turned_away_from_the_selection() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    select_by_j(&mut dashboard, &mut text, window(), 11);
    assert_eq!(first(&dashboard, Panel::Running), 0);
    let pos = over_list(&dashboard, Panel::Running);
    wheel(&mut dashboard, &mut text, pos, -80.0);
    assert_eq!(first(&dashboard, Panel::Running), 2);

    for width in [1441.0, 1460.0, 1440.0] {
        dashboard.prepare(Size::new(width, 900.0), &mut text);
        assert_eq!(first(&dashboard, Panel::Running), 2, "{width}");
        assert!(shows(&dashboard, Panel::Running, 13), "{width}: perf-probe");
        assert!(
            !shows(&dashboard, Panel::Running, 11),
            "{width}: api-refactor"
        );
        assert_eq!(dashboard.view.selected, Some(11));
    }
}

/// D2: the latest of a selection and a wheel decides the page, whether or
/// not a frame came between them. J selects docs-sync (12, on the page),
/// then the wheel pages twice: perf-probe's page. The selection still
/// shows in the panels the wheel did not turn. K, after the wheel, brings
/// api-refactor (11) back.
#[test]
fn the_latest_of_a_selection_and_a_wheel_decides_the_page() {
    for frame_between in [true, false] {
        let what = format!("a frame between: {frame_between}");
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        select_by_j(&mut dashboard, &mut text, window(), 11);
        let pos = over_list(&dashboard, Panel::Running);
        let (j, modifiers) = (Key::Char('j'), Modifiers::default());
        dashboard.dispatch(Event::KeyDown { key: j, modifiers }, &mut text);
        if frame_between {
            dashboard.prepare(window(), &mut text);
        }
        assert_eq!(dashboard.view.selected, Some(12), "{what}");
        wheel(&mut dashboard, &mut text, pos, -80.0);
        assert_eq!(first(&dashboard, Panel::Running), 2, "{what}");
        assert!(shows(&dashboard, Panel::Running, 13), "{what}: perf-probe");
        assert_eq!(
            indicator(&dashboard, Panel::Running).as_deref(),
            Some("上に 2 件"),
            "{what}"
        );
        assert!(shows(&dashboard, Panel::Timeline, 12), "{what}: the lane");
        assert_eq!(dashboard.view.selected, Some(12), "{what}");

        key(&mut dashboard, &mut text, Key::Char('k'));
        assert_eq!(dashboard.view.selected, Some(11), "{what}");
        assert_eq!(first(&dashboard, Panel::Running), 0, "{what}");
        assert!(
            shows(&dashboard, Panel::Running, 11),
            "{what}: api-refactor"
        );
    }
}

/// D3: two columns of a low and a high card height, L L H H, at 1440×900:
/// the wheel pages twice to the last page (H H). Made taller so that the
/// list holds a low and a high row but not two high rows, the list starts
/// over at the top (L L, H H), though one step back (L H, H) does not fit.
#[test]
fn more_room_finds_the_earliest_page_that_shows_the_rest() {
    let size = window();
    let extra = [0, 0, 4, 4];
    let (mut dashboard, mut text) = running_at(&extra, size);
    let card = |d: &Dashboard, n: usize| rect(&d.tree, parts(d, Panel::Running).items[n].0);
    let (low, high) = (
        card(&dashboard, 0)[3],
        card(&running_at(&[4], size).0, 0)[3],
    );
    let room = rect(&dashboard.tree, parts(&dashboard, Panel::Running).list)[3];
    let pos = over_list(&dashboard, Panel::Running);
    wheel_at(&mut dashboard, &mut text, size, pos, -WHEEL_STEP);
    wheel_at(&mut dashboard, &mut text, size, pos, -WHEEL_STEP);
    assert_eq!(first(&dashboard, Panel::Running), 2);
    assert_eq!(shown_panes(&dashboard, Panel::Running), [3, 4]);

    // A row gap is 8 px: room for L+H, halfway to H+H.
    let wanted = low + 8.0 + high + (high - low) / 2.0;
    let taller = Size::new(size.width, size.height + (wanted - room).ceil());
    dashboard.prepare(taller, &mut text);
    let room = rect(&dashboard.tree, parts(&dashboard, Panel::Running).list)[3];
    assert!(
        low + 8.0 + high <= room && room < high + 8.0 + high,
        "{room}"
    );
    assert_eq!(first(&dashboard, Panel::Running), 0);
    assert_eq!(shown_panes(&dashboard, Panel::Running), [1, 2, 3, 4]);
    assert_eq!(indicator(&dashboard, Panel::Running), None);
    assert_whole_items(&mut dashboard, &mut text, "a taller window");
}

/// D5: a wheel over a list that turns no page — back at the first page,
/// sideways, less than a step, forward at the last page — has nothing to
/// present.
#[test]
fn a_wheel_that_turns_no_page_presents_nothing() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    assert!(!dashboard.prepare(window(), &mut text));
    let pos = over_list(&dashboard, Panel::Running);
    let steps = [
        (Point::new(0.0, WHEEL_STEP), 0),
        (Point::new(-WHEEL_STEP, 0.0), 0),
        (Point::new(0.0, -10.0), 0),
        // The 10 px forward adds up with these 30 px to one step.
        (Point::new(0.0, -30.0), 1),
        (Point::new(0.0, -WHEEL_STEP), 2),
        // The last page: forward turns nothing.
        (Point::new(0.0, -WHEEL_STEP), 2),
    ];
    for (delta, paged) in steps {
        let before = first(&dashboard, Panel::Running);
        dashboard.dispatch(Event::Wheel { pos, delta }, &mut text);
        let turned = paged != before;
        assert_eq!(
            dashboard.tree.needs_update(window()),
            turned,
            "{delta:?} from {before}"
        );
        dashboard.prepare(window(), &mut text);
        assert_eq!(first(&dashboard, Panel::Running), paged, "{delta:?}");
        assert!(!dashboard.prepare(window(), &mut text), "{delta:?}");
    }
}

/// At 1100×700 the running list holds one card at a time. With
/// api-refactor (11) selected, J selects docs-sync (12) and a wheel step
/// forward over the running list follows: the wheel came last, so it turns
/// on from the page that shows docs-sync, to perf-probe (13) — whether a
/// frame was laid out between the two or both came before one.
#[test]
fn a_wheel_after_a_selection_turns_from_its_page() {
    let small = Size::new(1100.0, 700.0);
    for frame_between in [true, false] {
        let mut dashboard = Dashboard::new(Grouping::Orchestrator);
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(small, &mut text);
        select_by_j(&mut dashboard, &mut text, small, 11);
        assert_eq!(shown_panes(&dashboard, Panel::Running), [11]);
        let pos = over_list(&dashboard, Panel::Running);

        let (key, modifiers) = (Key::Char('j'), Modifiers::default());
        dashboard.dispatch(Event::KeyDown { key, modifiers }, &mut text);
        assert_eq!(dashboard.view.selected, Some(12));
        if frame_between {
            dashboard.prepare(small, &mut text);
            assert_eq!(shown_panes(&dashboard, Panel::Running), [12]);
        }
        wheel_at(&mut dashboard, &mut text, small, pos, -WHEEL_STEP);

        let what = format!("frame between: {frame_between}");
        assert_eq!(dashboard.view.selected, Some(12), "{what}");
        assert_eq!(first(&dashboard, Panel::Running), 2, "{what}");
        assert_eq!(shown_panes(&dashboard, Panel::Running), [13], "{what}");
        assert_eq!(
            indicator(&dashboard, Panel::Running).as_deref(),
            Some("上に 2 件"),
            "{what}"
        );
        assert_whole_items(&mut dashboard, &mut text, &what);
    }
}

/// Two columns of cards L L H H (247 and 307 px). At 1440×900 one step
/// shows L H, H, hiding the last high card (the rows of 307 px do not fit
/// twice in 512 px). Made 1440×980 (592 px), the page from the second card
/// still does not fit (L H, H: 622 px), but from the first one everything
/// does (L L, H H: 562 px): the page goes back to it and hides nothing.
#[test]
fn a_larger_window_shows_everything_when_an_earlier_page_fits() {
    let (small, large) = (Size::new(1440.0, 900.0), Size::new(1440.0, 980.0));
    let (mut dashboard, mut text) = running_at(&[0, 0, 4, 4], small);
    let pos = over_list(&dashboard, Panel::Running);
    wheel_at(&mut dashboard, &mut text, small, pos, -WHEEL_STEP);
    assert_eq!(first(&dashboard, Panel::Running), 1);
    assert_eq!(shown_panes(&dashboard, Panel::Running), [2, 3]);
    assert_eq!(
        indicator(&dashboard, Panel::Running).as_deref(),
        Some("上に 1 件 · 下に 1 件")
    );

    dashboard.prepare(large, &mut text);
    let list = rect(&dashboard.tree, parts(&dashboard, Panel::Running).list);
    assert_eq!(list[3], 592.0, "the running list's height");
    assert_eq!(first(&dashboard, Panel::Running), 0);
    assert_eq!(shown_panes(&dashboard, Panel::Running), [1, 2, 3, 4]);
    assert_eq!(indicator(&dashboard, Panel::Running), None);
    assert_whole_items(&mut dashboard, &mut text, "1440×900 → 1440×980");
    assert!(!dashboard.prepare(large, &mut text), "a frame after");
}
