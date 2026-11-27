use super::*;
use noa_gpui_core::Modifiers;

fn prepared(kind: &str) -> (WorkflowScreen, TextStore) {
    let mut screen = WorkflowScreen::demo(kind);
    let mut text = TextStore::new(Default::default());
    screen.prepare(Size::new(1280.0, 860.0), &mut text);
    (screen, text)
}

fn type_title(screen: &mut WorkflowScreen, text: &mut TextStore, value: &str) -> WidgetId {
    let field = screen
        .fields
        .iter()
        .find(|(_, f)| matches!(f, Field::Title))
        .unwrap()
        .0;
    let len = screen.tree.get::<TextField>(field).unwrap().text().len();
    screen.tree.set_focus(Some(field));
    screen
        .tree
        .update::<TextField, _>(field, |f, cx| f.select(cx, 0..len));
    screen.dispatch(Event::Text(value.into()), text);
    field
}

#[test]
fn runtime_notifications_do_not_erase_a_focused_edit() {
    let (mut screen, mut text) = prepared("editor");
    let field = type_title(&mut screen, &mut text, "新しいタスク名");
    let mut view = screen.view.clone();
    view.version += 1;
    view.run = Some(workflow::engine::demo_waiting(now()));
    screen.receive(view, &mut text);
    assert_eq!(screen.tree.focused(), Some(field));
    assert_eq!(
        screen.tree.get::<TextField>(field).unwrap().text(),
        "新しいタスク名"
    );
    assert!(screen.form.dirty);
    screen.act(Action::Save, &mut text).unwrap();
    assert_eq!(
        screen.editor.definition.node("task").unwrap().title,
        "新しいタスク名"
    );
    assert_ne!(
        screen
            .view
            .run
            .as_ref()
            .unwrap()
            .definition
            .node("task")
            .unwrap()
            .title,
        "新しいタスク名"
    );
}

#[test]
fn narrow_layout_and_zoom_preserve_unapplied_form_values() {
    let (mut screen, mut text) = prepared("editor");
    type_title(&mut screen, &mut text, "保持する編集");
    screen.act(Action::Zoom(0.1), &mut text).unwrap();
    assert_eq!(screen.form.title, "保持する編集");
    assert!(screen.form.dirty);
    screen.prepare(Size::new(720.0, 600.0), &mut text);
    assert!(screen.narrow);
    screen.act(Action::Details, &mut text).unwrap();
    screen.prepare(Size::new(720.0, 600.0), &mut text);
    let title = screen
        .fields
        .iter()
        .find(|(_, f)| matches!(f, Field::Title))
        .unwrap()
        .0;
    assert_eq!(
        screen.tree.get::<TextField>(title).unwrap().text(),
        "保持する編集"
    );
    let bounds = screen.bounds(title).unwrap();
    assert!(
        bounds.x >= 0.0 && bounds.x + bounds.width <= 721.0,
        "{bounds:?}"
    );
}

#[test]
fn keyboard_movement_is_undoable_and_keeps_canvas_focus() {
    let (mut screen, mut text) = prepared("editor");
    let card = screen.canvas.as_ref().unwrap().cards[0].widget;
    screen.tree.set_focus(Some(card));
    let before = screen.editor.definition.clone();
    screen.dispatch(
        Event::KeyDown {
            key: Key::ArrowRight,
            modifiers: Modifiers::default(),
        },
        &mut text,
    );
    assert_eq!(
        screen.editor.definition.nodes[0].position.x,
        before.nodes[0].position.x + 8.0
    );
    let focus = screen.tree.focused().unwrap();
    assert!(
        screen
            .canvas
            .as_ref()
            .unwrap()
            .cards
            .iter()
            .any(|c| c.widget == focus)
    );
    screen.dispatch(
        Event::KeyDown {
            key: Key::Char('z'),
            modifiers: Modifiers {
                command: true,
                ..Default::default()
            },
        },
        &mut text,
    );
    assert_eq!(screen.editor.definition, before);
}

#[test]
fn drag_is_one_undo_operation_and_escape_cancels_it() {
    let (mut screen, mut text) = prepared("editor");
    let card = screen.canvas.as_ref().unwrap().cards[0].widget;
    let rect = screen.bounds(card).unwrap();
    let p = Point::new(rect.x + 40.0, rect.y + 30.0);
    let before = screen.editor.definition.clone();
    screen.dispatch(
        Event::PointerDown {
            pos: p,
            button: PointerButton::Primary,
        },
        &mut text,
    );
    screen.dispatch(
        Event::PointerMove {
            pos: Point::new(p.x + 90.0, p.y + 72.0),
        },
        &mut text,
    );
    assert_ne!(
        screen.editor.definition.nodes[0].position,
        before.nodes[0].position
    );
    screen.dispatch(
        Event::KeyDown {
            key: Key::Escape,
            modifiers: Modifiers::default(),
        },
        &mut text,
    );
    assert_eq!(screen.editor.definition, before);
    assert!(!screen.editor.can_undo());
    screen.prepare(Size::new(1280.0, 860.0), &mut text);
    screen.dispatch(
        Event::PointerDown {
            pos: p,
            button: PointerButton::Primary,
        },
        &mut text,
    );
    screen.dispatch(
        Event::PointerMove {
            pos: Point::new(p.x + 90.0, p.y + 72.0),
        },
        &mut text,
    );
    screen.dispatch(
        Event::PointerUp {
            pos: p,
            button: PointerButton::Primary,
        },
        &mut text,
    );
    screen.act(Action::Undo, &mut text).unwrap();
    assert_eq!(screen.editor.definition, before);
}

