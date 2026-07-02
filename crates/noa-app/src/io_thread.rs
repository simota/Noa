//! The single io thread: owns the `Pty` outright (it isn't `Sync`, so it
//! can't be shared behind an `Arc` with the main thread), reads `PtyEvent`s,
//! feeds bytes into the shared `Terminal` through one long-lived
//! `noa_vt::Stream`, drains any reply bytes the terminal queued back out to
//! the pty, and pokes the winit event loop to redraw. Resize requests come
//! in from the main thread over `resize_rx`.

use std::sync::{Arc, Mutex};

use crossbeam_channel::Receiver;
use noa_core::GridSize;
use noa_grid::Terminal;
use noa_pty::{Pty, PtyWriter};
use winit::event_loop::EventLoopProxy;

use crate::events::UserEvent;

struct TerminalOutput {
    pending_writes: Vec<u8>,
    pending_clipboard_writes: Vec<String>,
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
    }
}

/// Spawn the io thread, which takes ownership of `pty`. Returns immediately;
/// the thread runs until the pty exits or errors, or the event loop is gone.
pub fn spawn(
    pty: Pty,
    writer: PtyWriter,
    terminal: Arc<Mutex<Terminal>>,
    proxy: EventLoopProxy<UserEvent>,
    resize_rx: Receiver<GridSize>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut stream = noa_vt::Stream::new();
        loop {
            crossbeam_channel::select! {
                recv(pty.event_rx()) -> msg => match msg {
                    Ok(noa_pty::PtyEvent::Data(bytes)) => {
                        let output = feed_terminal(&terminal, &mut stream, bytes.as_ref());
                        if !output.pending_writes.is_empty() {
                            let _ = writer.write(&output.pending_writes);
                            let _ = writer.flush();
                        }
                        for text in output.pending_clipboard_writes {
                            let _ = proxy.send_event(UserEvent::ClipboardWrite(text));
                        }
                        if proxy.send_event(UserEvent::Redraw).is_err() {
                            break; // event loop gone
                        }
                    }
                    Ok(noa_pty::PtyEvent::Exit(_)) | Ok(noa_pty::PtyEvent::Error(_)) => {
                        let _ = proxy.send_event(UserEvent::PtyExit);
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
            }
        }
    })
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
        assert!(
            terminal.try_lock().is_ok(),
            "terminal lock must be released before PTY writes"
        );
    }
}
