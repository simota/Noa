use std::collections::{BTreeMap, BTreeSet};

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{
    API_VERSION, MAX_ACTION_BYTES, MAX_DATA_BYTES, MAX_DEPTH, MAX_MANIFEST_BYTES,
    MAX_RENDERED_ITEMS, MAX_RENDERED_TEXT_BYTES, MAX_TABLE_ROWS, MAX_VIEW_NODES, WidgetError,
    check_id,
};

const MAX_JSON_NODES: usize = 20_000;
const MAX_STRING_BYTES: usize = 64 * 1024;
const MAX_COLUMNS: usize = 32;
/// A button sends at most this many text inputs with its event.
const MAX_BUTTON_INPUTS: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub api_version: String,
    pub id: String,
    pub version: String,
    pub title: String,
    pub description: String,
    pub data_schema: Value,
    pub initial_data: Value,
    pub view: Value,
    #[serde(default)]
    pub actions: Vec<Action>,
    /// Images packaged beside the manifest. Absent (not empty) for packages
    /// without images, so their canonical form and digest are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<BTreeMap<String, AssetRef>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetRef {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub kind: String,
    pub payload_schema: Value,
}

impl Manifest {
    pub fn from_slice(bytes: &[u8]) -> Result<Self, WidgetError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return fail("manifest exceeds 64 KiB");
        }
        // Value's usual deserializer silently keeps the last duplicate key.
        // Reject duplicates before interpreting permissions, actions or data.
        let StrictValue(value) = serde_json::from_slice::<StrictValue>(bytes)?;
        check_json(&value, MAX_MANIFEST_BYTES, "manifest")?;
        let manifest: Self = serde_json::from_value(value)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), WidgetError> {
        if self.api_version != API_VERSION {
            return fail(format!("unsupported apiVersion {:?}", self.api_version));
        }
        check_id(&self.id)?;
        if !valid_version(&self.version) {
            return fail("version must be a stable major.minor.patch version");
        }
        bounded_text(&self.title, 200, "title", false)?;
        bounded_text(&self.description, 2000, "description", false)?;
        validate_schema(&self.data_schema, "dataSchema", 0)?;
        if self.data_schema["type"] != "object" {
            return fail("dataSchema.type must be object");
        }
        if self.actions.len() > 64 {
            return fail("at most 64 actions are supported");
        }
        let mut action_ids = BTreeSet::new();
        for action in &self.actions {
            check_id(&action.id)?;
            if !action_ids.insert(action.id.as_str()) {
                return fail(format!("duplicate action id {:?}", action.id));
            }
            if action.kind != "agent-event" {
                return fail(format!("unsupported action kind {:?}", action.kind));
            }
            validate_schema(&action.payload_schema, &format!("action {}", action.id), 0)?;
            if action.payload_schema["type"] != "object" {
                return fail(format!(
                    "action {} payloadSchema.type must be object",
                    action.id
                ));
            }
        }
        crate::assets::validate_declarations(self.assets.as_ref())?;
        validate_node(
            &self.view,
            &self.data_schema,
            &action_ids,
            &mut BTreeSet::new(),
            &mut 0,
            0,
        )?;
        validate_text_inputs(&self.view, &self.data_schema, &self.actions)?;
        validate_image_sources(&self.view, &self.data_schema, &self.asset_ids())?;
        self.validate_data(&self.initial_data)
    }

    /// Validates both the data contract and every rendered reference. A data
    /// update is rejected before replacing the previous complete snapshot.
    pub fn validate_data(&self, data: &Value) -> Result<(), WidgetError> {
        check_json(data, MAX_DATA_BYTES, "data")?;
        validate_value(&self.data_schema, data, "data", 0)?;
        let mut budget = RenderBudget {
            items: MAX_RENDERED_ITEMS,
            text_bytes: MAX_RENDERED_TEXT_BYTES,
        };
        budget.text(self.title.len() + self.description.len())?;
        validate_rendered(&self.view, data, self, &mut budget)
    }

    pub fn validate_action(&self, action_id: &str, payload: &Value) -> Result<(), WidgetError> {
        let action = self
            .actions
            .iter()
            .find(|a| a.id == action_id)
            .ok_or_else(|| WidgetError::new(format!("unknown action {action_id:?}")))?;
        check_json(payload, MAX_ACTION_BYTES, "action payload")?;
        validate_value(&action.payload_schema, payload, "action payload", 0)
    }

    /// The literal or bound part of a payload whose text-input properties the
    /// host fills in at click time: those properties may be absent here, and
    /// the complete payload is validated again when the event is submitted.
    fn validate_partial_action(
        &self,
        action_id: &str,
        payload: &Value,
        inputs: &Map<String, Value>,
    ) -> Result<(), WidgetError> {
        let action = self
            .actions
            .iter()
            .find(|a| a.id == action_id)
            .ok_or_else(|| WidgetError::new(format!("unknown action {action_id:?}")))?;
        let mut schema = action.payload_schema.clone();
        if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
            required.retain(|key| key.as_str().is_none_or(|key| !inputs.contains_key(key)));
        }
        // An enum of whole payloads names complete payloads, input text
        // included; only the final payload can be compared with it.
        if let Some(schema) = schema.as_object_mut() {
            schema.remove("enum");
        }
        check_json(payload, MAX_ACTION_BYTES, "action payload")?;
        validate_value(&schema, payload, "action payload", 0)
    }

    /// Declared asset ids, empty when the manifest has no `assets`.
    pub fn asset_ids(&self) -> BTreeSet<&str> {
        self.assets
            .iter()
            .flat_map(|assets| assets.keys().map(String::as_str))
            .collect()
    }

    /// Canonical object key ordering makes whitespace and key ordering
    /// irrelevant. This digest identifies content, never grants authority.
    /// It covers the manifest only: a package with `assets` is identified by
    /// [`crate::assets::definition_digest`], which also covers image bytes.
    pub fn digest(&self) -> String {
        let value = serde_json::to_value(self).expect("a manifest is JSON serializable");
        let bytes = serde_json::to_vec(&canonical(value)).expect("a manifest is JSON serializable");
        content_digest(&bytes)
    }
}

