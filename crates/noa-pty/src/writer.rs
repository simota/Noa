//! The writable end of a [`Pty`](crate::Pty).

use std::io::{self, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, TryRecvError};

/// How recently the last pty write must have completed for the writer
/// thread to spin-poll before parking. Mirrors the io thread's hot-traffic
/// spin (same rationale, same tail): during serialized query/reply traffic
/// the next reply is enqueued 20–40µs after the previous write, so a parked
/// wake (20–80µs of scheduler latency) would otherwise sit on every
/// round-trip's critical path. Human-typing rates never keep this window
/// hot, so an interactive-but-quiet pane parks exactly as before.
const HOT_SPIN_WINDOW: Duration = Duration::from_millis(2);
/// Upper bound on one pre-park spin; caps the wasted spin at the trailing
/// edge of a write burst.
const HOT_SPIN_BUDGET: Duration = Duration::from_micros(150);
/// A completed write at or under this size counts as interactive traffic
/// (keystrokes and query replies are a handful of bytes) and arms the hot
/// spin; bulk writes (large pastes) do not.
const HOT_SPIN_MAX_WRITE: usize = 256;

trait WriteBuffer: AsRef<[u8]> + Send {}

impl<T: AsRef<[u8]> + Send> WriteBuffer for T {}

pub const WRITE_BYTE_CAP: usize = 8 * 1024 * 1024;
pub const WRITE_MIN_CHARGE: usize = 1024;

/// One budget for user input and terminal replies, including in-flight writes.
#[derive(Clone, Default)]
pub struct PtyWriteBudget(Arc<AtomicUsize>);

struct WriteReservation {
    budget: PtyWriteBudget,
    charge: usize,
}

impl Drop for WriteReservation {
    fn drop(&mut self) {
        self.budget.0.fetch_sub(self.charge, Ordering::AcqRel);
    }
}

/// Keeps a byte reservation alive through upstream queues and the real write.
pub struct BudgetedWrite<T> {
    data: T,
    reservation: WriteReservation,
}

impl<T: AsRef<[u8]>> AsRef<[u8]> for BudgetedWrite<T> {
    fn as_ref(&self) -> &[u8] {
        self.data.as_ref()
    }
}

impl<T> BudgetedWrite<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> BudgetedWrite<U> {
        BudgetedWrite {
            data: f(self.data),
            reservation: self.reservation,
        }
    }
}

impl PtyWriteBudget {
    fn reserve_len(&self, len: usize) -> Option<WriteReservation> {
        let charge = len.max(WRITE_MIN_CHARGE);
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(charge)
                    .filter(|&next| next <= WRITE_BYTE_CAP)
            })
            .ok()?;
        Some(WriteReservation {
            budget: self.clone(),
            charge,
        })
    }

    pub fn reserve<T: AsRef<[u8]>>(&self, data: T) -> Result<BudgetedWrite<T>, T> {
        match self.reserve_len(data.as_ref().len()) {
            Some(reservation) => Ok(BudgetedWrite { data, reservation }),
            None => Err(data),
        }
    }
}

struct WriteRequest(BudgetedWrite<Box<dyn WriteBuffer>>);

impl AsRef<[u8]> for WriteRequest {
    fn as_ref(&self) -> &[u8] {
        self.0.data.as_ref().as_ref()
    }
}

/// A cloneable, sendable handle for writing input bytes to the PTY master.
///
/// Writes are queued to a dedicated writer thread rather than hitting the
/// master fd directly: a blocking `write_all` stalls whenever the child stops
/// reading stdin (the kernel tty input queue holds only ~1KB in raw mode on
/// macOS), and running that on the io read loop froze the pane — pty reads,
/// redraws, and resizes all stop — and could deadlock permanently once the
/// child also blocked writing output. Queueing keeps every caller
/// non-blocking; the single consumer preserves write order across clones.
#[derive(Clone)]
pub struct PtyWriter {
    tx: Sender<Option<WriteRequest>>,
    budget: PtyWriteBudget,
    stopped: Arc<AtomicBool>,
    #[cfg(unix)]
    wake: Arc<std::os::unix::net::UnixStream>,
}

