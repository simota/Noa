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
        JsonRpcError { code: code.code(), message: message.into() }
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
    #[error("internal error: {0}")]
    Internal(String),
}

impl IpcError {
    pub const fn code(&self) -> ErrorCode {
        match self {
            IpcError::UnknownPane => ErrorCode::UnknownPane,
            IpcError::PaneClosed => ErrorCode::PaneClosed,
            IpcError::PayloadTooLarge => ErrorCode::PayloadTooLarge,
            IpcError::Internal(_) => ErrorCode::Internal,
        }
    }
}
