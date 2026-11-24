//! Log statistics: an indexing thread keeps an incremental per-hour index of
//! the agents' local logs (read-only), and `report` aggregates it for a period
//! and an agent filter. See `docs/specs/agent-usage.md` (L1-3, L2-6).

pub mod types;

mod aggregate;
mod agy;
mod claude;
mod codex;
mod index;
mod journal;
pub mod prices;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub use aggregate::report;
pub use index::Index;

use crate::paths::Roots;

/// A pass is re-run this often (stat only; changed files parsed).
const RESCAN_EVERY: Duration = Duration::from_secs(60);

struct Shared {
    index: Mutex<Arc<Index>>,
    stop: Arc<AtomicBool>,
}

/// Owns the indexing thread; dropping it stops and joins the thread.
pub struct StatsHandle {
    shared: Arc<Shared>,
    rescan: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

/// Starts the indexing thread and returns at once: loading the on-disk index
/// and every pass run on that thread. `wake` is called after each new index
/// is published.
pub fn start(roots: Roots, wake: Arc<dyn Fn() + Send + Sync>) -> StatsHandle {
    start_with_home(roots, dirs::home_dir(), wake)
}

fn start_with_home(
    roots: Roots,
    home: Option<std::path::PathBuf>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> StatsHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(Shared {
        index: Mutex::new(Arc::new(Index::default())),
        stop: stop.clone(),
    });
    let (tx, rx) = mpsc::channel::<()>();
    let worker = shared.clone();
    let thread = std::thread::Builder::new()
        .name("agent-usage-stats".into())
        .spawn(move || {
            let mut publish = |index: Index| {
                *worker.index.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(index);
                wake();
            };
            let mut indexer = index::Indexer::new(roots, home, stop.clone());
            publish(indexer.snapshot());
            while !stop.load(Ordering::Relaxed) {
                indexer.pass(&mut publish);
                match rx.recv_timeout(RESCAN_EVERY) {
                    Ok(()) => while rx.try_recv().is_ok() {},
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .ok();
    StatsHandle {
        shared,
        rescan: Some(tx),
        thread,
    }
}

impl StatsHandle {
    /// Runs a pass now (or right after the running one).
    pub fn rescan(&self) {
        if let Some(tx) = &self.rescan {
            // The thread only goes away when the handle drops.
            let _ = tx.send(());
        }
    }

    /// The latest index; a cheap `Arc` clone.
    pub fn index(&self) -> Arc<Index> {
        self.shared
            .index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Drop for StatsHandle {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.rescan.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;
    use testutil::*;

    #[test]
    fn large_tree_is_indexed_off_the_calling_thread() {
        let t = Tree::new();
        let mut lines = Vec::new();
        for i in 0..400 {
            lines.extend(claude_turn_and_reply(i * 10, &format!("m{i}"), 1));
        }
        for f in 0..200 {
            write(
                &t.claude_file(&format!("-p{}", f % 20), &format!("s{f}.jsonl")),
                &lines,
            );
        }
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        let began = Instant::now();
        let handle = start_with_home(
            t.roots(),
            None,
            Arc::new(move || {
                w.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let returned = began.elapsed();
        let first = handle.index();
        assert!(
            first.coverage().files_done < 200 || first.generation() == 0,
            "start returned after the pass ({returned:?})"
        );
        let deadline = Instant::now() + Duration::from_secs(120);
        let done = loop {
            let ix = handle.index();
            let cov = ix.coverage();
            if !cov.running && cov.files_done == 200 {
                break ix;
            }
            assert!(Instant::now() < deadline, "indexing did not finish");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(done.coverage().files_total, 200);
        assert_eq!(sum(&done).messages, 200 * 400);
        assert!(wakes.load(Ordering::SeqCst) >= 2);
        // A rescan of an unchanged tree finds nothing new and keeps the counts.
        let before = done.generation();
        handle.rescan();
        let deadline = Instant::now() + Duration::from_secs(30);
        while handle.index().generation() < before + 2 {
            assert!(Instant::now() < deadline, "rescan did not run");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(sum(&handle.index()).messages, 200 * 400);
        drop(handle);
        assert!(crate::paths::index_path(&t.own).is_file());
    }
}
