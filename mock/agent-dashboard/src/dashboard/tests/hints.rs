use crate::fleet::State;
use noa_gpui_core::widgets::{Kbd, Label, Tag};

use super::*;

fn hint_strip_of(dashboard: &Dashboard) -> WidgetId {
    let tree = &dashboard.tree;
    let slot = tree.parent(dashboard.status).unwrap();
    let bar = tree.parent(slot).unwrap();
    let at = tree.children(bar).iter().position(|c| *c == slot).unwrap();
    tree.children(bar)[at - 1]
}

#[test]
fn the_header_shows_each_shortcut() {
    for shown in [[true; 5], [false; 5]] {
        let (dashboard, _text) = showing(Grouping::Orchestrator, shown);
        let tree = &dashboard.tree;
        let items = tree.children(hint_strip_of(&dashboard));
        assert_eq!(items.len(), 5);
        for ((digit, _, title), item) in KEYS.into_iter().zip(items) {
            let parts = tree.children(*item);
            let kbd = tree.get::<Kbd>(parts[0]).unwrap();
            assert_eq!(kbd.keys(), [digit.to_string()].as_slice());
            let label = tree.get::<Label>(parts[1]).unwrap();
            assert_eq!(label.text(), title);
        }
    }
}

/// Width of one hint item, worked out from its parts and not read back from
/// the layout: a 20 px keycap, the 3 px item gap, the title (full-width
/// characters are one em, 11 px, wide) and 2 px padding on each side.
fn expected_item_width(panel: Panel) -> f32 {
    20.0 + 3.0 + 11.0 * panel.title().chars().count() as f32 + 4.0
}

/// The strip is the five items and four 8 px gaps: 332 px.
fn expected_strip_width() -> f32 {
    Panel::ALL.map(expected_item_width).iter().sum::<f32>() + 4.0 * 8.0
}

fn assert_strip_holds_its_items(dashboard: &Dashboard, what: &str) {
    let tree = &dashboard.tree;
    let strip = rect(tree, hint_strip_of(dashboard));
    let items = tree.children(hint_strip_of(dashboard));
    assert_eq!(items.len(), 5, "{what}");
    for (panel, item) in Panel::ALL.into_iter().zip(items) {
        let item = rect(tree, *item);
        assert!(
            (item[2] - expected_item_width(panel)).abs() <= 0.5,
            "{what}: {panel:?} item is {} px, expected {}",
            item[2],
            expected_item_width(panel)
        );
        assert!(
            item[0] >= strip[0] - 0.5 && item[0] + item[2] <= strip[0] + strip[2] + 0.5,
            "{what}: {panel:?} item {item:?} is not inside the strip {strip:?}"
        );
    }
    assert!(
        (strip[2] - expected_strip_width()).abs() <= 0.5,
        "{what}: strip is {} px, expected {}",
        strip[2],
        expected_strip_width()
    );
}

