//! What an independent review of the foundation found: the help's modal
//! must hold against assistive technology and against a press held when it
//! opens, a restarted Noa must be watched afresh, and the filter's count
//! must be laid out with the query it counts.

use std::sync::mpsc::{Receiver, Sender};

use noa_gpui_core::accesskit::{Action as Access, ActionRequest, TreeId};
use noa_gpui_core::widgets::{Label, TextField};
use noa_gpui_core::{access_id, access_item_id};

use super::*;

fn status_of(dashboard: &Dashboard) -> String {
    let status = dashboard.tree.get::<Label>(dashboard.status).unwrap();
    status.text().to_owned()
}

/// A live screen on the canned fleet, with what it asked of the feed so
/// far drained.
fn live_canned() -> (Dashboard, TextStore, Sender<Snapshot>, Receiver<Command>) {
    let (channel, snapshots, _notices, commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    snapshots.send(live(fleet::canned(), 0)).unwrap();
    frame(&mut dashboard, &mut text);
    commands.try_iter().for_each(drop);
    (dashboard, text, snapshots, commands)
}

/// The next frame: what the feed sent, then the layout.
fn frame(dashboard: &mut Dashboard, text: &mut TextStore) {
    dashboard.before_frame(text);
    dashboard.prepare(window(), text);
}

fn last_watch(commands: &Receiver<Command>) -> Option<Vec<u64>> {
    commands
        .try_iter()
        .filter_map(|c| match c {
            Command::Watch(panes) => Some(panes),
            Command::Focus(_) => None,
        })
        .last()
}

fn focused_panes(commands: &Receiver<Command>) -> Vec<u64> {
    commands
        .try_iter()
        .filter_map(|c| match c {
            Command::Focus(pane) => Some(pane),
            Command::Watch(_) => None,
        })
        .collect()
}

/// `?` as the platform sends it: the key (shift held), then its text.
fn question_mark(dashboard: &mut Dashboard, text: &mut TextStore) {
    let modifiers = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let key = Key::Char('/');
    dashboard.dispatch(Event::KeyDown { key, modifiers }, text);
    typed(dashboard, text, "?");
}

/// A request from assistive technology, carried out as the platform does
/// (`noa_gpui_platform`'s `run_accessibility_actions`): the tree acts on
/// it without `Screen::dispatch`, the screen handles what it said.
fn assistive(dashboard: &mut Dashboard, text: &mut TextStore, node: u64, action: Access) {
    let request = ActionRequest {
        action,
        target_tree: TreeId::ROOT,
        target_node: noa_gpui_core::accesskit::NodeId(node),
        data: None,
    };
    dashboard.tree.accessibility_action(&request, text);
    for message in dashboard.tree.take_messages() {
        dashboard.handle(message, text);
    }
    dashboard.after_input();
    dashboard.prepare(window(), text);
}

fn node_of(id: WidgetId) -> u64 {
    access_id(id).0
}

/// Whether the accessibility tree offers `id` as disabled.
fn disabled_for_assistive(dashboard: &Dashboard, id: WidgetId) -> bool {
    let update = dashboard.tree.accessibility_update(1.0);
    let (_, node) = update
        .nodes
        .iter()
        .find(|(node, _)| *node == access_id(id))
        .expect("the widget is in the accessibility tree");
    node.is_disabled()
}

#[test]
fn assistive_technology_cannot_reach_under_the_help() {
    let (mut dashboard, mut text, _snapshots, commands) = live_canned();
    // The drawer on 12 (docs-sync) puts a second 開く on screen.
    while dashboard.view.selected != Some(12) {
        key(&mut dashboard, &mut text, Key::Char('j'));
    }
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.view.detail, Some(12));
    commands.try_iter().for_each(drop);
    let (queue_open, top) = dashboard.queue_buttons.open[0];
    let (drawer_open, _) = dashboard.registry.open_buttons[0];
    let grouping = dashboard.grouping_control;
    let before = status_of(&dashboard);

    question_mark(&mut dashboard, &mut text);
    assert!(dashboard.view.help);
    for id in [queue_open, drawer_open] {
        assert!(disabled_for_assistive(&dashboard, id), "{id:?} is offered");
        assistive(&mut dashboard, &mut text, node_of(id), Access::Click);
        assistive(&mut dashboard, &mut text, node_of(id), Access::Focus);
    }
    let last_segment = access_item_id(grouping, Grouping::ALL.len() - 1).0;
    assistive(&mut dashboard, &mut text, last_segment, Access::Click);

    assert!(dashboard.view.help, "the help stays open");
    assert!(focused_panes(&commands).is_empty(), "a pane was focused");
    assert_eq!(status_of(&dashboard), before);
    assert_eq!(dashboard.view.selected, Some(12));
    assert_eq!(dashboard.view.grouping, Grouping::Orchestrator);
    assert_eq!(dashboard.tree.focused(), None, "focus went under the help");

    // Closed, the help gives the screen back to assistive technology.
    key(&mut dashboard, &mut text, Key::Escape);
    assert!(!dashboard.view.help);
    assert!(!disabled_for_assistive(&dashboard, queue_open));
    assistive(
        &mut dashboard,
        &mut text,
        node_of(queue_open),
        Access::Click,
    );
    assert_eq!(focused_panes(&commands), [top]);
}

/// The order the buttons go down and up around the help in
/// `the_help_cancels_a_left_press_whatever_else_is_held`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Chord {
    /// Left, then right; both held while the help opens.
    LeftThenRight,
    /// Right, then left; both held while the help opens.
    RightThenLeft,
    /// Left, then a right click done before the help opens.
    RightClickedMeanwhile,
}

