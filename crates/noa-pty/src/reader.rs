//! Background threads: one drains the PTY master into [`PtyEvent::Data`]
//! chunks, another waits on the child and reports its exit code.

use std::io::Read;
use std::sync::Arc;

use crossbeam_channel::Sender;
use portable_pty::Child;

use crate::data::{FlowGate, ReadBufferPool};
use crate::{PtyData, PtyEvent};

/// Size of each read buffer chunk (bytes).
const READ_CHUNK: usize = 64 * 1024;
const READ_BUFFER_POOL_CAPACITY: usize = 16;

/// Hard cap on pty output bytes in flight between this reader and the
/// consumer (the io thread — or, for a prespawned pane, nobody yet). Chosen
/// so a bulk flood arriving while the app is still booting (pty prespawn:
/// the child runs during font/window/GPU init, before any consumer exists)
/// is absorbed instead of blocking the child on a few MiB of channel slack,
/// while a runaway flood can never grow past this. Transient: buffers past
/// the pool's small cap are freed as soon as the consumer drops them.
const IN_FLIGHT_BYTE_BUDGET: usize = 32 * 1024 * 1024;

/// In-flight level above which the consumer is definitively behind (it drains
/// up to 1 MiB per parse batch) and the reader switches to coalescing reads:
/// a pty read returns at most the tty output queue (a few KiB on macOS), so
/// under flood a plain read-per-event loop emits sliver chunks 16x smaller
/// than [`READ_CHUNK`] — and every sliver costs the io thread a full
/// lock/fair-unlock parse hold. Below this level nothing changes: interactive
/// output (keystroke echoes, prompt repaints, DSR replies) is forwarded the
/// instant it is read.
const COALESCE_THRESHOLD: usize = 1024 * 1024;

/// Queued-event count above which the reader coalesces regardless of the
/// byte level. A macOS pty read returns at most 1 KiB per read, so a flood
/// with no (or a busy) consumer fills the event channel's 1024 slots with
/// sliver chunks at ~1 MiB total — blocking the reader on `send` right at
/// [`COALESCE_THRESHOLD`]'s doorstep and locking coalescing out. Slot
/// pressure is the earlier, chunk-size-independent congestion signal.
const COALESCE_SLOT_THRESHOLD: usize = 64;

/// How long a coalescing top-up waits for the next kernel readability, per
/// iteration. Only paid while the consumer is at least
/// [`COALESCE_THRESHOLD`] behind, so it delays nothing user-visible.
const COALESCE_POLL_MS: libc::c_int = 2;

#[cfg(unix)]
fn poll_readable(fd: std::os::fd::RawFd, timeout_ms: libc::c_int) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `pfd` is a valid pollfd for the duration of the call.
    let ready = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    ready > 0 && (pfd.revents & libc::POLLIN) != 0
}

