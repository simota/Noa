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
/// iteration (legacy blocking-fd path only). Only paid while the consumer is
/// at least [`COALESCE_THRESHOLD`] behind, so it delays nothing user-visible.
const COALESCE_POLL_MS: libc::c_int = 2;

/// Nonblocking-drain refill bridge: after the kernel tty queue runs dry
/// mid-chunk *while the producer is streaming*, retry the read this many
/// times (a `spin_loop` hint in between) before emitting the partial chunk.
/// The writing child is woken by the drain itself and refills the ~1KiB
/// queue within a scheduler quantum; a few retries bridge that gap and keep
/// chunks near [`READ_CHUNK`] instead of near the 1KiB kernel quantum —
/// every sliver chunk otherwise costs a channel send/wake *and* a full
/// terminal-lock parse hold downstream (measured: a 150MB flood emitted
/// ~105K sliver chunks, avg 1.5KiB, with the bridge gated on downstream
/// congestion only). "Streaming" is judged by the *last read's size*: a full
/// kernel quantum (≥[`STREAMING_READ_MIN`]) means the queue was brimming, so
/// more is imminent; a partial read means the burst's tail (a finished
/// frame, an interactive echo) — emit immediately, preserving reply latency
/// for request/response loops like DOOM-fire's DSR sync. Ghostty's
/// io-gather uses the same bounded spin (`bridge_spin_max`) with a
/// burst-size saturation gate for the same reason.
///
/// Every retry is `spin_loop`-hinted, never `yield_now`: with the apply
/// pipeline's worker threads runnable, `sched_yield` deschedules the reader
/// for a real quantum, the ~1KiB tty queue brims, the producer blocks, and
/// the whole drain stretches (wish#4: yielding retries cost ~32% of reader
/// time in `swtch_pri` and ~10% of end-to-end drain throughput on the
/// 140x40 CRLF staircase — 292 → 324-330 MB/s once removed). A refill gap
/// the spins can't cover just emits the partial chunk: smaller handoff,
/// but the reader stays on-core and keeps draining.
const REFILL_SPIN_MAX: u32 = 16;

/// A single read at or above this size counts as "the kernel queue was full
/// when drained" — the macOS pty output queue hands out at most ~1KiB per
/// read, so a full-quantum read implies the producer is ahead of us.
const STREAMING_READ_MIN: usize = 1024;

#[cfg(unix)]
fn poll_fd_events(fd: std::os::fd::RawFd, events: libc::c_short, timeout_ms: libc::c_int) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    // SAFETY: `pfd` is a valid pollfd for the duration of the call.
    let ready = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    ready > 0 && (pfd.revents & (events | libc::POLLHUP | libc::POLLERR)) != 0
}

#[cfg(unix)]
fn poll_readable(fd: std::os::fd::RawFd, timeout_ms: libc::c_int) -> bool {
    poll_fd_events(fd, libc::POLLIN, timeout_ms)
}

/// Block until the pty master is writable again (nonblocking writer path).
/// Returns on POLLOUT/POLLHUP/POLLERR — the follow-up `write` surfaces the
/// actual error, so a `false` here (EINTR-style poll failure) just retries.
#[cfg(unix)]
pub(crate) fn wait_writable(fd: std::os::fd::RawFd) {
    while !poll_fd_events(fd, libc::POLLOUT, 100) {}
}

