//! Main-thread → io-thread pty input queueing: the bounded channel plus its
//! ordered overflow buffer for bursts (huge pastes) the channel can't absorb.

use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, TrySendError};
use parking_lot::Mutex;

pub(crate) type PtyInput = Box<[u8]>;

pub(crate) const PTY_INPUT_QUEUE_CAPACITY: usize = 1024;

/// Ceiling on bytes parked in a pane's input overflow buffer. The overflow
/// absorbs a burst (huge paste) faster than the program reads; against a
/// program that *stops* reading it would otherwise grow without bound. Writes
/// past the cap are dropped — by then the target has ignored 64 MiB of input,
/// so preserving order matters more than preserving the excess.
pub(super) const PTY_INPUT_OVERFLOW_BYTE_CAP: usize = 64 * 1024 * 1024;

pub(crate) fn input_channel() -> (PtyInputQueue, Receiver<PtyInput>) {
    let (tx, rx) = crossbeam_channel::bounded(PTY_INPUT_QUEUE_CAPACITY);
    (PtyInputQueue::new(tx), rx)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueInputResult {
    Queued,
    Deferred,
    /// The overflow buffer is at [`PTY_INPUT_OVERFLOW_BYTE_CAP`]; the input
    /// was discarded rather than parked.
    Dropped,
    Disconnected,
}

/// Main-thread handle for queueing input bytes to a pane's io thread.
///
/// Wraps the bounded channel with an ordered overflow buffer: when the
/// channel is full (a huge paste against a program that isn't reading), the
/// excess parks in `overflow`, one spillover thread drains it with blocking
/// sends, and every later write joins the back of the buffer until it is
/// empty — so input bytes can never overtake each other. (A detached
/// blocking `send` per overflowing write, racing later `try_send`s, could
/// land typed keys in the middle of a deferred paste.)
#[derive(Clone)]
pub(crate) struct PtyInputQueue {
    tx: Sender<PtyInput>,
    overflow: Arc<Mutex<InputOverflow>>,
}

#[derive(Default)]
struct InputOverflow {
    queue: std::collections::VecDeque<PtyInput>,
    /// Sum of `queue`'s element lengths, enforced against
    /// [`PTY_INPUT_OVERFLOW_BYTE_CAP`].
    queued_bytes: usize,
    drainer_active: bool,
}

impl InputOverflow {
    /// Park `input` unless doing so would exceed the byte cap; reports whether
    /// it was accepted.
    fn park(&mut self, input: PtyInput) -> bool {
        if self.queued_bytes.saturating_add(input.len()) > PTY_INPUT_OVERFLOW_BYTE_CAP {
            return false;
        }
        self.queued_bytes += input.len();
        self.queue.push_back(input);
        true
    }

    fn pop(&mut self) -> Option<PtyInput> {
        let input = self.queue.pop_front()?;
        self.queued_bytes -= input.len();
        Some(input)
    }

    fn reset(&mut self) {
        self.queue.clear();
        self.queued_bytes = 0;
        self.drainer_active = false;
    }
}

impl PtyInputQueue {
    pub(crate) fn new(tx: Sender<PtyInput>) -> Self {
        PtyInputQueue {
            tx,
            overflow: Arc::new(Mutex::new(InputOverflow::default())),
        }
    }

    /// Queue `input` behind every byte accepted before it, blocking never.
    pub(crate) fn queue(&self, input: PtyInput) -> QueueInputResult {
        let mut overflow = self.overflow.lock();
        if overflow.drainer_active {
            return if overflow.park(input) {
                QueueInputResult::Deferred
            } else {
                QueueInputResult::Dropped
            };
        }
        match self.tx.try_send(input) {
            Ok(()) => QueueInputResult::Queued,
            Err(TrySendError::Full(input)) => {
                if !overflow.park(input) {
                    return QueueInputResult::Dropped;
                }
                overflow.drainer_active = true;
                drop(overflow);
                let drainer = self.clone();
                match std::thread::Builder::new()
                    .name("noa-pty-input-send".to_string())
                    .spawn(move || drainer.drain_overflow())
                {
                    Ok(_) => QueueInputResult::Deferred,
                    Err(err) => {
                        log::warn!("failed to spawn the pty input spillover thread: {err}");
                        self.overflow.lock().reset();
                        QueueInputResult::Disconnected
                    }
                }
            }
            Err(TrySendError::Disconnected(_)) => QueueInputResult::Disconnected,
        }
    }

    /// Spillover-thread body: forward parked inputs to the channel in order,
    /// blocking on capacity, until the buffer drains (or the io thread hangs
    /// up). `drainer_active` flips off under the same lock that observed the
    /// empty buffer, so a concurrent `queue` either sees the flag and parks
    /// behind us or sees the buffer already empty and sends directly.
    fn drain_overflow(&self) {
        loop {
            let input = {
                let mut overflow = self.overflow.lock();
                match overflow.pop() {
                    Some(input) => input,
                    None => {
                        overflow.drainer_active = false;
                        return;
                    }
                }
            };
            if self.tx.send(input).is_err() {
                self.overflow.lock().reset();
                return;
            }
        }
    }
}
