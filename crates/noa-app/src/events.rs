//! The custom winit user event this app drives its event loop with.

use crate::AppCommand;

/// Events posted from the io thread to the winit event loop via
/// [`winit::event_loop::EventLoopProxy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserEvent {
    /// A native app menu item or app-level shortcut was activated.
    AppCommand(AppCommand),
    /// New terminal output is available; request a redraw.
    Redraw,
    /// The pty's child process exited (or errored) — the app should close.
    PtyExit,
}
