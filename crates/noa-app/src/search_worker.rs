//! Latest-query search on immutable snapshots; no history scan on the UI thread.

use noa_grid::Terminal;
use parking_lot::{Condvar, Mutex};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

const DEBOUNCE: Duration = Duration::from_millis(35);

struct Job {
    terminal: Weak<Mutex<Terminal>>,
    query: String,
    generation: u64,
    notify: Box<dyn Fn() + Send>,
}

#[derive(Default)]
struct Pending {
    job: Option<Job>,
    shutdown: bool,
}

#[derive(Default)]
struct Shared {
    pending: Mutex<Pending>,
    ready: Condvar,
    generation: AtomicU64,
}

pub(crate) struct SearchWorker {
    shared: Arc<Shared>,
}

impl SearchWorker {
    pub(crate) fn new() -> std::io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let work = shared.clone();
        std::thread::Builder::new()
            .name("noa-search".into())
            .spawn(move || run(work))?;
        Ok(Self { shared })
    }

    pub(crate) fn submit(
        &self,
        terminal: Weak<Mutex<Terminal>>,
        query: String,
        notify: impl Fn() + Send + 'static,
    ) {
        let mut pending = self.shared.pending.lock();
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        pending.job = Some(Job {
            terminal,
            query,
            generation,
            notify: Box::new(notify),
        });
        self.shared.ready.notify_one();
    }

    pub(crate) fn cancel(&self) {
        let mut pending = self.shared.pending.lock();
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        pending.job = None;
        self.shared.ready.notify_one();
    }
}

impl Drop for SearchWorker {
    fn drop(&mut self) {
        let mut pending = self.shared.pending.lock();
        pending.shutdown = true;
        pending.job = None;
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        self.shared.ready.notify_one();
    }
}

fn run(shared: Arc<Shared>) {
    loop {
        let job = {
            let mut pending = shared.pending.lock();
            while pending.job.is_none() && !pending.shutdown {
                shared.ready.wait(&mut pending);
            }
            if pending.shutdown {
                return;
            }
            // Each edit restarts debounce; the slot retains at most one query.
            while !shared.ready.wait_for(&mut pending, DEBOUNCE).timed_out() {
                if pending.shutdown {
                    return;
                }
            }
            let Some(job) = pending.job.take() else {
                continue;
            };
            job
        };
        let cancelled = || shared.generation.load(Ordering::Acquire) != job.generation;
        while !cancelled() {
            let Some(terminal) = job.terminal.upgrade() else {
                break;
            };
            let (snapshot, space) = {
                let terminal = terminal.lock();
                if cancelled() {
                    break;
                }
                (
                    terminal.active().search_snapshot(),
                    terminal.grid_coordinate_generation(),
                )
            };
            let Some(matches) = snapshot.find_matches(&job.query, cancelled) else {
                break;
            };
            // Allocate the result's shared backing outside the terminal lock.
            let matches = Arc::from(matches.into_boxed_slice());
            let applied = {
                let mut terminal = terminal.lock();
                // Serialize publication with submit/cancel so a stale result
                // cannot race past a newer query or restore cleared highlights.
                let pending = shared.pending.lock();
                if pending.shutdown || cancelled() {
                    break;
                }
                terminal.grid_coordinate_generation() == space
                    && terminal.apply_search_snapshot(&snapshot, job.query.clone(), matches)
            };
            if applied {
                (job.notify)();
                break;
            }
            // Output changed during the scan. Wait briefly before retrying,
            // keeping a busy producer from driving a search spin loop.
            let mut pending = shared.pending.lock();
            if pending.shutdown || cancelled() {
                break;
            }
            shared.ready.wait_for(&mut pending, DEBOUNCE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_query_wins_and_submission_does_not_lock_the_terminal() {
        let terminal = Arc::new(Mutex::new(Terminal::new(noa_core::GridSize::new(20, 3))));
        noa_vt::Stream::new().feed(b"first latest", &mut *terminal.lock());
        let worker = SearchWorker::new().unwrap();
        let guard = terminal.lock();
        let (old_tx, old_rx) = crossbeam_channel::bounded(1);
        worker.submit(Arc::downgrade(&terminal), "first".into(), move || {
            let _ = old_tx.send(());
        });
        let (tx, rx) = crossbeam_channel::bounded(1);
        worker.submit(Arc::downgrade(&terminal), "latest".into(), move || {
            let _ = tx.send(());
        });
        drop(guard);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(old_rx.try_recv().is_err());
        let terminal = terminal.lock();
        assert_eq!(terminal.active().search.query(), "latest");
        assert_eq!(terminal.active().search.matches().len(), 1);
    }

    #[test]
    fn cancellation_cannot_restore_a_cleared_query() {
        let terminal = Arc::new(Mutex::new(Terminal::new(noa_core::GridSize::new(20, 3))));
        let worker = SearchWorker::new().unwrap();
        let guard = terminal.lock();
        let (tx, rx) = crossbeam_channel::bounded(1);
        worker.submit(Arc::downgrade(&terminal), "old".into(), move || {
            let _ = tx.send(());
        });
        worker.cancel();
        drop(guard);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert!(terminal.lock().active().search.query().is_empty());
    }
}
