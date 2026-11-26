//! The list pages instead of scrolling (AC3 of agent-dashboard-improve):
//! whole rows only, the wheel a row at a time, a selection the user makes
//! paged into view, and a list built again starting at the row its page
//! started at (dashboard-worker-list AC5).

use noa_gpui_core::Key;

use super::super::pager::WHEEL_STEP;

use super::*;

const SIZES: [Size; 3] = [
    Size::new(1100.0, 700.0),
    Size::new(1440.0, 900.0),
    Size::new(1920.0, 1200.0),
];

const SMALL: Size = Size::new(1100.0, 700.0);

/// The canned fleet with `extra` more running workers (panes 100 on).
fn busy_fleet(extra: u64) -> Fleet {
    let mut fleet = fleet::canned();
    for n in 0..extra {
        let mut worker = fleet.workers[0].clone();
        worker.pane = 100 + n;
        worker.label = format!("extra-{n}");
        fleet.workers.push(worker);
    }
    fleet
}

/// 現在 with 19 rows, more than any window here holds, laid out at `size`.
fn busy(size: Size) -> (Dashboard, TextStore) {
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(busy_fleet(12), 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(size, &mut text);
    (dashboard, text)
}

fn parts(dashboard: &Dashboard) -> &ListParts {
    dashboard.registry.list.as_ref().unwrap()
}

/// The indicator's text, `None` while it is hidden.
fn indicator(dashboard: &Dashboard) -> Option<String> {
    let id = parts(dashboard).indicator;
    let label = dashboard.tree.get::<Label>(id).unwrap();
    (!dashboard.tree.is_hidden(id)).then(|| label.text().to_owned())
}

/// Whether the row of `pane` is shown.
fn shows(dashboard: &Dashboard, pane: u64) -> bool {
    !dashboard.tree.is_hidden(row_of(dashboard, pane))
}

/// A wheel event over the list, then the next frame's layout at `size`.
fn wheel_at(dashboard: &mut Dashboard, text: &mut TextStore, size: Size, dy: f32) {
    let pos = center(&dashboard.tree, parts(dashboard).list);
    let delta = Point::new(0.0, dy);
    dashboard.dispatch(Event::Wheel { pos, delta }, text);
    dashboard.prepare(size, text);
}

/// `n` wheel steps toward the end (negative: toward the start).
fn steps(dashboard: &mut Dashboard, text: &mut TextStore, size: Size, n: i32) {
    for _ in 0..n.unsigned_abs() {
        wheel_at(dashboard, text, size, -WHEEL_STEP * n.signum() as f32);
    }
}

/// Presses J, laying out at `size` after each, until `pane` is selected.
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

/// The shown rows are one run of whole rows from `view.first`, each inside
/// the list; returns how many.
fn assert_whole_rows(dashboard: &Dashboard, what: &str) -> usize {
    let tree = &dashboard.tree;
    let parts = parts(dashboard);
    let list = rect(tree, parts.list);
    let shown = visible_rows(dashboard);
    let first = dashboard.view.first;
    let run: Vec<WidgetId> = parts.items[first..first + shown.len()]
        .iter()
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(shown, run, "{what}: the shown rows are not one run");
    for id in &shown {
        let row = rect(tree, *id);
        assert!(inside(row, list), "{what}: row {row:?} outside {list:?}");
    }
    shown.len()
}

#[test]
fn every_page_shows_whole_rows() {
    for size in SIZES {
        let (mut dashboard, mut text) = busy(size);
        let len = rows_of(&dashboard).len();
        let mut what = format!("{size:?} at the top");
        let page = assert_whole_rows(&dashboard, &what);
        assert!(page < len, "{what}: all {len} rows fit");
        // To the last page, a step at a time.
        for step in 1..len {
            steps(&mut dashboard, &mut text, size, 1);
            what = format!("{size:?} after {step} steps");
            assert_whole_rows(&dashboard, &what);
        }
        let last = assert_whole_rows(&dashboard, &what);
        assert_eq!(
            dashboard.view.first + last,
            len,
            "{what}: the last row shows"
        );
    }
}

#[test]
fn the_list_says_what_it_hides() {
    let (mut dashboard, mut text) = busy(SMALL);
    let len = rows_of(&dashboard).len();
    let page = visible_rows(&dashboard).len();
    assert_eq!(
        indicator(&dashboard),
        Some(format!("下に {} 件", len - page))
    );
    steps(&mut dashboard, &mut text, SMALL, 1);
    let page = visible_rows(&dashboard).len();
    assert_eq!(
        indicator(&dashboard),
        Some(format!("上に 1 件 · 下に {} 件", len - 1 - page))
    );
    // A list that holds everything says nothing.
    let (dashboard, _text) = laid_out(Grouping::Orchestrator);
    assert_eq!(indicator(&dashboard), None);
}

#[test]
fn the_wheel_pages_one_row() {
    let (mut dashboard, mut text) = busy(SMALL);
    wheel_at(&mut dashboard, &mut text, SMALL, -WHEEL_STEP / 2.0);
    assert_eq!(dashboard.view.first, 0, "less than a step turns nothing");
    wheel_at(&mut dashboard, &mut text, SMALL, -WHEEL_STEP / 2.0);
    assert_eq!(dashboard.view.first, 1, "two halves make a step");
    steps(&mut dashboard, &mut text, SMALL, -1);
    assert_eq!(dashboard.view.first, 0);
    steps(&mut dashboard, &mut text, SMALL, -1);
    assert_eq!(dashboard.view.first, 0, "nothing above the first row");
}

#[test]
fn one_long_wheel_event_pages_as_far_as_short_ones() {
    let (mut short, mut short_text) = busy(SMALL);
    steps(&mut short, &mut short_text, SMALL, 3);
    let (mut long, mut long_text) = busy(SMALL);
    wheel_at(&mut long, &mut long_text, SMALL, -3.0 * WHEEL_STEP);
    assert_eq!(long.view.first, 3);
    assert_eq!(long.view.first, short.view.first);
}

#[test]
fn a_long_wheel_event_stops_at_the_last_page() {
    let (mut dashboard, mut text) = busy(SMALL);
    let len = rows_of(&dashboard).len();
    wheel_at(&mut dashboard, &mut text, SMALL, -100.0 * WHEEL_STEP);
    let page = assert_whole_rows(&dashboard, "after a long wheel");
    assert_eq!(dashboard.view.first + page, len, "the last page");
    let hidden = indicator(&dashboard).unwrap();
    assert!(!hidden.contains('下'), "{hidden}");
}

#[test]
fn travel_past_the_end_does_not_eat_the_way_back() {
    let (mut dashboard, mut text) = busy(SMALL);
    wheel_at(&mut dashboard, &mut text, SMALL, -100.0 * WHEEL_STEP);
    let last = dashboard.view.first;
    // Further on: nothing is left below.
    steps(&mut dashboard, &mut text, SMALL, 5);
    assert_eq!(dashboard.view.first, last);
    // One step back is one row back, at once.
    steps(&mut dashboard, &mut text, SMALL, -1);
    assert_eq!(dashboard.view.first, last - 1);
}

#[test]
fn selecting_brings_the_row_into_view() {
    let (mut dashboard, mut text) = busy(SMALL);
    let rows = rows_of(&dashboard);
    let last = *rows.last().unwrap();
    assert!(!shows(&dashboard, last));
    select_by_j(&mut dashboard, &mut text, SMALL, last);
    assert!(shows(&dashboard, last), "J pages the selection into view");
    assert_whole_rows(&dashboard, "at the selection");
    // K back to the top pages back.
    for _ in 0..rows.len() {
        key(&mut dashboard, &mut text, Key::Char('k'));
        dashboard.prepare(SMALL, &mut text);
    }
    assert_eq!(dashboard.view.selected, Some(rows[0]));
    assert_eq!(dashboard.view.first, 0);
}

#[test]
fn the_latest_of_a_selection_and_a_wheel_decides_the_page() {
    let (mut dashboard, mut text) = busy(SMALL);
    let rows = rows_of(&dashboard);
    let last = *rows.last().unwrap();
    select_by_j(&mut dashboard, &mut text, SMALL, last);
    let first = dashboard.view.first;
    // The wheel after the selection: its page holds, the selection off it.
    steps(&mut dashboard, &mut text, SMALL, -3);
    assert_eq!(dashboard.view.first, first - 3);
    assert!(!shows(&dashboard, last));
    // A new size keeps the page the wheel turned.
    dashboard.prepare(Size::new(1120.0, 700.0), &mut text);
    assert_eq!(dashboard.view.first, first - 3);
    // The selection again after the wheel: it comes back into view.
    key(&mut dashboard, &mut text, Key::Char('k'));
    dashboard.prepare(SMALL, &mut text);
    assert!(shows(&dashboard, dashboard.view.selected.unwrap()));
}

/// Paged away from the selected row, the page stays where the wheel put it
/// when a live update changes only what time changes (the rows' times, in
/// place): the selection the user paged away from does not pull it back.
#[test]
fn the_wheel_does_not_fight_the_selection() {
    let (mut dashboard, mut text) = busy(SMALL);
    let rows = rows_of(&dashboard);
    let (selected, below) = (rows[1], rows[4]);
    select_by_j(&mut dashboard, &mut text, SMALL, selected);
    steps(&mut dashboard, &mut text, SMALL, 3);
    assert_eq!(dashboard.view.first, 3);
    assert!(!shows(&dashboard, selected));
    let row = row_of(&dashboard, below);
    let time = pulse_id(&dashboard, |p| matches!(p, Pulse::RowTime(p) if p == below));
    let time_text = |d: &Dashboard| d.tree.get::<Label>(time).unwrap().text().to_owned();
    let before = time_text(&dashboard);

    let mut later = busy_fleet(12);
    for worker in &mut later.workers {
        worker.since = format!("later {}", worker.pane);
    }
    assert!(busy_fleet(12).same_structure(&later));
    dashboard.apply(live(later, 0), &mut text);
    dashboard.prepare(SMALL, &mut text);
    assert_eq!(row_of(&dashboard, below), row, "updated in place");
    assert_ne!(time_text(&dashboard), before, "the times changed");
    assert_eq!(dashboard.view.selected, Some(selected));
    assert_eq!(dashboard.view.first, 3, "the page went back");
    assert!(!shows(&dashboard, selected));
}

/// The selected row on the page stays on it when the window shrinks so
/// that it would fall below the page: no wheel moved the page away from
/// it, so the page follows it, whole rows only.
#[test]
fn a_shown_selection_stays_shown_when_the_window_shrinks() {
    let small_page = visible_rows(&busy(SMALL).0).len();
    let big = Size::new(1440.0, 900.0);
    let (mut dashboard, mut text) = busy(big);
    let shown = visible_rows(&dashboard).len();
    let last = rows_of(&dashboard)[shown - 1];
    assert!(shown > small_page, "at {SMALL:?} {last} is below the page");
    select_by_j(&mut dashboard, &mut text, big, last);
    assert_eq!(dashboard.view.first, 0);

    dashboard.prepare(SMALL, &mut text);
    assert_eq!(dashboard.view.selected, Some(last));
    assert!(shows(&dashboard, last), "the page follows the selection");
    assert!(dashboard.view.first > 0);
    assert_whole_rows(&dashboard, "after the shrink");
    assert!(!dashboard.prepare(SMALL, &mut text), "a frame after");
}

#[test]
fn a_wheel_after_a_selection_turns_from_its_page() {
    let (mut paged, mut paged_text) = busy(SMALL);
    let last = *rows_of(&paged).last().unwrap();
    select_by_j(&mut paged, &mut paged_text, SMALL, last);
    let first = paged.view.first;

    // The same, but the last J and a wheel step back come before the
    // frame: the step is taken from the page that shows the selection.
    let (mut dashboard, mut text) = busy(SMALL);
    let before_last = rows_of(&dashboard)[rows_of(&dashboard).len() - 2];
    select_by_j(&mut dashboard, &mut text, SMALL, before_last);
    let (key, modifiers) = (Key::Char('j'), Modifiers::default());
    dashboard.dispatch(Event::KeyDown { key, modifiers }, &mut text);
    let pos = center(&dashboard.tree, parts(&dashboard).list);
    let delta = Point::new(0.0, WHEEL_STEP);
    dashboard.dispatch(Event::Wheel { pos, delta }, &mut text);
    dashboard.prepare(SMALL, &mut text);
    assert_eq!(dashboard.view.selected, Some(last));
    assert_eq!(dashboard.view.first, first - 1);
}

#[test]
fn a_larger_window_pages_back_what_fits() {
    let (mut dashboard, mut text) = busy(SMALL);
    let len = rows_of(&dashboard).len();
    wheel_at(&mut dashboard, &mut text, SMALL, -100.0 * WHEEL_STEP);
    let at_small = dashboard.view.first;
    let big = Size::new(1920.0, 1200.0);
    dashboard.prepare(big, &mut text);
    let page = assert_whole_rows(&dashboard, "at 1920×1200");
    assert!(dashboard.view.first < at_small, "rows that fit come back");
    assert_eq!(dashboard.view.first + page, len, "still to the end");
    if dashboard.view.first > 0 {
        // From one row earlier the rest would not fit.
        let hidden = indicator(&dashboard).unwrap();
        assert!(
            hidden.starts_with('上') && !hidden.contains('下'),
            "{hidden}"
        );
    }
}

#[test]
fn an_unchanged_frame_presents_nothing() {
    let (mut dashboard, mut text) = busy(SMALL);
    dashboard.prepare(SMALL, &mut text);
    assert!(!dashboard.prepare(SMALL, &mut text), "nothing changed");
    assert!(!dashboard.needs_update(SMALL));
}

#[test]
fn a_wheel_that_turns_no_page_presents_nothing() {
    let (mut dashboard, mut text) = busy(SMALL);
    dashboard.prepare(SMALL, &mut text);
    // Back at the top: nothing above.
    let pos = center(&dashboard.tree, parts(&dashboard).list);
    let delta = Point::new(0.0, WHEEL_STEP);
    dashboard.dispatch(Event::Wheel { pos, delta }, &mut text);
    assert!(!dashboard.needs_update(SMALL));
    assert!(!dashboard.prepare(SMALL, &mut text));
    assert_eq!(dashboard.view.first, 0);
}

#[test]
fn a_filter_change_starts_from_the_top_with_the_selection_in_view() {
    let (mut dashboard, mut text) = busy(SMALL);
    // The last of the extra workers, below the first page.
    let rows = rows_of(&dashboard);
    let last = *rows.iter().rev().find(|pane| **pane >= 100).unwrap();
    select_by_j(&mut dashboard, &mut text, SMALL, last);
    assert!(dashboard.view.first > 0);
    key(&mut dashboard, &mut text, Key::Char('/'));
    typed(&mut dashboard, &mut text, "/");
    typed(&mut dashboard, &mut text, "extra");
    dashboard.prepare(SMALL, &mut text);
    assert_eq!(dashboard.view.selected, Some(last), "it matches");
    assert!(shows(&dashboard, last), "the selection in view");
    // Cleared: from the top, the selection in view again.
    key(&mut dashboard, &mut text, Key::Escape);
    dashboard.prepare(SMALL, &mut text);
    assert_eq!(dashboard.filter.query, "");
    assert!(shows(&dashboard, last));
}

#[test]
fn a_scope_switch_starts_from_the_top() {
    let (mut dashboard, mut text) = busy(SMALL);
    steps(&mut dashboard, &mut text, SMALL, 4);
    assert_eq!(dashboard.view.first, 4);
    key(&mut dashboard, &mut text, Key::Char('3'));
    key(&mut dashboard, &mut text, Key::Char('1'));
    dashboard.prepare(SMALL, &mut text);
    assert_eq!(dashboard.view.first, 0);
    assert_eq!(
        indicator(&dashboard)
            .as_deref()
            .map(|t| t.starts_with('下')),
        Some(true)
    );
}

#[test]
fn another_server_starts_the_list_at_the_top() {
    let (mut dashboard, mut text) = busy(SMALL);
    steps(&mut dashboard, &mut text, SMALL, 4);
    let restarted = Snapshot {
        instance: Some("restarted".into()),
        ..live(busy_fleet(12), 0)
    };
    dashboard.apply(restarted, &mut text);
    dashboard.prepare(SMALL, &mut text);
    assert_eq!(dashboard.view.first, 0);
    assert_whole_rows(&dashboard, "on the new server");
}

/// dashboard-worker-list AC5: a live rebuild starts the list at the row
/// its page started at, so a row that comes above moves nothing the user
/// reads; a row that left takes the page to its place.
#[test]
fn a_live_rebuild_keeps_the_first_row() {
    let (mut dashboard, mut text) = busy(SMALL);
    steps(&mut dashboard, &mut text, SMALL, 5);
    let top = rows_of(&dashboard)[5];
    assert_eq!(dashboard.view.anchor, Some(top));
    let mut fleet = busy_fleet(12);
    let mut approval = fleet.workers[0].clone();
    approval.pane = 300;
    approval.label = "new-approval".into();
    approval.state = fleet::State::Permission;
    fleet.workers.push(approval);
    dashboard.apply(live(fleet.clone(), 0), &mut text);
    dashboard.prepare(SMALL, &mut text);
    assert_eq!(rows_of(&dashboard)[0], 14, "approvals by wait");
    assert!(rows_of(&dashboard).contains(&300));
    assert_eq!(
        rows_of(&dashboard)[dashboard.view.first],
        top,
        "the page stays"
    );
    assert_whole_rows(&dashboard, "after the rebuild");

    // The first row's worker gone: the page starts at its place.
    let at = dashboard.view.first;
    fleet.workers.retain(|w| w.pane != top);
    dashboard.apply(live(fleet, 0), &mut text);
    dashboard.prepare(SMALL, &mut text);
    assert_eq!(dashboard.view.first, at);
}
