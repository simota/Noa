//! The custom winit user event this app drives its event loop with.

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
    /// New terminal output is available; request a redraw.
    Redraw(WindowId, PaneId),
    /// The pty's child process exited (or errored) — the app should close.
    PtyExit(WindowId, PaneId),
}
