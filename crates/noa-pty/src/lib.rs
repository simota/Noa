//! `noa-pty` — PTY spawn + reader/writer threads.
//!
//! Ghostty analog: `termio/Exec.zig` + `pty.zig` + `Command.zig`.
//!
//! Spawns a PTY-backed shell, streams output bytes out over a
//! [`crossbeam_channel`] as [`PtyEvent`]s, and accepts input bytes / resizes.

mod data;
mod metrics;
mod pty;
mod reader;
mod shell_integration;
mod writer;

pub use data::PtyData;
pub use metrics::{PaneMetrics, ProcRecord, ProcSnapshot, foreground_tree};
pub use pty::{ForegroundProcessProbe, Pty, PtyConfig};
pub use writer::PtyWriter;

/// An event emitted by the PTY reader thread.
#[derive(Debug)]
pub enum PtyEvent {
    /// A chunk of bytes read from the PTY master.
    Data(PtyData),
    /// The child process exited with the given status code.
    Exit(i32),
    /// An error occurred while reading from the PTY master.
    Error(std::io::Error),
}

/// Errors that can occur while spawning or driving a [`Pty`].
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    /// Opening the PTY pair failed.
    #[error("failed to open pty: {0}")]
    OpenPty(String),
    /// Spawning the child process failed.
    #[error("failed to spawn child: {0}")]
    Spawn(String),
    /// Cloning the reader handle failed.
    #[error("failed to clone pty reader: {0}")]
    CloneReader(String),
    /// Taking the writer handle failed.
    #[error("failed to take pty writer: {0}")]
    TakeWriter(String),
    /// Resizing the PTY failed.
    #[error("failed to resize pty: {0}")]
    Resize(String),
    /// Spawning a reader/waiter thread failed (resource exhaustion).
    #[error("failed to spawn pty io thread: {0}")]
    SpawnThread(String),
}

/// Convenience result alias for this crate.
pub type Result<T> = std::result::Result<T, PtyError>;
