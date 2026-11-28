use super::*;
use noa_gpui_core::Modifiers;

fn prepared(kind: &str) -> (WorkflowScreen, TextStore) {
    let mut screen = WorkflowScreen::demo(kind);
    let mut text = TextStore::new(Default::default());
    screen.prepare(Size::new(1280.0, 860.0), &mut text);
    (screen, text)
}

#[test]
fn minimum_zoom_keeps_connection_targets_and_accessible_stage_labels() {
    let (mut screen, mut text) = prepared("editor");
    screen.act(Action::Zoom(-1.0), &mut text).unwrap();
    screen.prepare(Size::new(1280.0, 860.0), &mut text);
    assert_eq!(screen.zoom, 0.5);
    let ports: Vec<_> = screen
        .actions
        .iter()
        .filter(|(_, action)| matches!(action, Action::Source(_) | Action::Connect(_)))
        .map(|(id, _)| *id)
        .collect();
    assert!(!ports.is_empty());
    for id in ports {
        let rect = screen.bounds(id).unwrap();
        assert!(rect.width >= 32.0 && rect.height >= 32.0, "{rect:?}");
    }
    assert_eq!(
        screen.canvas.as_ref().unwrap().cards.len(),
        screen.editor.definition.nodes.len()
    );
}

#[test]
fn inspector_connection_controls_keep_the_same_minimum_target_as_ports() {
    let (mut screen, mut text) = prepared("editor");
    screen
        .act(Action::Source("task".into()), &mut text)
        .unwrap();
    screen
        .act(Action::Select("plan".into()), &mut text)
        .unwrap();
    screen.prepare(Size::new(1280.0, 860.0), &mut text);
    let mut checked = [false; 3];
    for (id, action) in &screen.actions {
        let Some(button) = screen.tree.get::<Button>(*id) else {
            continue;
        };
        let index = match action {
            Action::Source(_) => 0,
            Action::Connect(_) => 1,
            Action::Disconnect(_, _) => 2,
            _ => continue,
        };
        checked[index] = true;
        let rect = screen.bounds(*id).unwrap();
        assert!(
            rect.width >= 32.0 && rect.height >= 32.0,
            "{}: {rect:?}",
            button.label()
        );
    }
    assert_eq!(checked, [true; 3]);
}

#[test]
fn presets_roundtrip_assignments_without_overwriting_graph_edits() {
    let (mut screen, mut text) = prepared("editor");
    screen
        .editor
        .definition
        .nodes
        .iter_mut()
        .find(|n| n.id == "review")
        .unwrap()
        .agent = AgentChoice {
        provider: "claude".into(),
        model: Some("opus".into()),
        effort: Some("high".into()),
    };
    screen.preset_name = "Review with Claude".into();
    screen.act(Action::SavePreset, &mut text).unwrap();
    screen
        .editor
        .definition
        .nodes
        .iter_mut()
        .find(|n| n.id == "review")
        .unwrap()
        .agent = AgentChoice::default();
    screen.editor.definition.name = "A different title".into();
    screen.act(Action::ApplyPreset(0), &mut text).unwrap();
    assert_eq!(
        screen
            .editor
            .definition
            .node("review")
            .unwrap()
            .agent
            .provider,
        "claude"
    );
    assert_eq!(screen.editor.definition.name, "A different title");
    assert!(screen.editor.dirty());
}

