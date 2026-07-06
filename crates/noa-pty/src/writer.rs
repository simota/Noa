//! The writable end of a [`Pty`](crate::Pty).

use std::io::{self, Write};

use crossbeam_channel::Sender;

/// A cloneable, sendable handle for writing input bytes to the PTY master.
///
/// Writes are queued to a dedicated writer thread rather than hitting the
/// master fd directly: a blocking `write_all` stalls whenever the child stops
/// reading stdin (the kernel tty input queue holds only ~1KB in raw mode on
/// macOS), and running that on the io read loop froze the pane — pty reads,
/// redraws, and resizes all stop — and could deadlock permanently once the
/// child also blocked writing output. Queueing keeps every caller
/// non-blocking; the single consumer preserves write order across clones.
#[derive(Clone)]
pub struct PtyWriter {
    tx: Sender<Box<[u8]>>,
}

impl PtyWriter {
    /// Spawn the writer thread that owns the master's write half. The thread
    /// exits when every `PtyWriter` clone is dropped (channel disconnect) or a
    /// write fails (child gone → EIO); a write blocked on a full tty input
    /// queue is unblocked by the child reading, or errors out when the `Pty`
    /// drop kills the child.
    pub(crate) fn spawn(mut writer: Box<dyn Write + Send>) -> io::Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded::<Box<[u8]>>();
        std::thread::Builder::new()
            .name("noa-pty-writer".into())
            .spawn(move || {
                while let Ok(bytes) = rx.recv() {
                    if let Err(err) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
                        log::warn!("pty writer thread stopping: {err}");
                        return;
                    }
                }
            })?;
        Ok(Self { tx })
    }

    /// Queue all of `data` for the PTY master. Never blocks; errors only if
    /// the writer thread has stopped (write failure or teardown).
    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        self.tx
            .send(data.into())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "pty writer thread stopped"))
    }

    /// No-op for API symmetry: the writer thread flushes after every queued
    /// chunk.
    pub fn flush(&self) -> io::Result<()> {
        Ok(())
    }
}

impl std::fmt::Debug for PtyWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyWriter").finish_non_exhaustive()
    }
}