/// Content identity for manifests and idempotent request payloads. A digest
/// identifies bytes; callers must separately enforce their scope and owner.
pub fn content_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Resolve one literal or {"source":"data","pointer":"/..."} binding.
/// No expressions, interpolation, file paths or code are interpreted.
pub fn resolve(binding: &Value, data: &Value) -> Result<Value, WidgetError> {
    resolve_ref(binding, data).cloned()
}

fn resolve_ref<'a>(binding: &'a Value, data: &'a Value) -> Result<&'a Value, WidgetError> {
    let Some(pointer) = binding_pointer(binding)? else {
        return Ok(binding);
    };
    data.pointer(pointer)
        .ok_or_else(|| WidgetError::new(format!("data binding {pointer:?} does not exist")))
}

fn fail<T>(message: impl Into<String>) -> Result<T, WidgetError> {
    Err(WidgetError::new(message))
}

fn canonical(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<_> = object.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k, canonical(v)))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
        other => other,
    }
}

fn valid_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && (p.len() == 1 || !p.starts_with('0'))
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u32>().is_ok()
        })
}

fn bounded_text(text: &str, limit: usize, name: &str, empty: bool) -> Result<(), WidgetError> {
    if (!empty && text.trim().is_empty()) || text.chars().count() > limit || text.contains('\0') {
        return fail(format!(
            "{name} must contain {}..={limit} characters without NUL",
            usize::from(!empty)
        ));
    }
    Ok(())
}

fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>, WidgetError> {
    value
        .as_object()
        .ok_or_else(|| WidgetError::new(format!("{name} must be an object")))
}

fn string<'a>(value: &'a Value, name: &str) -> Result<&'a str, WidgetError> {
    value
        .as_str()
        .ok_or_else(|| WidgetError::new(format!("{name} must be a string")))
}

fn field<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a Value, WidgetError> {
    map.get(key)
        .ok_or_else(|| WidgetError::new(format!("missing field {key:?}")))
}

fn fields(map: &Map<String, Value>, allowed: &[&str], name: &str) -> Result<(), WidgetError> {
    if let Some(key) = map.keys().find(|key| !allowed.contains(&key.as_str())) {
        return fail(format!("unsupported {name} field {key:?}"));
    }
    Ok(())
}

fn check_json(value: &Value, bytes: usize, name: &str) -> Result<(), WidgetError> {
    fn visit(value: &Value, depth: usize, count: &mut usize) -> Result<(), WidgetError> {
        *count += 1;
        if depth > MAX_DEPTH || *count > MAX_JSON_NODES {
            return fail("JSON exceeds the depth or node limit");
        }
        match value {
            Value::String(s) if s.len() > MAX_STRING_BYTES || s.contains('\0') => {
                return fail("JSON string exceeds 64 KiB or contains NUL");
            }
            Value::Array(values) => {
                for value in values {
                    visit(value, depth + 1, count)?;
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    if key.len() > 1024 || key.contains('\0') {
                        return fail("JSON object key is too long or contains NUL");
                    }
                    visit(value, depth + 1, count)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(value, 0, &mut 0)?;
    if serde_json::to_vec(value)?.len() > bytes {
        return fail(format!("{name} exceeds {bytes} bytes"));
    }
    Ok(())
}

fn integer(value: &Value) -> bool {
    value.as_i64().is_some()
        || value.as_u64().is_some()
        || value
            .as_f64()
            .is_some_and(|n| n.is_finite() && n.fract() == 0.0)
}

fn has_type(value: &Value, kind: &str) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => integer(value),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    }
}

fn number_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    fn exact_integer(value: &Value) -> Option<i128> {
        if let Some(n) = value.as_i64() {
            return Some(i128::from(n));
        }
        if let Some(n) = value.as_u64() {
            return Some(i128::from(n));
        }
        let n = value.as_f64()?;
        (n.fract() == 0.0 && n >= i128::MIN as f64 && n < i128::MAX as f64).then_some(n as i128)
    }
    match (exact_integer(a), exact_integer(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        _ => a
            .as_f64()
            .expect("validated number")
            .partial_cmp(&b.as_f64().expect("validated number"))
            .expect("JSON numbers are finite"),
    }
}

fn json_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(_), Value::Number(_)) => number_cmp(a, b).is_eq(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| json_equal(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| json_equal(a, b)))
        }
        _ => a == b,
    }
}

fn validate_schema(schema: &Value, name: &str, depth: usize) -> Result<(), WidgetError> {
    if depth > MAX_DEPTH {
        return fail("schema exceeds the depth limit");
    }
    let schema = object(schema, name)?;
    let kind = string(field(schema, "type")?, "schema type")?;
    let mut allowed = vec!["type", "enum"];
    match kind {
        "object" => allowed.extend(["properties", "required", "additionalProperties"]),
        "array" => allowed.extend(["items", "minItems", "maxItems"]),
        "string" => allowed.extend(["minLength", "maxLength"]),
        "integer" | "number" => allowed.extend(["minimum", "maximum"]),
        "boolean" | "null" => {}
        _ => return fail(format!("unsupported schema type {kind:?}")),
    }
    fields(schema, &allowed, name)?;
    if kind == "object" {
        let properties = schema
            .get("properties")
            .map(|v| object(v, "properties"))
            .transpose()?;
        if let Some(properties) = properties {
            if properties.len() > 256 {
                return fail("schema has more than 256 properties");
            }
            for (key, sub) in properties {
                validate_schema(sub, &format!("{name}.properties.{key}"), depth + 1)?;
            }
        }
        if let Some(required) = schema.get("required") {
            let required = required
                .as_array()
                .ok_or_else(|| WidgetError::new("required must be an array"))?;
            let mut seen = BTreeSet::new();
            for key in required {
                let key = string(key, "required entry")?;
                if !seen.insert(key) || !properties.is_some_and(|p| p.contains_key(key)) {
                    return fail(format!(
                        "required property {key:?} is duplicated or undeclared"
                    ));
                }
            }
        }
        if schema
            .get("additionalProperties")
            .is_some_and(|v| !v.is_boolean())
        {
            return fail("additionalProperties must be a boolean");
        }
    }
    if kind == "array" {
        validate_schema(field(schema, "items")?, &format!("{name}.items"), depth + 1)?;
        bounds(schema, "minItems", "maxItems", true)?;
    }
    if kind == "string" {
        bounds(schema, "minLength", "maxLength", true)?;
    }
    if kind == "number" || kind == "integer" {
        bounds(schema, "minimum", "maximum", false)?;
    }
    if let Some(values) = schema.get("enum") {
        let values = values
            .as_array()
            .ok_or_else(|| WidgetError::new("enum must be an array"))?;
        if values.is_empty() || values.len() > 128 {
            return fail("enum must have 1..=128 values");
        }
        for (index, value) in values.iter().enumerate() {
            if !has_type(value, kind) || values[..index].iter().any(|v| json_equal(v, value)) {
                return fail("enum values must be unique and match their declared type");
            }
        }
    }
    Ok(())
}

