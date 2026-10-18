//! The custom winit user event this app drives its event loop with.

use crate::auto_approve::AutoApproveSignature;
use crate::session_store::{SessionCardId, SessionDelta};
use crate::{AppCommand, split_tree::PaneId};
use winit::window::WindowId;

/// Events posted from the io thread to the winit event loop via
/// [`winit::event_loop::EventLoopProxy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserEvent {
    /// A native app menu item or app-level shortcut was activated.
    AppCommand(AppCommand),
    TextPanelInput {
        window_id: WindowId,
        pane_id: PaneId,
        process: Option<String>,
        text: String,
        paste: bool,
    },
    TextPanelReturn {
        window_id: WindowId,
        pane_id: PaneId,
    },
    FilePreview {
        window_id: WindowId,
        pane_id: PaneId,
        title: String,
        text: String,
    },
    /// An OSC 52 clipboard write was accepted by the terminal policy.
    ClipboardWrite {
        window_id: WindowId,
        pane_id: PaneId,
        text: String,
    },
    /// An OSC 52 clipboard read (query) the policy allowed; the app fulfills
    /// it (subject to the ask/allow policy) by writing a base64 reply to the
    /// pane's pty. `target` is the selection identifier to echo (e.g. `"c"`).
    ClipboardRead {
        window_id: WindowId,
        pane_id: PaneId,
        target: String,
    },
    /// A desktop notification requested by the terminal via OSC 9 / OSC 777.
    /// The app posts it to the macOS notification center (unless the target
    /// window is focused) and bounces the Dock.
    Notify {
        window_id: WindowId,
        pane_id: PaneId,
        title: Option<String>,
        body: String,
    },
    /// The global quick-terminal hotkey fired (posted from the Carbon hotkey
    /// handler thread via the [`winit::event_loop::EventLoopProxy`]). Toggles
    /// the drop-down quick terminal's visibility.
    ToggleQuickTerminal,
    /// A session-sidebar state delta posted by the io thread (last-output
    /// upsert, unread bell, …). The main thread — which owns the
    /// [`crate::session_store::SessionStore`] — applies it on receipt
    /// (FR-1). Carries only GUI-agnostic ids, so it never leaks a windowing
    /// type into the store.
    SessionDelta(SessionDelta),
    /// A recognized agent prompt matched the conservative auto-approve matrix
    /// on a pane's live viewport. The main thread must re-read the terminal
    /// before writing anything to the pty.
    AutoApprove {
        id: SessionCardId,
        signature: AutoApproveSignature,
        region_hash: u64,
        disable_after: bool,
    },
    /// The pane's `OSC 22` pointer shape changed; re-pick the mouse cursor.
    MouseShapeChanged(WindowId, PaneId),
    /// New terminal output is available; request a redraw.
    Redraw(WindowId, PaneId),
    SearchUpdated(WindowId, PaneId),
    /// A hover-path existence probe finished on its worker thread
    /// (`App::hover_link_target` never stats the filesystem on the main
    /// thread — a network volume can block a metadata query indefinitely).
    /// The app records the answer in its probe cache — only if `generation`
    /// still matches the entry's in-flight probe, so a superseded straggler
    /// can't overwrite a newer answer — and re-syncs every window that was
    /// waiting on it so a confirmed path underlines without the mouse
    /// moving.
    PathProbe {
        generation: u64,
        path: std::path::PathBuf,
        exists: bool,
    },
    /// The pty's child process exited (or errored) — the app should close.
    PtyExit(WindowId, PaneId),
    /// The AppleScript bridge queued one or more `ScriptAction`s in
    /// `ScriptShared::actions` (applescript R-11, agent-automation L2.1).
    /// Carries no payload — like `IpcAction`, the work lives in a shared
    /// queue — so the AE handler can hand over non-`Eq` data; the main thread
    /// drains the whole queue on receipt, in order.
    AppleScript,
    /// A `noa-ipc` mutating RPC (`focusPane`/`newTab`/`split`/`closePane`/
    /// `sendText`) is waiting on the main thread (noa-server spec DEC-C). The
    /// payload lives in `App::ipc_pending`, keyed by `request_id`, because
    /// `UserEvent` derives `Eq` and so cannot carry a reply channel directly.
    IpcAction {
        request_id: u64,
    },
    /// A remote discovery or create-pane worker completed. Panels and worker
    /// results remain in `App::remote_pending`; the Eq event carries only its
    /// monotonic lookup id and never carries the bearer token.
    RemoteRequestCompleted {
        request_id: u64,
    },
    /// Deferred focus restore after a macOS native-tab close. Calling
    /// `focus_window()` synchronously from `close_tab` collides with AppKit
    /// still transferring key/firstResponder to the sibling tab, leaving the
    /// window key but its firstResponder pointed at the NSWindow rather than
    /// winit's text-input view — so `keyDown:` never reaches winit and input
    /// goes dead. Posting through the proxy re-runs the focus on a fresh
    /// event-loop iteration, after AppKit has finished the tab teardown.
    RestoreFocus {
        window_id: WindowId,
    },
}
