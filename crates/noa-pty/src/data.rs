use std::ops::Deref;
use std::sync::Arc;

use parking_lot::Mutex;

/// Bytes read from the PTY master.
///
/// Dropping values produced by the reader thread returns their backing buffer
/// to a small bounded pool. Manually constructed values simply own their bytes.
pub struct PtyData {
    buf: Option<Vec<u8>>,
    len: usize,
    pool: Option<Arc<ReadBufferPool>>,
}

impl PtyData {
    pub(crate) fn pooled(buf: Vec<u8>, len: usize, pool: Arc<ReadBufferPool>) -> Self {
        debug_assert!(len <= buf.len());
        Self {
            buf: Some(buf),
            len,
            pool: Some(pool),
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
            let data = PtyData::pooled(vec![b'x'; 8], 3, Arc::clone(&pool));
            assert_eq!(data.as_ref(), b"xxx");
            assert_eq!(pool.len(), 0);
            data.as_ptr()
        };
        assert_eq!(pool.len(), 1);

        let reused = pool.take();
        assert_eq!(reused.as_ptr(), first_ptr);
        pool.recycle(reused);
        PtyData::pooled(vec![b'y'; 8], 3, Arc::clone(&pool));
        assert_eq!(pool.len(), 1, "pool keeps only the configured spare buffer");
    }
}
