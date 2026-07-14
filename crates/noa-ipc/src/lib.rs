//! `noa-ipc` — JSON-RPC-over-WebSocket external control surface.
//!
//! Ghostty analog: none (new surface). Mirrors the pattern noa's existing
//! AppleScript bridge uses — mutation via an injected command channel,
//! reads via a snapshot the app publishes — but exposes it over loopback
//! TCP so non-macOS/non-AppleScript clients (CLI tools, dashboards, iOS via
//! tunnel) can drive noa. See `docs/specs/noa-server.md` for the locked
//! spec this crate implements (FR-1..19, NFR-1..5).
//!
//! This crate is GUI-agnostic: it does not depend on `winit`/`wgpu`/
//! `noa-app`. A later integration wires [`backend::IpcBackend`] to real
//! panel state and calls [`server::Server::start`] from the app.
//!
//! Concurrency model (NFR-3): no async runtime. Sync `tungstenite` +
//! thread-per-connection + `crossbeam`/`parking_lot`, matching the rest of
//! the codebase's `io_thread` model.

mod attach;
pub mod auth;
pub mod backend;
pub mod client;
pub mod error;
pub mod protocol;
pub mod push;
pub mod server;

pub use attach::{
    ATTACH_BACKPRESSURE_TIMEOUT, ATTACH_OUTPUT_CAPACITY_BYTES, ATTACH_TOKEN_TTL, AttachOutputError,
    AttachOutputSender,
};
pub use auth::{Scope, ScopeSet, constant_time_eq, load_or_create_token};
pub use backend::{GridResult, IpcBackend, PaneRef, TextResult, WindowRef};
pub use client::{AttachClient, Client, ClientError, DEFAULT_ATTACH_POLL_TIMEOUT};
pub use error::{ErrorCode, IpcError};
pub use protocol::{
    AttachParams, AttachResult, Attr, DetachParams, EventKind, GetTextResult, ListPanelsResult,
    PROTOCOL_VERSION, PaneIdResult, Panel, ResizePaneParams, Row, Span, SpanColor, SplitDirection,
    TextSource, WireId,
};
pub use push::{Broadcaster, EventMask, PushQueue};
pub use server::{ATTACH_HANDSHAKE_CLOSE_REASON, Server, ServerConfig, ServerHandle};
