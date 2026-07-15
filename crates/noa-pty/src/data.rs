use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};

/// Bytes read from the PTY master.
///
/// Dropping values produced by the reader thread returns their backing buffer
/// to a small bounded pool. Manually constructed values simply own their bytes.
pub struct PtyData {
    buf: Option<Vec<u8>>,
    len: usize,
    pool: Option<Arc<ReadBufferPool>>,
    /// Byte-quantity flow gate this chunk is billed against; dropping the
    /// chunk (the consumer finished parsing it) credits the bytes back.
    gate: Option<Arc<FlowGate>>,
}

impl PtyData {
    /// A reader-produced chunk. `gate` is the reader's in-flight byte gate:
    /// the chunk's `len` was already added by the reader and is subtracted
    /// when this value drops.
    pub(crate) fn pooled(
        buf: Vec<u8>,
        len: usize,
        pool: Arc<ReadBufferPool>,
        gate: Option<Arc<FlowGate>>,
    ) -> Self {
        debug_assert!(len <= buf.len());
        Self {
            buf: Some(buf),
            len,
            pool: Some(pool),
            gate,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf.as_deref().unwrap_or(&[])[..self.len]
    }
}

impl AsRef<[u8]> for PtyData {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Deref for PtyData {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl From<Vec<u8>> for PtyData {
    fn from(buf: Vec<u8>) -> Self {
        let len = buf.len();
        Self {
            buf: Some(buf),
            len,
            pool: None,
            gate: None,
        }
    }
}

impl From<Box<[u8]>> for PtyData {
    fn from(buf: Box<[u8]>) -> Self {
        Vec::from(buf).into()
    }
}

impl std::fmt::Debug for PtyData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyData")
            .field("len", &self.len)
            .field("pooled", &self.pool.is_some())
            .finish()
    }
}

impl Drop for PtyData {
    fn drop(&mut self) {
        if let (Some(pool), Some(buf)) = (self.pool.as_ref(), self.buf.take()) {
            pool.recycle(buf);
        }
        if let Some(gate) = self.gate.take() {
            gate.sub(self.len);
        }
    }
}

/// Byte-quantity flow control between the pty reader thread and the io-thread
/// consumer, replacing what event-count backpressure alone used to provide.
///
/// The reader bills every queued chunk's bytes here ([`FlowGate::add`]) and
/// parks once the total in flight crosses its budget ([`FlowGate::wait_below`]);
/// the consumer credits bytes back simply by dropping the [`PtyData`]
/// ([`FlowGate::sub`], via `Drop`). Counting *bytes* instead of channel slots
/// matters because pty reads are tty-output-queue sized (a few KiB), so a
/// slot-count bound alternately over-buffers (slots × 64 KiB) and
/// under-buffers (slots × 4 KiB) depending on chunk fill — most visibly
/// before a pane's io thread has started consuming at all (pty prespawn),
/// where the child of a flood workload used to block on a few MiB of slack.
pub(crate) struct FlowGate {
    bytes: Mutex<usize>,
    below: Condvar,
}

impl FlowGate {
    pub(crate) fn new() -> Self {
        Self {
            bytes: Mutex::new(0),
            below: Condvar::new(),
        }
    }

    /// Bytes currently queued between reader and consumer.
    pub(crate) fn level(&self) -> usize {
        *self.bytes.lock()
    }

    pub(crate) fn add(&self, n: usize) {
        *self.bytes.lock() += n;
    }

    fn sub(&self, n: usize) {
        let mut bytes = self.bytes.lock();
        *bytes = bytes.saturating_sub(n);
        drop(bytes);
        self.below.notify_all();
    }

    /// Park until the in-flight total drops below `budget`. Wakes on every
    /// consumer-side drop. A consumer that disappears entirely still releases
    /// the reader: dropping the event `Receiver` discards its queued
    /// `PtyData`s (crossbeam discards on receiver disconnect), each of which
    /// credits this gate on drop; the reader's next `send` then observes the
    /// disconnect and exits. The periodic timeout is a plain re-check, never
    /// an escape — the budget is a hard cap.
    pub(crate) fn wait_below(&self, budget: usize) {
        let mut bytes = self.bytes.lock();
        while *bytes >= budget {
            let _ = self.below.wait_for(&mut bytes, Duration::from_secs(1));
        }
    }
}

pub(crate) struct ReadBufferPool {
    chunk_size: usize,
    max_buffers: usize,
    buffers: Mutex<Vec<Vec<u8>>>,
}

impl ReadBufferPool {
    pub(crate) fn new(chunk_size: usize, max_buffers: usize) -> Self {
        Self {
            chunk_size,
            max_buffers,
            buffers: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn take(&self) -> Vec<u8> {
        self.buffers
            .lock()
            .pop()
            .unwrap_or_else(|| vec![0; self.chunk_size])
    }

    pub(crate) fn recycle(&self, mut buf: Vec<u8>) {
        if buf.capacity() != self.chunk_size {
            return;
        }
        if buf.len() != self.chunk_size {
            buf.resize(self.chunk_size, 0);
        }
        let mut buffers = self.buffers.lock();
        if buffers.len() < self.max_buffers {
            buffers.push(buf);
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.buffers.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pooled_data_returns_buffer_on_drop_up_to_the_pool_cap() {
        let pool = Arc::new(ReadBufferPool::new(8, 1));
        let first_ptr = {
            let data = PtyData::pooled(vec![b'x'; 8], 3, Arc::clone(&pool), None);
            assert_eq!(data.as_ref(), b"xxx");
            assert_eq!(pool.len(), 0);
            data.as_ptr()
        };
        assert_eq!(pool.len(), 1);

        let reused = pool.take();
        assert_eq!(reused.as_ptr(), first_ptr);
        pool.recycle(reused);
        PtyData::pooled(vec![b'y'; 8], 3, Arc::clone(&pool), None);
        assert_eq!(pool.len(), 1, "pool keeps only the configured spare buffer");
    }

    #[test]
    fn dropping_gated_data_credits_the_flow_gate() {
        let pool = Arc::new(ReadBufferPool::new(8, 1));
        let gate = Arc::new(FlowGate::new());
        gate.add(5);
        let data = PtyData::pooled(vec![b'x'; 8], 5, Arc::clone(&pool), Some(Arc::clone(&gate)));
        assert_eq!(gate.level(), 5);
        drop(data);
        assert_eq!(gate.level(), 0, "drop returns the chunk's bytes");
    }

    #[test]
    fn wait_below_parks_until_a_consumer_drop_crosses_the_budget() {
        let pool = Arc::new(ReadBufferPool::new(8, 1));
        let gate = Arc::new(FlowGate::new());
        gate.add(10);
        let chunk = PtyData::pooled(
            vec![b'x'; 16],
            10,
            Arc::clone(&pool),
            Some(Arc::clone(&gate)),
        );

        let waiter = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || gate.wait_below(8))
        };
        // Not a strict proof of parking, but gives the waiter a chance to
        // reach the condvar before the credit lands.
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            !waiter.is_finished(),
            "10 in flight >= budget 8: reader parks"
        );
        drop(chunk); // credits 10 -> level 0 < 8, wakes the waiter
        waiter.join().expect("waiter returns after the credit");
        assert_eq!(gate.level(), 0);
    }
}
