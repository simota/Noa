use crate::fleet::{State, Worker};
use noa_gpui_core::widgets::Label;

use super::*;

#[test]
fn the_most_urgent_card_starts_expanded() {
    let (dashboard, _text) = laid_out(Grouping::Orchestrator);
    let open: Vec<&str> = dashboard
        .queue_buttons
        .open
        .iter()
        .map(|(_, pane)| label_of(&dashboard, *pane))
        .collect();
    assert_eq!(open, ["fix-tests"]);
}

#[test]
fn details_expands_that_card_instead() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let (button, label) = dashboard.queue_buttons.details[1];
    let pos = center(&dashboard.tree, button);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(dashboard.view.expanded, Some(label));
    let open: Vec<u64> = dashboard
        .queue_buttons
        .open
        .iter()
        .map(|(_, pane)| *pane)
        .collect();
    assert_eq!(open, [label]);
}

#[test]
fn open_names_the_pane_it_would_go_to() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let (button, label) = dashboard.queue_buttons.open[0];
    let pos = center(&dashboard.tree, button);
    click(&mut dashboard, &mut text, pos);
    let notice = notice_of(&dashboard);
    let label = label_of(&dashboard, label);
    assert!(notice.contains(label), "{notice}");
}

#[test]
fn a_plain_command_has_no_model_tag() {
    let reported = crate::live::tests::reported_fleet();
    let sleeper = reported.worker(33).unwrap().clone();
    let mut fleet = Fleet {
        orchestrators: reported.orchestrators.clone(),
        workers: Vec::new(),
    };
    for (pane, state) in [
        (33, State::Running),
        (35, State::Finished),
        (36, State::Error),
    ] {
        fleet.workers.push(Worker {
            pane,
            state,
            ..sleeper.clone()
        });
    }
    let mut dashboard = Dashboard::of_snapshot(Grouping::Flat, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    assert_eq!(
        dashboard.queue_buttons.open.len(),
        1,
        "the queue card is expanded"
    );
    let tags = tags_of(&dashboard);
    // Kind tags: the running, done and queue cards.
    assert_eq!(tags.iter().filter(|t| *t == "sleep").count(), 3, "{tags:?}");
    assert!(!tags.iter().any(|t| t.contains("モデル")), "{tags:?}");
}

#[test]
fn the_queue_card_keeps_its_buttons_inside() {
    let long_line = "x".repeat(200);
    let mut fleet = fleet::canned();
    for worker in &mut fleet.workers {
        worker.orchestrator = REPORTED_ORCHESTRATOR.into();
        worker.branch = "factory/agent-dashboard".into();
    }
    let expanded = fleet.workers.iter_mut().find(|w| w.pane == 14).unwrap();
    expanded.screen_tail.push(long_line.clone());
    let collapsed = fleet.workers.iter_mut().find(|w| w.pane == 19).unwrap();
    collapsed.detail = "とても長い詳細 ".repeat(30);
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let tree = &dashboard.tree;

    let (open, pane) = dashboard.queue_buttons.open[0];
    assert_eq!(pane, 14);
    let actions = tree.parent(open).unwrap();
    let card = tree.parent(actions).unwrap();
    assert!(
        inside(rect(tree, open), rect(tree, card)),
        "開く leaves its card"
    );
    let slot = tree.children(actions)[2];
    let name = tree.get::<Label>(tree.children(slot)[0]).unwrap();
    assert_eq!(name.text(), REPORTED_ORCHESTRATOR);
    assert_ne!(
        name.shown_text(),
        name.text(),
        "the orchestrator is not cut"
    );
    let at = tree
        .children(card)
        .iter()
        .position(|c| *c == actions)
        .unwrap();
    let screen = tree.children(card)[at - 1];
    let lines = tree.children(screen);
    let last = tree.get::<Label>(*lines.last().unwrap()).unwrap();
    assert_eq!(last.text(), long_line);
    assert_ne!(last.shown_text(), last.text(), "the long line is not cut");
    for line in lines {
        assert!(
            inside(rect(tree, *line), rect(tree, screen)),
            "a screen line leaves its box"
        );
    }
    assert!(inside(rect(tree, screen), rect(tree, card)));

    for (details, _) in &dashboard.queue_buttons.details {
        let card = tree.parent(tree.parent(*details).unwrap()).unwrap();
        assert!(
            inside(rect(tree, *details), rect(tree, card)),
            "詳細 leaves its card"
        );
    }
}

#[test]
fn a_long_label_leaves_the_wait_inside_its_queue_card() {
    let mut fleet = fleet::canned();
    for worker in fleet.workers.iter_mut() {
        worker.label = LONG_LABEL.into();
    }
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let tree = &dashboard.tree;
    let sinces: Vec<WidgetId> = dashboard
        .pulses
        .iter()
        .filter(|(_, p)| matches!(p, Pulse::QueueSince(_)))
        .map(|(id, _)| *id)
        .collect();
    assert!(!sinces.is_empty());
    for since in sinces {
        let head = tree.parent(since).unwrap();
        let card = tree.parent(head).unwrap();
        for part in tree.children(head) {
            assert!(
                inside(rect(tree, *part), rect(tree, card)),
                "{:?} leaves its card {:?}",
                rect(tree, *part),
                rect(tree, card)
            );
        }
        assert!(rect(tree, since)[2] > 40.0, "the wait is squeezed");
    }
}