impl PtyWriter {
    /// Spawn the writer thread that owns the master's write half. The thread
    /// exits when every `PtyWriter` clone is dropped (channel disconnect) or a
    /// write fails (child gone → EIO); a write blocked on a full tty input
    /// queue is unblocked by the child reading or by `Pty` shutdown, including
    /// when a descendant keeps the slave open after its parent exits.
    ///
    /// `poll_fd` is an owned dup of the pty master, required when the master
    /// description carries `O_NONBLOCK` (the reader's drain mode): a full tty
    /// input queue then surfaces as `WouldBlock` instead of blocking, and the
    /// thread parks in `poll(POLLOUT)` until the child drains it — the exact
    /// behavior a blocking `write_all` had, minus the busy error.
    pub(crate) fn spawn(
        mut writer: Box<dyn Write + Send>,
        #[cfg_attr(not(unix), allow(unused_variables))] poll_fd: Option<std::os::fd::OwnedFd>,
    ) -> io::Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded::<Option<WriteRequest>>();
        let stopped = Arc::new(AtomicBool::new(false));
        let thread_stopped = stopped.clone();
        #[cfg(unix)]
        let (cancel, wake) = std::os::unix::net::UnixStream::pair()?;
        std::thread::Builder::new()
            .name("noa-pty-writer".into())
            .spawn(move || {
                let mut last_write_at: Option<Instant> = None;
                while !thread_stopped.load(Ordering::Acquire) {
                    let request = match rx.try_recv() {
                        Ok(request) => request,
                        Err(TryRecvError::Disconnected) => return,
                        Err(TryRecvError::Empty) => {
                            // Hot-traffic spin before parking (see
                            // `HOT_SPIN_WINDOW`); cold falls straight
                            // through to the blocking recv as before.
                            let mut spun = None;
                            if last_write_at.is_some_and(|at| at.elapsed() < HOT_SPIN_WINDOW) {
                                let deadline = Instant::now() + HOT_SPIN_BUDGET;
                                while Instant::now() < deadline {
                                    if let Ok(request) = rx.try_recv() {
                                        spun = Some(request);
                                        break;
                                    }
                                    std::hint::spin_loop();
                                }
                            }
                            match spun {
                                Some(request) => request,
                                None => match rx.recv() {
                                    Ok(request) => request,
                                    Err(_) => return,
                                },
                            }
                        }
                    };
                    let Some(request) = request else {
                        return;
                    };
                    if let Err(err) = write_all_pollout(
                        &mut writer,
                        &poll_fd,
                        request.as_ref(),
                        &thread_stopped,
                        #[cfg(unix)]
                        std::os::fd::AsRawFd::as_raw_fd(&cancel),
                    )
                    .and_then(|()| writer.flush())
                    {
                        if !thread_stopped.load(Ordering::Acquire) {
                            log::warn!("pty writer thread stopping: {err}");
                        }
                        return;
                    }
                    last_write_at =
                        (request.as_ref().len() <= HOT_SPIN_MAX_WRITE).then(Instant::now);
                }
            })?;
        Ok(Self {
            tx,
            budget: PtyWriteBudget::default(),
            stopped,
            #[cfg(unix)]
            wake: Arc::new(wake),
        })
    }

    /// Queue all bytes without blocking. WouldBlock means the shared byte
    /// budget is full; no prefix of this request was accepted.
    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let reservation = self.budget.reserve_len(data.len()).ok_or_else(queue_full)?;
        self.send(WriteRequest(BudgetedWrite {
            data: Box::new(Box::<[u8]>::from(data)),
            reservation,
        }))
    }

    /// Queue an owned buffer without copying it. The complete value is kept
    /// alive until the writer thread finishes the corresponding `write_all`
    /// and flush, so wrappers may carry an upstream memory-budget reservation.
    pub fn write_owned<T>(&self, data: T) -> io::Result<()>
    where
        T: AsRef<[u8]> + Send + 'static,
    {
        let reserved = self.budget.reserve(data).map_err(|_| queue_full())?;
        self.write_reserved(reserved)
    }

    pub fn budget(&self) -> PtyWriteBudget {
        self.budget.clone()
    }

    pub fn write_reserved<T: AsRef<[u8]> + Send + 'static>(
        &self,
        data: BudgetedWrite<T>,
    ) -> io::Result<()> {
        if !Arc::ptr_eq(&self.budget.0, &data.reservation.budget.0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reservation belongs to another pty",
            ));
        }
        if data.as_ref().len() > data.reservation.charge {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write exceeds its reservation",
            ));
        }
        self.send(WriteRequest(
            data.map(|bytes| Box::new(bytes) as Box<dyn WriteBuffer>),
        ))
    }

    fn send(&self, request: WriteRequest) -> io::Result<()> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "pty writer stopped",
            ));
        }
        self.tx
            .send(Some(request))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "pty writer thread stopped"))
    }

    /// Cancel queued writes and wake a writer parked on a full PTY. A slave
    /// held by a descendant must not retain reservations after the pane closes.
    pub(crate) fn shutdown(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.tx.send(None);
        #[cfg(unix)]
        {
            let _ = (&*self.wake).write_all(&[1]);
        }
    }

    /// No-op for API symmetry: the writer thread flushes after every queued
    /// chunk.
    pub fn flush(&self) -> io::Result<()> {
        Ok(())
    }
}

fn queue_full() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "pty write byte budget exhausted")
}

