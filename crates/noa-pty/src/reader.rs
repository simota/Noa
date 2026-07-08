//! Background threads: one drains the PTY master into [`PtyEvent::Data`]
//! chunks, another waits on the child and reports its exit code.

use std::io::Read;
use std::sync::Arc;

use crossbeam_channel::Sender;
use portable_pty::Child;

use crate::data::ReadBufferPool;
use crate::{PtyData, PtyEvent};

/// Size of each read buffer chunk (bytes).
const READ_CHUNK: usize = 64 * 1024;
const READ_BUFFER_POOL_CAPACITY: usize = 16;

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
            let pool = Arc::new(ReadBufferPool::new(READ_CHUNK, READ_BUFFER_POOL_CAPACITY));
            loop {
                let mut buf = pool.take();
                match reader.read(&mut buf) {
                    Ok(0) => {
                        pool.recycle(buf);
                        break;
                    }
                    Ok(n) => {
                        let chunk = PtyData::pooled(buf, n, Arc::clone(&pool));
                        if tx.send(PtyEvent::Data(chunk)).is_err() {
                            // Receiver dropped; nothing more to do.
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                        pool.recycle(buf);
                        continue;
                    }
                    Err(e) => {
                        pool.recycle(buf);
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
        let handle = spawn_reader(Box::new(ScriptedReader { steps: step_rx }), event_tx)
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
        let handle = spawn_reader(Box::new(ScriptedReader { steps: step_rx }), event_tx)
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
        let handle = spawn_reader(Box::new(ScriptedReader { steps: step_rx }), event_tx)
            .expect("spawn reader");

        step_tx.send(ReadStep::Data(b"orphaned")).unwrap();
        handle.join().unwrap();
    }
}