fn bounds(
    schema: &Map<String, Value>,
    min: &str,
    max: &str,
    unsigned: bool,
) -> Result<(), WidgetError> {
    for key in [min, max] {
        if let Some(value) = schema.get(key)
            && ((unsigned && value.as_u64().is_none()) || (!unsigned && value.as_f64().is_none()))
        {
            return fail(format!(
                "{key} must be a {}number",
                if unsigned { "nonnegative integer " } else { "" }
            ));
        }
    }
    if let (Some(a), Some(b)) = (schema.get(min), schema.get(max))
        && number_cmp(a, b).is_gt()
    {
        return fail(format!("{min} must not exceed {max}"));
    }
    Ok(())
}

fn validate_value(
    schema: &Value,
    value: &Value,
    at: &str,
    depth: usize,
) -> Result<(), WidgetError> {
    if depth > MAX_DEPTH {
        return fail("data exceeds the depth limit");
    }
    let kind = string(&schema["type"], "schema type")?;
    if !has_type(value, kind) {
        return fail(format!("{at} must be {kind}"));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array)
        && !values.iter().any(|v| json_equal(v, value))
    {
        return fail(format!("{at} is not an allowed enum value"));
    }
    match value {
        Value::Object(values) => {
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for key in required {
                    let key = string(key, "required entry")?;
                    if !values.contains_key(key) {
                        return fail(format!("{at} is missing required property {key:?}"));
                    }
                }
            }
            for (key, value) in values {
                if let Some(sub) = schema.get("properties").and_then(|p| p.get(key)) {
                    validate_value(sub, value, &format!("{at}/{key}"), depth + 1)?;
                } else if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                    return fail(format!("{at} has undeclared property {key:?}"));
                }
            }
        }
        Value::Array(values) => {
            length_bounds(schema, values.len(), "minItems", "maxItems", at)?;
            for (index, value) in values.iter().enumerate() {
                validate_value(&schema["items"], value, &format!("{at}/{index}"), depth + 1)?;
            }
        }
        Value::String(text) => {
            length_bounds(schema, text.chars().count(), "minLength", "maxLength", at)?
        }
        Value::Number(_)
            if schema
                .get("minimum")
                .is_some_and(|n| number_cmp(value, n).is_lt())
                || schema
                    .get("maximum")
                    .is_some_and(|n| number_cmp(value, n).is_gt()) =>
        {
            return fail(format!("{at} is outside its numeric bounds"));
        }
        _ => {}
    }
    Ok(())
}

fn length_bounds(
    schema: &Value,
    len: usize,
    min: &str,
    max: &str,
    at: &str,
) -> Result<(), WidgetError> {
    if schema
        .get(min)
        .and_then(Value::as_u64)
        .is_some_and(|n| (len as u64) < n)
        || schema
            .get(max)
            .and_then(Value::as_u64)
            .is_some_and(|n| (len as u64) > n)
    {
        return fail(format!("{at} is outside its {min}/{max} bounds"));
    }
    Ok(())
}

fn pointer_parts(pointer: &str) -> Result<Vec<String>, WidgetError> {
    if pointer.len() > 2048 || (!pointer.is_empty() && !pointer.starts_with('/')) {
        return fail("JSON pointer must be empty or start with / and be at most 2048 bytes");
    }
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    pointer[1..]
        .split('/')
        .map(|part| {
            let mut chars = part.chars();
            let mut decoded = String::new();
            while let Some(c) = chars.next() {
                if c == '~' {
                    decoded.push(match chars.next() {
                        Some('0') => '~',
                        Some('1') => '/',
                        _ => return fail("JSON pointer has an invalid ~ escape"),
                    });
                } else {
                    decoded.push(c);
                }
            }
            Ok(decoded)
        })
        .collect()
}

fn binding_pointer(value: &Value) -> Result<Option<&str>, WidgetError> {
    let Some(map) = value.as_object() else {
        return Ok(None);
    };
    if !map.contains_key("source") && !map.contains_key("pointer") {
        return Ok(None);
    }
    fields(map, &["source", "pointer"], "binding")?;
    if field(map, "source")? != "data" {
        return fail("binding source must be data");
    }
    let pointer = string(field(map, "pointer")?, "binding pointer")?;
    pointer_parts(pointer)?;
    Ok(Some(pointer))
}

fn schema_pointer<'a>(schema: &'a Value, pointer: &str) -> Result<&'a Value, WidgetError> {
    let mut at = schema;
    for part in pointer_parts(pointer)? {
        at = match at["type"].as_str() {
            Some("object") => at.get("properties").and_then(|p| p.get(&part)),
            Some("array")
                if part == "0" || (!part.starts_with('0') && part.parse::<usize>().is_ok()) =>
            {
                at.get("items")
            }
            _ => None,
        }
        .ok_or_else(|| {
            WidgetError::new(format!(
                "pointer {pointer:?} is not declared by the data schema"
            ))
        })?;
    }
    Ok(at)
}

