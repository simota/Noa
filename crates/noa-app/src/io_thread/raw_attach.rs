//! Lossless raw PTY output tap used by the dedicated attach channel.
//!
//! This is intentionally separate from [`super::ipc_tap::IpcOutputTap`]:
//! `noa.output` is a lossy, throttled row-diff notification, while an attach
//! stream is byte-faithful and disconnects when backpressure cannot clear.

use std::sync::Arc;

use parking_lot::Mutex;

use noa_grid::Terminal;
use noa_ipc::AttachOutputSender;
use noa_vt::SharedParser;

use super::input_queue::{PtyInputQueue, QueueInputResult};

pub(super) trait RawAttachOutput: Send + Sync {
    fn send(&self, bytes: Vec<u8>) -> bool;
    fn close(&self);
}

struct ServerAttachOutput(AttachOutputSender);

impl RawAttachOutput for ServerAttachOutput {
    fn send(&self, bytes: Vec<u8>) -> bool {
        self.0.send(bytes).is_ok()
    }

    fn close(&self) {
        self.0.close();
    }
}

#[derive(Clone)]
struct ActiveAttach {
    generation: u64,
    output: Arc<dyn RawAttachOutput>,
}

#[derive(Default)]
struct RawAttachState {
    closed: bool,
    active: Option<ActiveAttach>,
}

/// Pane-owned raw attach state shared by the app backend and the PTY io
/// thread. The state mutex is never held while output can block.
#[derive(Clone)]
pub(crate) struct RawAttachTap {
    state: Arc<Mutex<RawAttachState>>,
    parser: SharedParser,
}

impl Default for RawAttachTap {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(RawAttachState::default())),
            parser: SharedParser::default(),
        }
    }
}

impl RawAttachTap {
    /// Install one server generation. The caller holds the pane's Terminal
    /// lock so registration and synthetic seed generation form one atomic
    /// boundary with the io thread's feed path.
    pub(crate) fn register_and_seed(
        &self,
        generation: u64,
        output: AttachOutputSender,
        terminal: &Terminal,
    ) -> Result<Vec<u8>, ()> {
        self.register_output_and_seed(generation, Arc::new(ServerAttachOutput(output)), terminal)
    }

    fn register_output(&self, generation: u64, output: Arc<dyn RawAttachOutput>) -> Result<(), ()> {
        let replaced = {
            let mut state = self.state.lock();
            if state.closed {
                return Err(());
            }
            state.active.replace(ActiveAttach { generation, output })
        };
        if let Some(replaced) = replaced {
            replaced.output.close();
        }
        Ok(())
    }

    fn register_output_and_seed(
        &self,
        generation: u64,
        output: Arc<dyn RawAttachOutput>,
        terminal: &Terminal,
    ) -> Result<Vec<u8>, ()> {
        let pending = self.parser.pending_bytes().ok_or(())?;
        self.register_output(generation, output)?;
        let mut seed = terminal.synthetic_seed();
        seed.extend_from_slice(&pending);
        Ok(seed)
    }

    pub(super) fn parser(&self) -> SharedParser {
        self.parser.clone()
    }

    /// Clone the currently active generation's sink. Called while the same
    /// Terminal lock that parsed the bytes is still held; sending happens
    /// only after that lock is released.
    pub(super) fn sink(&self) -> Option<RawAttachSink> {
        let active = self.state.lock().active.clone()?;
        Some(RawAttachSink {
            generation: active.generation,
            output: active.output,
        })
    }

    /// Enqueue attach input only while `generation` is still active. Holding
    /// this short state lock through the nonblocking queue operation makes a
    /// stale socket unable to race a detach/new-generation transition.
    pub(crate) fn queue_input(
        &self,
        generation: u64,
        input: &PtyInputQueue,
        bytes: &[u8],
    ) -> Result<QueueInputResult, ()> {
        let state = self.state.lock();
        if state.closed
            || state
                .active
                .as_ref()
                .is_none_or(|active| active.generation != generation)
        {
            return Err(());
        }
        Ok(input.queue(bytes.to_vec().into_boxed_slice()))
    }

    pub(crate) fn detach(&self, generation: u64) -> bool {
        let detached = {
            let mut state = self.state.lock();
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.generation == generation)
            {
                state.active.take()
            } else {
                None
            }
        };
        if let Some(detached) = detached {
            detached.output.close();
            true
        } else {
            false
        }
    }

    pub(crate) fn is_available(&self) -> bool {
        !self.state.lock().closed
    }

    /// Permanently close this pane endpoint. Unlike `detach`, a later
    /// generation cannot register after pane teardown.
    pub(crate) fn shutdown(&self) {
        let active = {
            let mut state = self.state.lock();
            state.closed = true;
            state.active.take()
        };
        if let Some(active) = active {
            active.output.close();
        }
    }

    #[cfg(test)]
    pub(super) fn register_test(
        &self,
        generation: u64,
        output: Arc<dyn RawAttachOutput>,
    ) -> Result<(), ()> {
        self.register_output(generation, output)
    }

    #[cfg(test)]
    pub(super) fn register_test_and_seed(
        &self,
        generation: u64,
        output: Arc<dyn RawAttachOutput>,
        terminal: &Terminal,
    ) -> Result<Vec<u8>, ()> {
        self.register_output_and_seed(generation, output, terminal)
    }
}

pub(super) struct RawAttachSink {
    generation: u64,
    output: Arc<dyn RawAttachOutput>,
}

impl RawAttachSink {
    pub(super) fn send(self, tap: &RawAttachTap, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if !self.output.send(bytes.to_vec()) {
            tap.detach(self.generation);
        }
    }
}
