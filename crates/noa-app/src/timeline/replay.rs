//! `posts.jsonl` lines and their replay (`docs/specs/board-timeline.md`
//! L2-4): one `PostLine` per post, `{"canon","post","requestId","v"}`, and
//! the in-memory record a replayed line leaves behind.
//!
//! Replay decodes each line whole, so a line that is in the valid prefix
//! is one every later read can decode, but keeps only the fields a listing
//! needs (no body, no canon): a board's memory is its summaries; `get`,
//! search and the idempotency check read a post's line again by offset.

use std::io::{self, BufRead};

use noa_ipc::timeline::{
    CheckCounts, CheckResult, Post, PostInput, PostSummary, Seq, SourceRef, canon_bytes,
};
use serde::Deserialize;

/// `PostLine.v`.
pub(crate) const LINE_VERSION: u32 = 1;

/// Idempotency key B (§6.1): one source event, whoever relays it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SourceKey {
    pub namespace: String,
    pub session_id: String,
    pub run_id: Option<String>,
    pub event_id: String,
}

impl SourceKey {
    pub(crate) fn of(source: &SourceRef) -> Self {
        Self {
            namespace: source.namespace.clone(),
            session_id: source.session_id.clone(),
            run_id: source.run_id.clone(),
            event_id: source.event_id.clone(),
        }
    }
}

/// One post as the store keeps it in memory: its listing summary (with
/// `supersededBy` left empty, filled at read time) and where its line is.
#[derive(Clone, Debug)]
pub(crate) struct Rec {
    pub summary: PostSummary,
    pub request_id: String,
    pub source: SourceKey,
    pub offset: u64,
    pub len: u64,
}

impl Rec {
    pub(crate) fn of_post(post: &Post, offset: u64, len: u64) -> Self {
        let input = &post.input;
        Self {
            summary: PostSummary {
                artifacts: count(input.artifacts.len()),
                author: post.author.clone(),
                checks: check_counts(input.checks.iter().map(|check| check.result)),
                created_at: post.created_at.clone(),
                kind: input.kind,
                occurred_at: input.occurred_at.clone(),
                open_issues: count(input.open_issues.len()),
                outcome: input.outcome,
                post_id: post.post_id.clone(),
                producer: post.producer.clone(),
                provenance: post.provenance,
                run_id: post.run_id.clone(),
                seq: post.seq,
                source_namespace: input.source_ref.namespace.clone(),
                summary: input.summary.clone(),
                superseded_by: Vec::new(),
                supersedes_post_id: input.supersedes_post_id.clone(),
                task_ref: input.task_ref.clone(),
                title: input.title.clone(),
                worker_id: post.worker_id.clone(),
            },
            request_id: post.request_id.clone(),
            source: SourceKey::of(&input.source_ref),
            offset,
            len,
        }
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn check_counts(results: impl Iterator<Item = CheckResult>) -> CheckCounts {
    let mut counts = CheckCounts::default();
    for result in results {
        let slot = match result {
            CheckResult::Passed => &mut counts.passed,
            CheckResult::Failed => &mut counts.failed,
            CheckResult::NotRun => &mut counts.not_run,
            CheckResult::Inconclusive => &mut counts.inconclusive,
        };
        *slot += 1;
    }
    counts
}

/// The bytes of one `PostLine`, newline included. `canon` is the key-A
/// canon ([`canon_bytes`] with the producer), embedded as the object it
/// already is.
pub(crate) fn encode_line(canon: &[u8], post: &Post) -> Vec<u8> {
    let encoded = serde_json::to_vec(post).expect("a Post always serialises");
    let request = serde_json::to_vec(&post.request_id).expect("a string serialises");
    let mut line = Vec::with_capacity(canon.len() + encoded.len() + request.len() + 48);
    line.extend_from_slice(b"{\"canon\":");
    line.extend_from_slice(canon);
    line.extend_from_slice(b",\"post\":");
    line.extend_from_slice(&encoded);
    line.extend_from_slice(b",\"requestId\":");
    line.extend_from_slice(&request);
    line.extend_from_slice(format!(",\"v\":{LINE_VERSION}}}\n").as_bytes());
    line
}

/// The stored canon of a line, decoded: what a re-send is compared with.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredCanon {
    pub post: PostInput,
    pub producer_id: Option<String>,
    pub run_id: Option<String>,
}

impl StoredCanon {
    /// Key A's canon (with the producer).
    pub(crate) fn key_a(&self) -> Vec<u8> {
        canon_bytes(
            self.run_id.as_deref(),
            self.producer_id.as_deref(),
            &self.post,
        )
    }

