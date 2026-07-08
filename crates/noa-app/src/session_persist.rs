//! Off-main-thread session persistence.
//!
//! `App::persist_session` runs on every topology change (spawn/close/drag/…),
//! and a session-restore or close-group burst fires it once per tab. Writing
//! the file inline would block the main thread on disk I/O each time, so the
//! capture (which must read main-thread state) stays on the caller and only
//! the serialize+write moves here. The worker coalesces a queued burst down
//! to its latest state — intermediate snapshots are dead the moment a newer
//! one exists.
//!
//! Durability: dropping the handle closes the channel; the worker drains
//! what's queued (keeping only the newest state) and writes it before
//! exiting, and `Drop` joins the thread. `App` owns the handle, so the winit
//! `exiting` hook's final `persist_session` is always flushed to disk before
//! the process ends. Only a hard crash can lose an in-flight write — the same
//! window the previous synchronous version had, minus the disk latency.

use std::path::PathBuf;
use std::thread::JoinHandle;

use crate::session::{self, SessionState};

pub struct SessionPersister {
    tx: Option<crossbeam_channel::Sender<(PathBuf, SessionState)>>,
    worker: Option<JoinHandle<()>>,
}

impl SessionPersister {
    pub fn spawn() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<(PathBuf, SessionState)>();
        let worker = std::thread::Builder::new()
            .name("session-persist".to_string())
            .spawn(move || {
                while let Ok(mut job) = rx.recv() {
                    // Coalesce a burst to its newest snapshot before writing.
                    while let Ok(newer) = rx.try_recv() {
                        job = newer;
                    }
                    let (path, state) = job;
                    if let Err(err) = session::save(&path, &state) {
                        log::warn!("failed to save session state: {err}");
                    }
                }
            })
            .expect("failed to spawn the session-persist thread");
        Self {
            tx: Some(tx),
            worker: Some(worker),
        }
    }

    /// Queue `state` for an atomic write to `path`. Never blocks.
    pub fn save(&self, path: PathBuf, state: SessionState) {
        if let Some(tx) = self.tx.as_ref() {
            let _ = tx.send((path, state));
        }
    }

    /// Flush the newest queued state and stop the worker. After this returns,
    /// later `save` calls are ignored.
    pub fn flush(&mut self) {
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for SessionPersister {
    fn drop(&mut self) {
        // Close the channel so the worker drains the queue and exits, then
        // join so the final state is on disk before the process ends.
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A burst of saves must land the *latest* state on disk after drop.
    #[test]
    fn drop_flushes_the_newest_queued_state() {
        let dir = std::env::temp_dir().join(format!(
            "noa-session-persist-test-{}",
            std::process::id()
        ));
        let path = dir.join("session.json");
        let persister = SessionPersister::spawn();
        for generation in 0..10usize {
            let state = SessionState {
                windows: Vec::new(),
                focused_window: Some(generation),
            };
            persister.save(path.clone(), state);
        }
        drop(persister);
        let restored = session::load(&path).expect("session file written");
        assert_eq!(restored.focused_window, Some(9));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_flush_writes_the_newest_queued_state() {
        let dir = std::env::temp_dir().join(format!(
            "noa-session-persist-flush-test-{}",
            std::process::id()
        ));
        let path = dir.join("session.json");
        let mut persister = SessionPersister::spawn();
        for generation in 0..10usize {
            let state = SessionState {
                windows: Vec::new(),
                focused_window: Some(generation),
            };
            persister.save(path.clone(), state);
        }
        persister.flush();
        let restored = session::load(&path).expect("session file written");
        assert_eq!(restored.focused_window, Some(9));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
