use noa_gpui_core::widgets::{Kbd, Label};

use super::*;

/// dashboard-worker-list U3: the header holds no urgency counts, no 最長
/// tag and no panel chips: the list's switch counts what needs attention.
#[test]
fn the_header_holds_no_counts_longest_or_chips() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    for digit in ['2', '3', '4', '5'] {
        press(&mut dashboard, &mut text, digit, Modifiers::default());
        let tree = &dashboard.tree;
        let header = texts_under(tree, dashboard.header.bar);
        for gone in ["承認 ", "質問 ", "失敗 ", "静止 ", "最長", "作業中", "完了"] {
            assert!(
                !header.iter().any(|t| t.starts_with(gone)),
                "{digit}: {gone:?} in {header:?}"
            );
        }
        let mut under = vec![dashboard.header.bar];
        while let Some(id) = under.pop() {
            if let Some(kbd) = tree.get::<Kbd>(id) {
                assert_eq!(kbd.keys(), ["?".to_owned()].as_slice(), "{digit}");
            }
            under.extend(tree.children(id));
        }
    }
}

#[test]
fn the_status_label_is_cut_inside_its_slot() {
    let (default, _default_text) = laid_out(Grouping::Orchestrator);
    let (mut ended, mut ended_text) = laid_out(Grouping::Orchestrator);
    press(&mut ended, &mut ended_text, '3', Modifiers::default());
    let (mut opened, mut opened_text) = laid_out(Grouping::Orchestrator);
    let button = row_button(&opened, rows_of(&opened)[0]);
    let pos = center(&opened.tree, button);
    click(&mut opened, &mut opened_text, pos);
    let (mut narrow, mut narrow_text) = laid_out(Grouping::Orchestrator);
    let long: String = "とても長いワーカー名のペインへ移動します "
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
        ("終了", &ended, false),
        ("after 承認画面へ", &opened, false),
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
fn long_labels_leave_room_for_the_link_status() {
    let mut fleet = fleet::canned();
    for worker in fleet.workers.iter_mut() {
        worker.label = LONG_LABEL.into();
    }
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let status = dashboard.tree.get::<Label>(dashboard.status).unwrap();
    assert_eq!(
        status.shown_text(),
        status.text(),
        "the link status is squeezed"
    );
}

/// dashboard-worker-list AC2: with no worker at all the body says why,
/// and shows no panel keys (there are no panels).
#[test]
fn with_nothing_to_show_the_body_says_why() {
    let (channel, _snapshots, _notices, _commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    assert!(dashboard.registry.list.is_none(), "connecting");
    let tree = &dashboard.tree;
    let texts = texts_under(tree, dashboard.body);
    assert!(
        texts.contains(&"Noa に接続しています…".to_owned()),
        "{texts:?}"
    );
    let mut under = vec![dashboard.body];
    while let Some(id) = under.pop() {
        assert!(tree.get::<Kbd>(id).is_none(), "a key in the empty body");
        under.extend(tree.children(id));
    }
}