/// Spawn a thread that reads from `reader` until EOF/error, forwarding data
/// chunks as [`PtyEvent::Data`]. On read error it emits [`PtyEvent::Error`].
/// EOF (`read == 0`) simply ends the loop; child exit is reported by the
/// wait thread so the exit code is accurate.
///
/// `poll_fd` is an owned dup of the pty master used only to test readability
/// (never read from), enabling the congestion coalescing described on
/// [`COALESCE_THRESHOLD`]; `None` (fd unavailable) degrades to the plain
/// read-per-event loop.
pub(crate) fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    #[cfg_attr(not(unix), allow(unused_variables))] poll_fd: Option<std::os::fd::OwnedFd>,
    tx: Sender<PtyEvent>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("noa-pty-reader".into())
        .spawn(move || {
            let pool = Arc::new(ReadBufferPool::new(READ_CHUNK, READ_BUFFER_POOL_CAPACITY));
            let gate = Arc::new(FlowGate::new());
            // Chunk-size/congestion debug tap; checked once, not per chunk.
            let trace = std::env::var_os("NOA_PTY_READER_TRACE").is_some();
            loop {
                let mut buf = pool.take();
                let mut n = match reader.read(&mut buf) {
                    Ok(0) => {
                        pool.recycle(buf);
                        break;
                    }
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                        pool.recycle(buf);
                        continue;
                    }
                    Err(e) => {
                        pool.recycle(buf);
                        let _ = tx.send(PtyEvent::Error(e));
                        break;
                    }
                };
                // Congestion coalescing: while the consumer is measurably
                // behind, top this chunk up toward a full `READ_CHUNK` from
                // reads that are immediately (well, within a poll tick)
                // available, instead of emitting tty-queue-sized slivers.
                let mut eof = false;
                let mut pending_error = None;
                #[cfg(unix)]
                if let Some(fd) = &poll_fd {
                    use std::os::fd::AsRawFd as _;
                    while n < buf.len()
                        && (gate.level() >= COALESCE_THRESHOLD
                            || tx.len() >= COALESCE_SLOT_THRESHOLD)
                        && poll_readable(fd.as_raw_fd(), COALESCE_POLL_MS)
                    {
                        match reader.read(&mut buf[n..]) {
                            Ok(0) => {
                                eof = true;
                                break;
                            }
                            Ok(m) => n += m,
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                            Err(e) => {
                                pending_error = Some(e);
                                break;
                            }
                        }
                    }
                }
                if trace {
                    eprintln!(
                        "[pty-reader] chunk={n}B gate={}B queued={} eof={eof}",
                        gate.level(),
                        tx.len(),
                    );
                }
                gate.add(n);
                let chunk = PtyData::pooled(buf, n, Arc::clone(&pool), Some(Arc::clone(&gate)));
                if tx.send(PtyEvent::Data(chunk)).is_err() {
                    // Receiver dropped; nothing more to do.
                    break;
                }
                if let Some(e) = pending_error {
                    let _ = tx.send(PtyEvent::Error(e));
                    break;
                }
                if eof {
                    break;
                }
                // Byte-budget backpressure (see `IN_FLIGHT_BYTE_BUDGET`).
                gate.wait_below(IN_FLIGHT_BYTE_BUDGET);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind, Result};

    enum ReadStep {
        Data(&'static [u8]),
        Error(ErrorKind),
        Eof,
    }

    struct ScriptedReader {
        steps: crossbeam_channel::Receiver<ReadStep>,
    }

    impl Read for ScriptedReader {
        fn read(&mut self, out: &mut [u8]) -> Result<usize> {
            match self.steps.recv().expect("scripted read step") {
                ReadStep::Data(bytes) => {
                    out[..bytes.len()].copy_from_slice(bytes);
                    Ok(bytes.len())
                }
                ReadStep::Error(kind) => Err(Error::new(kind, "scripted read error")),
                ReadStep::Eof => Ok(0),
            }
        }
    }

    #[test]
    fn reader_reuses_returned_payload_buffers() {
        let (step_tx, step_rx) = crossbeam_channel::bounded(1);
        let (event_tx, event_rx) = crossbeam_channel::bounded(1);
        let handle = spawn_reader(Box::new(ScriptedReader { steps: step_rx }), None, event_tx)
            .expect("spawn reader");

        step_tx.send(ReadStep::Data(b"abc")).unwrap();
        let first = match event_rx.recv().unwrap() {
            PtyEvent::Data(bytes) => bytes,
            other => panic!("expected data, got {other:?}"),
        };
        let first_ptr = first.as_ptr();
        assert_eq!(first.as_ref(), b"abc");
        drop(first);

        step_tx.send(ReadStep::Data(b"def")).unwrap();
        let second = match event_rx.recv().unwrap() {
            PtyEvent::Data(bytes) => bytes,
            other => panic!("expected data, got {other:?}"),
        };
        assert_eq!(second.as_ref(), b"def");
        drop(second);

        step_tx.send(ReadStep::Data(b"ghi")).unwrap();
        let third = match event_rx.recv().unwrap() {
            PtyEvent::Data(bytes) => bytes,
            other => panic!("expected data, got {other:?}"),
        };
        assert_eq!(third.as_ref(), b"ghi");
        assert_eq!(third.as_ptr(), first_ptr);
        drop(third);

        step_tx.send(ReadStep::Eof).unwrap();
        handle.join().unwrap();
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn reader_emits_error_and_exits() {
        let (step_tx, step_rx) = crossbeam_channel::bounded(1);
        let (event_tx, event_rx) = crossbeam_channel::bounded(1);
        let handle = spawn_reader(Box::new(ScriptedReader { steps: step_rx }), None, event_tx)
            .expect("spawn reader");

        step_tx
            .send(ReadStep::Error(ErrorKind::PermissionDenied))
            .unwrap();
        match event_rx.recv().unwrap() {
            PtyEvent::Error(e) => assert_eq!(e.kind(), ErrorKind::PermissionDenied),
            other => panic!("expected error, got {other:?}"),
        }
        handle.join().unwrap();
    }

    #[test]
    fn reader_exits_when_receiver_is_dropped() {
        let (step_tx, step_rx) = crossbeam_channel::bounded(1);
        let (event_tx, event_rx) = crossbeam_channel::bounded(1);
        drop(event_rx);
        let handle = spawn_reader(Box::new(ScriptedReader { steps: step_rx }), None, event_tx)
            .expect("spawn reader");

        step_tx.send(ReadStep::Data(b"orphaned")).unwrap();
        handle.join().unwrap();
    }
}
