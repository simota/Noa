//! Board Timeline publisher (`docs/specs/board-timeline.md` L2-7): a
//! client-side outbox that makes a post durable on disk before it is sent,
//! so a client that crashes or loses its connection re-sends the same
//! envelope and the server collapses the duplicate by its request id.
//!
//! Shared by `+ctl timeline post` and every widget runner, which already
//! depend on this crate by path. One file per entry,
//! `<fnv1a64(boardId \0 producerId \0 requestId) hex>.json`, mode 0600,
//! written by staging + `sync_all` + rename + directory fsync. An entry never
//! holds a token or a control URL.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::client::{Client, ClientError};
use crate::timeline::{PostInput, PostParams, PostResult, Seq, TimelineErrorKind};

/// What one post is sent as; the outbox key is [`Envelope::key`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    pub board_id: String,
    pub post: PostInput,
    pub producer_id: String,
    pub request_id: String,
    pub run_id: Option<String>,
}

impl Envelope {
    pub fn key(&self) -> EnvelopeKey {
        EnvelopeKey {
            board_id: self.board_id.clone(),
            producer_id: self.producer_id.clone(),
            request_id: self.request_id.clone(),
        }
    }
}

/// One outbox entry's key: the server's idempotency key A
/// `(boardId, producerId, requestId)`. A request id alone does not name an
/// entry: the same `review-r1` may be pending on two boards.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvelopeKey {
    pub board_id: String,
    pub producer_id: String,
    pub request_id: String,
}

/// Where an entry stands: not yet acknowledged, durably stored by the
/// server, or refused/failed (kept so it can be re-sent).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum PublishState {
    Pending,
    Published {
        post_id: String,
        seq: Seq,
    },
    Failed {
        kind: Option<TimelineErrorKind>,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub attempts: u32,
    pub envelope: Envelope,
    pub state: PublishState,
    pub updated_at_ms: u64,
}

/// The board and run a runner registered, saved so a restart registers
/// with the same board and sends under the same run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunBinding {
    pub board_id: String,
    pub producer_id: String,
    pub run_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    #[error("outbox i/o: {0}")]
    Io(#[from] io::Error),
    /// An entry with the same key holds a different envelope; nothing was
    /// written or sent.
    #[error("the outbox already holds request {request_id} with a different post")]
    Conflict { request_id: String },
}

/// One outbox directory (mode 0700). `keep_published`: keep an entry's file
/// once the server acknowledged it (widgets show its state), instead of
/// removing it (`+ctl`).
#[derive(Debug)]
pub struct Outbox {
    dir: PathBuf,
    keep_published: bool,
}

impl Outbox {
    /// Opens (creating, mode 0700) the outbox at `dir`.
    pub fn open(dir: PathBuf, keep_published: bool) -> io::Result<Self> {
        create_private_dir(&dir)?;
        Ok(Self {
            dir,
            keep_published,
        })
    }

    /// Makes `env` durable before returning: an entry with the same key and
    /// the same envelope is returned as it is; a different envelope under the
    /// same key is [`OutboxError::Conflict`].
    pub fn stage(&self, env: &Envelope) -> Result<Entry, OutboxError> {
        let directory = self.lock()?;
        let key = env.key();
        if let Some(entry) = self.get(&key)? {
            if entry.envelope != *env {
                return Err(OutboxError::Conflict {
                    request_id: key.request_id,
                });
            }
            // A previous rename may have succeeded but its directory sync failed.
            directory.sync_all()?;
            return Ok(entry);
        }
        let entry = Entry {
            envelope: env.clone(),
            state: PublishState::Pending,
            attempts: 0,
            updated_at_ms: now_ms(),
        };
        self.write(&self.path(&key), &entry)?;
        Ok(entry)
    }