#[test]
fn the_help_cancels_a_left_press_whatever_else_is_held() {
    let (left, right) = (PointerButton::Primary, PointerButton::Secondary);
    let chords = [
        Chord::LeftThenRight,
        Chord::RightThenLeft,
        Chord::RightClickedMeanwhile,
    ];
    for chord in chords {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        let before = status_of(&dashboard);
        let (open, pane) = dashboard.queue_buttons.open[0];
        let pos = center(&dashboard.tree, open);
        dashboard.dispatch(Event::PointerMove { pos }, &mut text);
        let pressed = match chord {
            Chord::RightThenLeft => [right, left],
            _ => [left, right],
        };
        for button in pressed {
            dashboard.dispatch(Event::PointerDown { pos, button }, &mut text);
        }
        let up = |button| Event::PointerUp { pos, button };
        if chord == Chord::RightClickedMeanwhile {
            dashboard.dispatch(up(right), &mut text);
        }
        question_mark(&mut dashboard, &mut text);
        assert!(dashboard.view.help, "{chord:?}");
        key(&mut dashboard, &mut text, Key::Escape);
        assert!(!dashboard.view.help, "{chord:?}");
        dashboard.dispatch(up(left), &mut text);
        if chord != Chord::RightClickedMeanwhile {
            dashboard.dispatch(up(right), &mut text);
        }
        dashboard.prepare(window(), &mut text);
        assert_eq!(status_of(&dashboard), before, "開く fired: {chord:?}");
        assert_eq!(dashboard.view.selected, None, "{chord:?}");

        // Where the cancel reached the button, it no longer counts itself
        // pressed: a press that starts beside it and is let go over it
        // does nothing. (With the right button let go first, the tree had
        // already dropped the button's hold on the pointer, and nothing
        // reaches it: see the report.)
        if chord != Chord::RightClickedMeanwhile {
            let beside = Point::new(pos.x, pos.y + 400.0);
            dashboard.dispatch(Event::PointerMove { pos: beside }, &mut text);
            let button = left;
            dashboard.dispatch(
                Event::PointerDown {
                    pos: beside,
                    button,
                },
                &mut text,
            );
            dashboard.dispatch(Event::PointerMove { pos }, &mut text);
            dashboard.dispatch(up(left), &mut text);
            dashboard.prepare(window(), &mut text);
            assert_eq!(status_of(&dashboard), before, "a stale press: {chord:?}");
        }

        // A press of its own opens the pane as before.
        click(&mut dashboard, &mut text, pos);
        let label = label_of(&dashboard, pane).to_owned();
        let opened = format!("モック: {label} のペインへ移動します");
        assert_eq!(status_of(&dashboard), opened, "{chord:?}");
    }
}

#[test]
fn another_server_instance_is_watched_afresh() {
    let (mut dashboard, mut text, snapshots, commands) = live_canned();
    assert_eq!(dashboard.watched, Some(vec![14]), "the top card");

    // Noa restarted and gave every pane id out again; the feed dropped
    // what it watched on the old server. The new server's top card has
    // the same id, and it is asked for again.
    let restarted = |instance: &str| Snapshot {
        instance: Some(instance.to_owned()),
        ..live(fleet::canned(), 0)
    };
    snapshots.send(restarted("restarted")).unwrap();
    frame(&mut dashboard, &mut text);
    assert_eq!(last_watch(&commands), Some(vec![14]));

    // What is open on the new server is what is watched: the drawer's
    // worker until another restart closes the drawer.
    while dashboard.view.selected != Some(15) {
        key(&mut dashboard, &mut text, Key::Char('j'));
    }
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(last_watch(&commands), Some(vec![15]));
    snapshots.send(restarted("again")).unwrap();
    frame(&mut dashboard, &mut text);
    assert_eq!(dashboard.view.detail, None);
    assert_eq!(last_watch(&commands), Some(vec![14]));
}

#[test]
fn the_filter_field_is_where_its_count_puts_it_at_once() {
    let size_of = |tree: &UiTree, id| rect(tree, id);
    let start = |text: &mut TextStore| {
        let mut dashboard = Dashboard::new(Grouping::Orchestrator);
        dashboard.prepare(window(), text);
        key(&mut dashboard, text, Key::Char('/'));
        typed(&mut dashboard, text, "/");
        dashboard
    };
    // Where the field ends up once "codex" is in it and laid out.
    let mut text = TextStore::new(Default::default());
    let mut settled = start(&mut text);
    let empty_at = size_of(&settled.tree, settled.header.filter_field);
    typed(&mut settled, &mut text, "codex");
    let field = settled.header.filter_field;
    let settled_at = size_of(&settled.tree, field);
    assert!(
        settled_at[0] + 0.5 < empty_at[0],
        "the count moved the field left: {empty_at:?} → {settled_at:?}"
    );

    // The same input as the window delivers it: no frame between the
    // query and the click that follows it.
    let mut text = TextStore::new(Default::default());
    let mut dashboard = start(&mut text);
    dashboard.dispatch(Event::Text("codex".into()), &mut text);
    let field = dashboard.header.filter_field;
    assert_rect(size_of(&dashboard.tree, field), settled_at, "the field");
    let pos = Point::new(settled_at[0] + 1.0, settled_at[1] + settled_at[3] / 2.0);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(dashboard.tree.focused(), Some(field), "the click missed");
    // The click at the field's left edge put the caret before "codex".
    let end = Key::End;
    key(&mut dashboard, &mut text, end);
    press(&mut dashboard, &mut text, '1', Modifiers::default());
    typed(&mut dashboard, &mut text, "1");
    assert_eq!(dashboard.filter.query, "codex1");
    assert!(
        dashboard.panels.iter().all(Option::is_some),
        "a panel toggled"
    );
    let typed_text = dashboard.tree.get::<TextField>(field).unwrap().text();
    assert_eq!(typed_text, "codex1");
}