fn compatible(kind: &str, expected: &[&str]) -> bool {
    expected.contains(&kind) || (kind == "integer" && expected.contains(&"number"))
}

fn expression<'a>(
    value: &Value,
    schema: &'a Value,
    expected: &[&str],
    name: &str,
) -> Result<Option<&'a Value>, WidgetError> {
    if let Some(pointer) = binding_pointer(value)? {
        let target = schema_pointer(schema, pointer)?;
        if !compatible(string(&target["type"], "schema type")?, expected) {
            return fail(format!(
                "{name} binding {pointer:?} has an incompatible schema type"
            ));
        }
        Ok(Some(target))
    } else if expected.iter().any(|kind| has_type(value, kind)) {
        Ok(None)
    } else {
        fail(format!("{name} literal has an incompatible type"))
    }
}

fn validate_node(
    node: &Value,
    schema: &Value,
    actions: &BTreeSet<&str>,
    ids: &mut BTreeSet<String>,
    count: &mut usize,
    depth: usize,
) -> Result<(), WidgetError> {
    *count += 1;
    if *count > MAX_VIEW_NODES || depth > MAX_DEPTH {
        return fail("view exceeds the node or depth limit");
    }
    let node = object(node, "view node")?;
    let kind = string(field(node, "type")?, "node type")?;
    let id = string(field(node, "id")?, "node id")?;
    check_id(id)?;
    if !ids.insert(id.to_owned()) {
        return fail(format!("duplicate view node id {id:?}"));
    }
    let extra: &[&str] = match kind {
        "column" | "row" => &["children", "gap"],
        "section" => &["title", "children", "gap"],
        "text" => &["text", "tone"],
        "metric" => &["label", "value"],
        "badge" => &["text", "tone"],
        "progress" => &["value", "max", "label"],
        "table" => &["rows", "rowKey", "columns"],
        "list" => &["items", "itemKey", "textPointer"],
        "button" => &["label", "action", "payload", "inputs", "clearInputs"],
        "text-input" => &["label", "placeholder", "multiline", "maxLength", "submit"],
        "image" => &["src", "alt", "size", "width", "height", "shape"],
        _ => return fail(format!("unsupported component {kind:?}")),
    };
    let allowed: Vec<_> = ["type", "id"]
        .into_iter()
        .chain(extra.iter().copied())
        .collect();
    fields(node, &allowed, "view node")?;
    for (key, choices) in [
        ("gap", &["none", "xs", "sm", "md", "lg"][..]),
        (
            "tone",
            &["default", "muted", "success", "warning", "error"][..],
        ),
    ] {
        if let Some(value) = node.get(key)
            && !choices.contains(&string(value, key)?)
        {
            return fail(format!("unsupported {key} value"));
        }
    }
    match kind {
        "column" | "row" | "section" => {
            if kind == "section" {
                expression(field(node, "title")?, schema, &["string"], "section title")?;
            }
            let children = field(node, "children")?
                .as_array()
                .ok_or_else(|| WidgetError::new("children must be an array"))?;
            for child in children {
                validate_node(child, schema, actions, ids, count, depth + 1)?;
            }
        }
        "text" | "badge" => {
            expression(field(node, "text")?, schema, &["string"], "text")?;
        }
        "metric" => {
            expression(field(node, "label")?, schema, &["string"], "metric label")?;
            expression(
                field(node, "value")?,
                schema,
                &["string", "number"],
                "metric value",
            )?;
        }
        "progress" => {
            expression(field(node, "value")?, schema, &["number"], "progress value")?;
            if let Some(max) = node.get("max") {
                expression(max, schema, &["number"], "progress max")?;
            }
            if let Some(label) = node.get("label") {
                expression(label, schema, &["string"], "progress label")?;
            }
        }
        "table" | "list" => {
            let (rows_key, id_key) = if kind == "table" {
                ("rows", "rowKey")
            } else {
                ("items", "itemKey")
            };
            let rows_schema = expression(field(node, rows_key)?, schema, &["array"], rows_key)?;
            let key_pointer = string(field(node, id_key)?, id_key)?;
            pointer_parts(key_pointer)?;
            if let Some(rows_schema) = rows_schema {
                if rows_schema["items"]["type"] != "object" {
                    return fail("table/list entries must be objects");
                }
                let key_schema = schema_pointer(&rows_schema["items"], key_pointer)?;
                if !compatible(
                    string(&key_schema["type"], "key type")?,
                    &["string", "integer"],
                ) {
                    return fail("row/item keys must be strings or integers");
                }
            }
            if kind == "table" {
                let columns = field(node, "columns")?
                    .as_array()
                    .ok_or_else(|| WidgetError::new("columns must be an array"))?;
                if columns.is_empty() || columns.len() > MAX_COLUMNS {
                    return fail("table must have 1..=32 columns");
                }
                let mut pointers = BTreeSet::new();
                for column in columns {
                    let column = object(column, "table column")?;
                    fields(column, &["title", "pointer"], "table column")?;
                    bounded_text(
                        string(field(column, "title")?, "column title")?,
                        200,
                        "column title",
                        false,
                    )?;
                    let pointer = string(field(column, "pointer")?, "column pointer")?;
                    if !pointers.insert(pointer) {
                        return fail("table column pointers must be unique");
                    }
                    validate_cell_pointer(rows_schema, pointer, false)?;
                }
            } else {
                validate_cell_pointer(
                    rows_schema,
                    string(field(node, "textPointer")?, "list textPointer")?,
                    true,
                )?;
            }
        }
        "button" => {
            expression(field(node, "label")?, schema, &["string"], "button label")?;
            let action = string(field(node, "action")?, "button action")?;
            if !actions.contains(action) {
                return fail(format!("button refers to unknown action {action:?}"));
            }
            if let Some(payload) = node.get("payload") {
                expression(payload, schema, &["object"], "button payload")?;
            }
            validate_button_inputs(node, id)?;
        }
        "text-input" => validate_text_input_fields(node, schema, id)?,
        "image" => validate_image(node, schema)?,
        _ => unreachable!(),
    }
    Ok(())
}

