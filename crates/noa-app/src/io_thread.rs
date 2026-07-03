//! The single io thread: owns the `Pty` outright (it isn't `Sync`, so it
//! can't be shared behind an `Arc` with the main thread), reads `PtyEvent`s,
//! feeds bytes into the shared `Terminal` through one long-lived
//! `noa_vt::Stream`, drains any reply bytes the terminal queued back out to
//! the pty, and pokes the winit event loop to redraw. Resize and input
//! requests come in from the main thread over crossbeam channels.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use noa_core::GridSize;
use noa_grid::Terminal;
use noa_pty::{Pty, PtyWriter};
use winit::event_loop::EventLoopProxy;

use crate::events::UserEvent;
use crate::split_tree::PaneId;

pub(crate) type PtyInput = Box<[u8]>;

pub(crate) const PTY_INPUT_QUEUE_CAPACITY: usize = 1024;

/// Owned handle for stopping and joining a PTY io thread.
pub(crate) struct IoThreadHandle {
    shutdown_tx: Sender<()>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl IoThreadHandle {
    const JOIN_TIMEOUT: Duration = Duration::from_secs(2);

    pub(crate) fn shutdown_and_join(mut self) {
        let _ = self.shutdown_and_join_timeout(Self::JOIN_TIMEOUT);
    }

    fn shutdown_and_join_timeout(&mut self, timeout: Duration) -> bool {
        let _ = self.shutdown_tx.send(());
        let deadline = Instant::now() + timeout;
        while self.join.as_ref().is_some_and(|join| !join.is_finished())
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }

        let Some(join) = self.join.take() else {
            return true;
        };
        if !join.is_finished() {
            self.join = Some(join);
            log::warn!("pty io thread did not stop within {timeout:?}");
            return false;
        }
        if let Err(err) = join.join() {
            log::warn!("pty io thread panicked during shutdown: {err:?}");
            return false;
        }
        true
    }
}

pub(crate) fn input_channel() -> (Sender<PtyInput>, Receiver<PtyInput>) {
    crossbeam_channel::bounded(PTY_INPUT_QUEUE_CAPACITY)
}

struct TerminalOutput {
    pending_writes: Vec<u8>,
    pending_clipboard_writes: Vec<String>,
    synchronized_output: bool,
}

fn feed_terminal(
    terminal: &Arc<Mutex<Terminal>>,
    stream: &mut noa_vt::Stream,
    bytes: &[u8],
) -> TerminalOutput {
    let mut term = terminal.lock().expect("terminal mutex poisoned");
    stream.feed(bytes, &mut *term);
    TerminalOutput {
        pending_writes: term.take_pending_writes(),
        pending_clipboard_writes: term.take_pending_clipboard_writes(),
        synchronized_output: term.modes.synchronized_output(),
    }
}

fn write_pty_bytes(writer: &PtyWriter, bytes: &[u8]) {
    if let Err(err) = writer.write(bytes).and_then(|_| writer.flush()) {
        log::warn!("failed to write bytes to pty: {err}");
    }
}

fn should_request_redraw_after_terminal_output(output: &TerminalOutput) -> bool {
    !output.synchronized_output
}

