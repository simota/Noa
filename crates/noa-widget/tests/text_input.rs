//! `text-input` nodes and button `inputs`: an additive part of v1 whose text
//! only ever reaches the agent as a string inside an agent-event payload.

use noa_widget::{MAX_ACTION_BYTES, Manifest, schema_document};
use serde_json::{Value, json};

fn document() -> Value {
    serde_json::from_slice(include_bytes!(
        "../../../.noa-widgets/agent-request/widget.json"
    ))
    .unwrap()
}

fn parse(value: &Value) -> Result<Manifest, noa_widget::WidgetError> {
    Manifest::from_slice(&serde_json::to_vec(value).unwrap())
}

fn error(value: &Value) -> String {
    parse(value).unwrap_err().to_string()
}

/// Index of a direct child of the example's root column.
fn child(value: &mut Value, id: &str) -> usize {
    value["view"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .position(|node| node["id"] == id)
        .unwrap()
}

fn node<'a>(value: &'a mut Value, id: &str) -> &'a mut Value {
    let index = child(value, id);
    &mut value["view"]["children"][index]
}

#[test]
fn existing_v1_manifests_keep_validating_with_the_same_digest() {
    let bytes = include_bytes!("../../../.noa-widgets/test-summary/widget.json");
    let manifest = Manifest::from_slice(bytes).unwrap();
    assert_eq!(
        manifest.digest(),
        "sha256:5d76b30d5e960bbab1f14c91408635243a0c522a570b7c1ad59d5c35a2614ddb"
    );
}

#[test]
fn checked_in_example_validates_and_its_final_payload_is_checked() {
    let manifest = parse(&document()).unwrap();
    // Input properties are required by the action, but the host supplies
    // them at click time, so the button's literal part validates alone.
    manifest
        .validate_action(
            "request",
            &json!({"kind":"request","subject":"調べて","details":"行1\n行2"}),
        )
        .unwrap();
    for payload in [
        json!({"kind":"request","details":""}),
        json!({"kind":"request","subject":"","details":""}),
        json!({"kind":"request","subject":"x".repeat(121),"details":""}),
        json!({"kind":"request","subject":"ok","details":"x".repeat(4001)}),
        json!({"kind":"request","subject":"ok","details":"","extra":"x"}),
    ] {
        assert!(manifest.validate_action("request", &payload).is_err());
    }
}

#[test]
fn literal_payload_part_is_still_validated() {
    let mut value = document();
    node(&mut value, "send")["payload"] = json!({"kind":"other"});
    assert!(error(&value).contains("enum"));
    let mut value = document();
    node(&mut value, "send")
        .as_object_mut()
        .unwrap()
        .remove("payload");
    assert!(error(&value).contains("kind"), "kind stays required");
}

#[test]
fn inputs_must_name_existing_text_input_nodes() {
    let mut value = document();
    node(&mut value, "send")["inputs"]["subject"] = json!("missing");
    let message = error(&value);
    assert!(
        message.contains("send") && message.contains("missing"),
        "{message}"
    );
    let mut value = document();
    node(&mut value, "send")["inputs"]["subject"] = json!("status");
    let message = error(&value);
    assert!(
        message.contains("send") && message.contains("status") && message.contains("text-input"),
        "{message}"
    );
}

#[test]
fn input_properties_must_be_declared_strings_of_the_action() {
    let mut value = document();
    node(&mut value, "send")["inputs"]["note"] = json!("details");
    let message = error(&value);
    assert!(
        message.contains("send") && message.contains("note") && message.contains("string"),
        "{message}"
    );
    let mut value = document();
    value["actions"][0]["payloadSchema"]["properties"]["details"] = json!({"type":"integer"});
    let message = error(&value);
    assert!(
        message.contains("details") && message.contains("string"),
        "{message}"
    );
}

