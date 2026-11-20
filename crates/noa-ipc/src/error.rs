//! JSON-RPC 2.0 error codes (spec §L2 "エラーコード表").

use serde::Serialize;

/// A JSON-RPC error code. Standard `-326xx` range for parse/protocol
/// errors, `-3200x` implementation-defined range for noa-specific failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    Auth,
    UnknownPane,
    ScopeDenied,
    PaneClosed,
    PayloadTooLarge,
    VersionMismatch,
    AttachConflict,
    AttachHandshakeFailure,
    /// The request was handed to the app but no result came back in time:
    /// it may still be carried out (or already was), so the caller cannot
    /// treat it as refused.
    OutcomeUnknown,
    Internal,
}

impl ErrorCode {
    pub const fn code(self) -> i64 {
        match self {
            ErrorCode::ParseError => -32700,
            ErrorCode::InvalidRequest => -32600,
            ErrorCode::MethodNotFound => -32601,
            ErrorCode::InvalidParams => -32602,
            ErrorCode::Internal => -32603,
            ErrorCode::Auth => -32001,
            ErrorCode::UnknownPane => -32002,
            ErrorCode::ScopeDenied => -32003,
            ErrorCode::PaneClosed => -32004,
            ErrorCode::PayloadTooLarge => -32005,
            ErrorCode::VersionMismatch => -32006,
            ErrorCode::AttachConflict => -32007,
            ErrorCode::AttachHandshakeFailure => -32008,
            ErrorCode::OutcomeUnknown => -32009,
        }
    }
}

/// The wire-format JSON-RPC 2.0 error object.
#[derive(Clone, Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

impl JsonRpcError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        JsonRpcError {
            code: code.code(),
            message: message.into(),
        }
    }
}

/// Errors an [`crate::backend::IpcBackend`] implementation can return from
/// its fallible methods. Mapped 1:1 to [`ErrorCode`] by the server.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("unknown pane or window")]
    UnknownPane,
    #[error("pane closed")]
    PaneClosed,
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("unsupported backend operation: {0}")]
    Unsupported(&'static str),
    #[error("internal error: {0}")]
    Internal(String),
    /// A request the backend refuses for what it asks (JSON-RPC invalid
    /// params), with the reason as the message.
    #[error("{0}")]
    InvalidParams(String),
    /// The request was dispatched but its result did not arrive in time; it
    /// may still take effect.
    #[error("outcome unknown: {0}")]
    OutcomeUnknown(String),
}

impl IpcError {
    pub const fn code(&self) -> ErrorCode {
        match self {
            IpcError::UnknownPane => ErrorCode::UnknownPane,
            IpcError::PaneClosed => ErrorCode::PaneClosed,
            IpcError::PayloadTooLarge => ErrorCode::PayloadTooLarge,
            IpcError::Unsupported(_) => ErrorCode::Internal,
            IpcError::Internal(_) => ErrorCode::Internal,
            IpcError::InvalidParams(_) => ErrorCode::InvalidParams,
            IpcError::OutcomeUnknown(_) => ErrorCode::OutcomeUnknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // agent-orchestration D13: the app refuses a spawn nested too deep as
    // invalid params, with its own reason as the message.
    #[test]
    fn a_refused_request_is_invalid_params_with_its_reason() {
        let error = IpcError::InvalidParams("nesting goes one level deep".to_string());
        assert_eq!(error.code(), ErrorCode::InvalidParams);
        assert_eq!(error.code().code(), -32602);
        assert_eq!(error.to_string(), "nesting goes one level deep");
    }

    // Review finding: a request the app had not answered in time came back
    // as -32603, which `+ctl spawn --worktree` read as refused. It has its
    // own code.
    #[test]
    fn an_unanswered_request_has_its_own_code() {
        let error = IpcError::OutcomeUnknown("request timed out".to_string());
        assert_eq!(error.code(), ErrorCode::OutcomeUnknown);
        assert_eq!(error.code().code(), -32009);
        assert_ne!(error.code().code(), ErrorCode::Internal.code());
    }
}
