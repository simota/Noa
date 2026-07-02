//! The custom winit user event this app drives its event loop with.

use crate::AppCommand;
use winit::window::WindowId;

/// Events posted from the io thread to the winit event loop via
/// [`winit::event_loop::EventLoopProxy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserEvent {
    /// A native app menu item or app-level shortcut was activated.
    AppCommand(AppCommand),
    /// An OSC 52 clipboard write was accepted by the terminal policy.
    ClipboardWrite { window_id: WindowId, text: String },
    /// New terminal output is available; request a redraw.
    Redraw(WindowId),
    /// The pty's child process exited (or errored) — the app should close.
    PtyExit(WindowId),
}
