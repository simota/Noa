//! Serializes command-time snapshots in request order. Only the completion
//! runs on the event loop, where clipboard and window operations belong.

use super::{ActiveEventLoop, App, UserEvent};
use std::sync::mpsc::{self, Receiver, Sender};

type Completion = Box<dyn FnOnce(&mut App, &ActiveEventLoop) + Send>;
type Job = Box<dyn FnOnce() -> Completion + Send>;

pub(super) struct TextWorker {
    jobs: Sender<Job>,
    completed: Receiver<Completion>,
    pending: usize,
}

impl TextWorker {
    fn new(wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let (jobs, requests) = mpsc::channel::<Job>();
        // The event loop may be in a native dialog. Do not accumulate full
        // scrollback strings there while it is unable to consume them.
        let (results, completed) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("noa-copy".into())
            .spawn(move || {
                for job in requests {
                    if results.send(job()).is_err() {
                        break;
                    }
                    wake();
                }
            })?;
        Ok(Self {
            jobs,
            completed,
            pending: 0,
        })
    }

    fn submit<T: Send + 'static>(
        &mut self,
        work: impl FnOnce() -> T + Send + 'static,
        finish: impl FnOnce(&mut App, &ActiveEventLoop, T) + Send + 'static,
    ) -> Result<(), mpsc::SendError<Job>> {
        self.jobs.send(Box::new(move || {
            let result = work();
            Box::new(move |app, event_loop| finish(app, event_loop, result))
        }))?;
        self.pending += 1;
        Ok(())
    }
}

impl App {
    pub(super) fn queue_text_work<T: Send + 'static>(
        &mut self,
        work: impl FnOnce() -> T + Send + 'static,
        finish: impl FnOnce(&mut App, &ActiveEventLoop, T) + Send + 'static,
    ) {
        if self.text_worker.is_none() {
            let proxy = self.proxy.clone();
            match TextWorker::new(move || {
                let _ = proxy.send_event(UserEvent::TextReady);
            }) {
                Ok(worker) => self.text_worker = Some(worker),
                Err(err) => {
                    log::warn!("could not start text worker: {err}");
                    return;
                }
            }
        }
        let worker = self.text_worker.as_mut().unwrap();
        if worker.submit(work, finish).is_err() {
            log::warn!("text worker stopped");
            self.text_worker = None;
        }
    }

    pub(super) fn after_pending_text(&mut self, action: impl FnOnce(&mut App) + Send + 'static) {
        if self
            .text_worker
            .as_ref()
            .is_some_and(|worker| worker.pending > 0)
        {
            self.queue_text_work(|| (), move |app, _, ()| action(app));
        } else {
            action(self);
        }
    }

    pub(super) fn finish_text_work(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(worker) = &mut self.text_worker
            && let Ok(finish) = worker.completed.try_recv()
        {
            worker.pending -= 1;
            finish(self, event_loop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_worker_reads_command_time_text_in_order_while_terminal_is_locked() {
        let mut worker = TextWorker::new(|| {}).unwrap();
        let terminal =
            std::sync::Mutex::new(noa_grid::Terminal::new(noa_core::GridSize::new(8, 1)));
        let mut term = terminal.lock().unwrap();
        let mut stream = noa_vt::Stream::new();
        let (tx, rx) = mpsc::channel();
        for content in [b"first".as_slice(), b"second"] {
            stream.feed(b"\r\x1b[2K", &mut *term);
            stream.feed(content, &mut *term);
            term.select_all();
            let snapshot = term.active().selection_snapshot().unwrap();
            let tx = tx.clone();
            worker
                .submit(
                    move || {
                        tx.send((std::thread::current().id(), snapshot.into_text().unwrap()))
                            .unwrap();
                    },
                    |_, _, ()| {},
                )
                .unwrap();
        }
        for expected in ["first", "second"] {
            let (thread, text) = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            assert_ne!(thread, std::thread::current().id());
            assert_eq!(text, expected);
        }
        drop(term);
    }
}