/// Spawn a thread that reads from `reader` until EOF/error, forwarding data
/// chunks as [`PtyEvent::Data`]. On read error it emits [`PtyEvent::Error`].
/// EOF (`read == 0`) simply ends the loop; child exit is reported by the
/// wait thread so the exit code is accurate.
///
/// `poll_fd` is an owned dup of the pty master used only to test readability
/// (never read from). With `nonblocking` set (the master description carries
/// `O_NONBLOCK`), the loop drains the kernel tty queue until `EAGAIN` and
/// parks in a single `poll` when idle — full coalescing at ~2 syscalls per
/// KiB quantum instead of the legacy poll-before-every-read gate. Without it
/// (fd unavailable / non-unix) it degrades to the legacy blocking loop.
pub(crate) fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    #[cfg_attr(not(unix), allow(unused_variables))] poll_fd: Option<std::os::fd::OwnedFd>,
    #[cfg_attr(not(unix), allow(unused_variables))] nonblocking: bool,
    tx: Sender<PtyEvent>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("noa-pty-reader".into())
        .spawn(move || {
            // Latency-elevated QoS (macOS): this thread's wake latency *is*
            // the pty drain rate — the kernel tty queue holds ~1KiB, so
            // every scheduling delay while other pipeline threads are
            // runnable directly blocks the producing child's `write`.
            //
            // USER_INITIATED, deliberately not USER_INTERACTIVE: the tier
            // only needs to outrank the UTILITY pack workers so the reader
            // is never displaced from a performance core by batch packing.
            // The top tier was measured to change nothing on the drain
            // (tbench-faithful S1 411.7 / S4 380.4 MB/s at either level)
            // while taxing everything else at spawn time — an
            // INTERACTIVE-pinned reader shifts Apple Silicon's P/E
            // placement against the just-forked shell's init and regressed
            // real cmd.overhead from ~1284 to 1830-2050µs (ship-gate
            // finding).
            #[cfg(target_os = "macos")]
            // SAFETY: plain FFI call configuring the calling thread's own QoS.
            unsafe {
                libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0);
            }
            let pool = Arc::new(ReadBufferPool::new(READ_CHUNK, READ_BUFFER_POOL_CAPACITY));
            let gate = Arc::new(FlowGate::new());
            // Chunk-size/congestion debug tap; checked once, not per chunk.
            let trace = std::env::var_os("NOA_PTY_READER_TRACE").is_some();
            #[cfg(unix)]
            match poll_fd {
                Some(fd) if nonblocking => {
                    drain_nonblocking(&mut reader, fd, &tx, &pool, &gate, trace)
                }
                other => drain_blocking(&mut reader, other, &tx, &pool, &gate, trace),
            }
            #[cfg(not(unix))]
            drain_blocking(&mut reader, &tx, &pool, &gate, trace);
        })
}