#[test]
fn saved_run_inspection_has_no_execution_channel() {
    let run = workflow::model::Run::demo();
    let mut screen = WorkflowScreen::saved(run.clone());
    let mut text = TextStore::new(Default::default());
    screen.prepare(Size::new(1280.0, 860.0), &mut text);
    assert!(screen.channel.is_none());
    assert!(!screen.view.can_control);
    assert!(screen.send(Intent::Resume).is_err());
    assert_eq!(screen.canvas.as_ref().unwrap().cards.len(), run.nodes.len());
    assert!(screen.inspector.is_some());
    assert!(screen.fields.is_empty());
    assert!(
        screen
            .actions
            .iter()
            .any(|(_, action)| matches!(action, Action::PreviewText(..)))
    );
    assert!(!screen.actions.iter().any(|(_, action)| matches!(
        action,
        Action::Start | Action::Resume | Action::Pause | Action::Cancel | Action::Approve(..)
    )));
    for (id, action) in &screen.actions {
        if matches!(
            action,
            Action::Tab(Tab::Build) | Action::Validate | Action::Save
        ) {
            assert!(!screen.tree.is_enabled(*id));
        }
    }
    assert!(screen.act(Action::Tab(Tab::Build), &mut text).is_err());
    assert!(screen.act(Action::Add(NodeKind::Plan), &mut text).is_err());
    assert!(
        screen
            .act(Action::Select(String::new()), &mut text)
            .is_err()
    );

    screen.act(Action::Tab(Tab::History), &mut text).unwrap();
    let stage = screen
        .actions
        .iter()
        .find_map(|(_, action)| match action {
            Action::Select(id) => Some(id.clone()),
            _ => None,
        })
        .unwrap();
    screen
        .act(Action::Select(stage.clone()), &mut text)
        .unwrap();
    assert!(screen.tab == Tab::Run);
    assert_eq!(screen.selected_run.as_deref(), Some(stage.as_str()));
    assert!(screen.canvas.is_some());
    screen
        .act(
            Action::PreviewText("保存済みの報告".into(), "検証結果".into()),
            &mut text,
        )
        .unwrap();
    assert_eq!(
        screen
            .tree
            .get::<TextView>(screen.tree.focused().unwrap())
            .unwrap()
            .text(),
        "検証結果"
    );
    screen.act(Action::ClosePreview, &mut text).unwrap();
    assert_eq!(screen.selected_run.as_deref(), Some(stage.as_str()));
    assert_eq!(screen.view.run, Some(run));
}

#[test]
fn an_unavailable_runner_still_shows_the_startup_error() {
    let mut screen = WorkflowScreen::unavailable(
        PathBuf::from("/example/Noa"),
        "保存先を読み取れません".into(),
    );
    let mut text = TextStore::new(Default::default());
    screen.prepare(Size::new(1280.0, 860.0), &mut text);
    assert!(!screen.is_saved_run());
    assert!(screen.canvas.is_none());
    assert!(screen.inspector.is_none());
    assert_eq!(
        screen.view.notice.as_deref(),
        Some("保存先を読み取れません")
    );
}

#[test]
fn the_saved_demo_uses_the_same_read_only_path_as_saved_runs() {
    let (mut screen, mut text) = prepared("saved");
    assert!(screen.is_saved_run());
    assert!(screen.canvas.is_some());
    assert!(screen.inspector.is_some());
    assert!(screen.act(Action::Start, &mut text).is_err());
    assert!(screen.act(Action::SavePreset, &mut text).is_err());
    assert!(screen.act(Action::Tab(Tab::Build), &mut text).is_err());
    assert!(screen.channel.is_none());
    assert!(!screen.view.can_control);
}

#[test]
fn historical_failed_evidence_is_readable_with_its_attempt_and_target() {
    use workflow::model::{Artifact, ArtifactKind, AttemptRecord};
    let temporary = tempfile::tempdir().unwrap();
    let path = std::fs::canonicalize(temporary.path())
        .unwrap()
        .join("old-report.json");
    let contents = "{\"passed\":false,\"summary\":\"以前の対象に対する不合格結果\"}";
    std::fs::write(&path, contents).unwrap();
    let hash = workflow::adapters::fs::hash(contents.as_bytes());
    let previous_target = "a".repeat(64);
    let mut artifact = Artifact::new(
        ArtifactKind::Evidence,
        previous_target.clone(),
        "過去の試行で検証に失敗しました",
    );
    artifact.path = Some(path.clone());
    artifact.data = serde_json::json!({"report_sha256": hash});
    let mut run = workflow::engine::demo_failed(100);
    let node = run.nodes.get_mut("verify").unwrap();
    node.attempt = 2;
    node.history.push(AttemptRecord {
        attempt: 1,
        state: NodeState::Failed,
        message: "修正前の不合格".into(),
        outputs: vec![artifact],
        target_digest: previous_target.clone(),
    });
    let mut screen = WorkflowScreen::saved(run.clone());
    let mut text = TextStore::new(Default::default());
    screen.prepare(Size::new(1280.0, 860.0), &mut text);
    let action = screen
        .actions
        .iter()
        .find_map(|(_, action)| match action {
            Action::Evidence(file, _, _) if file == &path => Some(action.clone()),
            _ => None,
        })
        .unwrap();
    screen.act(action, &mut text).unwrap();
    let (title, body) = screen.evidence_preview.as_ref().unwrap();
    assert!(title.contains("過去の試行 #1"));
    assert!(title.contains(&short(&previous_target)));
    assert_eq!(body, contents);
    assert_eq!(screen.view.run, Some(run));
    assert!(
        screen.view.run.as_ref().unwrap().nodes["verify"]
            .outputs
            .is_empty()
    );
}

