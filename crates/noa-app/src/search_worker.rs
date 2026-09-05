//! Latest-query search on immutable snapshots; no history scan on the UI thread.

use noa_grid::Terminal;
use parking_lot::{Condvar, Mutex};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use crate::commands::SearchAction;

const DEBOUNCE: Duration = Duration::from_millis(35);

struct Job {
    terminal: Weak<Mutex<Terminal>>,
    screen_generation: u64,
    query: String,
    generation: u64,
    notify: Box<dyn Fn() + Send>,
}

#[derive(Default)]
struct Pending {
    job: Option<Job>,
    navigation: Option<PendingNavigation>,
    shutdown: bool,
}

struct PendingNavigation {
    terminal: Weak<Mutex<Terminal>>,
    screen_generation: u64,
    actions: Vec<SearchAction>,
}

#[derive(Default)]
struct Shared {
    pending: Mutex<Pending>,
    ready: Condvar,
    generation: AtomicU64,
    #[cfg(test)]
    after_snapshot: Mutex<Option<Box<dyn FnOnce() + Send>>>,
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
        screen_generation: u64,
        query: String,
        notify: impl Fn() + Send + 'static,
    ) {
        let mut pending = self.shared.pending.lock();
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        pending.navigation = Some(PendingNavigation {
            terminal: terminal.clone(),
            screen_generation,
            actions: Vec::new(),
        });
        pending.job = Some(Job {
            terminal,
            screen_generation,
            query,
            generation,
            notify: Box::new(notify),
        });
        self.shared.ready.notify_one();
    }

    /// Called under the target terminal lock, just like result publication.
    /// The request stays available after the worker takes the debounced job.
    pub(crate) fn queue_navigation(
        &self,
        terminal: &Arc<Mutex<Terminal>>,
        screen_generation: u64,
        action: SearchAction,
    ) -> bool {
        debug_assert!(matches!(
            action,
            SearchAction::FindNext | SearchAction::FindPrevious
        ));
        let mut pending = self.shared.pending.lock();
        let Some(navigation) = &mut pending.navigation else {
            return false;
        };
        if !navigation.terminal.ptr_eq(&Arc::downgrade(terminal))
            || navigation.screen_generation != screen_generation
        {
            return false;
        }
        navigation.actions.push(action);
        true
    }

    pub(crate) fn cancel(&self) {
        let mut pending = self.shared.pending.lock();
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        pending.job = None;
        pending.navigation = None;
        self.shared.ready.notify_one();
    }
}

