use noa_widget::{
    MAX_DATA_BYTES, MAX_MANIFEST_BYTES, Manifest, content_digest, resolve, schema_document,
};
use serde_json::{Value, json};

fn document() -> Value {
    serde_json::from_slice(include_bytes!(
        "../../../.noa-widgets/test-summary/widget.json"
    ))
    .unwrap()
}

fn parse(value: &Value) -> Result<Manifest, noa_widget::WidgetError> {
    Manifest::from_slice(&serde_json::to_vec(value).unwrap())
}

#[test]
fn checked_in_example_accepts_results_and_validates_actions() {
    let manifest = parse(&document()).unwrap();
    let mut data = manifest.initial_data.clone();
    data["summary"] = json!("3 passed, 1 failed");
    data["passed"] = json!(3);
    data["failed"] = json!(1);
    data["failures"] = json!([{"id":"cursor","name":"cursor wraps"}]);
    manifest.validate_data(&data).unwrap();
    manifest
        .validate_action("rerun-failed", &json!({}))
        .unwrap();
    assert!(
        manifest
            .validate_action("rerun-failed", &json!({"command":"rm -rf /"}))
            .is_err()
    );
    assert!(manifest.validate_action("undeclared", &json!({})).is_err());
}

#[test]
fn unknown_grammar_and_future_versions_fail_closed() {
    let mut value = document();
    value["apiVersion"] = json!("noa.widget/v2");
    assert!(
        parse(&value)
            .unwrap_err()
            .to_string()
            .contains("apiVersion")
    );
    for field in ["scripts", "permissions", "$schema"] {
        let mut value = document();
        value[field] = json!({});
        assert!(parse(&value).is_err(), "{field}");
    }
    let mut value = document();
    value["view"]["children"][0]["onclick"] = json!("execute()");
    assert!(parse(&value).is_err());
    let mut value = document();
    value["view"]["children"][0]["type"] = json!("html");
    assert!(parse(&value).is_err());
    for field in ["$ref", "pattern", "oneOf", "default"] {
        let mut value = document();
        value["dataSchema"]["properties"]["summary"][field] = json!("unsupported");
        assert!(parse(&value).is_err(), "{field}");
    }
}

