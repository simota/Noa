//! The seam between `noa-ipc` and whatever owns real panel state
//! (`noa-app`, in production; a `MockBackend` in tests). No Ghostty
//! analog — this mirrors the pattern the AppleScript bridge already uses
//! (`EventLoopProxy` for mutation, a shared snapshot for reads).

use crate::AttachOutputSender;
use crate::error::IpcError;
use crate::protocol::{Panel, Row, SplitDirection, TextSource};

/// An IPC-visible pane id. The app maps this to its internal pointer-derived
/// id; `noa-ipc` only ever sees `u64`.
pub type PaneRef = u64;

/// An IPC-visible window id.
pub type WindowRef = u64;

#[derive(Clone, Debug)]
pub struct TextResult {
    pub text: String,
    /// Whether the backend itself already truncated (e.g. to avoid an
    /// unbounded scrollback read). The server additionally applies
    /// [`crate::protocol::truncate_tail`] with the requested `maxBytes` and
    /// ORs the two flags — see that function's doc comment.
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct GridResult {
    pub cols: u32,
    /// Oldest retained session-absolute row coordinate.
    pub oldest_row: u64,
    /// Exclusive end of the retained session-absolute row range.
    pub next_row: u64,
    pub rows: Vec<Row>,
    /// Set when the backend itself stopped short of `rowCount` for a reason
    /// the server's own byte-budget trim ([`crate::protocol::cap_grid_rows`])
    /// wouldn't otherwise detect — e.g. a `rowCount` clamp applied before
    /// walking the terminal (F-1 / NFR-2). ORed with the server's own
    /// `hasMore` computation.
    pub has_more: bool,
}

/// The backend contract `noa-ipc`'s server dispatches every RPC method to.
/// Implementations must not block on network I/O; the server calls these
/// synchronously from a connection thread.
pub trait IpcBackend: Send + Sync + 'static {
    fn list_panels(&self) -> Vec<Panel>;

    fn get_text(
        &self,
        pane: PaneRef,
        source: TextSource,
        max_bytes: usize,
    ) -> Result<TextResult, IpcError>;

    fn get_grid(
        &self,
        pane: PaneRef,
        start_row: u64,
        row_count: u64,
    ) -> Result<GridResult, IpcError>;

    /// `paste`: `true` sends `text` through the bracketed-paste-aware
    /// encoding (the default); `false` writes `text`'s UTF-8 bytes to the
    /// pty as-is (see [`crate::protocol::SendTextParams::paste`]).
    fn send_text(&self, pane: PaneRef, text: &str, paste: bool) -> Result<(), IpcError>;

    fn focus_pane(&self, pane: PaneRef) -> Result<(), IpcError>;

    /// Creates a new tab (and its initial pane) in `window` (the active
    /// window if `None`), returning the new pane's ipc id.
    fn new_tab(&self, window: Option<WindowRef>) -> Result<PaneRef, IpcError>;

    fn split(&self, pane: PaneRef, direction: SplitDirection) -> Result<PaneRef, IpcError>;

    fn close_pane(&self, pane: PaneRef) -> Result<(), IpcError>;

    /// Checks that `pane` exists and supports raw attach before the server
    /// reserves a one-time token. Existing backend implementations remain
    /// source-compatible through the default unsupported result.
    fn validate_attach(&self, _pane: PaneRef) -> Result<(), IpcError> {
        Err(IpcError::Unsupported("validate_attach"))
    }

    /// Atomically registers `output` as the pane's raw PTY tap and snapshots
    /// the synthetic VT seed. The application implementation must perform
    /// both operations under one terminal lock acquisition. This method must
    /// return before any WebSocket write occurs.
    fn open_attach(
        &self,
        _pane: PaneRef,
        _generation: u64,
        _output: AttachOutputSender,
    ) -> Result<Vec<u8>, IpcError> {
        Err(IpcError::Unsupported("open_attach"))
    }

    /// Delivers one raw binary client frame to the attached pane's PTY input
    /// path. The generation prevents a stale socket from writing into a newer
    /// attach lease.
    fn write_attach(
        &self,
        _pane: PaneRef,
        _generation: u64,
        _bytes: &[u8],
    ) -> Result<(), IpcError> {
        Err(IpcError::Unsupported("write_attach"))
    }

    /// Releases application-owned tap/input state for one generation. The
    /// default is intentionally idempotent so legacy backends stay compatible.
    fn detach_attach(&self, _pane: PaneRef, _generation: u64) -> Result<(), IpcError> {
        Ok(())
    }

    /// Applies grid-first remote resize for an attached pane.
    fn resize_pane(&self, _pane: PaneRef, _cols: u16, _rows: u16) -> Result<(), IpcError> {
        Err(IpcError::Unsupported("resize_pane"))
    }
}