    /// Key B's canon (without the producer).
    pub(crate) fn key_b(&self) -> Vec<u8> {
        canon_bytes(self.run_id.as_deref(), None, &self.post)
    }
}

/// Only the post of a line (`get`).
#[derive(Deserialize)]
pub(crate) struct PostLine {
    pub post: Post,
}

/// Only the canon of a line.
#[derive(Deserialize)]
pub(crate) struct CanonLine {
    pub canon: StoredCanon,
}

/// Only the body of a line's post (search).
#[derive(Deserialize)]
pub(crate) struct BodyLine {
    pub post: BodyOnly,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BodyOnly {
    #[serde(default)]
    pub body_markdown: String,
}

/// A whole line, decoded as every reader later decodes it: the canon a
/// re-send is compared with ([`CanonLine`]), the post `get` returns
/// ([`PostLine`]). A line that decodes only partly is corrupt, not a
/// listing entry whose key can never be checked.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LineFull {
    #[serde(rename = "canon")]
    _canon: StoredCanon,
    post: Post,
    #[serde(rename = "requestId")]
    _request_id: String,
    v: u32,
}

/// Why replay stopped before the end of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Corrupt {
    /// 1-based line of `posts.jsonl`.
    pub line: u64,
    pub message: String,
}

#[derive(Debug, Default)]
pub(crate) struct Replayed {
    pub recs: Vec<Rec>,
    /// The offset just past the last good line: the durable prefix.
    pub end: u64,
    /// Bytes after `end` with no terminating newline (a torn append).
    pub torn: u64,
    pub corrupt: Option<Corrupt>,
}

/// Replays lines from `reader`, which starts at byte `offset` of the file,
/// where line `next_seq` (1-based) begins. Every newline-terminated line
/// must decode, belong to `board_id` and carry the next seq; the first
/// that does not stops replay as [`Corrupt`] (nothing after it is read).
/// A final segment without a newline is torn, not corrupt.
pub(crate) fn replay(
    mut reader: impl BufRead,
    offset: u64,
    next_seq: u64,
    board_id: &str,
) -> io::Result<Replayed> {
    let mut out = Replayed {
        end: offset,
        ..Replayed::default()
    };
    let mut buf = Vec::new();
    let mut seq = next_seq;
    loop {
        buf.clear();
        let read = reader.read_until(b'\n', &mut buf)?;
        if read == 0 {
            return Ok(out);
        }
        if buf.last() != Some(&b'\n') {
            out.torn = read as u64;
            return Ok(out);
        }
        let fail = |message: String| Corrupt { line: seq, message };
        let line = match serde_json::from_slice::<LineFull>(&buf) {
            Ok(line) => line,
            Err(err) => {
                out.corrupt = Some(fail(format!("line does not decode: {err}")));
                return Ok(out);
            }
        };
        if line.v != LINE_VERSION {
            out.corrupt = Some(fail(format!(
                "line version {} is not {LINE_VERSION}",
                line.v
            )));
            return Ok(out);
        }
        if line.post.seq != Seq(seq) {
            out.corrupt = Some(fail(format!(
                "seq {} where {seq} was expected",
                line.post.seq.0
            )));
            return Ok(out);
        }
        if line.post.board_id != board_id {
            out.corrupt = Some(fail(format!(
                "post belongs to board {:?}",
                line.post.board_id
            )));
            return Ok(out);
        }
        out.recs
            .push(Rec::of_post(&line.post, out.end, read as u64));
        out.end += read as u64;
        seq += 1;
    }
}