#[test]
fn a_task_handoff_keeps_its_form_separate_from_an_older_active_run() {
    let mut screen = WorkflowScreen::demo("approval");
    screen.view.deferred = true;
    let objective = "The newly selected task".to_owned();
    let origin = workflow::tasks::TaskOrigin {
        repo: screen.view.repository.clone(),
        task: "t-new".into(),
        task_key: "{\"local\":\"t-new\"}".into(),
        title: "A new task".into(),
        objective_digest: workflow::model::digest_bytes(objective.as_bytes()),
    };
    let mut screen = screen.with_task(origin.clone(), objective.clone());
    let mut text = TextStore::new(Default::default());
    screen.prepare(Size::new(1280.0, 860.0), &mut text);
    assert!(screen.preparing_run);
    assert!(!screen.can_start_run());
    let objective_field = screen
        .fields
        .iter()
        .find(|(_, field)| matches!(field, Field::Objective))
        .unwrap()
        .0;
    assert_eq!(
        screen.tree.get::<TextArea>(objective_field).unwrap().text(),
        objective
    );
    assert!(
        screen
            .actions
            .iter()
            .any(|(_, action)| matches!(action, Action::SavedRun))
    );

    // Asynchronous start failure does not replace this objective with the old run.
    screen.start_pending = true;
    let mut refused = screen.view.clone();
    refused.notice = Some("前の実行に判断待ちの工程があります".into());
    screen.receive(refused, &mut text);
    assert!(screen.preparing_run && !screen.start_pending);
    assert_eq!(screen.origin_task, Some(origin));
    assert_eq!(screen.objective, objective);

    screen.act(Action::SavedRun, &mut text).unwrap();
    assert!(!screen.preparing_run);
    assert!(
        !screen
            .fields
            .iter()
            .any(|(_, field)| matches!(field, Field::Objective))
    );
    screen
        .act(Action::Select(String::new()), &mut text)
        .unwrap();
    assert!(screen.preparing_run);
    assert_eq!(screen.objective, objective);
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
fn resizing_within_narrow_mode_keeps_the_inspector_inside_the_window() {
    let (mut screen, mut text) = prepared("editor");
    screen.prepare(Size::new(1000.0, 700.0), &mut text);
    screen.act(Action::Details, &mut text).unwrap();
    screen.prepare(Size::new(1000.0, 700.0), &mut text);
    let title = type_title(&mut screen, &mut text, "縮小しても保持する編集");
    let inspector = screen.inspector.unwrap();
    for width in [720.0, 980.0, 760.0] {
        screen.prepare(Size::new(width, 700.0), &mut text);
        assert_eq!(screen.inspector, Some(inspector));
        assert_eq!(screen.tree.focused(), Some(title));
        let bounds = screen.bounds(inspector).unwrap();
        assert!(
            bounds.x >= 0.0 && bounds.x + bounds.width <= width + 1.0,
            "{bounds:?}"
        );
    }
    assert_eq!(screen.form.title, "縮小しても保持する編集");
    assert!(screen.form.dirty);
}

#[test]
fn undo_discards_invalid_or_unchanged_input_before_earlier_graph_edits() {
    for invalid in [true, false] {
        let (mut screen, mut text) = prepared("editor");
        screen
            .editor
            .change(|definition| definition.nodes[0].position.x += 8.0);
        let graph = screen.editor.definition.clone();
        type_title(
            &mut screen,
            &mut text,
            if invalid { "" } else { "一時的な名前" },
        );
        if !invalid {
            type_title(&mut screen, &mut text, &graph.nodes[0].title);
        }
        assert!(screen.form.dirty);
        screen.act(Action::Undo, &mut text).unwrap();
        assert_eq!(screen.editor.definition, graph);
        assert_eq!(screen.form.title, graph.nodes[0].title);
        assert!(!screen.form.dirty);
        assert!(screen.editor.can_undo());
        assert!(!screen.editor.can_redo());
    }
}

#[test]
fn valid_unapplied_input_remains_redoable_after_undo() {
    let (mut screen, mut text) = prepared("editor");
    let before = screen.editor.definition.clone();
    type_title(&mut screen, &mut text, "やり直せる工程名");
    screen.act(Action::Undo, &mut text).unwrap();
    assert_eq!(screen.editor.definition, before);
    screen.act(Action::Redo, &mut text).unwrap();
    assert_eq!(screen.editor.definition.nodes[0].title, "やり直せる工程名");
}

#[test]
fn reloading_a_conflict_does_not_require_invalid_input_to_be_applied() {
    let (mut screen, mut text) = prepared("editor");
    screen.act(Action::Settings, &mut text).unwrap();
    screen.form.duration = "invalid".into();
    screen.form.dirty = true;
    let mut view = screen.view.clone();
    view.definition.name = "別のウィンドウで保存した名前".into();
    view.definition.revision += 1;
    view.version += 1;
    screen.receive(view.clone(), &mut text);
    assert!(screen.editor.conflict);
    screen.act(Action::Reload, &mut text).unwrap();
    assert_eq!(screen.editor.definition, view.definition);
    assert!(!screen.editor.conflict);
    assert!(!screen.form.dirty);
}

#[test]
fn asynchronously_saved_presets_appear_without_leaving_settings() {
    let (mut screen, mut text) = prepared("editor");
    screen.act(Action::Settings, &mut text).unwrap();
    let mut view = screen.view.clone();
    view.presets.push(AgentPreset::from_definition(
        "保存した担当構成".into(),
        &view.definition,
    ));
    screen.receive(view, &mut text);
    assert!(
        screen
            .actions
            .iter()
            .any(|(_, action)| matches!(action, Action::ApplyPreset(0)))
    );
}

#[test]
fn preset_updates_wait_until_a_focused_settings_edit_can_be_preserved() {
    let (mut screen, mut text) = prepared("editor");
    screen.act(Action::Settings, &mut text).unwrap();
    let name = screen
        .fields
        .iter()
        .find(|(_, field)| matches!(field, Field::Name))
        .unwrap()
        .0;
    screen.tree.set_focus(Some(name));
    screen.dispatch(Event::Text("編集中".into()), &mut text);
    let draft = screen.form.name.clone();
    let mut view = screen.view.clone();
    view.presets.push(AgentPreset::from_definition(
        "新しい担当構成".into(),
        &view.definition,
    ));
    screen.receive(view, &mut text);
    assert_eq!(screen.tree.focused(), Some(name));
    assert!(screen.deferred_inspector);
    screen.tree.set_focus(None);
    screen.prepare(Size::new(1280.0, 860.0), &mut text);
    assert!(!screen.deferred_inspector);
    assert_eq!(screen.form.name, draft);
    assert!(screen.form.dirty);
    assert!(
        screen
            .actions
            .iter()
            .any(|(_, action)| matches!(action, Action::ApplyPreset(0)))
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

fn question_screen() -> (WorkflowScreen, TextStore) {
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
            input_sequence: Some(41),
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
    (screen, text)
}

fn answer_field(screen: &WorkflowScreen) -> WidgetId {
    screen
        .fields
        .iter()
        .find(|(_, field)| matches!(field, Field::Answer(id) if id == "plan"))
        .unwrap()
        .0
}

#[test]
fn automatically_displayed_question_keeps_the_answer_node_identity() {
    let (mut screen, mut text) = question_screen();
    let field = answer_field(&screen);
    screen.tree.set_focus(Some(field));
    screen.dispatch(Event::Text("ログイン画面です".into()), &mut text);
    assert_eq!(
        screen.answers.get("plan").map(String::as_str),
        Some("ログイン画面です")
    );
    assert!(screen.selected_run.is_none());
    let action = screen
        .actions
        .iter()
        .find_map(|(_, action)| match action {
            Action::Answer(target) => Some(target.clone()),
            _ => None,
        })
        .unwrap();
    let run = screen.view.run.as_ref().unwrap();
    assert_eq!(action.run_id, run.id);
    assert_eq!(action.node_id, "plan");
    assert_eq!(action.attempt, run.nodes["plan"].attempt);
    assert_eq!(action.request_id, run.nodes["plan"].request_id);
    assert_eq!(
        Some(action.input_sequence),
        run.nodes["plan"].input_sequence
    );
}

#[test]
fn unrelated_runtime_updates_preserve_the_current_question_draft() {
    let (mut screen, mut text) = question_screen();
    let field = answer_field(&screen);
    screen.tree.set_focus(Some(field));
    screen.dispatch(Event::Text("入力中の回答".into()), &mut text);
    let mut view = screen.view.clone();
    view.run.as_mut().unwrap().updated_at += 1;
    screen.receive(view, &mut text);
    assert_eq!(screen.tree.focused(), Some(field));
    assert_eq!(
        screen.answers.get("plan").map(String::as_str),
        Some("入力中の回答")
    );
    assert_eq!(
        screen.tree.get::<TextArea>(field).unwrap().text(),
        "入力中の回答"
    );
}

#[test]
fn a_replaced_question_cannot_reuse_a_focused_answer_from_another_request() {
    for change in ["run", "attempt", "request", "question", "sequence", "state"] {
        let (mut screen, mut text) = question_screen();
        let field = answer_field(&screen);
        screen.tree.set_focus(Some(field));
        screen.dispatch(Event::Text("前の質問への回答".into()), &mut text);
        let mut view = screen.view.clone();
        let run = view.run.as_mut().unwrap();
        let node = run.nodes.get_mut("plan").unwrap();
        match change {
            "run" => run.id = "another-run".into(),
            "attempt" => node.attempt += 1,
            "request" => node.request_id = "another-request".into(),
            "question" => node.message = "変更後の質問".into(),
            "sequence" => node.input_sequence = Some(42),
            "state" => node.state = NodeState::Running,
            _ => unreachable!(),
        }
        screen.receive(view, &mut text);
        assert!(!screen.answers.contains_key("plan"), "{change}");
        assert!(!screen.tree.contains(field), "{change}");
        if change != "state" {
            assert!(
                screen
                    .tree
                    .get::<TextArea>(answer_field(&screen))
                    .unwrap()
                    .text()
                    .is_empty()
            );
        }
    }
}

#[test]
fn unversioned_input_requests_require_reconciliation_before_answering() {
    let (mut screen, mut text) = question_screen();
    let mut view = screen.view.clone();
    view.run
        .as_mut()
        .unwrap()
        .nodes
        .get_mut("plan")
        .unwrap()
        .input_sequence = None;
    screen.receive(view, &mut text);
    assert!(
        !screen
            .fields
            .iter()
            .any(|(_, field)| matches!(field, Field::Answer(_)))
    );
    assert!(
        !screen
            .actions
            .iter()
            .any(|(_, action)| matches!(action, Action::Answer(_)))
    );
    assert!(
        screen
            .actions
            .iter()
            .any(|(_, action)| matches!(action, Action::Reconcile(node) if node == "plan"))
    );
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