/// Spawn the io thread, which takes ownership of `pty`. Returns immediately;
/// the thread runs until the pty exits or errors, or the event loop is gone.
pub fn spawn(
    pty: Pty,
    terminal: Arc<Mutex<Terminal>>,
    proxy: EventLoopProxy<UserEvent>,
    window_id: winit::window::WindowId,
    pane_id: PaneId,
    resize_rx: Receiver<GridSize>,
    input_rx: Receiver<PtyInput>,
) -> IoThreadHandle {
    let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
    let join = std::thread::spawn(move || {
        let writer = pty.writer();
        let mut stream = noa_vt::Stream::new();
        loop {
            crossbeam_channel::select! {
                recv(shutdown_rx) -> _ => break,
                recv(pty.event_rx()) -> msg => match msg {
                    Ok(noa_pty::PtyEvent::Data(bytes)) => {
                        let output = feed_terminal(&terminal, &mut stream, bytes.as_ref());
                        if !output.pending_writes.is_empty() {
                            write_pty_bytes(&writer, &output.pending_writes);
                        }
                        let should_redraw = should_request_redraw_after_terminal_output(&output);
                        for text in output.pending_clipboard_writes {
                            let _ = proxy.send_event(UserEvent::ClipboardWrite {
                                window_id,
                                pane_id,
                                text,
                            });
                        }
                        if should_redraw && proxy.send_event(UserEvent::Redraw(window_id, pane_id)).is_err() {
                            break; // event loop gone
                        }
                    }
                    Ok(noa_pty::PtyEvent::Exit(_)) | Ok(noa_pty::PtyEvent::Error(_)) => {
                        let _ = proxy.send_event(UserEvent::PtyExit(window_id, pane_id));
                        break;
                    }
                    Err(_) => break, // channel closed
                },
                recv(resize_rx) -> msg => match msg {
                    Ok(size) => {
                        let _ = pty.resize(size);
                    }
                    Err(_) => break, // main thread / App dropped
                },
                recv(input_rx) -> msg => match msg {
                    Ok(bytes) => write_pty_bytes(&writer, bytes.as_ref()),
                    Err(_) => break, // main thread / App dropped
                },
            }
        }
    });
    IoThreadHandle {
        shutdown_tx,
        join: Some(join),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_terminal_returns_pending_writes_after_releasing_lock() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
        let mut stream = noa_vt::Stream::new();

        let output = feed_terminal(&terminal, &mut stream, b"\x1b[6n");

        assert_eq!(output.pending_writes, b"\x1b[1;1R");
        assert!(output.pending_clipboard_writes.is_empty());
        assert!(!output.synchronized_output);
        assert!(
            terminal.try_lock().is_ok(),
            "terminal lock must be released before PTY writes"
        );
    }

    #[test]
    fn synchronized_output_suppresses_redraw_until_release() {
        let terminal = Arc::new(Mutex::new(Terminal::new(GridSize::new(80, 24))));
        let mut stream = noa_vt::Stream::new();

        let output = feed_terminal(&terminal, &mut stream, b"\x1b[?2026hhidden");

        assert!(output.synchronized_output);
        assert!(!should_request_redraw_after_terminal_output(&output));

        let output = feed_terminal(&terminal, &mut stream, b"\x1b[?2026l");

        assert!(!output.synchronized_output);
        assert!(should_request_redraw_after_terminal_output(&output));
    }

    #[test]
    fn input_channel_is_bounded_and_nonblocking_for_ui_thread() {
        fn input(bytes: &[u8]) -> PtyInput {
            bytes.to_vec().into_boxed_slice()
        }

        let (tx, rx) = input_channel();
        for _ in 0..PTY_INPUT_QUEUE_CAPACITY {
            tx.try_send(input(b"x")).expect("queue has capacity");
        }

        match tx.try_send(input(b"y")) {
            Err(crossbeam_channel::TrySendError::Full(bytes)) => {
                assert_eq!(bytes.as_ref(), b"y");
            }
            other => panic!("expected a full input queue, got {other:?}"),
        }
        assert_eq!(rx.len(), PTY_INPUT_QUEUE_CAPACITY);
    }

    #[test]
    fn io_thread_handle_shutdown_joins_within_timeout() {
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
        let join = std::thread::spawn(move || {
            let _ = shutdown_rx.recv();
        });
        let mut handle = IoThreadHandle {
            shutdown_tx,
            join: Some(join),
        };

        assert!(handle.shutdown_and_join_timeout(Duration::from_millis(500)));
        assert!(handle.join.is_none());
    }

    #[test]
    fn pane_io_thread_shutdown_joins_all_blocked_handles_within_timeout() {
        let mut handles = Vec::new();
        for _ in 0..3 {
            let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
            let join = std::thread::spawn(move || {
                let _ = shutdown_rx.recv();
            });
            handles.push(IoThreadHandle {
                shutdown_tx,
                join: Some(join),
            });
        }

        for handle in &mut handles {
            assert!(handle.shutdown_and_join_timeout(Duration::from_millis(500)));
            assert!(handle.join.is_none());
        }
    }
}