/// Field shapes of a `text-input` node. References to other nodes are checked
/// by [`validate_text_inputs`] once every node ID is known.
fn validate_text_input_fields(
    node: &Map<String, Value>,
    schema: &Value,
    id: &str,
) -> Result<(), WidgetError> {
    if let Some(label) = node.get("label") {
        expression(label, schema, &["string"], "text-input label")?;
    }
    if let Some(placeholder) = node.get("placeholder") {
        bounded_text(
            string(placeholder, &format!("text-input {id} placeholder"))?,
            200,
            &format!("text-input {id} placeholder"),
            true,
        )?;
    }
    if node.get("multiline").is_some_and(|v| !v.is_boolean()) {
        return fail(format!("text-input {id} multiline must be a boolean"));
    }
    if let Some(max) = node.get("maxLength")
        && !max
            .as_u64()
            .is_some_and(|n| (1..=MAX_ACTION_BYTES as u64).contains(&n))
    {
        return fail(format!(
            "text-input {id} maxLength must be an integer in 1..={MAX_ACTION_BYTES}"
        ));
    }
    if let Some(submit) = node.get("submit") {
        check_id(string(submit, "text-input submit")?)?;
    }
    Ok(())
}

fn validate_button_inputs(node: &Map<String, Value>, id: &str) -> Result<(), WidgetError> {
    if node.get("clearInputs").is_some_and(|v| !v.is_boolean()) {
        return fail(format!("button {id} clearInputs must be a boolean"));
    }
    let Some(inputs) = node.get("inputs") else {
        return Ok(());
    };
    let inputs = object(inputs, &format!("button {id} inputs"))?;
    if inputs.is_empty() || inputs.len() > MAX_BUTTON_INPUTS {
        return fail(format!(
            "button {id} inputs must map 1..={MAX_BUTTON_INPUTS} payload properties"
        ));
    }
    for (property, input) in inputs {
        if property.is_empty() {
            return fail(format!("button {id} inputs has an empty property name"));
        }
        check_id(string(input, &format!("button {id} inputs.{property}"))?)?;
    }
    Ok(())
}

fn collect_nodes<'a>(node: &'a Value, nodes: &mut Vec<(&'a str, &'a Value)>) {
    if let Some(id) = node["id"].as_str() {
        nodes.push((id, node));
    }
    for child in node["children"].as_array().into_iter().flatten() {
        collect_nodes(child, nodes);
    }
}

