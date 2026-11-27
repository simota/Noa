use super::*;
use noa_gpui_core::widgets::TextField;
use noa_gpui_scene::Size;
use serde_json::json;
use std::time::Instant;

fn request_manifest() -> Manifest {
    Manifest::from_slice(include_bytes!(
        "../../../../.noa-widgets/agent-request/widget.json"
    ))
    .unwrap()
}

fn snapshot(manifest: &Manifest, revision: u64, data: Value) -> HostMessage {
    HostMessage::Snapshot {
        instance_id: "wi_0123456789abcdef0123456789abcdef".into(),
        definition_digest: manifest.digest(),
        data_revision: revision,
        manifest: serde_json::to_value(manifest).unwrap(),
        data,
    }
}

#[test]
fn a_button_keeps_keyboard_focus_when_data_rebuilds_the_body() {
    let manifest = request_manifest();
    let mut screen =
        WidgetScreen::from_snapshot(snapshot(&manifest, 1, manifest.initial_data.clone())).unwrap();
    let mut text = TextStore::new(Default::default());
    let original = screen.inputs.buttons[0].widget;
    screen.tree.set_focus(Some(original));
    let mut data = manifest.initial_data.clone();
    data["status"] = json!("New producer status");
    screen
        .apply_snapshot(snapshot(&manifest, 2, data), &mut text)
        .unwrap();
    let next = screen.inputs.buttons[0].widget;
    assert_eq!(screen.tree.focused(), Some(next));
    assert!(screen.tree.is_enabled(next));
}

#[test]
fn narrow_rows_wrap_inputs_and_resize_without_resetting_the_caret() {
    let mut document = serde_json::to_value(request_manifest()).unwrap();
    let first = document["view"]["children"][2].clone();
    let mut second = first.clone();
    second["id"] = json!("other-input");
    second.as_object_mut().unwrap().remove("submit");
    document["view"]["children"][2] = json!({"type":"row","id":"inputs","children":[first,second]});
    let manifest = Manifest::from_slice(&serde_json::to_vec(&document).unwrap()).unwrap();
    let mut screen = WidgetScreen::preview(manifest.clone(), manifest.initial_data).unwrap();
    let mut text = TextStore::new(Default::default());
    screen.prepare(Size::new(960.0, 900.0), &mut text);
    let subject = screen
        .inputs
        .fields
        .iter()
        .find(|field| field.id == "subject")
        .unwrap()
        .widget;
    let other = screen
        .inputs
        .fields
        .iter()
        .find(|field| field.id == "other-input")
        .unwrap()
        .widget;
    screen.tree.set_focus(Some(subject));
    screen.dispatch(Event::Text("日本語の下書き".into()), &mut text);
    screen
        .tree
        .update::<TextField, _>(subject, |field, cx| field.select(cx, 3..9));
    screen.prepare(Size::new(480.0, 900.0), &mut text);
    let first = screen.tree.window_rect(subject).unwrap();
    let next = screen.tree.window_rect(other).unwrap();
    assert!(
        first.width > 400.0 && first.x >= 0.0 && first.x + first.width <= 480.1,
        "{first:?}"
    );
    assert!(
        next.y >= first.y + first.height,
        "narrow rows must stack: {first:?} {next:?}"
    );
    screen.prepare(Size::new(960.0, 900.0), &mut text);
    let field = screen.tree.get::<TextField>(subject).unwrap();
    assert_eq!(field.text(), "日本語の下書き");
    assert_eq!(field.selection(), 3..9);
    assert_eq!(screen.tree.focused(), Some(subject));
}

