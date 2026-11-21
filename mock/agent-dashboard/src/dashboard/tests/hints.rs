use crate::fleet::State;
use noa_gpui_core::widgets::{Kbd, Label, Tag};

use super::*;

#[test]
fn each_panel_title_shows_its_shortcut() {
    let some_hidden = shown_only(&[Panel::Running, Panel::Graph]);
    for shown in [[true; 5], some_hidden] {
        let (dashboard, _text) = showing(Grouping::Orchestrator, shown);
        let tree = &dashboard.tree;
        for (digit, panel, title) in KEYS {
            let Some(id) = dashboard.panels[panel as usize] else {
                assert!(!shown[panel as usize], "{panel:?} is missing");
                continue;
            };
            let row = tree.children(id)[0];
            let parts = tree.children(row);
            let kbd = tree.get::<Kbd>(parts[0]).unwrap();
            assert_eq!(kbd.keys(), [digit.to_string()].as_slice(), "{panel:?}");
            let label = tree.get::<Label>(parts[1]).unwrap();
            assert_eq!(label.text(), panel.title());
            assert_eq!(label.text(), title);
        }
    }
}

fn chips_of(dashboard: &Dashboard) -> Vec<(Vec<String>, String)> {
    let tree = &dashboard.tree;
    dashboard
        .header
        .chips
        .iter()
        .map(|chip| {
            assert_eq!(tree.parent(*chip), Some(dashboard.header.bar));
            let parts = tree.children(*chip);
            let keys = tree.get::<Kbd>(parts[0]).unwrap().keys().to_vec();
            let title = tree.get::<Label>(parts[1]).unwrap().text().to_owned();
            (keys, title)
        })
        .collect()
}

#[test]
fn hidden_panels_show_as_chips_in_the_header() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    assert!(chips_of(&dashboard).is_empty());
    press(&mut dashboard, &mut text, '4', Modifiers::default());
    press(&mut dashboard, &mut text, '2', Modifiers::default());
    assert_eq!(
        chips_of(&dashboard),
        [
            (vec!["2".to_owned()], "作業中".to_owned()),
            (vec!["4".to_owned()], "状態の推移".to_owned()),
        ]
    );
    let tree = &dashboard.tree;
    let bar = rect(tree, dashboard.header.bar);
    for chip in &dashboard.header.chips {
        assert!(inside(rect(tree, *chip), bar), "a chip leaves the header");
    }
    press(&mut dashboard, &mut text, '2', Modifiers::default());
    press(&mut dashboard, &mut text, '4', Modifiers::default());
    assert!(chips_of(&dashboard).is_empty());
}

#[test]
fn the_status_label_is_cut_inside_its_slot() {
    let (default, _default_text) = laid_out(Grouping::Orchestrator);
    let (off, _off_text) = showing(Grouping::Orchestrator, [false; 5]);
    let (mut opened, mut opened_text) = laid_out(Grouping::Orchestrator);
    let (button, _) = opened.queue_buttons.open[0];
    let pos = center(&opened.tree, button);
    click(&mut opened, &mut opened_text, pos);
    let queue_only = shown_only(&[Panel::Running, Panel::Done, Panel::Timeline, Panel::Graph]);
    let (mut narrow, mut narrow_text) = showing(Grouping::Orchestrator, queue_only);
    let long: String = "モック: とても長いワーカー名のペインへ移動します "
        .chars()
        .cycle()
        .take(300)
        .collect();
    let status = narrow.status;
    narrow
        .tree
        .update::<Label, _>(status, |l, cx| l.set_text(cx, long));
    narrow.prepare(window(), &mut narrow_text);
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
