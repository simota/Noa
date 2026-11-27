//! Text inputs: typed text reaches the agent only as a payload string of a
//! button's event, Enter never submits during an IME conversion, limits are
//! checked before sending, and typed text survives live data snapshots.

use super::*;
use noa_gpui_core::Modifiers;
use noa_gpui_core::widgets::{TextArea, TextField};
use serde_json::json;

fn document() -> Value {
    serde_json::from_slice(include_bytes!(
        "../../../../.noa-widgets/agent-request/widget.json"
    ))
    .unwrap()
}

fn manifest_from(value: &Value) -> Manifest {
    Manifest::from_slice(&serde_json::to_vec(value).unwrap()).unwrap()
}

fn manifest() -> Manifest {
    manifest_from(&document())
}

fn preview() -> WidgetScreen {
    let manifest = manifest();
    let data = manifest.initial_data.clone();
    WidgetScreen::preview(manifest, data).unwrap()
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

fn field(screen: &WidgetScreen, id: &str) -> render::InputField {
    screen
        .inputs
        .fields
        .iter()
        .find(|field| field.id == id)
        .unwrap()
        .clone()
}

fn text_of(screen: &WidgetScreen, id: &str) -> String {
    field(screen, id).text(&screen.tree)
}

fn focus(screen: &mut WidgetScreen, id: &str) {
    let widget = field(screen, id).widget;
    screen.tree.set_focus(Some(widget));
}

fn key(screen: &mut WidgetScreen, key: Key, modifiers: Modifiers, text: &mut TextStore) {
    screen.dispatch(Event::KeyDown { key, modifiers }, text);
}

fn enter(screen: &mut WidgetScreen, text: &mut TextStore) {
    key(screen, Key::Enter, Modifiers::default(), text);
}

fn cmd_enter(screen: &mut WidgetScreen, text: &mut TextStore) {
    let modifiers = Modifiers {
        command: true,
        meta: true,
        command_is_meta: true,
        ..Modifiers::default()
    };
    key(screen, Key::Enter, modifiers, text);
}

fn type_into(screen: &mut WidgetScreen, id: &str, value: &str, text: &mut TextStore) {
    focus(screen, id);
    screen.dispatch(Event::Text(value.into()), text);
}

fn compose(screen: &mut WidgetScreen, value: &str, text: &mut TextStore) {
    screen.dispatch(
        Event::Composition {
            text: value.into(),
            cursor: Some((value.len(), value.len())),
        },
        text,
    );
}

fn click_send(screen: &mut WidgetScreen, text: &mut TextStore) {
    let send = screen.inputs.buttons[0].widget;
    screen.tree.set_focus(Some(send));
    key(screen, Key::Space, Modifiers::default(), text);
}

fn notice(screen: &WidgetScreen) -> String {
    screen
        .tree
        .get::<Label>(screen.status)
        .unwrap()
        .text()
        .to_owned()
}

fn request(subject: &str, details: &str) -> Action {
    Action {
        id: "request".into(),
        payload: json!({"kind":"request","subject":subject,"details":details}),
    }
}

#[test]
fn inputs_render_as_a_text_field_and_a_text_area() {
    let screen = preview();
    let subject = field(&screen, "subject");
    let details = field(&screen, "details");
    assert!(screen.tree.get::<TextField>(subject.widget).is_some());
    assert!(screen.tree.get::<TextArea>(details.widget).is_some());
    assert_eq!(subject.submit.as_deref(), Some("send"));
    assert_eq!(subject.max_length, Some(120));
}

#[test]
fn enter_in_a_single_line_input_sends_the_typed_text_as_a_payload_string() {
    let mut screen = preview();
    let mut text = TextStore::new(Default::default());
    let literal = "$(touch /tmp/unwanted) <script>bad()</script>";
    type_into(&mut screen, "details", "背景", &mut text);
    type_into(&mut screen, "subject", literal, &mut text);
    enter(&mut screen, &mut text);
    assert_eq!(screen.preview_events(), &[request(literal, "背景")]);
    // The offline preview displays the event with the typed value.
    assert!(notice(&screen).contains("unwanted"), "{}", notice(&screen));
    // clearInputs: the preview accepts the event, so the inputs are cleared.
    assert_eq!(text_of(&screen, "subject"), "");
    assert_eq!(text_of(&screen, "details"), "");
}

#[test]
fn the_button_sends_inputs_and_inputs_without_clear_keep_their_text() {
    let mut value = document();
    value["view"]["children"][4]["clearInputs"] = json!(false);
    let manifest = manifest_from(&value);
    let data = manifest.initial_data.clone();
    let mut screen = WidgetScreen::preview(manifest, data).unwrap();
    let mut text = TextStore::new(Default::default());
    type_into(&mut screen, "subject", "調べて", &mut text);
    click_send(&mut screen, &mut text);
    assert_eq!(screen.preview_events(), &[request("調べて", "")]);
    assert_eq!(text_of(&screen, "subject"), "調べて");
}

#[test]
fn enter_during_an_ime_composition_does_not_submit() {
    let mut screen = preview();
    let mut text = TextStore::new(Default::default());
    focus(&mut screen, "subject");
    compose(&mut screen, "しらべて", &mut text);
    enter(&mut screen, &mut text);
    assert!(screen.preview_events().is_empty());
    // Committing the conversion ends the composition; Enter then submits.
    screen.dispatch(Event::Text("調べて".into()), &mut text);
    assert_eq!(text_of(&screen, "subject"), "調べて");
    enter(&mut screen, &mut text);
    assert_eq!(screen.preview_events(), &[request("調べて", "")]);
}

#[test]
fn multiline_enter_inserts_a_line_break_and_cmd_enter_submits() {
    let mut screen = preview();
    let mut text = TextStore::new(Default::default());
    type_into(&mut screen, "subject", "件名", &mut text);
    type_into(&mut screen, "details", "行1", &mut text);
    enter(&mut screen, &mut text);
    screen.dispatch(Event::Text("行2".into()), &mut text);
    assert!(screen.preview_events().is_empty());
    assert_eq!(text_of(&screen, "details"), "行1\n行2");
    // Cmd+Enter during a conversion belongs to the IME: no event, no newline.
    compose(&mut screen, "へんかん", &mut text);
    cmd_enter(&mut screen, &mut text);
    assert!(screen.preview_events().is_empty());
    assert_eq!(text_of(&screen, "details"), "行1\n行2");
    screen.dispatch(Event::Text("変換".into()), &mut text);
    cmd_enter(&mut screen, &mut text);
    assert_eq!(screen.preview_events(), &[request("件名", "行1\n行2変換")]);
}

#[test]
fn an_input_longer_than_its_max_length_is_not_sent() {
    let mut screen = preview();
    let mut text = TextStore::new(Default::default());
    type_into(&mut screen, "subject", &"あ".repeat(121), &mut text);
    enter(&mut screen, &mut text);
    assert!(screen.preview_events().is_empty());
    let message = notice(&screen);
    assert!(
        message.contains("subject") && message.contains("120") && message.contains("121"),
        "{message}"
    );
    assert_eq!(text_of(&screen, "subject"), "あ".repeat(121));
}

#[test]
fn schema_limits_and_the_payload_size_are_checked_before_sending() {
    let mut value = document();
    // Without the input's own limits only the action schema and the 16 KiB
    // payload limit apply; both are checked with Manifest::validate_action.
    for id in [2, 3] {
        value["view"]["children"][id]
            .as_object_mut()
            .unwrap()
            .remove("maxLength");
    }
    value["actions"][0]["payloadSchema"]["properties"]["details"] = json!({"type":"string"});
    let manifest = manifest_from(&value);
    let data = manifest.initial_data.clone();
    let mut screen = WidgetScreen::preview(manifest, data).unwrap();
    let mut text = TextStore::new(Default::default());
    type_into(&mut screen, "subject", &"x".repeat(121), &mut text);
    click_send(&mut screen, &mut text);
    assert!(screen.preview_events().is_empty());
    assert!(notice(&screen).contains("maxLength"), "{}", notice(&screen));

    let mut screen = {
        let manifest = manifest_from(&value);
        let data = manifest.initial_data.clone();
        WidgetScreen::preview(manifest, data).unwrap()
    };
    type_into(&mut screen, "subject", "ok", &mut text);
    type_into(&mut screen, "details", &"x".repeat(17 * 1024), &mut text);
    click_send(&mut screen, &mut text);
    assert!(screen.preview_events().is_empty());
    assert!(notice(&screen).contains("16384"), "{}", notice(&screen));
    // An empty required subject (minLength 1) is refused the same way.
    let mut screen = preview();
    click_send(&mut screen, &mut text);
    assert!(screen.preview_events().is_empty());
    assert!(notice(&screen).contains("subject"), "{}", notice(&screen));
}

#[test]
fn typed_text_and_focus_survive_a_live_data_snapshot() {
    let manifest = manifest();
    let mut screen =
        WidgetScreen::from_snapshot(snapshot(&manifest, 1, manifest.initial_data.clone())).unwrap();
    let mut text = TextStore::new(Default::default());
    type_into(&mut screen, "subject", "書きかけ", &mut text);
    type_into(&mut screen, "details", "一行目\n二行目", &mut text);
    let before = field(&screen, "details").widget;
    let data = json!({"status":"処理中です。","task":"別のタスク"});
    screen
        .apply_snapshot(snapshot(&manifest, 2, data), &mut text)
        .unwrap();
    let after = field(&screen, "details").widget;
    assert_ne!(before, after, "the view was rebuilt");
    assert_eq!(text_of(&screen, "subject"), "書きかけ");
    assert_eq!(text_of(&screen, "details"), "一行目\n二行目");
    assert_eq!(screen.tree.focused(), Some(after));
}

#[test]
fn live_inputs_are_sent_and_cleared_only_after_the_service_accepts() {
    let manifest = manifest();
    let mut screen =
        WidgetScreen::from_snapshot(snapshot(&manifest, 1, manifest.initial_data.clone())).unwrap();
    let (bridge, requests) = Bridge::recording();
    screen.attach_bridge(bridge);
    let mut text = TextStore::new(Default::default());
    type_into(&mut screen, "subject", "調べて", &mut text);
    enter(&mut screen, &mut text);
    let sent = serde_json::to_value(requests.try_recv().unwrap()).unwrap();
    assert_eq!(
        sent["payload"],
        json!({"kind":"request","subject":"調べて","details":""})
    );
    // A refused event keeps the text for another attempt.
    let refuse = |request_id: &Value| {
        Update::Message(HostMessage::ActionResult {
            request_id: request_id.as_str().unwrap().into(),
            ok: false,
            error: Some("event queue is full".into()),
            error_kind: None,
        })
    };
    screen.receive(refuse(&sent["requestId"]), &mut text);
    assert_eq!(text_of(&screen, "subject"), "調べて");
    // While pending, Enter does not send a second event.
    enter(&mut screen, &mut text);
    let sent = serde_json::to_value(requests.try_recv().unwrap()).unwrap();
    enter(&mut screen, &mut text);
    assert!(requests.try_recv().is_err());
    // Text typed into another input after sending is not discarded.
    type_into(&mut screen, "details", "追記", &mut text);
    screen.receive(
        Update::Message(HostMessage::ActionResult {
            request_id: sent["requestId"].as_str().unwrap().into(),
            ok: true,
            error: None,
            error_kind: None,
        }),
        &mut text,
    );
    assert_eq!(text_of(&screen, "subject"), "");
    assert_eq!(text_of(&screen, "details"), "追記");
}

fn live() -> (Manifest, WidgetScreen) {
    let manifest = manifest();
    let screen =
        WidgetScreen::from_snapshot(snapshot(&manifest, 1, manifest.initial_data.clone())).unwrap();
    (manifest, screen)
}

fn update_status(screen: &mut WidgetScreen, manifest: &Manifest, text: &mut TextStore) {
    let data = json!({"status":"処理中です。","task":"別のタスク"});
    screen
        .apply_snapshot(snapshot(manifest, 2, data), text)
        .unwrap();
}

#[test]
fn a_live_data_snapshot_keeps_the_caret_where_the_person_is_editing() {
    let (manifest, mut screen) = live();
    let mut text = TextStore::new(Default::default());
    type_into(&mut screen, "subject", "あいう", &mut text);
    let widget = field(&screen, "subject").widget;
    screen
        .tree
        .update::<TextField, _>(widget, |field, cx| field.select(cx, 0..0));
    update_status(&mut screen, &manifest, &mut text);
    screen.dispatch(Event::Text("X".into()), &mut text);
    assert_eq!(text_of(&screen, "subject"), "Xあいう");
}

#[test]
fn a_live_data_snapshot_during_an_ime_composition_keeps_it_and_enter_does_not_submit() {
    let (manifest, mut screen) = live();
    let mut text = TextStore::new(Default::default());
    type_into(&mut screen, "subject", "書き", &mut text);
    compose(&mut screen, "かけ", &mut text);
    update_status(&mut screen, &manifest, &mut text);
    let widget = field(&screen, "subject").widget;
    assert_eq!(
        screen.tree.get::<TextField>(widget).unwrap().composition(),
        "かけ"
    );
    assert_eq!(screen.tree.focused(), Some(widget));
    enter(&mut screen, &mut text);
    assert!(
        screen.instance.as_ref().unwrap().pending.is_none(),
        "Enter during a conversion must not send"
    );
}