#[test]
fn fixture_switching_uses_the_same_renderer_and_never_sends_a_live_action() {
    let manifest = request_manifest();
    let mut data = manifest.initial_data.clone();
    data["status"] = json!("Fixture failure");
    let fixtures = noa_widget::discovery::Fixtures {
        values: vec![noa_widget::discovery::Fixture {
            name: "error.json".into(),
            data: data.clone(),
        }],
        diagnostics: Vec::new(),
    };
    let mut screen = WidgetScreen::preview(manifest.clone(), manifest.initial_data.clone())
        .unwrap()
        .with_fixtures(fixtures);
    let mut text = TextStore::new(Default::default());
    screen.switch_fixture(true, &mut text);
    assert_eq!(screen.data, data);
    assert!(screen.instance.is_none());
    assert!(screen.preview_events.is_empty());
    screen.switch_fixture(false, &mut text);
    assert_eq!(screen.data, manifest.initial_data);
}

#[test]
fn receipt_and_data_updates_never_imply_processing_completion() {
    let manifest = request_manifest();
    let mut screen =
        WidgetScreen::from_snapshot(snapshot(&manifest, 1, manifest.initial_data.clone())).unwrap();
    let mut text = TextStore::new(Default::default());
    let operation = WidgetOperation {
        event_id: "we_0123456789abcdef0123456789abcdef_1".into(),
        seq: 1,
        action_id: "request".into(),
        action_request_id: Some("ui-request".into()),
        version: 1,
        state: OperationState::Accepted,
        message: String::new(),
        accepted_at_ms: Some(1_000),
        updated_at_ms: Some(1_000),
    };
    let status = |operation: WidgetOperation| HostMessage::Status {
        instance_id: "wi_0123456789abcdef0123456789abcdef".into(),
        data_updated_at_ms: Some(1_000),
        owner_pane_id: Some(noa_ipc::WireId(42)),
        pending_operations: usize::from(!operation.state.is_terminal()),
        total_operations: 1,
        operations: vec![operation],
    };
    screen.receive(Update::Message(status(operation.clone())), &mut text);
    let mut data = manifest.initial_data.clone();
    data["status"] = json!("Some unrelated live data");
    screen
        .apply_snapshot(snapshot(&manifest, 2, data), &mut text)
        .unwrap();
    assert_eq!(
        screen.operation_ui.as_ref().unwrap().operations[0].state,
        OperationState::Accepted
    );
    let mut completed = operation.clone();
    completed.version = 2;
    completed.state = OperationState::Completed;
    screen.receive(Update::Message(status(completed)), &mut text);
    screen.receive(Update::Message(status(operation)), &mut text);
    assert_eq!(
        screen.operation_ui.as_ref().unwrap().operations[0].state,
        OperationState::Completed,
        "an old status cannot roll back a report"
    );
}

#[test]
fn an_uncertain_receipt_is_confirmed_only_by_its_original_ui_request_id() {
    let manifest = request_manifest();
    let mut screen =
        WidgetScreen::from_snapshot(snapshot(&manifest, 1, manifest.initial_data.clone())).unwrap();
    let (bridge, requests) = Bridge::recording();
    screen.attach_bridge(bridge);
    let mut text = TextStore::new(Default::default());
    screen.activate(Action {
        id: "request".into(),
        payload: json!({"kind":"request","subject":"Check","details":""}),
    });
    let HostRequest::Action { request_id, .. } = requests.try_recv().unwrap() else {
        panic!("expected the UI action");
    };
    screen.receive(
        Update::Message(HostMessage::ActionResult {
            request_id: request_id.clone(),
            ok: false,
            error: Some("reply was uncertain".into()),
            error_kind: Some("outcome_unknown".into()),
        }),
        &mut text,
    );
    let operation = |action_request_id, seq| WidgetOperation {
        event_id: format!("we_0123456789abcdef0123456789abcdef_{seq}"),
        seq,
        action_id: "request".into(),
        action_request_id: Some(action_request_id),
        version: 1,
        state: OperationState::Accepted,
        message: String::new(),
        accepted_at_ms: None,
        updated_at_ms: None,
    };
    let status = |operations: Vec<WidgetOperation>| HostMessage::Status {
        instance_id: "wi_0123456789abcdef0123456789abcdef".into(),
        data_updated_at_ms: None,
        owner_pane_id: None,
        pending_operations: operations.len(),
        total_operations: operations.len(),
        operations,
    };
    let unrelated = operation("unrelated-ui-request".into(), 1);
    screen.receive(Update::Message(status(vec![unrelated.clone()])), &mut text);
    assert!(screen.instance.as_ref().unwrap().pending.is_some());
    assert!(screen.retry.is_some());
    screen.receive(
        Update::Message(status(vec![operation(request_id, 2), unrelated])),
        &mut text,
    );
    assert!(screen.instance.as_ref().unwrap().pending.is_none());
    assert!(screen.retry.is_none());
    assert_eq!(
        screen.operation_ui.as_ref().unwrap().operations[0].state,
        OperationState::Accepted
    );
    assert!(
        requests.try_recv().is_err(),
        "receipt confirmation sends no second action"
    );
}

