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

pub mod auth;
pub mod backend;
pub mod error;
pub mod protocol;
pub mod push;
pub mod server;

pub use auth::{Scope, ScopeSet, constant_time_eq, load_or_create_token};
pub use backend::{GridResult, IpcBackend, PaneRef, TextResult, WindowRef};
pub use error::{ErrorCode, IpcError};
pub use protocol::{
    Attr, EventKind, PROTOCOL_VERSION, Panel, Row, Span, SpanColor, SplitDirection, TextSource,
    WireId,
};
pub use push::{Broadcaster, EventMask, PushQueue};
pub use server::{Server, ServerConfig, ServerHandle};