/// `write_all` that tolerates a nonblocking master: on `WouldBlock` it parks
/// in `poll(POLLOUT)` on `poll_fd` until the child drains the tty input
/// queue, then resumes. Without a poll fd (non-unix, dup failure — in which
/// case `O_NONBLOCK` was never set and `WouldBlock` cannot occur) it behaves
/// exactly like `write_all`.
fn write_all_pollout(
    writer: &mut Box<dyn Write + Send>,
    #[cfg_attr(not(unix), allow(unused_variables))] poll_fd: &Option<std::os::fd::OwnedFd>,
    mut buf: &[u8],
    stopped: &AtomicBool,
    #[cfg(unix)] cancel: std::os::fd::RawFd,
) -> io::Result<()> {
    while !buf.is_empty() {
        if stopped.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "pty writer stopped",
            ));
        }
        match writer.write(buf) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write whole buffer",
                ));
            }
            Ok(n) => buf = &buf[n..],
            #[cfg(unix)]
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && poll_fd.is_some() => {
                use std::os::fd::AsRawFd as _;
                if let Some(fd) = poll_fd {
                    if !crate::reader::wait_ready_or_cancel(fd.as_raw_fd(), libc::POLLOUT, cancel) {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "pty writer stopped",
                        ));
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

impl std::fmt::Debug for PtyWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyWriter").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crossbeam_channel::{Receiver, Sender, bounded};

    use super::*;

    struct BlockingWriter {
        entered: Sender<()>,
        release: Receiver<()>,
    }

    impl Write for BlockingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.entered.send(());
            self.release
                .recv()
                .map_err(|_| io::Error::other("release channel closed"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct TrackedBuffer {
        bytes: Box<[u8]>,
        dropped: Sender<()>,
    }

    impl AsRef<[u8]> for TrackedBuffer {
        fn as_ref(&self) -> &[u8] {
            &self.bytes
        }
    }

    impl Drop for TrackedBuffer {
        fn drop(&mut self) {
            let _ = self.dropped.send(());
        }
    }

    #[test]
    fn owned_buffer_lives_until_the_real_pty_write_finishes() {
        let (entered_tx, entered_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(1);
        let (dropped_tx, dropped_rx) = bounded(1);
        let writer = PtyWriter::spawn(
            Box::new(BlockingWriter {
                entered: entered_tx,
                release: release_rx,
            }),
            None,
        )
        .unwrap();

        writer
            .write_owned(TrackedBuffer {
                bytes: b"guarded".to_vec().into_boxed_slice(),
                dropped: dropped_tx,
            })
            .unwrap();
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer should enter the real write");
        assert!(
            dropped_rx.try_recv().is_err(),
            "the owned wrapper was released before write_all completed"
        );

        release_tx.send(()).unwrap();
        dropped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the owned wrapper should release after the write");
    }

    #[test]
    fn replies_and_upstream_input_share_one_nonblocking_budget() {
        let writer = PtyWriter::spawn(Box::new(io::sink()), None).unwrap();
        let budget = writer.budget();
        let input = budget.reserve(vec![0; WRITE_BYTE_CAP]).ok().unwrap();
        assert_eq!(
            writer.write(b"reply").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            writer.write_owned(vec![1]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(input);
        assert_eq!(budget.0.load(Ordering::Acquire), 0);
        writer.write(b"reply").unwrap();
    }

    #[test]
    fn writer_failure_releases_queued_and_in_flight_reservations() {
        let (entered_tx, entered_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(1);
        let writer = PtyWriter::spawn(
            Box::new(BlockingWriter {
                entered: entered_tx,
                release: release_rx,
            }),
            None,
        )
        .unwrap();
        let budget = writer.budget();
        writer.write(b"first").unwrap();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let remaining = budget
            .reserve(vec![0; WRITE_BYTE_CAP - WRITE_MIN_CHARGE])
            .ok()
            .unwrap();
        writer.write_reserved(remaining).unwrap();
        assert_eq!(
            writer.write(b"reply").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(release_tx); // Force the real write to fail.
        let deadline = Instant::now() + Duration::from_secs(1);
        while budget.0.load(Ordering::Acquire) != 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(budget.0.load(Ordering::Acquire), 0);
        assert_eq!(
            writer.write(b"after failure").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(budget.0.load(Ordering::Acquire), 0);
    }

    #[test]
    #[cfg(unix)]
    fn shutdown_releases_bytes_even_when_a_peer_keeps_the_slave_open() {
        struct FullSocket {
            stream: std::os::unix::net::UnixStream,
            blocked: Sender<()>,
        }
        impl Write for FullSocket {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let result = self.stream.write(bytes);
                if result
                    .as_ref()
                    .is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock)
                {
                    let _ = self.blocked.try_send(());
                }
                result
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let (mut stream, _held_peer) = std::os::unix::net::UnixStream::pair().unwrap();
        stream.set_nonblocking(true).unwrap();
        // Fill the kernel buffer first so cancellation must wake POLLOUT,
        // rather than merely discard a request the worker has not started.
        loop {
            match stream.write(&[0; 65536]) {
                Ok(n) => assert!(n > 0),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("fill socket: {error}"),
            }
        }
        let fd = stream.try_clone().unwrap().into();
        let (blocked, entered) = bounded(1);
        let writer = PtyWriter::spawn(Box::new(FullSocket { stream, blocked }), Some(fd)).unwrap();
        let budget = writer.budget();
        writer.write_owned(vec![0; WRITE_BYTE_CAP]).unwrap();
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        writer.shutdown();
        let deadline = Instant::now() + Duration::from_secs(1);
        while budget.0.load(Ordering::Acquire) != 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(budget.0.load(Ordering::Acquire), 0);
        assert_eq!(
            writer.write(b"late").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}