#[test]
fn thousand_row_updates_preserve_focus_and_skip_an_identical_resolved_view() {
    let mut document: Value = serde_json::from_slice(include_bytes!(
        "../../../../.noa-widgets/test-summary/widget.json"
    ))
    .unwrap();
    document["dataSchema"]["properties"]["failures"]["maxItems"] = json!(1000);
    document["dataSchema"]["properties"]["sequence"] = json!({"type":"integer"});
    let manifest = Manifest::from_slice(&serde_json::to_vec(&document).unwrap()).unwrap();
    let mut data = manifest.initial_data.clone();
    data["failures"] = json!(
        (0..1000)
            .map(|i| json!({"id":format!("row-{i}"),"name":format!("Test {i}")}))
            .collect::<Vec<_>>()
    );
    data["failed"] = json!(1000);
    let initial_snapshot = snapshot(&manifest, 1, data.clone());
    let started = Instant::now();
    let mut screen = WidgetScreen::from_snapshot(initial_snapshot).unwrap();
    let initial = started.elapsed();
    let mut text = TextStore::new(Default::default());
    let viewport = Size::new(960.0, 720.0);
    let started = Instant::now();
    screen.prepare(viewport, &mut text);
    let initial_prepare = started.elapsed();
    let initial_stats = screen.tree.stats();
    let button = screen.actions[0].0;
    screen.tree.set_focus(Some(button));
    let children = screen.tree.children(screen.body).to_vec();
    data["sequence"] = json!(2);
    let next_snapshot = snapshot(&manifest, 2, data.clone());
    let started = Instant::now();
    screen.apply_snapshot(next_snapshot, &mut text).unwrap();
    let unchanged = started.elapsed();
    let started = Instant::now();
    screen.prepare(viewport, &mut text);
    let unchanged_prepare = started.elapsed();
    let unchanged_stats = screen.tree.stats();
    assert_eq!(screen.tree.children(screen.body), children);
    assert_eq!(screen.tree.focused(), Some(button));
    data["failures"][999]["name"] = json!("Last row changed");
    let next_snapshot = snapshot(&manifest, 3, data);
    let started = Instant::now();
    screen.apply_snapshot(next_snapshot, &mut text).unwrap();
    let changed = started.elapsed();
    let started = Instant::now();
    screen.prepare(viewport, &mut text);
    let changed_prepare = started.elapsed();
    let changed_stats = screen.tree.stats();
    assert_eq!(screen.tree.focused(), Some(screen.actions[0].0));
    assert!(screen.tree.len() > 1000);
    eprintln!(
        "1000-row widget, {} tree nodes, 960x720 logical viewport; single-run diagnostics, prepare includes layout/text/display list, no GPU or frame-rate measurement:\n\
         initial model/tree={initial:?}, prepare={initial_prepare:?}, stats={initial_stats:?}\n\
         unchanged-view model/tree={unchanged:?}, prepare={unchanged_prepare:?}, stats={unchanged_stats:?}\n\
         one-row-change model/tree={changed:?}, prepare={changed_prepare:?}, stats={changed_stats:?}",
        screen.tree.len()
    );
}