    pub fn get(&self, key: &EnvelopeKey) -> io::Result<Option<Entry>> {
        let entry: Option<Entry> = read_optional(&self.path(key))?;
        if entry.as_ref().is_some_and(|e| e.envelope.key() != *key) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "outbox key does not match file",
            ));
        }
        Ok(entry)
    }

    pub fn entries(&self) -> io::Result<Vec<Entry>> {
        self.entry_files()?.collect()
    }

    fn entry_files(&self) -> io::Result<impl Iterator<Item = io::Result<Entry>>> {
        let read_entry = |file: io::Result<fs::DirEntry>| -> io::Result<Option<Entry>> {
            let file = file?;
            let name = file.file_name();
            let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
                return Ok(None);
            };
            if stem.len() != 16 || !stem.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Ok(None);
            }
            // A concurrent CLI acknowledgement may have removed this entry.
            let path = file.path();
            let entry = read_optional::<Entry>(&path)?;
            if entry
                .as_ref()
                .is_some_and(|e| self.path(&e.envelope.key()) != path)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "outbox key does not match file",
                ));
            }
            Ok(entry)
        };
        Ok(fs::read_dir(&self.dir)?.filter_map(move |file| read_entry(file).transpose()))
    }

    /// Records `state` for the entry; [`PublishState::Published`] removes
    /// its file unless the outbox keeps published entries.
    pub fn record(&self, key: &EnvelopeKey, state: PublishState) -> io::Result<()> {
        let _directory = self.lock()?;
        let entry = self.get(key)?.ok_or(io::ErrorKind::NotFound)?;
        self.record_entry(entry, state)?;
        Ok(())
    }

    pub fn save_run(&self, run: &RunBinding) -> io::Result<()> {
        self.write(&self.dir.join("run.json"), run)
    }

    pub fn load_run(&self) -> io::Result<Option<RunBinding>> {
        read_optional(&self.dir.join("run.json"))
    }

    // Separate opens lock the same directory across threads and processes.
    // Hold it only for read/modify/write, never while waiting for the server.
    fn lock(&self) -> io::Result<File> {
        let dir = File::open(&self.dir)?;
        dir.lock()?;
        Ok(dir)
    }

    fn path(&self, key: &EnvelopeKey) -> PathBuf {
        let mut hash = 0xcbf29ce484222325u64;
        for byte in key
            .board_id
            .bytes()
            .chain([0])
            .chain(key.producer_id.bytes())
            .chain([0])
            .chain(key.request_id.bytes())
        {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
        self.dir.join(format!("{hash:016x}.json"))
    }

    fn record_entry(&self, mut entry: Entry, state: PublishState) -> io::Result<PublishState> {
        // A late timeout from a concurrent publisher cannot undo an ack.
        let state = if matches!(entry.state, PublishState::Published { .. }) {
            if self.keep_published {
                return Ok(entry.state);
            }
            entry.state
        } else {
            state
        };
        let path = self.path(&entry.envelope.key());
        if !self.keep_published && matches!(state, PublishState::Published { .. }) {
            fs::remove_file(path)?;
            File::open(&self.dir)?.sync_all()?;
        } else {
            entry.attempts = entry.attempts.saturating_add(1);
            entry.updated_at_ms = now_ms();
            entry.state = state.clone();
            self.write(&path, &entry)?;
        }
        Ok(state)
    }

    fn finish_publish(&self, sent: &Envelope, state: PublishState) -> io::Result<PublishState> {
        let _directory = self.lock()?;
        match self.get(&sent.key())? {
            Some(current) if current.envelope == *sent => self.record_entry(current, state),
            // Another sender can already have received the CLI ack and removed the file.
            None if !self.keep_published => Ok(state),
            _ => Err(io::Error::other(
                "staged outbox entry changed during publish",
            )),
        }
    }

    fn write(&self, path: &Path, value: &impl Serialize) -> io::Result<()> {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let (tmp, mut file) = loop {
            let tmp = self
                .dir
                .join(format!(".{:016x}.tmp", rand::random::<u64>()));
            match opts.open(&tmp) {
                Ok(file) => break (tmp, file),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        };
        let result = (|| {
            #[cfg(unix)]
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.write_all(&serde_json::to_vec(value)?)?;
            file.sync_all()?;
            fs::rename(&tmp, path)?;
            File::open(&self.dir)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    // Persist each new directory's name before an envelope can be sent.
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.try_exists()? {
        create_private_dir(parent)?;
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    match builder.create(path) {
        Ok(()) => {}
        Err(e)
            if e.kind() == io::ErrorKind::AlreadyExists && fs::symlink_metadata(path)?.is_dir() => {
        }
        Err(e) => return Err(e),
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    File::open(parent)?.sync_all()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn read_optional<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Where a publisher sends a post: a [`Client`], or a fake in tests.
pub trait PostSink {
    fn post(&mut self, p: &PostParams) -> Result<PostResult, ClientError>;
}

impl PostSink for Client {
    fn post(&mut self, p: &PostParams) -> Result<PostResult, ClientError> {
        self.timeline_post(p)
    }
}

/// Stages `env`, sends it and records the outcome: `Published` only after
/// the server's durable acknowledgement.
pub fn publish(sink: &mut dyn PostSink, outbox: &Outbox, env: Envelope) -> PublishState {
    let entry = match outbox.stage(&env) {
        Ok(entry) => entry,
        Err(e) => {
            return PublishState::Failed {
                kind: match e {
                    OutboxError::Conflict { .. } => Some(TimelineErrorKind::Conflict),
                    _ => None,
                },
                message: e.to_string(),
            };
        }
    };
    if matches!(entry.state, PublishState::Published { .. }) {
        return if outbox.keep_published {
            entry.state
        } else {
            outbox
                .finish_publish(&env, entry.state)
                .unwrap_or_else(local_failure)
        };
    }
    let params = PostParams {
        board_id: env.board_id,
        producer_id: env.producer_id,
        request_id: env.request_id,
        run_id: env.run_id,
        post: env.post,
    };
    let state = match sink.post(&params) {
        Ok(ack) => PublishState::Published {
            post_id: ack.post_id,
            seq: ack.seq,
        },
        Err(ClientError::Rpc { code, message }) => PublishState::Failed {
            kind: TimelineErrorKind::from_code(code),
            message,
        },
        Err(ClientError::Timeout(_)) => PublishState::Failed {
            kind: None,
            message: "timed out waiting for the post acknowledgement".into(),
        },
        // Transport diagnostics can include the connection URL.
        Err(_) => PublishState::Failed {
            kind: None,
            message: "post was not acknowledged".into(),
        },
    };
    outbox
        .finish_publish(&entry.envelope, state)
        .unwrap_or_else(local_failure)
}

fn local_failure(e: io::Error) -> PublishState {
    PublishState::Failed {
        kind: None,
        message: e.to_string(),
    }
}

/// Sends every entry that is not yet published, each with its stored
/// envelope unchanged; returns each entry's key with its new state.
pub fn resend_pending(
    sink: &mut dyn PostSink,
    outbox: &Outbox,
) -> Vec<(EnvelopeKey, PublishState)> {
    let files = match outbox.entry_files() {
        Ok(files) => files,
        Err(e) => {
            log::warn!("could not enumerate timeline outbox: {e}");
            return Vec::new();
        }
    };
    // Snapshot before publishing changes the directory. A damaged entry
    // stays available for inspection without blocking unrelated requests.
    let pending: Vec<_> = files
        .filter_map(|file| match file {
            Ok(entry) if matches!(entry.state, PublishState::Published { .. }) => None,
            Ok(entry) => Some(entry.envelope),
            Err(e) => {
                log::warn!("could not read timeline outbox entry: {e}");
                None
            }
        })
        .collect();
    pending
        .into_iter()
        .map(|env| (env.key(), publish(sink, outbox, env)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Temp(PathBuf);

    impl Temp {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("noa-outbox-{:016x}", rand::random::<u64>())))
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    impl<F: FnMut(&PostParams) -> Result<PostResult, ClientError>> PostSink for F {
        fn post(&mut self, p: &PostParams) -> Result<PostResult, ClientError> {
            self(p)
        }
    }

    fn envelope() -> Envelope {
        serde_json::from_value(json!({
            "boardId":"bd-0123456789abcdef","producerId":"review:s1","requestId":"review-s1-r1",
            "runId":"rn-0123456789abcdef",
            "post":{"schemaVersion":1,"kind":"summary","outcome":"partial","title":"結果","summary":"s",
                    "sourceRef":{"namespace":"review","sessionId":"s1","eventId":"r1"}}
        })).unwrap()
    }

    fn ack() -> PostResult {
        PostResult {
            post_id: "po-0000000000000001".into(),
            seq: Seq(1),
            created_at: "2026-01-01T00:00:00Z".into(),
            duplicate: false,
        }
    }

    #[test]
    fn ac5_stage_then_ack_marks_published() {
        let temp = Temp::new();
        let outbox = Outbox::open(temp.0.join("review/s1"), true).unwrap();
        let env = envelope();
        let key = env.key();
        let binding = RunBinding {
            board_id: env.board_id.clone(),
            run_id: env.run_id.clone().unwrap(),
            producer_id: env.producer_id.clone(),
        };
        assert_eq!(outbox.load_run().unwrap(), None);
        outbox.save_run(&binding).unwrap();
        let state = publish(
            &mut |_: &PostParams| {
                let staged = outbox.get(&key).unwrap().unwrap();
                assert_eq!(staged.envelope, env);
                assert_eq!(staged.state, PublishState::Pending);
                assert_eq!(staged.attempts, 0);
                Ok(ack())
            },
            &outbox,
            env.clone(),
        );
        assert_eq!(
            state,
            PublishState::Published {
                post_id: ack().post_id,
                seq: Seq(1)
            }
        );
        let reopened = Outbox::open(outbox.dir.clone(), true).unwrap();
        assert_eq!(reopened.load_run().unwrap(), Some(binding));
        let entries = reopened.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].attempts, 1);
        assert_eq!(entries[0].state, state);
        assert_eq!(
            publish(
                &mut |_: &PostParams| panic!("already acknowledged"),
                &reopened,
                env
            ),
            state
        );
        #[cfg(unix)]
        for (path, mode) in [
            (reopened.dir.clone(), 0o700),
            (reopened.path(&key), 0o600),
            (reopened.dir.join("run.json"), 0o600),
        ] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                mode
            );
        }
    }

    #[test]
    fn ac5_stage_conflict_refused() {
        let temp = Temp::new();
        let outbox = Outbox::open(temp.0.clone(), true).unwrap();
        let first = envelope();
        let mut second = first.clone();
        second.post.title.push('!');
        let barrier = std::sync::Barrier::new(2);
        let stage = |env: &Envelope| {
            barrier.wait();
            outbox.stage(env)
        };
        std::thread::scope(|s| {
            let a = s.spawn(|| stage(&first));
            let b = s.spawn(|| stage(&second));
            assert!(matches!(
                (a.join().unwrap(), b.join().unwrap()),
                (Ok(_), Err(OutboxError::Conflict { .. }))
                    | (Err(OutboxError::Conflict { .. }), Ok(_))
            ));
        });
        let staged = outbox.get(&first.key()).unwrap().unwrap();
        let other = if staged.envelope == first {
            second
        } else {
            first
        };
        assert!(matches!(
            publish(
                &mut |_: &PostParams| panic!("conflict sent"),
                &outbox,
                other
            ),
            PublishState::Failed {
                kind: Some(TimelineErrorKind::Conflict),
                ..
            }
        ));
        assert_eq!(outbox.entries().unwrap(), [staged]);
    }

    #[test]
    fn ac2_remove_after_ack_mode() {
        let temp = Temp::new();
        let outbox = Outbox::open(temp.0.clone(), false).unwrap();
        let env = envelope();
        let state = publish(
            &mut |_: &PostParams| {
                Err(ClientError::Rpc {
                    code: -32014,
                    message: "queue full".into(),
                })
            },
            &outbox,
            env.clone(),
        );
        let entry = outbox.get(&env.key()).unwrap().unwrap();
        assert_eq!(
            state,
            PublishState::Failed {
                kind: Some(TimelineErrorKind::Busy),
                message: "queue full".into()
            }
        );
        assert_eq!(entry.state, state);
        assert_eq!(entry.attempts, 1);
        assert!(matches!(
            publish(&mut |_: &PostParams| Ok(ack()), &outbox, env.clone()),
            PublishState::Published { .. }
        ));
        assert_eq!(outbox.get(&env.key()).unwrap(), None);
    }

    #[test]
    fn ac2_stage_failure_does_not_send() {
        let temp = Temp::new();
        let outbox = Outbox::open(temp.0.clone(), false).unwrap();
        let env = envelope();
        fs::write(outbox.path(&env.key()), b"incomplete entry").unwrap();
        assert!(matches!(
            publish(
                &mut |_: &PostParams| panic!("unstaged post sent"),
                &outbox,
                env
            ),
            PublishState::Failed { kind: None, .. }
        ));
    }

    #[test]
    fn ac5_resend_pending_uses_stored_envelope() {
        let temp = Temp::new();
        let env = envelope();
        let mut sent = Vec::new();
        {
            let outbox = Outbox::open(temp.0.clone(), true).unwrap();
            publish(
                &mut |p: &PostParams| {
                    sent.push(serde_json::to_vec(p).unwrap());
                    Err(ClientError::Timeout("lost ack".into()))
                },
                &outbox,
                env.clone(),
            );
        }
        let outbox = Outbox::open(temp.0.clone(), true).unwrap();
        let corrupt = temp.0.join("0000000000000000.json");
        fs::write(&corrupt, b"incomplete").unwrap();
        let states = resend_pending(
            &mut |p: &PostParams| {
                sent.push(serde_json::to_vec(p).unwrap());
                Ok(PostResult {
                    duplicate: true,
                    ..ack()
                })
            },
            &outbox,
        );
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0], sent[1]);
        assert_eq!(fs::read(corrupt).unwrap(), b"incomplete");
        assert_eq!(
            states,
            [(
                env.key(),
                PublishState::Published {
                    post_id: ack().post_id,
                    seq: Seq(1)
                }
            )]
        );
        assert_eq!(outbox.get(&env.key()).unwrap().unwrap().attempts, 2);
        assert!(
            resend_pending(
                &mut |_: &PostParams| panic!("published entry resent"),
                &outbox
            )
            .is_empty()
        );
    }

    // Review defect 1: two pending posts with one request id on two boards
    // are two entries.
    #[test]
    fn the_key_is_board_producer_and_request() {
        let envelope = |board: &str, producer: &str| Envelope {
            board_id: board.to_string(),
            post: serde_json::from_value(json!({
                "schemaVersion": 1, "kind": "summary", "outcome": "succeeded",
                "title": "t", "summary": "s",
                "sourceRef": { "namespace": "review", "sessionId": "s1", "eventId": "verdict-r1" },
            }))
            .unwrap(),
            producer_id: producer.to_string(),
            request_id: "review-r1".to_string(),
            run_id: None,
        };
        let a = envelope("bd-000000000000000a", "review:s1");
        let b = envelope("bd-000000000000000b", "review:s1");
        let other = envelope("bd-000000000000000a", "review:s2");
        assert_eq!(
            a.key(),
            EnvelopeKey {
                board_id: "bd-000000000000000a".to_string(),
                producer_id: "review:s1".to_string(),
                request_id: "review-r1".to_string(),
            }
        );
        let keys: std::collections::HashSet<EnvelopeKey> = [a.key(), b.key(), other.key(), a.key()]
            .into_iter()
            .collect();
        assert_eq!(keys.len(), 3);
        // `get` and `record` name an entry by the whole key.
        let _: fn(&Outbox, &EnvelopeKey) -> io::Result<Option<Entry>> = Outbox::get;
        let _: fn(&Outbox, &EnvelopeKey, PublishState) -> io::Result<()> = Outbox::record;
    }

    #[test]
    fn publish_state_wire_form() {
        assert_eq!(
            serde_json::to_value(PublishState::Published {
                post_id: "po-0123456789abcdef".to_string(),
                seq: Seq(9),
            })
            .unwrap(),
            json!({ "state": "published", "postId": "po-0123456789abcdef", "seq": "9" })
        );
        assert_eq!(
            serde_json::to_value(PublishState::Failed {
                kind: Some(TimelineErrorKind::Busy),
                message: "busy".to_string(),
            })
            .unwrap(),
            json!({ "state": "failed", "kind": "busy", "message": "busy" })
        );
        assert_eq!(
            serde_json::from_value::<PublishState>(json!({ "state": "pending" })).unwrap(),
            PublishState::Pending
        );
    }
}
