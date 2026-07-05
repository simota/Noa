//! Background threads: one drains the PTY master into [`PtyEvent::Data`]
//! chunks, another waits on the child and reports its exit code.

use std::io::Read;

use crossbeam_channel::Sender;
use portable_pty::Child;

use crate::PtyEvent;

/// Size of each read buffer chunk (bytes).
const READ_CHUNK: usize = 64 * 1024;

/// Spawn a thread that reads from `reader` until EOF/error, forwarding data
/// chunks as [`PtyEvent::Data`]. On read error it emits [`PtyEvent::Error`].
/// EOF (`read == 0`) simply ends the loop; child exit is reported by the
/// wait thread so the exit code is accurate.
pub(crate) fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    tx: Sender<PtyEvent>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("noa-pty-reader".into())
        .spawn(move || {
            let mut buf = vec![0u8; READ_CHUNK];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        let chunk: Box<[u8]> = buf[..n].into();
                        if tx.send(PtyEvent::Data(chunk)).is_err() {
                            // Receiver dropped; nothing more to do.
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        let _ = tx.send(PtyEvent::Error(e));
                        break;
                    }
                }
            }
        })
}

/// Spawn a thread that blocks on the child, then emits [`PtyEvent::Exit`]
/// with its exit code (signal terminations report code 1).
pub(crate) fn spawn_waiter(
    mut child: Box<dyn Child + Send + Sync>,
    tx: Sender<PtyEvent>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("noa-pty-waiter".into())
        .spawn(move || {
            let code = match child.wait() {
                Ok(status) => status.exit_code() as i32,
                Err(_) => -1,
            };
            let _ = tx.send(PtyEvent::Exit(code));
        })
}
