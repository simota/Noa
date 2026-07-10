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
    /// The global session-sidebar hotkey fired (same Carbon mechanism as
    /// [`Self::ToggleQuickTerminal`]). Toggles the sidebar on the focused
    /// window only (FR-4).
    ToggleSidebar,
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
    /// New terminal output is available; request a redraw.
    Redraw(WindowId, PaneId),
    /// The pty's child process exited (or errored) — the app should close.
    PtyExit(WindowId, PaneId),
    /// AppleScript `input text`: write text to a resolved pane's pty on the
    /// main thread (applescript R-7). The `window_id`/`pane_id` are frozen at
    /// AE-resolve time; the write is dropped if the target is gone, and the
    /// bracketed-paste wrapping is decided at process time from the pane's
    /// live mode. Never touches winit objects from the AE handler (R-11).
    WriteText {
        window_id: WindowId,
        pane_id: PaneId,
        text: String,
    },
    /// AppleScript `focus` / `select tab` / `activate window` (applescript
    /// R-5/AC-6/AC-15): raise the target's native tab/window to the front and
    /// move split focus to `pane_id`. Unlike a plain split-focus this always
    /// re-orders the window (even when `pane_id` is already focused), and when
    /// `activate_app` is set it also brings the whole app forward
    /// (`activateIgnoringOtherApps`) for the application-level `activate`. Ids
    /// frozen at AE-resolve time.
    RaiseWindow {
        window_id: WindowId,
        pane_id: PaneId,
        activate_app: bool,
    },
    /// AppleScript `close` on a terminal (applescript R-6/AC-16): close one
    /// split pane through the existing confirm/close path. Ids frozen at
    /// AE-resolve time.
    ClosePane {
        window_id: WindowId,
        pane_id: PaneId,
    },
    /// AppleScript `new window` / `new tab` (applescript R-3): spawn a tab,
    /// optionally in a saved cwd and/or running an initial command. Routed as a
    /// dedicated event rather than an `AppCommand` so the payload does not have
    /// to be threaded through the parameterless [`AppCommand`] variants.
    SpawnTab {
        window_target: AppleScriptSpawnTarget,
        cwd: Option<String>,
        command: Option<String>,
    },
}

/// Whether an AppleScript-driven spawn joins the focused window's tab group or
/// starts a fresh native window — the scripting-visible analog of the internal
/// `SpawnTarget` (kept out of `events` to avoid leaking an `app`-private type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppleScriptSpawnTarget {
    /// Join the focused window's tab group (`new tab`).
    CurrentWindow,
    /// Start a fresh native window / tab group (`new window`).
    NewWindow,
}
