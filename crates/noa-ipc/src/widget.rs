//! Repository widget control and the private host's JSON-lines protocol.
//!
//! Widget hosts use their inherited pipe, never the global control token.
//! The public RPC remains Noa's trusted-controller interface; an owner pane
//! or consumer id is an identity for coordination, not an authorization grant.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ErrorCode, Scope, WireId};

pub const CAPABILITY_REPO_WIDGETS_V1: &str = "repoWidgetsV1";
pub const MAX_BRIDGE_LINE_BYTES: usize = 256 * 1024;
pub const MAX_DATA_BYTES: usize = 128 * 1024;
pub const MAX_ACTION_BYTES: usize = 16 * 1024;
pub const MAX_EVENT_PAGE: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListParams {
    pub repo: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DescribeParams {
    pub repo: String,
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenParams {
    pub repo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pane_id: Option<WireId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstanceParams {
    pub instance_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetParams {
    pub instance_id: String,
    pub expected_revision: u64,
    pub data: Value,
    pub request_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventsParams {
    pub instance_id: String,
    pub consumer_id: String,
    #[serde(default)]
    pub after_seq: u64,
    #[serde(default = "event_page_default")]
    pub limit: usize,
}

fn event_page_default() -> usize {
    32
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AckParams {
    pub instance_id: String,
    pub consumer_id: String,
    pub through_seq: u64,
    pub request_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloseParams {
    pub instance_id: String,
    pub request_id: String,
}

#[derive(Clone, Debug)]
pub enum WidgetCall {
    List(ListParams),
    Describe(DescribeParams),
    Open(OpenParams),
    Get(InstanceParams),
    Set(SetParams),
    Events(EventsParams),
    Ack(AckParams),
    Close(CloseParams),
    Forget(InstanceParams),
}

impl WidgetCall {
    pub fn parse(method: &str, params: Value) -> Result<Option<Self>, WidgetError> {
        if !matches!(
            method,
            "noa.widget.list"
                | "noa.widget.describe"
                | "noa.widget.open"
                | "noa.widget.get"
                | "noa.widget.set"
                | "noa.widget.events"
                | "noa.widget.ack"
                | "noa.widget.close"
                | "noa.widget.forget"
        ) {
            return Ok(None);
        }
        if !params.is_object() {
            return Err(WidgetError::invalid("params must be an object"));
        }
        fn decode<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, WidgetError> {
            serde_json::from_value(params).map_err(|err| WidgetError::invalid(err.to_string()))
        }
        let call = match method {
            "noa.widget.list" => Self::List(decode(params)?),
            "noa.widget.describe" => Self::Describe(decode(params)?),
            "noa.widget.open" => Self::Open(decode(params)?),
            "noa.widget.get" => Self::Get(decode(params)?),
            "noa.widget.set" => Self::Set(decode(params)?),
            "noa.widget.events" => Self::Events(decode(params)?),
            "noa.widget.ack" => Self::Ack(decode(params)?),
            "noa.widget.close" => Self::Close(decode(params)?),
            "noa.widget.forget" => Self::Forget(decode(params)?),
            _ => unreachable!(),
        };
        call.validate()?;
        Ok(Some(call))
    }

    pub fn method(&self) -> &'static str {
        match self {
            Self::List(_) => "noa.widget.list",
            Self::Describe(_) => "noa.widget.describe",
            Self::Open(_) => "noa.widget.open",
            Self::Get(_) => "noa.widget.get",
            Self::Set(_) => "noa.widget.set",
            Self::Events(_) => "noa.widget.events",
            Self::Ack(_) => "noa.widget.ack",
            Self::Close(_) => "noa.widget.close",
            Self::Forget(_) => "noa.widget.forget",
        }
    }

    pub fn params(&self) -> Value {
        match self {
            Self::List(p) => serde_json::to_value(p),
            Self::Describe(p) => serde_json::to_value(p),
            Self::Open(p) => serde_json::to_value(p),
            Self::Get(p) => serde_json::to_value(p),
            Self::Set(p) => serde_json::to_value(p),
            Self::Events(p) => serde_json::to_value(p),
            Self::Ack(p) => serde_json::to_value(p),
            Self::Close(p) => serde_json::to_value(p),
            Self::Forget(p) => serde_json::to_value(p),
        }
        .expect("widget params contain only JSON values")
    }

    pub fn scope(&self) -> Scope {
        match self {
            Self::List(_) | Self::Describe(_) | Self::Get(_) => Scope::Read,
            // Reading events claims the instance's single consumer.
            _ => Scope::Control,
        }
    }

    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Open(p) => Some(&p.request_id),
            Self::Set(p) => Some(&p.request_id),
            Self::Ack(p) => Some(&p.request_id),
            Self::Close(p) => Some(&p.request_id),
            _ => None,
        }
    }

    pub fn validate(&self) -> Result<(), WidgetError> {
        if let Some(id) = self.request_id() {
            validate_token("requestId", id)?;
        }
        match self {
            Self::List(p) => validate_repo(&p.repo),
            Self::Describe(p) => {
                validate_repo(&p.repo)?;
                validate_token("id", &p.id)
            }
            Self::Open(p) => {
                validate_repo(&p.repo)?;
                match (&p.id, &p.definition) {
                    (Some(id), None) => validate_token("id", id)?,
                    (None, Some(definition)) => validate_size(definition, 64 * 1024)?,
                    _ => {
                        return Err(WidgetError::invalid(
                            "provide exactly one of id or definition",
                        ));
                    }
                }
                if let Some(data) = &p.data {
                    validate_size(data, MAX_DATA_BYTES)?;
                }
                Ok(())
            }
            Self::Get(p) | Self::Forget(p) => validate_instance(&p.instance_id),
            Self::Set(p) => {
                validate_instance(&p.instance_id)?;
                validate_size(&p.data, MAX_DATA_BYTES)
            }
            Self::Events(p) => {
                validate_instance(&p.instance_id)?;
                validate_token("consumerId", &p.consumer_id)?;
                if p.limit == 0 || p.limit > MAX_EVENT_PAGE {
                    return Err(WidgetError::invalid("limit must be 1..64"));
                }
                Ok(())
            }
            Self::Ack(p) => {
                validate_instance(&p.instance_id)?;
                validate_token("consumerId", &p.consumer_id)
            }
            Self::Close(p) => validate_instance(&p.instance_id),
        }
    }
}

fn validate_repo(repo: &str) -> Result<(), WidgetError> {
    if repo.len() > 4096 || !std::path::Path::new(repo).is_absolute() || repo.contains('\0') {
        return Err(WidgetError::invalid(
            "repo must be an absolute directory path",
        ));
    }
    Ok(())
}

pub fn validate_token(name: &str, id: &str) -> Result<(), WidgetError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
    {
        return Err(WidgetError::invalid(format!("invalid {name}")));
    }
    Ok(())
}

pub fn validate_instance(id: &str) -> Result<(), WidgetError> {
    if id.len() != 35
        || !id.starts_with("wi_")
        || !id.as_bytes()[3..].iter().all(u8::is_ascii_hexdigit)
    {
        return Err(WidgetError::invalid("invalid instanceId"));
    }
    Ok(())
}

pub fn validate_size(value: &Value, max: usize) -> Result<(), WidgetError> {
    if serde_json::to_vec(value).map_or(true, |bytes| bytes.len() > max) {
        return Err(WidgetError::new(
            WidgetErrorKind::LimitReached,
            "widget payload exceeds its size limit",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum HostMessage {
    Snapshot {
        instance_id: String,
        definition_digest: String,
        data_revision: u64,
        manifest: Value,
        data: Value,
    },
    ActionResult {
        request_id: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_kind: Option<String>,
    },
    Closed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum HostRequest {
    Ready,
    Action {
        request_id: String,
        action_id: String,
        data_revision: u64,
        payload: Value,
    },
    Closed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetEvent {
    pub event_id: String,
    pub seq: u64,
    pub action_id: String,
    pub data_revision: u64,
    pub payload: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetErrorKind {
    InvalidParams,
    Unsupported,
    Conflict,
    NotFound,
    Busy,
    OutcomeUnknown,
    IoError,
    LimitReached,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct WidgetError {
    pub kind: WidgetErrorKind,
    pub message: String,
}

impl WidgetError {
    pub fn new(kind: WidgetErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(WidgetErrorKind::InvalidParams, message)
    }

    pub fn unsupported() -> Self {
        Self::new(
            WidgetErrorKind::Unsupported,
            "repository widgets are unavailable",
        )
    }

    pub fn code(&self) -> ErrorCode {
        match self.kind {
            WidgetErrorKind::InvalidParams => ErrorCode::InvalidParams,
            WidgetErrorKind::Unsupported => ErrorCode::Unsupported,
            WidgetErrorKind::Conflict => ErrorCode::WidgetConflict,
            WidgetErrorKind::NotFound => ErrorCode::WidgetNotFound,
            WidgetErrorKind::Busy => ErrorCode::Busy,
            WidgetErrorKind::OutcomeUnknown => ErrorCode::OutcomeUnknown,
            WidgetErrorKind::IoError => ErrorCode::StoreIo,
            WidgetErrorKind::LimitReached => ErrorCode::LimitReached,
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            WidgetErrorKind::InvalidParams => "invalid_params",
            WidgetErrorKind::Unsupported => "unsupported",
            WidgetErrorKind::Conflict => "conflict",
            WidgetErrorKind::NotFound => "not_found",
            WidgetErrorKind::Busy => "busy",
            WidgetErrorKind::OutcomeUnknown => "outcome_unknown",
            WidgetErrorKind::IoError => "io_error",
            WidgetErrorKind::LimitReached => "limit_reached",
        }
    }

    pub fn data(&self) -> Value {
        serde_json::json!({ "kind": self.kind_name() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn malformed_paths_ids_unknown_fields_and_unbounded_pages_are_refused() {
        for (method, params) in [
            ("noa.widget.list", json!({"repo":"relative"})),
            ("noa.widget.get", json!({"instanceId":"../../outside"})),
            ("noa.widget.list", json!({"repo":"/repo","execute":"bad"})),
            (
                "noa.widget.events",
                json!({"instanceId":"wi_0123456789abcdef0123456789abcdef","consumerId":"a","limit":65}),
            ),
            ("noa.widget.open", json!(["/repo", "widget", null, "req"])),
        ] {
            assert_eq!(
                WidgetCall::parse(method, params).unwrap_err().kind,
                WidgetErrorKind::InvalidParams
            );
        }
    }

    #[test]
    fn events_claim_a_consumer_and_require_control() {
        let call = WidgetCall::parse(
            "noa.widget.events",
            json!({
                "instanceId":"wi_0123456789abcdef0123456789abcdef", "consumerId":"agent"
            }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(call.scope(), Scope::Control);
        assert_eq!(call.params()["limit"], 32);
    }

    #[test]
    fn host_cannot_name_another_instance_or_request_a_command() {
        assert!(
            serde_json::from_value::<HostRequest>(json!({
                "type":"action", "requestId":"a", "actionId":"refresh", "dataRevision":1,
                "payload":{}, "instanceId":"other"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<HostRequest>(json!({"type":"exec","command":"sh"})).is_err()
        );
    }
}
