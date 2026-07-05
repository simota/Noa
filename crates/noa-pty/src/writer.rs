//! The writable end of a [`Pty`](crate::Pty).

use std::io::{self, Write};
use std::sync::Arc;

use parking_lot::Mutex;

/// A cloneable, sendable handle for writing input bytes to the PTY master.
///
/// Writing sends data to the slave (the child's stdin). The underlying writer
/// is shared behind a mutex so multiple clones can safely interleave writes.
#[derive(Clone)]
pub struct PtyWriter {
    inner: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl PtyWriter {
    pub(crate) fn new(writer: Box<dyn Write + Send>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(writer)),
        }
    }

    /// Write all of `data` to the PTY master.
    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        let mut w = self.inner.lock();
        w.write_all(data)
    }

    /// Flush any buffered bytes to the PTY master.
    pub fn flush(&self) -> io::Result<()> {
        let mut w = self.inner.lock();
        w.flush()
    }
}

impl std::fmt::Debug for PtyWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyWriter").finish_non_exhaustive()
    }
}
