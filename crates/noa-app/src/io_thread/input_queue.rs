//! Main-thread → io-thread pty input queueing: the bounded channel plus its
//! ordered overflow buffer for bursts (huge pastes) the channel can't absorb.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use parking_lot::Mutex;

pub(crate) type PtyInput = Box<[u8]>;

pub(crate) const PTY_INPUT_QUEUE_CAPACITY: usize = 1024;

/// Ceiling on all bytes pending for one pane, including both the bounded
/// channel and its overflow buffer. A message-count-only limit is insufficient:
/// raw attach accepts 1 MiB messages, so 1024 channel slots could otherwise pin
/// roughly 1 GiB before the overflow limit was even consulted.
pub(super) const PTY_INPUT_PENDING_BYTE_CAP: usize = 8 * 1024 * 1024;
/// Small frames are charged at least this much so container/allocation
/// overhead is bounded along with payload bytes.
pub(super) const PTY_INPUT_PENDING_MIN_CHARGE: usize = 1024;

pub(crate) fn input_channel() -> (PtyInputQueue, Receiver<QueuedPtyInput>) {
    let (tx, rx) = crossbeam_channel::bounded(PTY_INPUT_QUEUE_CAPACITY);
    (PtyInputQueue::new(tx), rx)
}

/// Input plus a shared byte-budget reservation. The reservation follows the
/// bytes through the channel, overflow queue, and PTY writer queue and is
/// released only after the real PTY write completes (or the bytes are dropped).
pub(crate) struct QueuedPtyInput {
    bytes: PtyInput,
    pending_bytes: Arc<AtomicUsize>,
    charge: usize,
}

impl QueuedPtyInput {
    fn reserve(input: PtyInput, pending_bytes: Arc<AtomicUsize>) -> Result<Self, PtyInput> {
        let charge = input.len().max(PTY_INPUT_PENDING_MIN_CHARGE);
        if pending_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(charge)
                    .filter(|next| *next <= PTY_INPUT_PENDING_BYTE_CAP)
            })
            .is_err()
        {
            return Err(input);
        }
        Ok(Self {
            bytes: input,
            pending_bytes,
            charge,
        })
    }
}

impl AsRef<[u8]> for QueuedPtyInput {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

impl Drop for QueuedPtyInput {
    fn drop(&mut self) {
        self.pending_bytes.fetch_sub(self.charge, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueInputResult {
    Queued,
    Deferred,
    /// The pane is at [`PTY_INPUT_PENDING_BYTE_CAP`]; the input was discarded
    /// rather than queued or parked.
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
    tx: Sender<QueuedPtyInput>,
    overflow: Arc<Mutex<InputOverflow>>,
    pending_bytes: Arc<AtomicUsize>,
}

#[derive(Default)]
struct InputOverflow {
    queue: std::collections::VecDeque<QueuedPtyInput>,
    drainer_active: bool,
}

impl InputOverflow {
    fn park(&mut self, input: QueuedPtyInput) {
        self.queue.push_back(input);
    }

    fn pop(&mut self) -> Option<QueuedPtyInput> {
        self.queue.pop_front()
    }

    fn reset(&mut self) {
        self.queue.clear();
        self.drainer_active = false;
    }
}

impl PtyInputQueue {
    fn new(tx: Sender<QueuedPtyInput>) -> Self {
        PtyInputQueue {
            tx,
            overflow: Arc::new(Mutex::new(InputOverflow::default())),
            pending_bytes: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Reserve `input` against this pane's shared byte budget without routing
    /// it through the io thread's input channel, returning the reservation
    /// wrapper for the caller to hand straight to the PTY writer (the
    /// main-thread input fast path — keystrokes must not wait out the io
    /// thread's output batch). `None` means the pane is at
    /// [`PTY_INPUT_PENDING_BYTE_CAP`] and the input must be dropped, exactly
    /// as [`queue`](Self::queue) would return [`QueueInputResult::Dropped`].
    /// The returned wrapper shares this pane's budget with `queue`, so both
    /// paths are capped together.
    pub(crate) fn reserve(&self, input: PtyInput) -> Option<QueuedPtyInput> {
        QueuedPtyInput::reserve(input, Arc::clone(&self.pending_bytes)).ok()
    }

    /// Queue `input` behind every byte accepted before it, blocking never.
    pub(crate) fn queue(&self, input: PtyInput) -> QueueInputResult {
        let Ok(input) = QueuedPtyInput::reserve(input, Arc::clone(&self.pending_bytes)) else {
            return QueueInputResult::Dropped;
        };
        let mut overflow = self.overflow.lock();
        if overflow.drainer_active {
            overflow.park(input);
            return QueueInputResult::Deferred;
        }
        match self.tx.try_send(input) {
            Ok(()) => QueueInputResult::Queued,
            Err(TrySendError::Full(input)) => {
                overflow.park(input);
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