/// Cross-node rules for text inputs: a button's `inputs` name text-input
/// nodes and string properties its action declares, and a `submit` names a
/// button that sends the input. Input text is only ever a payload string.
fn validate_text_inputs(
    view: &Value,
    data_schema: &Value,
    actions: &[Action],
) -> Result<(), WidgetError> {
    let mut nodes = Vec::new();
    collect_nodes(view, &mut nodes);
    let find = |id: &str| nodes.iter().find(|(node, _)| *node == id).map(|(_, n)| *n);
    // Buttons first: a wrong `inputs` entry is reported before the
    // `submit` that it consequently fails to satisfy.
    let buttons = nodes.iter().filter(|(_, node)| node["type"] == "button");
    let others = nodes.iter().filter(|(_, node)| node["type"] != "button");
    for (id, node) in buttons.chain(others) {
        match node["type"].as_str() {
            Some("button") => {
                let Some(inputs) = node.get("inputs").and_then(Value::as_object) else {
                    continue;
                };
                let action_id = node["action"].as_str().unwrap_or_default();
                let action = actions
                    .iter()
                    .find(|a| a.id == action_id)
                    .ok_or_else(|| WidgetError::new(format!("unknown action {action_id:?}")))?;
                // A bound payload's schema shows which properties it may carry.
                let payload_properties = match node.get("payload") {
                    None => None,
                    Some(payload) => match binding_pointer(payload)? {
                        Some(pointer) => {
                            // Data must not be able to carry an input property
                            // that the typed text would silently overwrite.
                            let bound = schema_pointer(data_schema, pointer)?;
                            if bound["additionalProperties"] != false {
                                return fail(format!(
                                    "button {id} payload binding {pointer:?} must set additionalProperties to false when the button has inputs"
                                ));
                            }
                            bound.get("properties").and_then(Value::as_object)
                        }
                        None => payload.as_object(),
                    },
                };
                for (property, input_id) in inputs {
                    let input_id = input_id.as_str().unwrap_or_default();
                    let input = find(input_id).ok_or_else(|| {
                        WidgetError::new(format!(
                            "button {id} inputs.{property} refers to unknown node {input_id:?}"
                        ))
                    })?;
                    if input["type"] != "text-input" {
                        return fail(format!(
                            "button {id} inputs.{property} refers to {input_id:?}, which is not a text-input"
                        ));
                    }
                    let target = action
                        .payload_schema
                        .get("properties")
                        .and_then(|p| p.get(property))
                        .filter(|p| p["type"] == "string")
                        .ok_or_else(|| {
                            WidgetError::new(format!(
                                "button {id} inputs.{property}: action {action_id} payloadSchema must declare {property:?} with type string"
                            ))
                        })?;
                    if payload_properties.is_some_and(|p| p.contains_key(property)) {
                        return fail(format!(
                            "button {id} inputs.{property} is also set by the button payload"
                        ));
                    }
                    if let (Some(own), Some(limit)) = (
                        input.get("maxLength").and_then(Value::as_u64),
                        target.get("maxLength").and_then(Value::as_u64),
                    ) && own > limit
                    {
                        return fail(format!(
                            "text-input {input_id} maxLength {own} exceeds action {action_id} {property:?} maxLength {limit}"
                        ));
                    }
                }
            }
            Some("text-input") => {
                let Some(submit) = node.get("submit").and_then(Value::as_str) else {
                    continue;
                };
                let button = find(submit).ok_or_else(|| {
                    WidgetError::new(format!(
                        "text-input {id} submit refers to unknown node {submit:?}"
                    ))
                })?;
                if button["type"] != "button" {
                    return fail(format!(
                        "text-input {id} submit refers to {submit:?}, which is not a button"
                    ));
                }
                let sends = button
                    .get("inputs")
                    .and_then(Value::as_object)
                    .is_some_and(|inputs| inputs.values().any(|v| v == id));
                if !sends {
                    return fail(format!(
                        "text-input {id} submit button {submit:?} does not list {id:?} in its inputs"
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_cell_pointer(
    rows: Option<&Value>,
    pointer: &str,
    text_only: bool,
) -> Result<(), WidgetError> {
    pointer_parts(pointer)?;
    if let Some(rows) = rows {
        let target = schema_pointer(&rows["items"], pointer)?;
        let kinds = if text_only {
            &["string"][..]
        } else {
            &["string", "number", "boolean", "null"][..]
        };
        if !compatible(string(&target["type"], "cell type")?, kinds) {
            return fail("table/list cell has an incompatible type");
        }
    }
    Ok(())
}

fn resolved(
    value: &Value,
    data: &Value,
    expected: &[&str],
    name: &str,
) -> Result<Value, WidgetError> {
    let value = resolve(value, data)?;
    if !expected.iter().any(|kind| has_type(&value, kind)) {
        return fail(format!("resolved {name} has an incompatible type"));
    }
    Ok(value)
}

fn validate_rendered(
    node: &Value,
    data: &Value,
    manifest: &Manifest,
    remaining: &mut RenderBudget,
) -> Result<(), WidgetError> {
    render_budget(remaining, 1)?;
    for key in ["text", "title", "label", "value", "max"] {
        if let Some(value) = node.get(key) {
            remaining.value(resolve_ref(value, data)?)?;
        }
    }
    let kind = string(&node["type"], "node type")?;
    match kind {
        "column" | "row" | "section" => {
            if kind == "section" {
                resolved(&node["title"], data, &["string"], "section title")?;
            }
            if let Some(children) = node["children"].as_array() {
                for child in children {
                    validate_rendered(child, data, manifest, remaining)?;
                }
            }
        }
        "text" | "badge" => {
            resolved(&node["text"], data, &["string"], "text")?;
        }
        "metric" => {
            resolved(&node["label"], data, &["string"], "metric label")?;
            resolved(&node["value"], data, &["string", "number"], "metric value")?;
        }
        "progress" => {
            let value = resolved(&node["value"], data, &["number"], "progress value")?;
            let max = resolved(
                node.get("max").unwrap_or(&json!(1)),
                data,
                &["number"],
                "progress max",
            )?;
            let (value, max) = (value.as_f64().unwrap(), max.as_f64().unwrap());
            if max <= 0.0 || value < 0.0 || value > max {
                return fail("progress requires 0 <= value <= max and max > 0");
            }
            if let Some(label) = node.get("label") {
                resolved(label, data, &["string"], "progress label")?;
            }
        }
        "table" | "list" => {
            let (rows_key, key_key) = if kind == "table" {
                ("rows", "rowKey")
            } else {
                ("items", "itemKey")
            };
            let rows = resolved(&node[rows_key], data, &["array"], rows_key)?;
            let rows = rows.as_array().unwrap();
            if rows.len() > MAX_TABLE_ROWS {
                return fail("table/list exceeds 1000 entries");
            }
            let cells = if kind == "table" {
                if let Some(columns) = node["columns"].as_array() {
                    for column in columns {
                        remaining.value(&column["title"])?;
                    }
                }
                node["columns"].as_array().map_or(0, Vec::len) + 1
            } else {
                2
            };
            render_budget(remaining, rows.len() * cells)?;
            let key_pointer = string(&node[key_key], key_key)?;
            let mut keys = BTreeSet::new();
            for row in rows {
                if !row.is_object() {
                    return fail("table/list entries must be objects");
                }
                let key = row
                    .pointer(key_pointer)
                    .ok_or_else(|| WidgetError::new("table/list key is missing"))?;
                let key = if let Some(s) = key.as_str() {
                    if s.is_empty() || s.len() > 1024 {
                        return fail("table/list string key must have 1..=1024 bytes");
                    }
                    format!("s:{s}")
                } else if key.as_i64().is_some() || key.as_u64().is_some() {
                    format!("i:{key}")
                } else {
                    return fail("table/list key must be a string or a 64-bit integer");
                };
                if !keys.insert(key) {
                    return fail("table/list keys must be unique");
                }
                if kind == "table" {
                    for column in node["columns"]
                        .as_array()
                        .ok_or_else(|| WidgetError::new("columns must be an array"))?
                    {
                        validate_cell(
                            row,
                            string(&column["pointer"], "column pointer")?,
                            false,
                            remaining,
                        )?;
                    }
                } else {
                    validate_cell(
                        row,
                        string(&node["textPointer"], "textPointer")?,
                        true,
                        remaining,
                    )?;
                }
            }
        }
        "button" => {
            resolved(&node["label"], data, &["string"], "button label")?;
            let payload = resolve(node.get("payload").unwrap_or(&json!({})), data)?;
            let action = string(&node["action"], "button action")?;
            match node.get("inputs").and_then(Value::as_object) {
                Some(inputs) => manifest.validate_partial_action(action, &payload, inputs)?,
                None => manifest.validate_action(action, &payload)?,
            }
        }
        "text-input" => {
            if let Some(label) = node.get("label") {
                resolved(label, data, &["string"], "text-input label")?;
            }
            if let Some(placeholder) = node.get("placeholder") {
                remaining.value(placeholder)?;
            }
        }
        "image" => validate_rendered_image(node, data, manifest, remaining)?,
        _ => return fail(format!("unsupported component {kind:?}")),
    }
    Ok(())
}

fn validate_cell(
    row: &Value,
    pointer: &str,
    text_only: bool,
    budget: &mut RenderBudget,
) -> Result<(), WidgetError> {
    let value = row
        .pointer(pointer)
        .ok_or_else(|| WidgetError::new(format!("table/list cell {pointer:?} is missing")))?;
    if value.is_array() || value.is_object() || (text_only && !value.is_string()) {
        return fail("table/list cell has an incompatible type");
    }
    budget.value(value)?;
    Ok(())
}

struct RenderBudget {
    items: usize,
    text_bytes: usize,
}

impl RenderBudget {
    fn text(&mut self, bytes: usize) -> Result<(), WidgetError> {
        self.text_bytes = self
            .text_bytes
            .checked_sub(bytes)
            .ok_or_else(|| WidgetError::new("expanded view text exceeds 512 KiB"))?;
        Ok(())
    }

    fn value(&mut self, value: &Value) -> Result<(), WidgetError> {
        let bytes = match value {
            Value::String(text) => text.len(),
            Value::Number(_) | Value::Bool(_) | Value::Null => value.to_string().len(),
            _ => 0,
        };
        self.text(bytes)
    }
}

fn render_budget(remaining: &mut RenderBudget, count: usize) -> Result<(), WidgetError> {
    remaining.items = remaining
        .items
        .checked_sub(count)
        .ok_or_else(|| WidgetError::new("expanded view exceeds 8192 nodes, rows and cells"))?;
    Ok(())
}

/// Square box sizes for `image.size`, in logical pixels.
pub const IMAGE_SIZES: [(&str, u32); 3] = [("sm", 48), ("md", 96), ("lg", 192)];
/// Largest explicit `image.width` / `image.height`, in logical pixels.
pub const MAX_IMAGE_BOX: u64 = 1024;

/// Image field checks that do not depend on the declared assets; the asset
/// references themselves are checked by `validate_image_sources`.
fn validate_image(node: &Map<String, Value>, schema: &Value) -> Result<(), WidgetError> {
    let id = string(field(node, "id")?, "node id")?;
    if let Some(target) = expression(field(node, "src")?, schema, &["string"], "image src")?
        && target.get("enum").is_none()
    {
        return fail(format!(
            "image {id:?} src binding must point to a string schema whose enum lists asset ids"
        ));
    }
    let alt = field(node, "alt")?;
    expression(alt, schema, &["string"], "image alt")?;
    if let Some(alt) = alt.as_str() {
        bounded_text(alt, 500, "image alt", false)?;
    }
    if let Some(size) = node.get("size") {
        let size = string(size, "image size")?;
        if !IMAGE_SIZES.iter().any(|(name, _)| *name == size) {
            return fail(format!("image {id:?} size must be sm, md or lg"));
        }
        if node.contains_key("width") || node.contains_key("height") {
            return fail(format!(
                "image {id:?} takes either size or width/height, not both"
            ));
        }
    }
    match (node.get("width"), node.get("height")) {
        (None, None) => {}
        (Some(width), Some(height)) => {
            for value in [width, height] {
                if !value
                    .as_u64()
                    .is_some_and(|n| (1..=MAX_IMAGE_BOX).contains(&n))
                {
                    return fail(format!(
                        "image {id:?} width and height must be integers 1..={MAX_IMAGE_BOX}"
                    ));
                }
            }
        }
        _ => {
            return fail(format!(
                "image {id:?} width and height must be given together"
            ));
        }
    }
    if let Some(shape) = node.get("shape")
        && !matches!(string(shape, "image shape")?, "square" | "circle")
    {
        return fail(format!("image {id:?} shape must be square or circle"));
    }
    Ok(())
}

/// Every image source must name a declared asset: a literal directly, a data
/// binding through its schema enum. Runs after `validate_node` accepted the
/// tree's shape.
fn validate_image_sources(
    node: &Value,
    schema: &Value,
    assets: &BTreeSet<&str>,
) -> Result<(), WidgetError> {
    if node["type"] == "image" {
        let id = node["id"].as_str().unwrap_or_default();
        let src = &node["src"];
        let ids: Vec<&Value> = match binding_pointer(src)? {
            None => vec![src],
            Some(pointer) => schema_pointer(schema, pointer)?["enum"]
                .as_array()
                .map(|values| values.iter().collect())
                .unwrap_or_default(),
        };
        for value in ids {
            let asset = string(value, "image src")?;
            if !assets.contains(asset) {
                return fail(format!(
                    "image {id:?} refers to undeclared asset {asset:?}; declare it in assets"
                ));
            }
        }
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            validate_image_sources(child, schema, assets)?;
        }
    }
    Ok(())
}

fn validate_rendered_image(
    node: &Value,
    data: &Value,
    manifest: &Manifest,
    remaining: &mut RenderBudget,
) -> Result<(), WidgetError> {
    let src = resolved(&node["src"], data, &["string"], "image src")?;
    let src = src.as_str().expect("checked string");
    if !manifest.asset_ids().contains(src) {
        return fail(format!("image src {src:?} is not a declared asset"));
    }
    remaining.value(&resolved(&node["alt"], data, &["string"], "image alt")?)
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictValue;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("JSON without duplicate object keys")
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(StrictValue(value.into()))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(StrictValue(value.into()))
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(StrictValue(value.into()))
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(value)
                    .map(|n| StrictValue(Value::Number(n)))
                    .ok_or_else(|| E::custom("nonfinite number"))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(StrictValue(value.into()))
            }
            fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(StrictValue(value.into()))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(StrictValue(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(StrictValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut values = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom(format!("duplicate object key {key:?}")));
                    }
                    let StrictValue(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(StrictValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(StrictVisitor)
    }
}

/// Editor-facing JSON Schema for the v1 grammar. Semantic checks (bindings,
/// unique IDs, row keys and resource limits) still run in Manifest::from_slice.
pub fn schema_document() -> Value {
    fn node(extra: Value, required: &[&str], kinds: &[&str]) -> Value {
        let mut properties = json!({"type":{"enum":kinds},"id":{"$ref":"#/$defs/id"}})
            .as_object()
            .unwrap()
            .clone();
        properties.extend(extra.as_object().unwrap().clone());
        let required: Vec<_> = ["type", "id"]
            .into_iter()
            .chain(required.iter().copied())
            .collect();
        json!({"type":"object","additionalProperties":false,"properties":properties,"required":required})
    }
    let text = json!({"anyOf":[{"type":"string"},{"$ref":"#/$defs/binding"}]});
    let number = json!({"anyOf":[{"type":"number"},{"$ref":"#/$defs/binding"}]});
    let rows = json!({"anyOf":[{"type":"array","maxItems":MAX_TABLE_ROWS,"items":{"type":"object"}},{"$ref":"#/$defs/binding"}]});
    let gap = json!({"enum":["none","xs","sm","md","lg"]});
    let tone = json!({"enum":["default","muted","success","warning","error"]});
    let children =
        json!({"type":"array","maxItems":MAX_VIEW_NODES,"items":{"$ref":"#/$defs/node"}});
    let mut nodes = vec![
        node(
            json!({"children":children,"gap":gap}),
            &["children"],
            &["column", "row"],
        ),
        node(
            json!({"children":children,"gap":gap,"title":text}),
            &["children", "title"],
            &["section"],
        ),
        node(
            json!({"text":text,"tone":tone}),
            &["text"],
            &["text", "badge"],
        ),
        node(
            json!({"label":text,"value":{"anyOf":[{"type":"string"},{"type":"number"},{"$ref":"#/$defs/binding"}]}}),
            &["label", "value"],
            &["metric"],
        ),
        node(
            json!({"value":number,"max":number,"label":text}),
            &["value"],
            &["progress"],
        ),
        node(
            json!({"rows":rows,"rowKey":{"type":"string"},"columns":{"type":"array","minItems":1,"maxItems":MAX_COLUMNS,"items":{"type":"object","additionalProperties":false,"required":["title","pointer"],"properties":{"title":{"type":"string","minLength":1,"maxLength":200},"pointer":{"type":"string"}}}}}),
            &["rows", "rowKey", "columns"],
            &["table"],
        ),
        node(
            json!({"items":rows,"itemKey":{"type":"string"},"textPointer":{"type":"string"}}),
            &["items", "itemKey", "textPointer"],
            &["list"],
        ),
        node(
            json!({"label":text,"action":{"$ref":"#/$defs/id"},"payload":{"type":"object"},
                "inputs":{"type":"object","minProperties":1,"maxProperties":MAX_BUTTON_INPUTS,"propertyNames":{"minLength":1},"additionalProperties":{"$ref":"#/$defs/id"}},
                "clearInputs":{"type":"boolean"}}),
            &["label", "action"],
            &["button"],
        ),
        node(
            json!({"label":text,"placeholder":{"type":"string","maxLength":200},"multiline":{"type":"boolean"},
                "maxLength":{"type":"integer","minimum":1,"maximum":MAX_ACTION_BYTES},"submit":{"$ref":"#/$defs/id"}}),
            &[],
            &["text-input"],
        ),
    ];
    let pixels = json!({"type":"integer","minimum":1,"maximum":MAX_IMAGE_BOX});
    nodes.push(node(
        json!({"src":{"anyOf":[{"$ref":"#/$defs/id"},{"$ref":"#/$defs/binding"}]},"alt":text,
            "size":{"enum":["sm","md","lg"]},"width":pixels,"height":pixels,"shape":{"enum":["square","circle"]}}),
        &["src", "alt"],
        &["image"],
    ));
    json!({
        "$schema":"https://json-schema.org/draft/2020-12/schema",
        "title":"Noa repository widget v1",
        "type":"object",
        "additionalProperties":false,
        "required":["apiVersion","id","version","title","description","dataSchema","initialData","view"],
        "properties":{
            "apiVersion":{"const":API_VERSION},
            "id":{"$ref":"#/$defs/id"},
            "version":{"type":"string","pattern":"^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$"},
            "title":{"type":"string","minLength":1,"maxLength":200},
            "description":{"type":"string","minLength":1,"maxLength":2000},
            "dataSchema":{"$ref":"#/$defs/dataSchema"},
            "initialData":{"type":"object"},
            "view":{"$ref":"#/$defs/node"},
            "assets":{"type":"object","minProperties":1,"maxProperties":crate::assets::MAX_ASSETS,"propertyNames":{"$ref":"#/$defs/id"},
                "additionalProperties":{"type":"object","additionalProperties":false,"required":["path"],"properties":{"path":{"type":"string","maxLength":crate::assets::MAX_PATH_BYTES,"pattern":"^assets(/[^/.\\\\][^/\\\\]*)+$"}}}},
            "actions":{"type":"array","maxItems":64,"items":{"type":"object","additionalProperties":false,"required":["id","kind","payloadSchema"],"properties":{"id":{"$ref":"#/$defs/id"},"kind":{"const":"agent-event"},"payloadSchema":{"$ref":"#/$defs/dataSchema"}}}}
        },
        "$defs":{
            "id":{"type":"string","maxLength":64,"pattern":"^[a-z][a-z0-9]*(-[a-z0-9]+)*$"},
            "binding":{"type":"object","additionalProperties":false,"required":["source","pointer"],"properties":{"source":{"const":"data"},"pointer":{"type":"string","maxLength":2048,"pattern":"^(?:/(?:[^~]|~[01])*)*$"}}},
            "node":{"oneOf":nodes},
            "dataSchema":{"type":"object","additionalProperties":false,"required":["type"],"properties":{
                "type":{"enum":["object","array","string","number","integer","boolean","null"]},
                "properties":{"type":"object","maxProperties":256,"additionalProperties":{"$ref":"#/$defs/dataSchema"}},
                "required":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
                "additionalProperties":{"type":"boolean"},
                "items":{"$ref":"#/$defs/dataSchema"},
                "enum":{"type":"array","minItems":1,"maxItems":128,"uniqueItems":true},
                "minimum":{"type":"number"},"maximum":{"type":"number"},
                "minLength":{"type":"integer","minimum":0},"maxLength":{"type":"integer","minimum":0},
                "minItems":{"type":"integer","minimum":0},"maxItems":{"type":"integer","minimum":0}
            }}
        }
    })
}