#[test]
fn approval_button_carries_the_evidence_digest_and_demo_cannot_publish() {
    let (mut screen, mut text) = prepared("approval");
    let action = screen
        .actions
        .iter()
        .find_map(|(_, a)| {
            if let Action::Approve(id, digest) = a {
                Some((id.clone(), digest.clone()))
            } else {
                None
            }
        })
        .unwrap();
    let expected = screen
        .view
        .run
        .as_ref()
        .unwrap()
        .nodes
        .get("approval")
        .unwrap()
        .approval_digest
        .clone()
        .unwrap();
    assert_eq!(action.1, expected);
    let before = screen.view.run.clone();
    assert!(
        screen
            .act(Action::Approve(action.0, action.1), &mut text)
            .is_err()
    );
    assert_eq!(screen.view.run, before);
}

#[test]
fn adopting_an_external_definition_refreshes_the_editor_controls() {
    let (mut screen, mut text) = prepared("editor");
    let mut view = screen.view.clone();
    view.definition.nodes[0].title = "外部で保存したタスク".into();
    view.definition.revision += 1;
    view.version += 1;
    screen.receive(view, &mut text);
    let title = screen
        .fields
        .iter()
        .find(|(_, f)| matches!(f, Field::Title))
        .unwrap()
        .0;
    assert_eq!(
        screen.tree.get::<TextField>(title).unwrap().text(),
        "外部で保存したタスク"
    );
    assert!(!screen.form.dirty);
    assert!(!screen.editor.dirty());
}

#[test]
fn automatically_displayed_question_keeps_the_answer_node_identity() {
    use workflow::engine::{Engine, Input};
    use workflow::model::{Artifact, ArtifactKind, TargetSnapshot};
    let definition = WorkflowDefinition::standard();
    let target = TargetSnapshot {
        base_sha: "a".repeat(40),
        head_sha: "a".repeat(40),
        dirty_digest: "b".repeat(64),
    };
    let mut engine = Engine::new(
        "test-run".into(),
        definition,
        PathBuf::from("/workspace/project"),
        "目的と完了条件".into(),
        target.clone(),
        100,
    )
    .unwrap();
    engine.handle(Input::Start { now: 101 }).unwrap();
    engine
        .handle(Input::Completed {
            node_id: "task".into(),
            attempt: 1,
            outputs: vec![Artifact::new(ArtifactKind::Task, target.digest(), "目的")],
            target: None,
            now: 102,
        })
        .unwrap();
    engine
        .handle(Input::Waiting {
            node_id: "plan".into(),
            attempt: 1,
            external_ref: Some("pane-plan".into()),
            message: "対象機能を選んでください".into(),
            now: 103,
        })
        .unwrap();
    let (mut screen, mut text) = prepared("editor");
    screen.view.run = Some(engine.run().clone());
    screen.tab = Tab::Run;
    screen.preparing_run = false;
    screen.selected_run = None;
    screen.build_body(&mut text);
    let field = screen
        .fields
        .iter()
        .find(|(_, f)| matches!(f,Field::Answer(id) if id=="plan"))
        .unwrap()
        .0;
    screen.tree.set_focus(Some(field));
    screen.dispatch(Event::Text("ログイン画面です".into()), &mut text);
    assert_eq!(
        screen.answers.get("plan").map(String::as_str),
        Some("ログイン画面です")
    );
    assert!(screen.selected_run.is_none());
}

#[test]
fn terminal_result_is_kept_until_a_new_run_is_explicitly_requested() {
    let (mut screen, mut text) = prepared("failed");
    screen.selected_run = None;
    screen.build_body(&mut text);
    assert!(
        screen
            .actions
            .iter()
            .any(|(_, a)| matches!(a,Action::Retry(id) if id=="verify"))
    );
    assert!(
        !screen
            .actions
            .iter()
            .any(|(_, a)| matches!(a, Action::Start))
    );
    screen
        .act(Action::Select(String::new()), &mut text)
        .unwrap();
    assert!(
        screen
            .actions
            .iter()
            .any(|(_, a)| matches!(a, Action::Start))
    );
}

#[test]
fn escape_dismisses_the_stop_confirmation_without_canceling_the_run() {
    let (mut screen, mut text) = prepared("approval");
    let before = screen.view.run.clone();
    screen.act(Action::Cancel, &mut text).unwrap();
    assert!(
        screen
            .actions
            .iter()
            .any(|(_, a)| matches!(a, Action::ConfirmCancel))
    );
    screen.dispatch(
        Event::KeyDown {
            key: Key::Escape,
            modifiers: Modifiers::default(),
        },
        &mut text,
    );
    assert!(
        !screen
            .actions
            .iter()
            .any(|(_, a)| matches!(a, Action::ConfirmCancel))
    );
    assert_eq!(screen.view.run, before);
}

#[test]
fn background_updates_do_not_replace_an_open_evidence_preview() {
    let (mut screen, mut text) = prepared("approval");
    screen
        .act(
            Action::PreviewText("公開するPR本文".into(), "Reviewed changes".into()),
            &mut text,
        )
        .unwrap();
    let evidence = screen.tree.focused().unwrap();
    let mut view = screen.view.clone();
    view.run.as_mut().unwrap().updated_at += 1;
    screen.receive(view, &mut text);
    assert_eq!(screen.tree.focused(), Some(evidence));
    assert_eq!(
        screen.tree.get::<TextView>(evidence).unwrap().text(),
        "Reviewed changes"
    );
}
