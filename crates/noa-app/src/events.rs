//! The custom winit user event this app drives its event loop with.

/// Events posted from the io thread to the winit event loop via
/// [`winit::event_loop::EventLoopProxy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserEvent {
    /// New terminal output is available; request a redraw.
    Redraw,
    /// The pty's child process exited (or errored) — the app should close.
    PtyExit,
}
