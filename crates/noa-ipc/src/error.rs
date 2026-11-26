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
    /// The pane is not ready for what the request needs (`noa.sendText`
    /// with `requirePasteMode` before the pane enabled bracketed paste).
    /// Nothing was done; the caller may retry.
    NotReady,
    /// A spawn would exceed the caller's `maxChildren` for its parent pane.
    LimitReached,
    /// Board timeline (`docs/specs/board-timeline.md` L2-3): an idempotency
    /// key or the one-summary-per-run rule matched a different payload.
    TimelineConflict,
    /// Board timeline: the board, post, run or pane binding is unknown.
    TimelineNotFound,
    /// Board timeline: the service queue is full, a read timed out, or
    /// another Noa instance holds the board.
    Busy,
    /// Board timeline: the append would take the board over its size limit.
    StorageFull,
    /// Board timeline: a write, sync or rename failed; nothing was acked.
    StoreIo,
    /// Board timeline: the board's post log has an unparsable line or a seq
    /// regression; reads serve the valid prefix, writes are refused.
    CorruptStore,
    /// Board timeline: this backend has no timeline service, or the post's
    /// schema version is not one it takes.
    Unsupported,
    /// A repository widget revision, consumer or request id conflicts.
    WidgetConflict,
    /// A repository widget instance or definition is unknown.
    WidgetNotFound,
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
            ErrorCode::NotReady => -32010,
            ErrorCode::LimitReached => -32011,
            ErrorCode::TimelineConflict => -32012,
            ErrorCode::TimelineNotFound => -32013,
            ErrorCode::Busy => -32014,
            ErrorCode::StorageFull => -32015,
            ErrorCode::StoreIo => -32016,
            ErrorCode::CorruptStore => -32017,
            ErrorCode::Unsupported => -32018,
            ErrorCode::WidgetConflict => -32019,
            ErrorCode::WidgetNotFound => -32020,
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
    /// Refused without effect because the pane is not ready yet; the reason
    /// is the message.
    #[error("{0}")]
    NotReady(String),
    /// Refused because a limit the request named was reached; the reason is
    /// the message.
    #[error("{0}")]
    LimitReached(String),
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
            IpcError::NotReady(_) => ErrorCode::NotReady,
            IpcError::LimitReached(_) => ErrorCode::LimitReached,
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

    // REL-01/REL-02: a send refused before the pane enabled bracketed paste
    // and a spawn over `maxChildren` each have their own code, so a client
    // can retry the first and report the second without parsing messages.
    #[test]
    fn not_ready_and_limit_reached_have_their_own_codes() {
        let not_ready = IpcError::NotReady("pane 3 has not enabled bracketed paste".to_string());
        assert_eq!(not_ready.code(), ErrorCode::NotReady);
        assert_eq!(not_ready.code().code(), -32010);
        assert_eq!(
            not_ready.to_string(),
            "pane 3 has not enabled bracketed paste"
        );
        let limit = IpcError::LimitReached("2 children (1 live, 1 exited)".to_string());
        assert_eq!(limit.code(), ErrorCode::LimitReached);
        assert_eq!(limit.code().code(), -32011);
    }
}