/// Nonblocking drain loop (macOS/unix with `O_NONBLOCK` on the master): read
/// until `EAGAIN`, chunk full, or EOF; park in one `poll(POLLIN)` only when
/// the queue is truly empty. Under congestion a bounded yield-retry bridges
/// the child's refill gap so chunks stay near [`READ_CHUNK`]; interactive
/// slivers are emitted on the first `EAGAIN`, preserving input latency.
#[cfg(unix)]
fn drain_nonblocking(
    reader: &mut Box<dyn Read + Send>,
    fd: std::os::fd::OwnedFd,
    tx: &Sender<PtyEvent>,
    pool: &Arc<ReadBufferPool>,
    gate: &Arc<FlowGate>,
    trace: bool,
) {
    use std::os::fd::AsRawFd as _;
    let raw = fd.as_raw_fd();
    // Whether the previous chunk ended while the producer was still
    // streaming (its last read filled a kernel quantum). Carried across
    // chunk boundaries so the empty-queue moment right after a mid-flood
    // handoff spins briefly instead of paying a full poll-park + wake —
    // two context switches per ~1KiB kernel quantum otherwise.
    let mut was_streaming = false;
    loop {
        let mut buf = pool.take();
        let mut n = 0usize;
        let mut eof = false;
        let mut pending_error = None;
        let mut spins = 0u32;
        let mut streaming = false;
        loop {
            match reader.read(&mut buf[n..]) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(m) => {
                    n += m;
                    spins = 0;
                    streaming = m >= STREAMING_READ_MIN;
                    if n == buf.len() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if n == 0 {
                        if was_streaming && spins < REFILL_SPIN_MAX {
                            // Mid-flood refill gap at a chunk boundary:
                            // bridge it on-core like the mid-chunk case.
                            spins += 1;
                            std::hint::spin_loop();
                            continue;
                        }
                        was_streaming = false;
                        // Idle: park until the next byte (or hangup) arrives.
                        poll_readable(raw, -1);
                        continue;
                    }
                    let congested =
                        gate.level() >= COALESCE_THRESHOLD || tx.len() >= COALESCE_SLOT_THRESHOLD;
                    if (streaming || congested) && spins < REFILL_SPIN_MAX {
                        spins += 1;
                        std::hint::spin_loop();
                        continue;
                    }
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    pending_error = Some(e);
                    break;
                }
            }
        }
        was_streaming = streaming;
        if n == 0 && !eof && pending_error.is_none() {
            pool.recycle(buf);
            continue;
        }
        if trace {
            eprintln!(
                "[pty-reader] chunk={n}B gate={}B queued={} eof={eof}",
                gate.level(),
                tx.len(),
            );
        }
        if n > 0 {
            gate.add(n);
            let chunk = PtyData::pooled(buf, n, Arc::clone(pool), Some(Arc::clone(gate)));
            if tx.send(PtyEvent::Data(chunk)).is_err() {
                // Receiver dropped; nothing more to do.
                return;
            }
        } else {
            pool.recycle(buf);
        }
        if let Some(e) = pending_error {
            let _ = tx.send(PtyEvent::Error(e));
            return;
        }
        if eof {
            return;
        }
        // Byte-budget backpressure (see `IN_FLIGHT_BYTE_BUDGET`).
        gate.wait_below(IN_FLIGHT_BYTE_BUDGET);
    }
}

/// Legacy blocking-fd loop: one blocking read per chunk, with a
/// poll-gated top-up while the consumer is measurably behind.
fn drain_blocking(
    reader: &mut Box<dyn Read + Send>,
    #[cfg(unix)] poll_fd: Option<std::os::fd::OwnedFd>,
    tx: &Sender<PtyEvent>,
    pool: &Arc<ReadBufferPool>,
    gate: &Arc<FlowGate>,
    trace: bool,
) {
    loop {
        let mut buf = pool.take();
        let mut n = match reader.read(&mut buf) {
            Ok(0) => {
                pool.recycle(buf);
                return;
            }
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                pool.recycle(buf);
                continue;
            }
            Err(e) => {
                pool.recycle(buf);
                let _ = tx.send(PtyEvent::Error(e));
                return;
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
                && (gate.level() >= COALESCE_THRESHOLD || tx.len() >= COALESCE_SLOT_THRESHOLD)
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
        let chunk = PtyData::pooled(buf, n, Arc::clone(pool), Some(Arc::clone(gate)));
        if tx.send(PtyEvent::Data(chunk)).is_err() {
            // Receiver dropped; nothing more to do.
            return;
        }
        if let Some(e) = pending_error {
            let _ = tx.send(PtyEvent::Error(e));
            return;
        }
        if eof {
            return;
        }
        // Byte-budget backpressure (see `IN_FLIGHT_BYTE_BUDGET`).
        gate.wait_below(IN_FLIGHT_BYTE_BUDGET);
    }
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
        let handle = spawn_reader(
            Box::new(ScriptedReader { steps: step_rx }),
            None,
            false,
            event_tx,
        )
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
        let handle = spawn_reader(
            Box::new(ScriptedReader { steps: step_rx }),
            None,
            false,
            event_tx,
        )
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
        let handle = spawn_reader(
            Box::new(ScriptedReader { steps: step_rx }),
            None,
            false,
            event_tx,
        )
        .expect("spawn reader");

        step_tx.send(ReadStep::Data(b"orphaned")).unwrap();
        handle.join().unwrap();
    }
}