#[test]
fn duplicate_json_keys_and_duplicate_node_ids_are_rejected() {
    let bytes = serde_json::to_string(&document()).unwrap();
    let duplicate = bytes.replacen(
        "\"apiVersion\":",
        "\"apiVersion\":\"noa.widget/v2\",\"apiVersion\":",
        1,
    );
    assert!(
        Manifest::from_slice(duplicate.as_bytes())
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
    let mut value = document();
    value["view"]["children"][0]["id"] = json!("root");
    assert!(
        parse(&value)
            .unwrap_err()
            .to_string()
            .contains("duplicate view")
    );
    let mut value = document();
    let action = value["actions"][0].clone();
    value["actions"].as_array_mut().unwrap().push(action);
    assert!(
        parse(&value)
            .unwrap_err()
            .to_string()
            .contains("duplicate action")
    );
}

#[test]
fn bindings_are_json_pointers_checked_against_the_schema_and_data() {
    let mut value = document();
    value["view"]["children"][0]["text"] = json!({"source":"data","pointer":"/failed"});
    assert!(
        parse(&value)
            .unwrap_err()
            .to_string()
            .contains("incompatible")
    );
    for pointer in ["summary", "/summary~2", "/missing", "#/summary"] {
        let mut value = document();
        value["view"]["children"][0]["text"] = json!({"source":"data","pointer":pointer});
        assert!(parse(&value).is_err(), "{pointer}");
    }
    assert_eq!(
        resolve(
            &json!({"source":"data","pointer":"/a~1b/~0name"}),
            &json!({"a/b":{"~name":"ok"}})
        )
        .unwrap(),
        json!("ok")
    );
    assert_eq!(
        resolve(&json!("literal ${uninterpreted}"), &json!({})).unwrap(),
        json!("literal ${uninterpreted}")
    );
    assert!(
        resolve(
            &json!({"source":"file","pointer":"/etc/passwd"}),
            &json!({})
        )
        .is_err()
    );
    assert!(resolve(&json!({"source":"data","pointer":"/missing"}), &json!({})).is_err());
    assert!(
        resolve(
            &json!({"source":"data","pointer":"","script":"x"}),
            &json!({})
        )
        .is_err()
    );
}

#[test]
fn initial_and_updated_data_must_satisfy_all_constraints() {
    let mut value = document();
    value["initialData"]["failed"] = json!(-1);
    assert!(parse(&value).is_err());
    let manifest = parse(&document()).unwrap();
    for bad in [json!(-1), json!(0.5), json!("0"), Value::Null] {
        let mut data = manifest.initial_data.clone();
        data["failed"] = bad;
        assert!(manifest.validate_data(&data).is_err());
    }
    let mut data = manifest.initial_data.clone();
    data.as_object_mut().unwrap().remove("summary");
    assert!(manifest.validate_data(&data).is_err());
    let mut data = manifest.initial_data.clone();
    data["unknown"] = json!(true);
    assert!(manifest.validate_data(&data).is_err());
    let mut data = manifest.initial_data.clone();
    data["summary"] = json!("あ".repeat(2000));
    manifest.validate_data(&data).unwrap();
    data["summary"] = json!("あ".repeat(2001));
    assert!(manifest.validate_data(&data).is_err());
}

#[test]
fn row_keys_are_required_unique_and_declared_even_for_initially_empty_tables() {
    let mut value = document();
    value["view"]["children"][2]["rowKey"] = json!("/undeclared");
    assert!(parse(&value).is_err());
    let manifest = parse(&document()).unwrap();
    for rows in [
        json!([{"id":"same","name":"a"},{"id":"same","name":"b"}]),
        json!([{"id":"","name":"a"}]),
        json!([{"name":"a"}]),
    ] {
        let mut data = manifest.initial_data.clone();
        data["failures"] = rows;
        assert!(manifest.validate_data(&data).is_err());
    }
}

#[test]
fn button_payload_is_validated_after_resolving_bindings() {
    let mut value = document();
    value["actions"][0]["payloadSchema"] = json!({
        "type":"object","required":["failed"],"additionalProperties":false,
        "properties":{"failed":{"type":"integer","minimum":0}}
    });
    assert!(
        parse(&value).is_err(),
        "implicit empty payload cannot satisfy required fields"
    );
    value["view"]["children"][3]["payload"] = json!({"failed":0});
    parse(&value).unwrap();
    value["view"]["children"][3]["payload"] = json!({"failed":-1});
    assert!(parse(&value).is_err());
}

#[test]
fn progress_list_and_badge_obey_the_same_data_contract() {
    let mut value = document();
    value["view"] = json!({"type":"section","id":"root","title":"Results","children":[
        {"type":"progress","id":"progress","value":{"source":"data","pointer":"/passed"},"max":10,"label":"Progress"},
        {"type":"badge","id":"badge","text":"Running","tone":"muted"},
        {"type":"list","id":"list","items":{"source":"data","pointer":"/failures"},"itemKey":"/id","textPointer":"/name"}
    ]});
    let manifest = parse(&value).unwrap();
    let mut data = manifest.initial_data.clone();
    data["passed"] = json!(11);
    assert!(manifest.validate_data(&data).is_err());
    data["passed"] = json!(10);
    data["failures"] = json!([{"id":"one","name":"One"}]);
    manifest.validate_data(&data).unwrap();
    value["view"]["children"][0]["max"] = json!(0);
    assert!(parse(&value).is_err());
}

#[test]
fn resource_caps_apply_to_documents_data_views_and_rows() {
    assert!(Manifest::from_slice(&vec![b' '; MAX_MANIFEST_BYTES + 1]).is_err());
    let mut value = document();
    value["view"]["children"] = Value::Array(
        (0..512)
            .map(|i| json!({"type":"text","id":format!("node-{i}"),"text":"ok"}))
            .collect(),
    );
    assert!(parse(&value).is_err());
    let mut value = document();
    let mut node = json!({"type":"text","id":"text","text":"x"});
    for i in 0..33 {
        node = json!({"type":"column","id":format!("depth-{i}"),"children":[node]});
    }
    value["view"] = node;
    assert!(parse(&value).is_err());
    let manifest = parse(&document()).unwrap();
    assert!(
        manifest
            .validate_data(&json!({"summary":"x".repeat(MAX_DATA_BYTES)}))
            .is_err()
    );
    let mut value = document();
    value["dataSchema"]["properties"]["failures"]
        .as_object_mut()
        .unwrap()
        .remove("maxItems");
    let manifest = parse(&value).unwrap();
    let mut data = manifest.initial_data.clone();
    data["failures"] = Value::Array(
        (0..1001)
            .map(|i| json!({"id":format!("id-{i}"),"name":"x"}))
            .collect(),
    );
    assert!(
        manifest
            .validate_data(&data)
            .unwrap_err()
            .to_string()
            .contains("1000")
    );
}

#[test]
fn schema_subset_checks_types_ranges_enums_and_unknown_properties() {
    let mut value = document();
    value["dataSchema"]["properties"]["summary"]["enum"] = json!(["a", "a"]);
    assert!(parse(&value).is_err());
    let mut value = document();
    value["dataSchema"]["properties"]["passed"]["maximum"] = json!(-1);
    assert!(parse(&value).is_err());
    let mut value = document();
    value["dataSchema"]["required"] = json!(["missing"]);
    assert!(parse(&value).is_err());
    let schema = schema_document();
    assert_eq!(schema["properties"]["apiVersion"]["const"], "noa.widget/v1");
    assert_eq!(schema["additionalProperties"], false);
}

#[test]
fn integer_bounds_do_not_round_at_the_f64_precision_limit() {
    let mut value = document();
    value["dataSchema"]["properties"]["passed"]["maximum"] = json!(9_007_199_254_740_992_u64);
    let manifest = parse(&value).unwrap();
    let mut data = manifest.initial_data.clone();
    data["passed"] = json!(9_007_199_254_740_993_u64);
    assert!(manifest.validate_data(&data).is_err());
    data["passed"] = json!(9_007_199_254_740_992_u64);
    manifest.validate_data(&data).unwrap();
    value["dataSchema"]["properties"]["passed"]["enum"] = json!([0, 0.0]);
    assert!(parse(&value).is_err());
}

#[test]
fn repeated_bindings_cannot_multiply_a_small_data_set_into_an_unbounded_view() {
    let mut value = document();
    value["dataSchema"]["properties"]["failures"]["maxItems"] = json!(1000);
    let table = value["view"]["children"][2].clone();
    value["view"]["children"] = Value::Array(
        (0..5)
            .map(|i| {
                let mut node = table.clone();
                node["id"] = json!(format!("table-{i}"));
                node
            })
            .collect(),
    );
    let manifest = parse(&value).unwrap();
    let mut data = manifest.initial_data.clone();
    data["failures"] = Value::Array(
        (0..1000)
            .map(|i| json!({"id":format!("id-{i}"),"name":"x"}))
            .collect(),
    );
    assert!(
        manifest
            .validate_data(&data)
            .unwrap_err()
            .to_string()
            .contains("expanded view")
    );
}

#[test]
fn digests_are_content_based_and_sha256_is_not_a_permission() {
    let compact = serde_json::to_vec(&document()).unwrap();
    let pretty = serde_json::to_vec_pretty(&document()).unwrap();
    let original = Manifest::from_slice(&compact).unwrap();
    assert_eq!(
        original.digest(),
        Manifest::from_slice(&pretty).unwrap().digest()
    );
    let mut changed = document();
    changed["title"] = json!("Changed");
    assert_ne!(original.digest(), parse(&changed).unwrap().digest());
    assert_eq!(
        content_digest(b"abc"),
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn repeated_text_bindings_have_a_shared_byte_budget() {
    let mut value = document();
    value["dataSchema"]["properties"]["summary"]["maxLength"] = json!(65536);
    value["view"]["children"] = Value::Array(
        (0..9)
            .map(|i| {
                json!({
                    "type":"text","id":format!("text-{i}"),
                    "text":{"source":"data","pointer":"/summary"}
                })
            })
            .collect(),
    );
    let manifest = parse(&value).unwrap();
    let mut data = manifest.initial_data.clone();
    data["summary"] = json!("x".repeat(64 * 1024));
    assert!(
        manifest
            .validate_data(&data)
            .unwrap_err()
            .to_string()
            .contains("expanded view text")
    );
}