#[test]
fn input_properties_cannot_also_come_from_the_payload() {
    let mut value = document();
    node(&mut value, "send")["payload"] = json!({"kind":"request","subject":"fixed"});
    let message = error(&value);
    assert!(
        message.contains("send") && message.contains("subject") && message.contains("payload"),
        "{message}"
    );
    // A bound payload whose schema may carry the property collides too.
    let mut value = document();
    value["dataSchema"]["properties"]["request"] = json!({
        "type":"object","additionalProperties":false,"required":["kind"],
        "properties":{"kind":{"type":"string","enum":["request"]},"subject":{"type":"string"}}
    });
    value["dataSchema"]["required"]
        .as_array_mut()
        .unwrap()
        .push(json!("request"));
    value["initialData"]["request"] = json!({"kind":"request"});
    node(&mut value, "send")["payload"] = json!({"source":"data","pointer":"/request"});
    assert!(error(&value).contains("subject"));
    value["dataSchema"]["properties"]["request"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("subject");
    parse(&value).unwrap();
}

#[test]
fn input_max_length_cannot_exceed_the_schema_max_length() {
    let mut value = document();
    node(&mut value, "subject")["maxLength"] = json!(121);
    let message = error(&value);
    assert!(
        message.contains("subject") && message.contains("121") && message.contains("120"),
        "{message}"
    );
    node(&mut value, "subject")["maxLength"] = json!(120);
    parse(&value).unwrap();
    // Either limit alone is enough.
    value["actions"][0]["payloadSchema"]["properties"]["subject"] =
        json!({"type":"string","minLength":1});
    node(&mut value, "subject")["maxLength"] = json!(5000);
    parse(&value).unwrap();
    node(&mut value, "subject")
        .as_object_mut()
        .unwrap()
        .remove("maxLength");
    parse(&value).unwrap();
}

#[test]
fn submit_names_a_button_that_sends_the_input() {
    let mut value = document();
    node(&mut value, "subject")["submit"] = json!("nowhere");
    let message = error(&value);
    assert!(
        message.contains("subject") && message.contains("nowhere"),
        "{message}"
    );
    let mut value = document();
    node(&mut value, "subject")["submit"] = json!("status");
    let message = error(&value);
    assert!(
        message.contains("subject") && message.contains("button"),
        "{message}"
    );
    let mut value = document();
    node(&mut value, "send")["inputs"]
        .as_object_mut()
        .unwrap()
        .remove("subject");
    value["actions"][0]["payloadSchema"]["required"] = json!(["kind", "details"]);
    let message = error(&value);
    assert!(
        message.contains("subject") && message.contains("send") && message.contains("inputs"),
        "{message}"
    );
    node(&mut value, "subject")
        .as_object_mut()
        .unwrap()
        .remove("submit");
    parse(&value).unwrap();
}

#[test]
fn text_input_and_button_fields_have_strict_shapes() {
    let cases: [(&str, &str, Value); 11] = [
        ("subject", "onSubmit", json!("x")),
        ("subject", "value", json!("prefilled")),
        ("subject", "multiline", json!("yes")),
        ("subject", "maxLength", json!(0)),
        ("subject", "maxLength", json!(MAX_ACTION_BYTES + 1)),
        ("subject", "maxLength", json!("10")),
        (
            "subject",
            "placeholder",
            json!({"source":"data","pointer":"/status"}),
        ),
        ("subject", "placeholder", json!("x".repeat(201))),
        ("send", "clearInputs", json!("true")),
        ("send", "inputs", json!({})),
        ("send", "inputs", json!({"subject":"Not An Id"})),
    ];
    for (id, field, bad) in cases {
        let mut value = document();
        node(&mut value, id)[field] = bad;
        assert!(parse(&value).is_err(), "{id}.{field}");
    }
    let mut value = document();
    let many: serde_json::Map<String, Value> = (0..17)
        .map(|i| (format!("p{i}"), json!("details")))
        .collect();
    node(&mut value, "send")["inputs"] = Value::Object(many);
    assert!(error(&value).contains("1..=16"));
    // A minimal text-input needs only its id and type.
    let mut value = document();
    *node(&mut value, "subject") = json!({"type":"text-input","id":"subject"});
    parse(&value).unwrap();
}

#[test]
fn text_input_label_is_a_checked_binding() {
    let mut value = document();
    node(&mut value, "subject")["label"] = json!({"source":"data","pointer":"/task"});
    parse(&value).unwrap();
    node(&mut value, "subject")["label"] = json!({"source":"data","pointer":"/missing"});
    assert!(parse(&value).is_err());
    node(&mut value, "subject")["label"] = json!(3);
    assert!(parse(&value).is_err());
}

#[test]
fn editor_schema_describes_the_new_component_and_button_fields() {
    let schema = schema_document();
    let nodes = schema["$defs"]["node"]["oneOf"].as_array().unwrap();
    let text_input = nodes
        .iter()
        .find(|n| n["properties"]["type"]["enum"] == json!(["text-input"]))
        .unwrap();
    assert_eq!(text_input["required"], json!(["type", "id"]));
    for key in ["label", "placeholder", "multiline", "maxLength", "submit"] {
        assert!(text_input["properties"].get(key).is_some(), "{key}");
    }
    let button = nodes
        .iter()
        .find(|n| n["properties"]["type"]["enum"] == json!(["button"]))
        .unwrap();
    assert_eq!(button["properties"]["clearInputs"]["type"], "boolean");
    assert_eq!(
        button["properties"]["inputs"]["additionalProperties"]["$ref"],
        "#/$defs/id"
    );
}

#[test]
fn a_whole_payload_enum_is_checked_on_the_final_payload_not_the_partial_one() {
    let mut value = document();
    value["actions"][0]["payloadSchema"]["enum"] =
        json!([{"kind":"request","subject":"ok","details":""}]);
    let manifest = parse(&value).unwrap();
    manifest
        .validate_action(
            "request",
            &json!({"kind":"request","subject":"ok","details":""}),
        )
        .unwrap();
    assert!(
        manifest
            .validate_action(
                "request",
                &json!({"kind":"request","subject":"other","details":""}),
            )
            .is_err()
    );
}

/// A bound payload whose schema allows more properties could carry an input
/// property in data that the typed text would silently overwrite.
#[test]
fn a_bound_payload_must_close_its_properties_so_inputs_cannot_overwrite_data() {
    for request in [
        json!({"type":"object","properties":{"kind":{"type":"string"}}}),
        json!({"type":"object","properties":{"kind":{"type":"string"}},"additionalProperties":true}),
        json!({"type":"object"}),
    ] {
        let mut value = document();
        value["dataSchema"]["properties"]["request"] = request.clone();
        value["initialData"]["request"] = json!({"kind":"request"});
        node(&mut value, "send")["payload"] = json!({"source":"data","pointer":"/request"});
        let message = error(&value);
        assert!(
            message.contains("additionalProperties"),
            "{request}: {message}"
        );
    }
    let mut value = document();
    value["dataSchema"]["properties"]["request"] = json!({
        "type":"object","required":["kind"],"additionalProperties":false,
        "properties":{"kind":{"type":"string","enum":["request"]}}
    });
    value["initialData"]["request"] = json!({"kind":"request"});
    node(&mut value, "send")["payload"] = json!({"source":"data","pointer":"/request"});
    parse(&value).unwrap();
}
