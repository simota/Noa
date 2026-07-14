//! The writable end of a [`Pty`](crate::Pty).

use std::io::{self, Write};

use crossbeam_channel::Sender;

trait WriteBuffer: AsRef<[u8]> + Send {}

impl<T: AsRef<[u8]> + Send> WriteBuffer for T {}

enum WriteRequest {
    Bytes(Box<[u8]>),
    /// Retains the caller's owned wrapper until the writer thread completes
    /// (or abandons) the real PTY write. This lets an upstream byte-budget
    /// reservation travel through this otherwise-unbounded queue.
    Owned(Box<dyn WriteBuffer>),
}

impl AsRef<[u8]> for WriteRequest {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Owned(bytes) => bytes.as_ref().as_ref(),
        }
    }
}

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
    tx: Sender<WriteRequest>,
}

impl PtyWriter {
    /// Spawn the writer thread that owns the master's write half. The thread
    /// exits when every `PtyWriter` clone is dropped (channel disconnect) or a
    /// write fails (child gone → EIO); a write blocked on a full tty input
    /// queue is unblocked by the child reading, or errors out when the `Pty`
    /// drop kills the child.
    pub(crate) fn spawn(mut writer: Box<dyn Write + Send>) -> io::Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded::<WriteRequest>();
        std::thread::Builder::new()
            .name("noa-pty-writer".into())
            .spawn(move || {
                while let Ok(request) = rx.recv() {
                    if let Err(err) = writer
                        .write_all(request.as_ref())
                        .and_then(|()| writer.flush())
                    {
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
        self.send(WriteRequest::Bytes(data.into()))
    }

    /// Queue an owned buffer without copying it. The complete value is kept
    /// alive until the writer thread finishes the corresponding `write_all`
    /// and flush, so wrappers may carry an upstream memory-budget reservation.
    pub fn write_owned<T>(&self, data: T) -> io::Result<()>
    where
        T: AsRef<[u8]> + Send + 'static,
    {
        self.send(WriteRequest::Owned(Box::new(data)))
    }

    fn send(&self, request: WriteRequest) -> io::Result<()> {
        self.tx
            .send(request)
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crossbeam_channel::{Receiver, Sender, bounded};

    use super::*;

    struct BlockingWriter {
        entered: Sender<()>,
        release: Receiver<()>,
    }

    impl Write for BlockingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.entered.send(());
            self.release
                .recv()
                .map_err(|_| io::Error::other("release channel closed"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct TrackedBuffer {
        bytes: Box<[u8]>,
        dropped: Sender<()>,
    }

    impl AsRef<[u8]> for TrackedBuffer {
        fn as_ref(&self) -> &[u8] {
            &self.bytes
        }
    }

    impl Drop for TrackedBuffer {
        fn drop(&mut self) {
            let _ = self.dropped.send(());
        }
    }

    #[test]
    fn owned_buffer_lives_until_the_real_pty_write_finishes() {
        let (entered_tx, entered_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(1);
        let (dropped_tx, dropped_rx) = bounded(1);
        let writer = PtyWriter::spawn(Box::new(BlockingWriter {
            entered: entered_tx,
            release: release_rx,
        }))
        .unwrap();

        writer
            .write_owned(TrackedBuffer {
                bytes: b"guarded".to_vec().into_boxed_slice(),
                dropped: dropped_tx,
            })
            .unwrap();
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer should enter the real write");
        assert!(
            dropped_rx.try_recv().is_err(),
            "the owned wrapper was released before write_all completed"
        );

        release_tx.send(()).unwrap();
        dropped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the owned wrapper should release after the write");
    }
}