impl Drop for SearchWorker {
    fn drop(&mut self) {
        let mut pending = self.shared.pending.lock();
        pending.shutdown = true;
        pending.job = None;
        pending.navigation = None;
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
                if cancelled() || terminal.screen_generation() != job.screen_generation {
                    break;
                }
                (
                    terminal.active().search_snapshot(),
                    terminal.grid_coordinate_generation(),
                )
            };
            #[cfg(test)]
            if let Some(after_snapshot) = shared.after_snapshot.lock().take() {
                after_snapshot();
            }
            let Some(matches) = snapshot.find_matches(&job.query, cancelled) else {
                break;
            };
            // Allocate the result's shared backing outside the terminal lock.
            let matches = Arc::from(matches.into_boxed_slice());
            let applied = {
                let mut terminal = terminal.lock();
                // Serialize publication with submit/cancel so a stale result
                // cannot race past a newer query or restore cleared highlights.
                let mut pending = shared.pending.lock();
                if pending.shutdown
                    || cancelled()
                    || terminal.screen_generation() != job.screen_generation
                {
                    break;
                }
                let applied = terminal.grid_coordinate_generation() == space
                    && terminal.apply_search_snapshot(&snapshot, job.query.clone(), matches);
                if applied && let Some(navigation) = pending.navigation.take() {
                    for action in navigation.actions {
                        match action {
                            SearchAction::FindNext => {
                                terminal.search_next();
                            }
                            SearchAction::FindPrevious => {
                                terminal.search_previous();
                            }
                            _ => unreachable!("only search navigation is queued"),
                        }
                    }
                }
                applied
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
        let mut pending = shared.pending.lock();
        if !cancelled() {
            pending.navigation = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pause_after_snapshot(
        worker: &SearchWorker,
    ) -> (
        crossbeam_channel::Receiver<()>,
        crossbeam_channel::Sender<()>,
    ) {
        let (reached_tx, reached_rx) = crossbeam_channel::bounded(1);
        let (resume_tx, resume_rx) = crossbeam_channel::bounded(1);
        *worker.shared.after_snapshot.lock() = Some(Box::new(move || {
            reached_tx.send(()).unwrap();
            resume_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }));
        (reached_rx, resume_tx)
    }

    #[test]
    fn screen_switch_during_search_discards_the_query() {
        for (setup, switch) in [
            ("", "\x1b[?1049h"),
            ("\x1b[?1049h", "\x1b[?1049l"),
            ("\x1b[?47h\x1b[?47l", "\x1b[?47h\x1b[?47l"),
            ("\x1b[?1049h", "\x1b[?1049h"),
            ("", "\x1bc"),
        ] {
            for during_scan in [false, true] {
                let terminal = Arc::new(Mutex::new(Terminal::new(noa_core::GridSize::new(20, 3))));
                let worker = SearchWorker::new().unwrap();
                let barrier = during_scan.then(|| pause_after_snapshot(&worker));
                let mut guard = terminal.lock();
                noa_vt::Stream::new().feed(setup.as_bytes(), &mut *guard);
                let (tx, rx) = crossbeam_channel::bounded(1);
                worker.submit(
                    Arc::downgrade(&terminal),
                    guard.screen_generation(),
                    "old".into(),
                    move || {
                        let _ = tx.send(());
                    },
                );
                if let Some((reached, _)) = &barrier {
                    drop(guard);
                    reached.recv_timeout(Duration::from_secs(2)).unwrap();
                    guard = terminal.lock();
                }
                assert!(worker.queue_navigation(
                    &terminal,
                    guard.screen_generation(),
                    SearchAction::FindNext
                ));
                noa_vt::Stream::new().feed(switch.as_bytes(), &mut *guard);
                assert!(!worker.queue_navigation(
                    &terminal,
                    guard.screen_generation(),
                    SearchAction::FindNext
                ));
                drop(guard);
                if let Some((_, resume)) = barrier {
                    resume.send(()).unwrap();
                }
                assert_eq!(
                    rx.recv_timeout(Duration::from_secs(2)),
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected),
                    "obsolete search must be discarded: setup={setup:?}, switch={switch:?}, during_scan={during_scan}",
                );
                assert!(terminal.lock().active().search.query().is_empty());
            }
        }
    }

    #[test]
    fn navigation_during_scan_survives_retry_after_output() {
        let terminal = Arc::new(Mutex::new(Terminal::new(noa_core::GridSize::new(20, 3))));
        noa_vt::Stream::new().feed(b"foo foo", &mut *terminal.lock());
        let worker = SearchWorker::new().unwrap();
        let (reached, resume) = pause_after_snapshot(&worker);
        let (tx, rx) = crossbeam_channel::bounded(1);
        worker.submit(
            Arc::downgrade(&terminal),
            terminal.lock().screen_generation(),
            "foo".into(),
            move || {
                tx.send(()).unwrap();
            },
        );
        reached.recv_timeout(Duration::from_secs(2)).unwrap();
        {
            let mut guard = terminal.lock();
            noa_vt::Stream::new().feed(b" foo", &mut *guard);
            assert!(worker.queue_navigation(
                &terminal,
                guard.screen_generation(),
                SearchAction::FindNext
            ));
            assert!(worker.queue_navigation(
                &terminal,
                guard.screen_generation(),
                SearchAction::FindNext
            ));
            assert!(guard.active().search.query().is_empty());
        }
        resume.send(()).unwrap();
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let guard = terminal.lock();
        assert_eq!(guard.active().search.matches().len(), 3);
        assert_eq!(guard.active().search.active_index(), Some(1));
        assert!(!worker.queue_navigation(
            &terminal,
            guard.screen_generation(),
            SearchAction::FindNext
        ));
    }

    #[test]
    fn latest_query_wins_and_submission_does_not_lock_the_terminal() {
        let terminal = Arc::new(Mutex::new(Terminal::new(noa_core::GridSize::new(20, 3))));
        noa_vt::Stream::new().feed(b"first latest latest", &mut *terminal.lock());
        let worker = SearchWorker::new().unwrap();
        let guard = terminal.lock();
        let (old_tx, old_rx) = crossbeam_channel::bounded(1);
        worker.submit(
            Arc::downgrade(&terminal),
            guard.screen_generation(),
            "first".into(),
            move || {
                let _ = old_tx.send(());
            },
        );
        assert!(worker.queue_navigation(
            &terminal,
            guard.screen_generation(),
            SearchAction::FindNext
        ));
        let (tx, rx) = crossbeam_channel::bounded(1);
        worker.submit(
            Arc::downgrade(&terminal),
            guard.screen_generation(),
            "latest".into(),
            move || {
                let _ = tx.send(());
            },
        );
        drop(guard);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(old_rx.try_recv().is_err());
        let terminal = terminal.lock();
        assert_eq!(terminal.active().search.query(), "latest");
        assert_eq!(terminal.active().search.matches().len(), 2);
        assert_eq!(terminal.active().search.active_index(), Some(1));
    }

    #[test]
    fn cancellation_cannot_restore_a_cleared_query() {
        let terminal = Arc::new(Mutex::new(Terminal::new(noa_core::GridSize::new(20, 3))));
        let worker = SearchWorker::new().unwrap();
        let guard = terminal.lock();
        let (tx, rx) = crossbeam_channel::bounded(1);
        worker.submit(
            Arc::downgrade(&terminal),
            guard.screen_generation(),
            "old".into(),
            move || {
                let _ = tx.send(());
            },
        );
        assert!(worker.queue_navigation(
            &terminal,
            guard.screen_generation(),
            SearchAction::FindNext
        ));
        worker.cancel();
        assert!(!worker.queue_navigation(
            &terminal,
            guard.screen_generation(),
            SearchAction::FindNext
        ));
        drop(guard);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert!(terminal.lock().active().search.query().is_empty());
    }
}