#[test]
fn the_hint_strip_fits_the_header() {
    let open = |dashboard: &mut Dashboard, text: &mut TextStore| {
        let (button, _) = dashboard.queue_buttons.open[0];
        let pos = center(&dashboard.tree, button);
        click(dashboard, text, pos);
    };
    let (default, _default_text) = laid_out(Grouping::Orchestrator);
    let (mut opened, mut opened_text) = laid_out(Grouping::Orchestrator);
    open(&mut opened, &mut opened_text);
    let queue_only = shown_only(&[Panel::Running, Panel::Done, Panel::Timeline, Panel::Graph]);
    let (mut narrow, mut narrow_text) = showing(Grouping::Orchestrator, queue_only);
    let long = "モック: とても長いワーカー名のペインへ移動します ".repeat(6);
    let status = narrow.status;
    narrow
        .tree
        .update::<Label, _>(status, |l, cx| l.set_text(cx, long));
    narrow.prepare(window(), &mut narrow_text);
    let (off, _off_text) = showing(Grouping::Orchestrator, [false; 5]);
    let default_widths: Vec<f32> = {
        let tree = &default.tree;
        let bar = tree.parent(hint_strip_of(&default)).unwrap();
        tree.children(bar)[..6]
            .iter()
            .map(|c| rect(tree, *c)[2])
            .collect()
    };
    for dashboard in [&default, &off, &opened, &narrow] {
        let tree = &dashboard.tree;
        let strip = hint_strip_of(dashboard);
        let bar = tree.parent(strip).unwrap();
        let rects: Vec<[f32; 4]> = tree.children(bar).iter().map(|c| rect(tree, *c)).collect();
        for pair in rects.windows(2) {
            assert!(pair[0][0] + pair[0][2] <= pair[1][0] + 0.5, "{rects:?}");
        }
        for (rect, expected) in rects.iter().zip(&default_widths) {
            assert!((rect[2] - expected).abs() <= 0.5, "{rects:?}");
        }
        let last = rects.last().unwrap();
        assert!(
            last[0] + last[2] <= window().width - 16.0 + 0.5,
            "{rects:?}"
        );
        assert_strip_holds_its_items(dashboard, "header state");
    }
    // The status label lives inside its own container; it must stay inside
    // it, and where its text is longer than the container it is cut short.
    for (name, dashboard, cut) in [
        ("default", &default, false),
        ("all off", &off, false),
        ("after 開く", &opened, true),
        ("long status", &narrow, true),
    ] {
        let tree = &dashboard.tree;
        let slot = rect(tree, tree.parent(dashboard.status).unwrap());
        let label = rect(tree, dashboard.status);
        assert!(
            label[0] >= slot[0] - 0.5 && label[0] + label[2] <= slot[0] + slot[2] + 0.5,
            "{name}: status label {label:?} is not inside its container {slot:?}"
        );
        let status = tree.get::<Label>(dashboard.status).unwrap();
        assert_eq!(
            status.shown_text() != status.text(),
            cut,
            "{name}: shows {:?} of {:?}",
            status.shown_text(),
            status.text()
        );
    }
    let (default_tree, default_strip) = (&default.tree, hint_strip_of(&default));
    let default_rect = rect(default_tree, default_strip);
    let mut states: Vec<(String, [bool; 5])> = Panel::ALL
        .map(|p| (format!("{p:?} off"), shown_only(&[p])))
        .into();
    states.push(("all off".into(), [false; 5]));
    for (name, shown) in states {
        let (dashboard, _text) = showing(Grouping::Orchestrator, shown);
        let actual = rect(&dashboard.tree, hint_strip_of(&dashboard));
        assert_rect(actual, default_rect, &format!("hint strip, {name}"));
    }
}

#[test]
fn the_longest_wait_tag_needs_a_wait_text() {
    let fleet = crate::live::tests::reported_fleet();
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let tags = tags_of(&dashboard);
    assert!(!tags.iter().any(|t| t.starts_with("最長")), "{tags:?}");

    let mut fleet = fleet::canned();
    let mut notice = fleet.workers[0].clone();
    notice.pane = 99;
    notice.state = State::Finished;
    notice.attention = true;
    notice.since = String::new();
    notice.wait_minutes = 600.0;
    fleet.workers.push(notice);
    let dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let longest: Vec<String> = tags_of(&dashboard)
        .into_iter()
        .filter(|t| t.starts_with("最長"))
        .collect();
    assert_eq!(longest, ["最長 spec-review 6 分"]);
}

#[test]
fn a_long_longest_label_leaves_room_for_the_link_status() {
    let mut fleet = fleet::canned();
    for worker in fleet.workers.iter_mut() {
        worker.label = LONG_LABEL.into();
    }
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let status = rect(&dashboard.tree, dashboard.status);
    assert!(
        status[2] >= 100.0,
        "the link status is squeezed: {status:?}"
    );
    let longest = pulse_id(&dashboard, |p| matches!(p, Pulse::Longest));
    let longest = dashboard.tree.get::<Tag>(longest).unwrap().text();
    assert!(longest.contains('…'), "{longest}");
}
