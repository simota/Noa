//! The `noa-timeline` service thread (`docs/specs/board-timeline.md` D1,
//! L2-5): owns the [`Store`] and answers jobs one at a time, so this
//! process has one writer per board without any lock of its own. Calls
//! reach it from IPC connection threads over a bounded channel; the UI
//! thread and the io/PTY threads never touch it.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded};
use noa_ipc::Panel;
use noa_ipc::timeline::{TimelineCall, TimelineError, TimelineErrorKind};
use serde_json::Value;

use super::store::{RealIo, Store, StoreIo, StoreLimits};

/// Jobs waiting for the service; a full queue answers `busy`.
const QUEUE_CAPACITY: usize = 64;

/// How long a call waits for its answer: under the client's 5 s control
/// timeout, so the reply (or `outcome_unknown`) arrives before it gives up.
const SERVICE_WAIT: Duration = Duration::from_secs(4);

pub(crate) const THREAD_NAME: &str = "noa-timeline";

pub(crate) struct Job {
    call: TimelineCall,
    panels: Arc<Vec<Panel>>,
    reply: Sender<Result<Value, TimelineError>>,
}

/// The sending side of the service. Cheap to clone.
#[derive(Clone)]
pub(crate) struct TimelineHandle {
    tx: Sender<Job>,
    /// Set by the service once it tried to open the store.
    opened: Arc<OnceLock<bool>>,
    /// Disconnected once `opened` is set: what [`TimelineHandle::opened`]
    /// waits on.
    settled: Receiver<()>,
}

/// Starts the service on `root` (the boards directory), opened with this
/// process's server instance id. Only the thread is started here: the
/// directory is created and the boards are replayed on the service thread,
/// so the caller (the UI thread) does no file I/O. Whether the store
/// opened is [`TimelineHandle::opened`].
pub(crate) fn start(root: PathBuf) -> io::Result<TimelineHandle> {
    start_with(
        root,
        noa_ipc::server::server_instance_id().to_owned(),
        StoreLimits::default(),
        Box::new(RealIo),
        |_| {},
    )
}

/// [`start`] with its parts injectable; `prepare` sees the store before the
/// first job (tests install probes).
pub(crate) fn start_with(
    root: PathBuf,
    instance_id: String,
    limits: StoreLimits,
    io: Box<dyn StoreIo>,
    prepare: impl FnOnce(&mut Store) + Send + 'static,
) -> io::Result<TimelineHandle> {
    let (tx, rx) = bounded::<Job>(QUEUE_CAPACITY);
    let opened = Arc::new(OnceLock::new());
    let (settle, settled) = bounded::<()>(0);
    let result = Arc::clone(&opened);
    thread::Builder::new()
        .name(THREAD_NAME.to_owned())
        .spawn(move || {
            // The directory first, answered at once: that decides whether
            // the capability is advertised. The boards are replayed after,
            // while calls already queue.
            let store = Store::create(root, instance_id, limits, io);
            let _ = result.set(store.is_ok());
            drop(settle);
            let mut store = match store {
                Ok(store) => store,
                Err(err) => {
                    log::warn!("timeline: store not opened, board timeline disabled: {err}");
                    return;
                }
            };
            if let Err(err) = store.scan() {
                log::warn!("timeline: boards not loaded ({err}); the next call tries again");
            }
            prepare(&mut store);
            serve(&rx, &mut store);
        })?;
    Ok(TimelineHandle {
        tx,
        opened,
        settled,
    })
}

fn serve(rx: &Receiver<Job>, store: &mut Store) {
    for job in rx {
        let result = store.handle(job.call, &job.panels);
        // The caller may have timed out and gone; nothing to do then.
        let _ = job.reply.send(result);
    }
}

impl TimelineHandle {
    /// Whether the store opened: its directory exists (made by the service
    /// if not) and took its `0700` `chmod`, which a read-only volume or
    /// another owner's directory refuses. Waits up to [`SERVICE_WAIT`]
    /// while the service is still finding out (it creates one directory),
    /// so it is for connection threads (`noa.hello`), not the UI thread.
    pub(crate) fn opened(&self) -> bool {
        if self.opened.get().is_none() {
            let _ = self.settled.recv_timeout(SERVICE_WAIT);
        }
        self.opened.get().copied().unwrap_or(false)
    }

    /// One call, answered by the service: `busy` when its queue is full or
    /// a read waits too long; `outcome_unknown` (with the request id to
    /// send again) when a write does, since it may still be applied.
    pub(crate) fn call(
        &self,
        call: TimelineCall,
        panels: Arc<Vec<Panel>>,
    ) -> Result<Value, TimelineError> {
        self.call_within(call, panels, SERVICE_WAIT)
    }

    fn call_within(
        &self,
        call: TimelineCall,
        panels: Arc<Vec<Panel>>,
        wait: Duration,
    ) -> Result<Value, TimelineError> {
        let write = call.is_write();
        let request_id = call.request_id().map(str::to_owned);
        let (reply, answer) = bounded(1);
        match self.tx.try_send(Job {
            call,
            panels,
            reply,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(TimelineError::new(
                    TimelineErrorKind::Busy,
                    "the timeline service is busy; try again",
                ));
            }
            Err(TrySendError::Disconnected(_)) => return Err(stopped()),
        }
        match answer.recv_timeout(wait) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) if write => {
                let mut err = TimelineError::new(
                    TimelineErrorKind::OutcomeUnknown,
                    "the timeline service did not answer in time; the write may still land, \
                     send it again with the same requestId",
                );
                err.request_id = request_id;
                Err(err)
            }
            Err(RecvTimeoutError::Timeout) => Err(TimelineError::new(
                TimelineErrorKind::Busy,
                "the timeline service did not answer in time; try again",
            )),
            Err(RecvTimeoutError::Disconnected) => Err(stopped()),
        }
    }
}

fn stopped() -> TimelineError {
    TimelineError::new(TimelineErrorKind::IoError, "the timeline service stopped")
}

#[cfg(test)]
impl TimelineHandle {
    pub(crate) fn call_with_wait(
        &self,
        call: TimelineCall,
        panels: Arc<Vec<Panel>>,
        wait: Duration,
    ) -> Result<Value, TimelineError> {
        self.call_within(call, panels, wait)
    }
}
