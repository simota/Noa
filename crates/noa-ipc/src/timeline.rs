//! Board Timeline wire contract (`docs/specs/board-timeline.md` L2-2/L2-3):
//! the params/result types of the eight `noa.timeline.*` methods, their
//! errors, the shape and limit checks every caller and the store share, and
//! the canonical payload the store's idempotency keys compare.
//!
//! No Ghostty analog: the timeline is a Noa-only control surface. Like the
//! rest of [`crate::protocol`], every struct is camelCase with its fields in
//! sorted key order, and the per-board `seq` is a decimal string on the wire.
//! Only [`PostInput`] refuses unknown fields, so a typo in a hand-written
//! summary file is refused rather than dropped.

use std::cmp::Ordering;
use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::Scope;
use crate::error::ErrorCode;
use crate::protocol::WireId;

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_TITLE_CHARS: usize = 120;
pub const MAX_SUMMARY_CHARS: usize = 400;
pub const MAX_BODY_BYTES: usize = 32 * 1024;
pub const MAX_POST_PARAMS_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 192 * 1024;
pub const MAX_BOARD_BYTES: u64 = 100 * 1024 * 1024;
pub const LIST_DEFAULT: usize = 20;
pub const LIST_MAX: usize = 50;
pub const CHANGES_DEFAULT: usize = 100;
pub const CHANGES_MAX: usize = 200;
/// `requestId`, `producerId`, `sessionId`, `eventId` (charset and length)
/// and `label`/`role` (length only).
pub const MAX_TOKEN_BYTES: usize = 128;
/// The most items in each of a post's `openIssues`, `nextActions`,
/// `checks`, `artifacts` and `basedOnPostIds`. Normalising adds at most 20
/// bytes per item (`"x"` becomes `{"ref":null,"text":"x"}`; an artifact
/// gains `"digest":null`, a check `"evidenceRef":null`) and under 1 KiB of
/// spelled-out absent fields, so a post from [`MAX_POST_PARAMS_BYTES`] of
/// params normalises to under 75 KiB, and the stored [`Post`] that `get`
/// returns stays far inside [`MAX_RESPONSE_BYTES`]. Without a cap, 64 KiB
/// of one-character issues normalised to over 192 KiB.
pub const MAX_LIST_ITEMS: usize = 100;
/// `noa.timeline.boards` `limit`: default and most.
pub const BOARDS_DEFAULT: usize = 50;
pub const BOARDS_MAX: usize = 200;
/// `noa.timeline.list` `query`, in Unicode scalar values.
pub const MAX_QUERY_CHARS: usize = 200;

/// The `sourceRef.namespace` values a post without a registered run may
/// carry (stored with [`Provenance::Unverified`]).
pub const UNREGISTERED_NAMESPACES: [&str; 2] = ["result-file", "manual"];

const TASK_REF_KINDS: [&str; 3] = ["local", "github", "other"];
const ARTIFACT_KINDS: [&str; 5] = ["file", "commit", "pr", "url", "other"];

// ---------------------------------------------------------------------------
// Seq

/// A post's per-board sequence number: u64 internally, decimal string on
/// the wire (as [`WireId`]), so a JS client never rounds it. A JSON number
/// is refused: one above 2^53 has already lost precision in such a client.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Seq(pub u64);

impl Serialize for Seq {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Seq {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct SeqVisitor;
        impl Visitor<'_> for SeqVisitor {
            type Value = Seq;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a decimal string seq")
            }
            fn visit_str<E>(self, v: &str) -> Result<Seq, E>
            where
                E: de::Error,
            {
                v.parse::<u64>()
                    .map(Seq)
                    .map_err(|_| E::custom("invalid decimal seq"))
            }
        }
        deserializer.deserialize_str(SeqVisitor)
    }
}

// ---------------------------------------------------------------------------
// Ids and tokens

/// The four minted id kinds, each `<prefix>` + 16 lowercase hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IdKind {
    Board,
    Worker,
    Run,
    Post,
}

impl IdKind {
    pub const fn prefix(self) -> &'static str {
        match self {
            IdKind::Board => "bd-",
            IdKind::Worker => "wk-",
            IdKind::Run => "rn-",
            IdKind::Post => "po-",
        }
    }
}

/// `^<prefix>[0-9a-f]{16}$`. A board id is checked with this before any
/// path is built from it.
pub fn valid_id(kind: IdKind, s: &str) -> bool {
    s.strip_prefix(kind.prefix()).is_some_and(|hex| {
        hex.len() == 16
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// 1..=[`MAX_TOKEN_BYTES`] bytes of `[A-Za-z0-9._:/-]`.
pub fn valid_token(s: &str) -> bool {
    (1..=MAX_TOKEN_BYTES).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'/' | b'-'))
}

// ---------------------------------------------------------------------------
// Enums

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostKind {
    Summary,
    Milestone,
    Blocker,
    Decision,
    Handoff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Partial,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckResult {
    Passed,
    Failed,
    NotRun,
    Inconclusive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishOwner {
    Worker,
    Runner,
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducerKind {
    Worker,
    Runner,
    Human,
}

/// `unverified`: a post with no registered run (`runId: null`), shown as
/// 「出所未照合」.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Registered,
    Unverified,
}

// ---------------------------------------------------------------------------
// Post parts

/// Where a post came from: with `eventId`, the idempotency key B of a
/// source event however many times it is relayed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    pub event_id: String,
    pub locator: Option<String>,
    pub namespace: String,
    pub run_id: Option<String>,
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRef {
    pub id: String,
    /// `local`, `github` or `other`.
    pub kind: String,
    pub title: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub digest: Option<String>,
    /// `file`, `commit`, `pr`, `url` or `other`.
    pub kind: String,
    pub label: String,
    pub r#ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub evidence_ref: Option<String>,
    pub name: String,
    pub reported_by: String,
    pub result: CheckResult,
}

/// An open issue or next action. Deserialises from a bare string too, which
/// becomes `{text, ref: null}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", from = "ItemRepr")]
pub struct Item {
    pub r#ref: Option<String>,
    pub text: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ItemRepr {
    Text(String),
    Full {
        #[serde(default, rename = "ref")]
        r#ref: Option<String>,
        text: String,
    },
}

impl From<ItemRepr> for Item {
    fn from(repr: ItemRepr) -> Self {
        match repr {
            ItemRepr::Text(text) => Item { r#ref: None, text },
            ItemRepr::Full { r#ref, text } => Item { r#ref, text },
        }
    }
}

/// Who a post without a registered run says wrote it; ignored when the post
/// has a `runId` (the run's worker is the author then).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorHint {
    pub label: Option<String>,
    pub role: Option<String>,
}

/// The client-supplied part of a post: the only part a client sends, and
/// what the idempotency keys compare ([`canon_bytes`]).
///
/// Every field is written, an absent `author` as `null` like every other
/// absent option, so a client that spells out the defaults takes the same
/// canon. A stored [`Post`] leaves `author` out of its flattened input: its
/// own [`Post::author`] (the resolved author) is the one key on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PostInput {
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub author: Option<AuthorHint>,
    #[serde(default)]
    pub based_on_post_ids: Vec<String>,
    #[serde(default)]
    pub body_markdown: String,
    #[serde(default)]
    pub checks: Vec<Check>,
    pub kind: PostKind,
    #[serde(default)]
    pub next_actions: Vec<Item>,
    /// RFC 3339 UTC, `YYYY-MM-DDTHH:MM:SS[.fff]Z`.
    pub occurred_at: Option<String>,
    #[serde(default)]
    pub open_issues: Vec<Item>,
    pub outcome: Outcome,
    pub retry_of_run_id: Option<String>,
    pub schema_version: u32,
    pub source_ref: SourceRef,
    pub summary: String,
    pub supersedes_post_id: Option<String>,
    pub task_ref: Option<TaskRef>,
    pub title: String,
}

impl PostInput {
    /// Shape and limit checks that need no store: schema version, title
    /// 1..=120 and summary 1..=400 chars (blank refused), body ≤ 32 KiB,
    /// at most [`MAX_LIST_ITEMS`] items per list, tokens, id formats,
    /// `occurredAt` format and date, `taskRef`/artifact kinds, artifact
    /// labels ≤ [`MAX_TOKEN_BYTES`]. Over a limit is refused, never
    /// truncated.
    pub fn validate(&self) -> Result<(), TimelineError> {
        check_schema_version(u64::from(self.schema_version))?;
        check_text("title", &self.title, MAX_TITLE_CHARS)?;
        check_text("summary", &self.summary, MAX_SUMMARY_CHARS)?;
        if self.body_markdown.len() > MAX_BODY_BYTES {
            return Err(TimelineError::invalid_params(format!(
                "bodyMarkdown is {} bytes; the most is {MAX_BODY_BYTES}",
                self.body_markdown.len()
            )));
        }
        for (what, items) in [
            ("openIssues", self.open_issues.len()),
            ("nextActions", self.next_actions.len()),
            ("checks", self.checks.len()),
            ("artifacts", self.artifacts.len()),
            ("basedOnPostIds", self.based_on_post_ids.len()),
        ] {
            if items > MAX_LIST_ITEMS {
                return Err(TimelineError::invalid_params(format!(
                    "{what} has {items} items; the most is {MAX_LIST_ITEMS}"
                )));
            }
        }
        if let Some(at) = &self.occurred_at
            && !valid_utc_timestamp(at)
        {
            return Err(TimelineError::invalid_params(
                "occurredAt must be RFC 3339 UTC: YYYY-MM-DDTHH:MM:SS[.fff]Z",
            ));
        }
        if let Some(author) = &self.author {
            check_label("author.label", author.label.as_deref())?;
            check_label("author.role", author.role.as_deref())?;
        }
        let source = &self.source_ref;
        check_token("sourceRef.namespace", &source.namespace)?;
        check_token("sourceRef.sessionId", &source.session_id)?;
        check_token("sourceRef.eventId", &source.event_id)?;
        if let Some(run) = &source.run_id {
            check_token("sourceRef.runId", run)?;
        }
        if let Some(task) = &self.task_ref {
            check_task_ref(task)?;
        }
        if let Some(run) = &self.retry_of_run_id {
            check_id("retryOfRunId", IdKind::Run, run)?;
        }
        for artifact in &self.artifacts {
            if !ARTIFACT_KINDS.contains(&artifact.kind.as_str()) {
                return Err(TimelineError::invalid_params(format!(
                    "artifact kind {:?} is not one of {}",
                    artifact.kind,
                    ARTIFACT_KINDS.join(", ")
                )));
            }
            check_label("artifacts.label", Some(&artifact.label))?;
        }
        for post in &self.based_on_post_ids {
            check_id("basedOnPostIds", IdKind::Post, post)?;
        }
        if let Some(post) = &self.supersedes_post_id {
            check_id("supersedesPostId", IdKind::Post, post)?;
        }
        Ok(())
    }

    /// The form the canon is taken of. Deserialisation already gives every
    /// spelling of one input the same value (absent arrays `[]`, absent
    /// options `None`, a string item `{text, ref: None}`), so this is the
    /// identity; it names the step the store and the clients share.
    pub fn normalized(self) -> Self {
        self
    }
}

/// The canonical payload compared by both idempotency keys: compact JSON of
/// `{post, producerId, runId}` with every struct's fields in their fixed
/// order, so a client's key order never matters. Key A passes the
/// `producer_id`; key B passes `None` (a source event is one event whoever
/// relays it).
pub fn canon_bytes(run_id: Option<&str>, producer_id: Option<&str>, post: &PostInput) -> Vec<u8> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct PostCanon<'a> {
        post: &'a PostInput,
        producer_id: Option<&'a str>,
        run_id: Option<&'a str>,
    }
    serde_json::to_vec(&PostCanon {
        post,
        producer_id,
        run_id,
    })
    .expect("a PostInput always serialises")
}

// ---------------------------------------------------------------------------
// Stored posts

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Author {
    pub agent_kind: Option<String>,
    pub label: Option<String>,
    pub role: Option<String>,
    pub worker_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Producer {
    pub id: String,
    pub kind: ProducerKind,
}

/// One stored post, as `noa.timeline.get` returns it: the server's fields
/// with the client's [`PostInput`] flattened in, all but its `author` hint
/// (the key `author` is the resolved [`Author`], written once). A `Post`
/// read back therefore has `input.author` `None`, as the store keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Post {
    pub author: Author,
    pub board_id: String,
    pub created_at: String,
    pub post_id: String,
    pub producer: Producer,
    pub provenance: Provenance,
    pub request_id: String,
    pub run_id: Option<String>,
    /// Always [`SCHEMA_VERSION`]. Not written separately: the flattened
    /// input's `schemaVersion` is the one key on the wire.
    #[serde(skip, default = "schema_version")]
    pub schema_version: u32,
    pub seq: Seq,
    pub worker_id: Option<String>,
    #[serde(flatten, serialize_with = "serialize_stored_input")]
    pub input: PostInput,
}

fn schema_version() -> u32 {
    SCHEMA_VERSION
}

/// [`PostInput`]'s fields without `author`, as a [`Post`] flattens them.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredInput<'a> {
    artifacts: &'a [Artifact],
    based_on_post_ids: &'a [String],
    body_markdown: &'a str,
    checks: &'a [Check],
    kind: PostKind,
    next_actions: &'a [Item],
    occurred_at: &'a Option<String>,
    open_issues: &'a [Item],
    outcome: Outcome,
    retry_of_run_id: &'a Option<String>,
    schema_version: u32,
    source_ref: &'a SourceRef,
    summary: &'a str,
    supersedes_post_id: &'a Option<String>,
    task_ref: &'a Option<TaskRef>,
    title: &'a str,
}

fn serialize_stored_input<S>(input: &PostInput, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    // Destructured in full, so a field added to `PostInput` must be added
    // here too.
    let PostInput {
        artifacts,
        author: _,
        based_on_post_ids,
        body_markdown,
        checks,
        kind,
        next_actions,
        occurred_at,
        open_issues,
        outcome,
        retry_of_run_id,
        schema_version,
        source_ref,
        summary,
        supersedes_post_id,
        task_ref,
        title,
    } = input;
    StoredInput {
        artifacts,
        based_on_post_ids,
        body_markdown,
        checks,
        kind: *kind,
        next_actions,
        occurred_at,
        open_issues,
        outcome: *outcome,
        retry_of_run_id,
        schema_version: *schema_version,
        source_ref,
        summary,
        supersedes_post_id,
        task_ref,
        title,
    }
    .serialize(serializer)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckCounts {
    pub failed: u32,
    pub inconclusive: u32,
    pub not_run: u32,
    pub passed: u32,
}

/// A post in `list`/`changes`: no body, counts instead of lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostSummary {
    pub artifacts: u32,
    pub author: Author,
    pub checks: CheckCounts,
    pub created_at: String,
    pub kind: PostKind,
    pub occurred_at: Option<String>,
    pub open_issues: u32,
    pub outcome: Outcome,
    pub post_id: String,
    pub producer: Producer,
    pub provenance: Provenance,
    pub run_id: Option<String>,
    pub seq: Seq,
    pub source_namespace: String,
    pub summary: String,
    #[serde(default)]
    pub superseded_by: Vec<String>,
    pub supersedes_post_id: Option<String>,
    pub task_ref: Option<TaskRef>,
    pub title: String,
    pub worker_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Runs

/// Who may post a run's summary. `producerId` omitted with owner `worker`
/// means the run's `workerId`; owners `runner` and `manual` have no default
/// and must name it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishPolicy {
    pub owner: PublishOwner,
    pub producer_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunInfo {
    pub board_id: String,
    pub created_at: String,
    pub publish_policy: PublishPolicy,
    pub retry_of_run_id: Option<String>,
    pub run_id: String,
    pub summary_post_id: Option<String>,
    pub task_ref: Option<TaskRef>,
    pub worker_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Method params and results

/// Exactly one of `paneId` and `boardId`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextParams {
    pub board_id: Option<String>,
    pub pane_id: Option<WireId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextWorker {
    pub agent_kind: Option<String>,
    pub label: Option<String>,
    /// Every binding of `serverInstanceId`, `paneId` and `surfaceId` matches
    /// the current listing.
    pub live: bool,
    /// Only while `live`.
    pub pane_id: Option<WireId>,
    #[serde(default)]
    pub runs: Vec<RunInfo>,
    pub surface_id: Option<String>,
    pub worker_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSelf {
    pub pane_id: WireId,
    /// `root` or `worker`.
    pub role: String,
    #[serde(default)]
    pub runs: Vec<RunInfo>,
    pub worker_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextResult {
    pub archived: bool,
    pub board_id: String,
    pub head_seq: Seq,
    /// The calling pane's place on the board (wire `self`); `None` when the
    /// context was asked by `boardId`.
    #[serde(rename = "self")]
    pub me: Option<ContextSelf>,
    pub name: String,
    pub server_instance_id: String,
    #[serde(default)]
    pub workers: Vec<ContextWorker>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardsParams {
    /// Only boards after this position in the [`board_listing_order`]
    /// (the previous page's [`BoardsResult::next_before`]); absent = from
    /// the newest. A position, not a reference: the board it names need
    /// not exist.
    pub before: Option<BoardCursor>,
    /// At most [`BOARDS_MAX`]; default [`BOARDS_DEFAULT`].
    pub limit: Option<usize>,
}

/// `createdAt` and `lastActivityAt` are RFC 3339 UTC like
/// [`PostInput::occurred_at`], since a [`BoardCursor`] carries one back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardInfo {
    pub archived: bool,
    pub board_id: String,
    pub bytes: u64,
    pub created_at: String,
    pub head_seq: Seq,
    /// The greatest `createdAt` among the board's posts, so it never
    /// decreases; `None` while the board has none.
    pub last_activity_at: Option<String>,
    pub name: String,
    pub posts: u64,
}

impl BoardInfo {
    /// What the [`board_listing_order`] sorts by: the later (as instants) of
    /// `createdAt` and `lastActivityAt`, so the key never decreases, even
    /// when a post records a `createdAt` earlier than the board's own (a
    /// clock correction); `createdAt` while the board has no posts.
    pub fn activity_at(&self) -> &str {
        match self.last_activity_at.as_deref() {
            Some(last) if instant_key(last) > instant_key(&self.created_at) => last,
            _ => &self.created_at,
        }
    }

    /// The position just after this board, which continues a listing.
    pub fn cursor(&self) -> BoardCursor {
        BoardCursor {
            activity_at: self.activity_at().to_owned(),
            board_id: self.board_id.clone(),
        }
    }
}

/// A position in the [`board_listing_order`]: a board's
/// [`BoardInfo::activity_at`] and `boardId`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardCursor {
    pub activity_at: String,
    pub board_id: String,
}

impl BoardCursor {
    /// Whether `board` comes after this position, so a `boards` call with
    /// `before` = `self` may return it.
    pub fn admits(&self, board: &BoardInfo) -> bool {
        listing_order(
            (&self.activity_at, &self.board_id),
            (board.activity_at(), &board.board_id),
        ) == Ordering::Less
    }
}

/// The `noa.timeline.boards` order, `Less` when `a` is listed first:
/// newer [`BoardInfo::activity_at`] first (compared as instants), equal
/// ones by `boardId` ascending. Board ids are unique, so the order is total
/// and a page boundary falls between two boards, never inside a tie.
pub fn board_listing_order(a: &BoardInfo, b: &BoardInfo) -> Ordering {
    listing_order(
        (a.activity_at(), &a.board_id),
        (b.activity_at(), &b.board_id),
    )
}

fn listing_order(a: (&str, &str), b: (&str, &str)) -> Ordering {
    instant_key(b.0)
        .cmp(&instant_key(a.0))
        .then_with(|| a.1.cmp(b.1))
}

/// A [`valid_utc_timestamp`] as a key that sorts by instant: the date and
/// time are fixed width, and the fraction is read as nine digits padded
/// with zeros, so `…:05Z` and `…:05.000Z` are equal and both precede
/// `…:05.5Z` (which plain byte order would put first).
fn instant_key(s: &str) -> (&[u8], [u8; 9]) {
    let (whole, rest) = s.as_bytes().split_at(s.len().min(19));
    let mut fraction = [b'0'; 9];
    for (slot, digit) in fraction
        .iter_mut()
        .zip(rest.iter().filter(|b| b.is_ascii_digit()))
    {
        *slot = *digit;
    }
    (whole, fraction)
}

/// In the [`board_listing_order`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardsResult {
    pub boards: Vec<BoardInfo>,
    /// A board follows the last one returned.
    pub has_more: bool,
    /// The last returned board's [`BoardInfo::cursor`], to pass back as
    /// [`BoardsParams::before`]; `None` when no board is returned.
    pub next_before: Option<BoardCursor>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRunParams {
    pub board_id: Option<String>,
    pub board_name: Option<String>,
    pub label: Option<String>,
    pub orchestrator_pane_id: Option<WireId>,
    pub publish_policy: PublishPolicy,
    pub request_id: String,
    pub retry_of_run_id: Option<String>,
    pub role: Option<String>,
    pub task_ref: Option<TaskRef>,
    pub worker_pane_id: Option<WireId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRunResult {
    pub board_id: String,
    /// `false` when the request id was seen before with the same payload.
    pub created: bool,
    pub producer_id: String,
    pub run_id: String,
    pub worker_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPublishPolicyParams {
    pub board_id: String,
    pub publish_policy: PublishPolicy,
    pub request_id: String,
    pub run_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostParams {
    pub board_id: String,
    pub post: PostInput,
    pub producer_id: String,
    pub request_id: String,
    pub run_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostResult {
    pub created_at: String,
    /// The post was already stored under this key with the same payload.
    pub duplicate: bool,
    pub post_id: String,
    pub seq: Seq,
}

/// An empty list matches anything.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Filter {
    #[serde(default)]
    pub agent_kinds: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<PostKind>,
    #[serde(default)]
    pub outcomes: Vec<Outcome>,
    #[serde(default)]
    pub worker_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListParams {
    pub before_seq: Option<Seq>,
    pub board_id: String,
    pub filter: Option<Filter>,
    pub limit: Option<usize>,
    /// At most [`MAX_QUERY_CHARS`]; matched against title, summary and body.
    pub query: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesParams {
    pub after_seq: Seq,
    pub board_id: String,
    pub limit: Option<usize>,
}

/// `list` (descending, `nextBeforeSeq`) and `changes` (ascending,
/// `nextAfterSeq`). Only posts with `seq ≤ headSeq`, taken at the start of
/// the call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageResult {
    pub has_more: bool,
    pub head_seq: Seq,
    pub next_after_seq: Option<Seq>,
    pub next_before_seq: Option<Seq>,
    pub posts: Vec<PostSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetParams {
    pub board_id: String,
    pub post_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetResult {
    pub post: Post,
    #[serde(default)]
    pub superseded_by: Vec<String>,
}

// ---------------------------------------------------------------------------
// Calls

pub const METHOD_CONTEXT: &str = "noa.timeline.context";
pub const METHOD_BOARDS: &str = "noa.timeline.boards";
pub const METHOD_REGISTER_RUN: &str = "noa.timeline.registerRun";
pub const METHOD_SET_PUBLISH_POLICY: &str = "noa.timeline.setPublishPolicy";
pub const METHOD_POST: &str = "noa.timeline.post";
pub const METHOD_LIST: &str = "noa.timeline.list";
pub const METHOD_GET: &str = "noa.timeline.get";
pub const METHOD_CHANGES: &str = "noa.timeline.changes";

/// One parsed and shape-checked `noa.timeline.*` request.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "one call per request, moved once to the timeline service; the variants \
              hold their params directly as the frozen contract (board-timeline L2-2) has them"
)]
pub enum TimelineCall {
    Context(ContextParams),
    Boards(BoardsParams),
    RegisterRun(RegisterRunParams),
    SetPublishPolicy(SetPublishPolicyParams),
    Post(PostParams),
    List(ListParams),
    Get(GetParams),
    Changes(ChangesParams),
}

impl TimelineCall {
    /// `Ok(None)`: `method` is not one of the eight (the server answers
    /// -32601). Otherwise the params (a JSON object; `null` for none) are
    /// decoded and every check that needs no store is run: a bad board id
    /// is refused here, before any path is built from it.
    pub fn parse(method: &str, params: Value) -> Result<Option<Self>, TimelineError> {
        type Decode = fn(Value) -> Result<TimelineCall, TimelineError>;
        let decode_call: Decode = match method {
            METHOD_CONTEXT => |p| decode(p).map(TimelineCall::Context),
            METHOD_BOARDS => |p| decode(p).map(TimelineCall::Boards),
            METHOD_REGISTER_RUN => |p| decode(p).map(TimelineCall::RegisterRun),
            METHOD_SET_PUBLISH_POLICY => |p| decode(p).map(TimelineCall::SetPublishPolicy),
            METHOD_POST => |p| decode(p).map(TimelineCall::Post),
            METHOD_LIST => |p| decode(p).map(TimelineCall::List),
            METHOD_GET => |p| decode(p).map(TimelineCall::Get),
            METHOD_CHANGES => |p| decode(p).map(TimelineCall::Changes),
            _ => return Ok(None),
        };
        // A request without params is an empty object; any other non-object
        // is refused (serde would read an array positionally).
        let params = match params {
            Value::Null => Value::Object(serde_json::Map::new()),
            Value::Object(_) => params,
            _ => {
                return Err(TimelineError::invalid_params(
                    "params must be a JSON object",
                ));
            }
        };
        if method == METHOD_POST
            && let Some(version) = params
                .get("post")
                .and_then(|post| post.get("schemaVersion"))
                .and_then(Value::as_u64)
        {
            // Before decoding: a newer version's post may carry fields this
            // one refuses as unknown, and is `unsupported`, not malformed.
            check_schema_version(version)?;
        }
        let call = decode_call(params)?;
        call.validate()?;
        Ok(Some(call))
    }

    pub fn method(&self) -> &'static str {
        match self {
            TimelineCall::Context(_) => METHOD_CONTEXT,
            TimelineCall::Boards(_) => METHOD_BOARDS,
            TimelineCall::RegisterRun(_) => METHOD_REGISTER_RUN,
            TimelineCall::SetPublishPolicy(_) => METHOD_SET_PUBLISH_POLICY,
            TimelineCall::Post(_) => METHOD_POST,
            TimelineCall::List(_) => METHOD_LIST,
            TimelineCall::Get(_) => METHOD_GET,
            TimelineCall::Changes(_) => METHOD_CHANGES,
        }
    }

    /// `control` for the writes, `read` for the rest.
    pub fn scope(&self) -> Scope {
        if self.is_write() {
            Scope::Control
        } else {
            Scope::Read
        }
    }

    /// `registerRun`, `setPublishPolicy` and `post`.
    pub fn is_write(&self) -> bool {
        matches!(
            self,
            TimelineCall::RegisterRun(_)
                | TimelineCall::SetPublishPolicy(_)
                | TimelineCall::Post(_)
        )
    }

    pub fn request_id(&self) -> Option<&str> {
        match self {
            TimelineCall::RegisterRun(p) => Some(&p.request_id),
            TimelineCall::SetPublishPolicy(p) => Some(&p.request_id),
            TimelineCall::Post(p) => Some(&p.request_id),
            _ => None,
        }
    }

    fn validate(&self) -> Result<(), TimelineError> {
        match self {
            TimelineCall::Context(p) => {
                match (&p.board_id, &p.pane_id) {
                    (Some(board), None) => check_id("boardId", IdKind::Board, board)?,
                    (None, Some(_)) => {}
                    _ => {
                        return Err(TimelineError::invalid_params(
                            "context takes exactly one of paneId and boardId",
                        ));
                    }
                }
                Ok(())
            }
            TimelineCall::Boards(p) => {
                check_limit(p.limit, BOARDS_MAX)?;
                if let Some(cursor) = &p.before {
                    check_id("before.boardId", IdKind::Board, &cursor.board_id)?;
                    if !valid_utc_timestamp(&cursor.activity_at) {
                        return Err(TimelineError::invalid_params(
                            "before.activityAt must be RFC 3339 UTC: YYYY-MM-DDTHH:MM:SS[.fff]Z",
                        ));
                    }
                }
                Ok(())
            }
            TimelineCall::RegisterRun(p) => {
                check_token("requestId", &p.request_id)?;
                if let Some(board) = &p.board_id {
                    check_id("boardId", IdKind::Board, board)?;
                }
                if let Some(name) = &p.board_name {
                    check_text("boardName", name, MAX_TITLE_CHARS)?;
                }
                check_label("label", p.label.as_deref())?;
                check_label("role", p.role.as_deref())?;
                if let Some(task) = &p.task_ref {
                    check_task_ref(task)?;
                }
                if let Some(run) = &p.retry_of_run_id {
                    check_id("retryOfRunId", IdKind::Run, run)?;
                }
                check_policy(&p.publish_policy)
            }
            TimelineCall::SetPublishPolicy(p) => {
                check_id("boardId", IdKind::Board, &p.board_id)?;
                check_id("runId", IdKind::Run, &p.run_id)?;
                check_token("requestId", &p.request_id)?;
                check_policy(&p.publish_policy)
            }
            TimelineCall::Post(p) => {
                check_id("boardId", IdKind::Board, &p.board_id)?;
                check_token("producerId", &p.producer_id)?;
                check_token("requestId", &p.request_id)?;
                match &p.run_id {
                    Some(run) => check_id("runId", IdKind::Run, run)?,
                    None => {
                        let namespace = p.post.source_ref.namespace.as_str();
                        if !UNREGISTERED_NAMESPACES.contains(&namespace) {
                            return Err(TimelineError::invalid_params(format!(
                                "a post without runId needs sourceRef.namespace {}, not {namespace:?}",
                                UNREGISTERED_NAMESPACES.join(" or ")
                            )));
                        }
                    }
                }
                p.post.validate()
            }
            TimelineCall::List(p) => {
                check_id("boardId", IdKind::Board, &p.board_id)?;
                check_limit(p.limit, LIST_MAX)?;
                if let Some(query) = &p.query
                    && query.chars().count() > MAX_QUERY_CHARS
                {
                    return Err(TimelineError::invalid_params(format!(
                        "query is over {MAX_QUERY_CHARS} characters"
                    )));
                }
                if let Some(filter) = &p.filter {
                    for worker in &filter.worker_ids {
                        check_id("filter.workerIds", IdKind::Worker, worker)?;
                    }
                }
                Ok(())
            }
            TimelineCall::Get(p) => {
                check_id("boardId", IdKind::Board, &p.board_id)?;
                check_id("postId", IdKind::Post, &p.post_id)
            }
            TimelineCall::Changes(p) => {
                check_id("boardId", IdKind::Board, &p.board_id)?;
                check_limit(p.limit, CHANGES_MAX)
            }
        }
    }
}

fn check_schema_version(version: u64) -> Result<(), TimelineError> {
    if version == u64::from(SCHEMA_VERSION) {
        Ok(())
    } else {
        Err(TimelineError::new(
            TimelineErrorKind::Unsupported,
            format!("schemaVersion {version} is not supported (this Noa takes {SCHEMA_VERSION})"),
        ))
    }
}

fn decode<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, TimelineError> {
    serde_json::from_value(params)
        .map_err(|err| TimelineError::invalid_params(format!("invalid params: {err}")))
}

fn check_text(what: &str, text: &str, max_chars: usize) -> Result<(), TimelineError> {
    if text.chars().all(char::is_whitespace) {
        return Err(TimelineError::invalid_params(format!(
            "{what} must not be empty or blank"
        )));
    }
    let chars = text.chars().count();
    if chars > max_chars {
        return Err(TimelineError::invalid_params(format!(
            "{what} is {chars} characters; the most is {max_chars}"
        )));
    }
    Ok(())
}

fn check_token(what: &str, token: &str) -> Result<(), TimelineError> {
    if valid_token(token) {
        Ok(())
    } else {
        Err(TimelineError::invalid_params(format!(
            "{what} must be 1 to {MAX_TOKEN_BYTES} bytes of A-Z a-z 0-9 . _ : / -"
        )))
    }
}

fn check_label(what: &str, label: Option<&str>) -> Result<(), TimelineError> {
    match label {
        Some(label) if label.len() > MAX_TOKEN_BYTES => Err(TimelineError::invalid_params(
            format!("{what} is over {MAX_TOKEN_BYTES} bytes"),
        )),
        _ => Ok(()),
    }
}

fn check_id(what: &str, kind: IdKind, id: &str) -> Result<(), TimelineError> {
    if valid_id(kind, id) {
        Ok(())
    } else {
        Err(TimelineError::invalid_params(format!(
            "{what} {id:?} is not {}<16 lowercase hex digits>",
            kind.prefix()
        )))
    }
}

fn check_limit(limit: Option<usize>, max: usize) -> Result<(), TimelineError> {
    match limit {
        Some(limit) if limit == 0 || limit > max => Err(TimelineError::invalid_params(format!(
            "limit must be 1 to {max}"
        ))),
        _ => Ok(()),
    }
}

fn check_task_ref(task: &TaskRef) -> Result<(), TimelineError> {
    if !TASK_REF_KINDS.contains(&task.kind.as_str()) {
        return Err(TimelineError::invalid_params(format!(
            "taskRef kind {:?} is not one of {}",
            task.kind,
            TASK_REF_KINDS.join(", ")
        )));
    }
    if task.id.trim().is_empty() {
        return Err(TimelineError::invalid_params(
            "taskRef id must not be empty",
        ));
    }
    Ok(())
}

fn check_policy(policy: &PublishPolicy) -> Result<(), TimelineError> {
    match (&policy.producer_id, policy.owner) {
        (Some(producer), _) => check_token("publishPolicy.producerId", producer),
        (None, PublishOwner::Worker) => Ok(()),
        (None, owner @ (PublishOwner::Runner | PublishOwner::Manual)) => {
            Err(TimelineError::invalid_params(format!(
                "publishPolicy owner {} needs a producerId (only owner worker defaults to the \
                 run's workerId)",
                serde_json::to_value(owner).expect("an owner serialises")
            )))
        }
    }
}

/// `YYYY-MM-DDTHH:MM:SS[.f…]Z` with in-range fields: a day that exists in
/// its month (leap years by the Gregorian rule), no leap second.
fn valid_utc_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = b.get(range)?;
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| part.iter().fold(0, |n, d| n * 10 + u32::from(d - b'0')))
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        digits(0..4),
        digits(5..7),
        digits(8..10),
        digits(11..13),
        digits(14..16),
        digits(17..19),
    ) else {
        return false;
    };
    let separators = b.get(4) == Some(&b'-')
        && b.get(7) == Some(&b'-')
        && b.get(10) == Some(&b'T')
        && b.get(13) == Some(&b':')
        && b.get(16) == Some(&b':');
    let rest = &b[19.min(b.len())..];
    let tail_ok = match rest {
        [b'Z'] => true,
        [b'.', fraction @ .., b'Z'] => {
            (1..=9).contains(&fraction.len()) && fraction.iter().all(u8::is_ascii_digit)
        }
        _ => false,
    };
    separators
        && tail_ok
        && (1..=12).contains(&month)
        && (1..=days_in_month(year, month)).contains(&day)
        && hour < 24
        && minute < 60
        && second < 60
}

/// Days in `month` (1..=12) of `year`.
fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

// ---------------------------------------------------------------------------
// Errors

/// `error.data.kind` of a timeline failure (L2-3). Scope denials and
/// unknown methods keep their existing codes and carry no kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineErrorKind {
    InvalidParams,
    Unsupported,
    OutcomeUnknown,
    Conflict,
    NotFound,
    Busy,
    StorageFull,
    IoError,
    CorruptStore,
}

impl TimelineErrorKind {
    const ALL: [TimelineErrorKind; 9] = [
        TimelineErrorKind::InvalidParams,
        TimelineErrorKind::Unsupported,
        TimelineErrorKind::OutcomeUnknown,
        TimelineErrorKind::Conflict,
        TimelineErrorKind::NotFound,
        TimelineErrorKind::Busy,
        TimelineErrorKind::StorageFull,
        TimelineErrorKind::IoError,
        TimelineErrorKind::CorruptStore,
    ];

    pub const fn code(self) -> ErrorCode {
        match self {
            TimelineErrorKind::InvalidParams => ErrorCode::InvalidParams,
            TimelineErrorKind::Unsupported => ErrorCode::Unsupported,
            TimelineErrorKind::OutcomeUnknown => ErrorCode::OutcomeUnknown,
            TimelineErrorKind::Conflict => ErrorCode::TimelineConflict,
            TimelineErrorKind::NotFound => ErrorCode::TimelineNotFound,
            TimelineErrorKind::Busy => ErrorCode::Busy,
            TimelineErrorKind::StorageFull => ErrorCode::StorageFull,
            TimelineErrorKind::IoError => ErrorCode::StoreIo,
            TimelineErrorKind::CorruptStore => ErrorCode::CorruptStore,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            TimelineErrorKind::InvalidParams => "invalid_params",
            TimelineErrorKind::Unsupported => "unsupported",
            TimelineErrorKind::OutcomeUnknown => "outcome_unknown",
            TimelineErrorKind::Conflict => "conflict",
            TimelineErrorKind::NotFound => "not_found",
            TimelineErrorKind::Busy => "busy",
            TimelineErrorKind::StorageFull => "storage_full",
            TimelineErrorKind::IoError => "io_error",
            TimelineErrorKind::CorruptStore => "corrupt_store",
        }
    }

    /// The kind a JSON-RPC error code stands for. The Rust client's
    /// `ClientError::Rpc` keeps only `{code, message}`, so a caller derives
    /// the kind from the code (-32602 reads as `invalid_params`).
    pub fn from_code(code: i64) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.code().code() == code)
    }
}

impl fmt::Display for TimelineErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A refused or failed timeline request: its kind, a message for people,
/// and the `error.data` fields that kind carries.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct TimelineError {
    pub kind: TimelineErrorKind,
    pub message: String,
    /// `outcome_unknown`: the request id to send again.
    pub request_id: Option<String>,
    /// `conflict`: the post already stored under the key.
    pub existing_post_id: Option<String>,
    /// `conflict`: the run already registered under the key.
    pub existing_run_id: Option<String>,
    /// `corrupt_store`: the 1-based line of `posts.jsonl` that stopped
    /// replay.
    pub line: Option<u64>,
}

impl TimelineError {
    pub fn new(kind: TimelineErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            request_id: None,
            existing_post_id: None,
            existing_run_id: None,
            line: None,
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(TimelineErrorKind::InvalidParams, message)
    }

    /// What a backend without a timeline service answers.
    pub fn unsupported() -> Self {
        Self::new(
            TimelineErrorKind::Unsupported,
            "this Noa has no board timeline",
        )
    }

    pub const fn code(&self) -> ErrorCode {
        self.kind.code()
    }

    /// `error.data`: `{kind}` plus whichever of `requestId`,
    /// `existingPostId`, `existingRunId` and `line` are set.
    pub fn data(&self) -> Value {
        let mut data = serde_json::Map::new();
        data.insert("kind".to_string(), self.kind.as_str().into());
        let fields = [
            ("requestId", self.request_id.clone().map(Value::from)),
            (
                "existingPostId",
                self.existing_post_id.clone().map(Value::from),
            ),
            (
                "existingRunId",
                self.existing_run_id.clone().map(Value::from),
            ),
            ("line", self.line.map(Value::from)),
        ];
        for (key, value) in fields {
            if let Some(value) = value {
                data.insert(key.to_string(), value);
            }
        }
        Value::Object(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BOARD: &str = "bd-0123456789abcdef";
    const RUN: &str = "rn-0123456789abcdef";
    const POST: &str = "po-0123456789abcdef";

    fn input() -> Value {
        json!({
            "schemaVersion": 1,
            "kind": "summary",
            "outcome": "succeeded",
            "title": "Built the parser",
            "summary": "All VT tests pass.",
            "sourceRef": { "namespace": "worker", "sessionId": "s1", "eventId": "e1" },
        })
    }

    fn post_input() -> PostInput {
        serde_json::from_value(input()).unwrap()
    }

    fn post_params(post: Value) -> Value {
        json!({
            "boardId": BOARD,
            "runId": RUN,
            "producerId": "wk-0123456789abcdef",
            "requestId": "req-1",
            "post": post,
        })
    }

    fn kind_of(result: Result<Option<TimelineCall>, TimelineError>) -> TimelineErrorKind {
        result.expect_err("refused").kind
    }

    #[test]
    fn seq_is_decimal_string() {
        assert_eq!(
            serde_json::to_string(&Seq(u64::MAX)).unwrap(),
            "\"18446744073709551615\""
        );
        assert_eq!(serde_json::from_str::<Seq>("\"42\"").unwrap(), Seq(42));
        assert_eq!(
            serde_json::from_value::<Seq>(json!("18446744073709551615")).unwrap(),
            Seq(u64::MAX)
        );
        // A JSON number is refused: above 2^53 a JS client has rounded it.
        assert!(serde_json::from_str::<Seq>("42").is_err());
        assert!(serde_json::from_value::<Seq>(json!(9_007_199_254_740_993_u64)).is_err());
        assert!(serde_json::from_str::<Seq>("\"-1\"").is_err());
        let page = PageResult {
            has_more: false,
            head_seq: Seq(7),
            next_after_seq: Some(Seq(7)),
            next_before_seq: None,
            posts: Vec::new(),
        };
        assert_eq!(
            serde_json::to_value(&page).unwrap(),
            json!({
                "hasMore": false, "headSeq": "7", "nextAfterSeq": "7",
                "nextBeforeSeq": null, "posts": [],
            })
        );
    }

    #[test]
    fn ids_and_tokens() {
        assert!(valid_id(IdKind::Board, BOARD));
        assert!(!valid_id(IdKind::Run, BOARD));
        assert!(!valid_id(IdKind::Board, "bd-0123456789ABCDEF"));
        assert!(!valid_id(IdKind::Board, "bd-0123456789abcde"));
        assert!(!valid_id(IdKind::Board, "bd-../../etc/passwd0"));
        assert!(valid_token("review:s-1/verdict.r2_x"));
        assert!(!valid_token(""));
        assert!(!valid_token("has space"));
        assert!(!valid_token(&"a".repeat(MAX_TOKEN_BYTES + 1)));
        assert!(valid_token(&"a".repeat(MAX_TOKEN_BYTES)));
    }

    #[test]
    fn canon_golden_bytes() {
        let bytes = canon_bytes(Some(RUN), Some("wk-0123456789abcdef"), &post_input());
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            concat!(
                r#"{"post":{"artifacts":[],"author":null,"basedOnPostIds":[],"bodyMarkdown":"","#,
                r#""checks":[],"#,
                r#""kind":"summary","nextActions":[],"occurredAt":null,"openIssues":[],"#,
                r#""outcome":"succeeded","retryOfRunId":null,"schemaVersion":1,"#,
                r#""sourceRef":{"eventId":"e1","locator":null,"namespace":"worker","#,
                r#""runId":null,"sessionId":"s1"},"summary":"All VT tests pass.","#,
                r#""supersedesPostId":null,"taskRef":null,"title":"Built the parser"},"#,
                r#""producerId":"wk-0123456789abcdef","runId":"rn-0123456789abcdef"}"#,
            )
        );
        // Key B: no producer.
        let key_b = String::from_utf8(canon_bytes(Some(RUN), None, &post_input())).unwrap();
        assert!(key_b.ends_with(r#""producerId":null,"runId":"rn-0123456789abcdef"}"#));
    }

    #[test]
    fn canon_ignores_key_order_and_spelled_out_defaults() {
        let reordered: PostInput = serde_json::from_str(
            r#"{"sourceRef":{"sessionId":"s1","eventId":"e1","namespace":"worker","runId":null},
                "title":"Built the parser","summary":"All VT tests pass.","outcome":"succeeded",
                "kind":"summary","schemaVersion":1,"checks":[],"openIssues":[],"taskRef":null}"#,
        )
        .unwrap();
        assert_eq!(
            canon_bytes(Some(RUN), Some("p"), &reordered.normalized()),
            canon_bytes(Some(RUN), Some("p"), &post_input().normalized())
        );
        // A client that spells out `"author": null` takes the canon of one
        // that leaves it out.
        let mut spelled = input();
        spelled["author"] = Value::Null;
        let spelled: PostInput = serde_json::from_value(spelled).unwrap();
        assert_eq!(
            canon_bytes(Some(RUN), Some("p"), &spelled.normalized()),
            canon_bytes(Some(RUN), Some("p"), &post_input().normalized())
        );
    }

    #[test]
    fn a_string_item_is_text_without_ref() {
        let mut value = input();
        value["openIssues"] = json!(["flaky test", { "text": "docs", "ref": "README.md" }]);
        let post: PostInput = serde_json::from_value(value).unwrap();
        assert_eq!(
            post.open_issues,
            vec![
                Item {
                    r#ref: None,
                    text: "flaky test".to_string()
                },
                Item {
                    r#ref: Some("README.md".to_string()),
                    text: "docs".to_string()
                },
            ]
        );
        assert_eq!(
            serde_json::to_value(&post.open_issues[0]).unwrap(),
            json!({ "ref": null, "text": "flaky test" })
        );
    }

    #[test]
    fn post_input_refuses_unknown_fields() {
        let mut value = input();
        value["titel"] = json!("typo");
        assert!(serde_json::from_value::<PostInput>(value).is_err());
    }

    #[test]
    fn a_stored_post_round_trips_with_one_schema_version_and_author() {
        let mut post = Post {
            author: Author {
                agent_kind: Some("claude".to_string()),
                worker_id: Some("wk-0123456789abcdef".to_string()),
                ..Author::default()
            },
            board_id: BOARD.to_string(),
            created_at: "2026-11-26T00:00:00Z".to_string(),
            post_id: POST.to_string(),
            producer: Producer {
                id: "wk-0123456789abcdef".to_string(),
                kind: ProducerKind::Worker,
            },
            provenance: Provenance::Registered,
            request_id: "req-1".to_string(),
            run_id: Some(RUN.to_string()),
            schema_version: SCHEMA_VERSION,
            seq: Seq(3),
            worker_id: Some("wk-0123456789abcdef".to_string()),
            input: post_input(),
        };
        let text = serde_json::to_string(&post).unwrap();
        assert_eq!(text.matches("\"schemaVersion\"").count(), 1);
        assert_eq!(text.matches("\"author\"").count(), 1);
        assert!(text.contains("\"seq\":\"3\""));
        let back: Post = serde_json::from_str(&text).unwrap();
        assert_eq!(back, post);

        // A hint left in the input is not written: `author` is still the
        // resolved author, once, and reads back without the hint.
        post.input.author = Some(AuthorHint {
            label: Some("hint".to_string()),
            role: None,
        });
        let text = serde_json::to_string(&post).unwrap();
        assert_eq!(text.matches("\"author\"").count(), 1);
        assert!(!text.contains("hint"), "{text}");
        let back: Post = serde_json::from_str(&text).unwrap();
        assert_eq!(back.author, post.author);
        assert_eq!(back.input.author, None);
    }

    #[test]
    fn context_self_is_wire_self() {
        let ctx = ContextResult {
            archived: false,
            board_id: BOARD.to_string(),
            head_seq: Seq(0),
            me: Some(ContextSelf {
                pane_id: WireId(4),
                role: "root".to_string(),
                runs: Vec::new(),
                worker_id: None,
            }),
            name: "board".to_string(),
            server_instance_id: "abc".to_string(),
            workers: Vec::new(),
        };
        let value = serde_json::to_value(&ctx).unwrap();
        assert_eq!(value["self"]["paneId"], json!("4"));
        assert_eq!(serde_json::from_value::<ContextResult>(value).unwrap(), ctx);
    }

    #[test]
    fn parse_context() {
        let call = TimelineCall::parse(METHOD_CONTEXT, json!({ "paneId": "3" })).unwrap();
        assert_eq!(
            call,
            Some(TimelineCall::Context(ContextParams {
                board_id: None,
                pane_id: Some(WireId(3))
            }))
        );
        let call = TimelineCall::parse(METHOD_CONTEXT, json!({ "boardId": BOARD }))
            .unwrap()
            .unwrap();
        assert_eq!(call.scope(), Scope::Read);
        assert!(!call.is_write());
        assert_eq!(call.request_id(), None);
        assert_eq!(
            kind_of(TimelineCall::parse(METHOD_CONTEXT, json!({}))),
            TimelineErrorKind::InvalidParams
        );
        assert_eq!(
            kind_of(TimelineCall::parse(
                METHOD_CONTEXT,
                json!({ "paneId": "3", "boardId": BOARD })
            )),
            TimelineErrorKind::InvalidParams
        );
    }

    #[test]
    fn parse_boards() {
        let call = TimelineCall::parse(METHOD_BOARDS, Value::Null)
            .unwrap()
            .unwrap();
        assert_eq!(
            call,
            TimelineCall::Boards(BoardsParams {
                before: None,
                limit: None
            })
        );
        assert_eq!(call.scope(), Scope::Read);
        assert!(TimelineCall::parse(METHOD_BOARDS, json!({ "limit": 200 })).is_ok());
        assert_eq!(
            kind_of(TimelineCall::parse(METHOD_BOARDS, json!({ "limit": 201 }))),
            TimelineErrorKind::InvalidParams
        );
    }

    // Second review: `boards` pages by a (activityAt, boardId) cursor.
    #[test]
    fn parse_boards_cursor() {
        let call = TimelineCall::parse(
            METHOD_BOARDS,
            json!({ "before": { "activityAt": "2026-11-26T10:00:05.5Z", "boardId": BOARD },
                    "limit": 200 }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            call,
            TimelineCall::Boards(BoardsParams {
                before: Some(BoardCursor {
                    activity_at: "2026-11-26T10:00:05.5Z".into(),
                    board_id: BOARD.into(),
                }),
                limit: Some(200),
            })
        );
        assert!(TimelineCall::parse(METHOD_BOARDS, json!({ "before": null })).is_ok());
        for before in [
            json!({ "activityAt": "2026-11-26T10:00:05Z", "boardId": "bd-1" }),
            json!({ "activityAt": "2026-11-26T10:00:05Z", "boardId": POST }),
            json!({ "activityAt": "2026-11-26 10:00:05Z", "boardId": BOARD }),
            json!({ "activityAt": "2026-02-30T10:00:05Z", "boardId": BOARD }),
            json!({ "activityAt": "2026-11-26T10:00:05+09:00", "boardId": BOARD }),
            json!({ "boardId": BOARD }),
            json!({ "activityAt": "2026-11-26T10:00:05Z" }),
            json!("bd-0123456789abcdef"),
            json!(5),
        ] {
            assert_eq!(
                kind_of(TimelineCall::parse(
                    METHOD_BOARDS,
                    json!({ "before": before.clone() })
                )),
                TimelineErrorKind::InvalidParams,
                "{before}"
            );
        }
    }

    #[test]
    fn boards_wire_shape() {
        let cursor = BoardCursor {
            activity_at: "2026-11-26T10:00:05Z".into(),
            board_id: BOARD.into(),
        };
        let params = BoardsParams {
            before: Some(cursor.clone()),
            limit: Some(10),
        };
        let wire = serde_json::to_value(&params).unwrap();
        assert_eq!(
            wire,
            json!({ "before": { "activityAt": "2026-11-26T10:00:05Z", "boardId": BOARD },
                    "limit": 10 })
        );
        // What a client sends is what the server parses.
        assert_eq!(
            TimelineCall::parse(METHOD_BOARDS, wire).unwrap().unwrap(),
            TimelineCall::Boards(params)
        );
        // A non-empty page carries the last board's cursor.
        let board = BoardInfo {
            archived: false,
            board_id: BOARD.into(),
            bytes: 512,
            created_at: "2026-11-26T09:00:00Z".into(),
            head_seq: Seq(3),
            last_activity_at: Some("2026-11-26T10:00:05Z".into()),
            name: "main".into(),
            posts: 3,
        };
        assert_eq!(board.cursor(), cursor);
        let result = BoardsResult {
            boards: vec![board],
            has_more: true,
            next_before: Some(cursor),
        };
        assert_eq!(
            serde_json::to_string(&result).unwrap(),
            format!(
                concat!(
                    r#"{{"boards":[{{"archived":false,"boardId":"{BOARD}","bytes":512,"#,
                    r#""createdAt":"2026-11-26T09:00:00Z","headSeq":"3","#,
                    r#""lastActivityAt":"2026-11-26T10:00:05Z","name":"main","posts":3}}],"#,
                    r#""hasMore":true,"nextBefore":{{"activityAt":"2026-11-26T10:00:05Z","boardId":"{BOARD}"}}}}"#
                ),
                BOARD = BOARD
            )
        );
        // An empty page: nothing follows and there is no cursor.
        let empty = BoardsResult {
            boards: Vec::new(),
            has_more: false,
            next_before: None,
        };
        assert_eq!(
            serde_json::to_string(&empty).unwrap(),
            r#"{"boards":[],"hasMore":false,"nextBefore":null}"#
        );
    }

    fn board_info(i: u64, activity: Option<&str>, created: &str) -> BoardInfo {
        BoardInfo {
            archived: i.is_multiple_of(3),
            // Distinct for distinct i: an odd multiplier is a bijection mod 2^64.
            board_id: format!("bd-{:016x}", i.wrapping_mul(0x9e37_79b9_7f4a_7c15)),
            bytes: 0,
            created_at: created.into(),
            head_seq: Seq(0),
            last_activity_at: activity.map(str::to_owned),
            name: format!("board {i}"),
            posts: 0,
        }
    }

    #[test]
    fn board_listing_order_is_newest_activity_first_then_board_id() {
        let at = |s: &str| board_info(1, Some(s), "2026-01-01T00:00:00Z");
        // Compared as instants, not bytes: "…05Z" < "…05.5Z" < "…06Z".
        let order = [
            at("2026-11-26T10:00:06Z"),
            at("2026-11-26T10:00:05.5Z"),
            at("2026-11-26T10:00:05Z"),
            at("2026-11-25T23:59:59.999999999Z"),
        ];
        for pair in order.windows(2) {
            assert_eq!(
                board_listing_order(&pair[0], &pair[1]),
                Ordering::Less,
                "{} before {}",
                pair[0].activity_at(),
                pair[1].activity_at()
            );
        }
        // No posts yet: createdAt stands in.
        let empty = board_info(2, None, "2026-11-26T10:00:05.7Z");
        assert_eq!(empty.activity_at(), "2026-11-26T10:00:05.7Z");
        assert_eq!(board_listing_order(&order[0], &empty), Ordering::Less);
        assert_eq!(board_listing_order(&empty, &order[1]), Ordering::Less);
        // Equal instants (with and without a fraction) fall back to boardId.
        let mut a = at("2026-11-26T10:00:05Z");
        let mut b = at("2026-11-26T10:00:05.000Z");
        a.board_id = "bd-0000000000000001".into();
        b.board_id = "bd-0000000000000002".into();
        assert_eq!(board_listing_order(&a, &b), Ordering::Less);
        assert_eq!(board_listing_order(&b, &a), Ordering::Greater);
        assert_eq!(board_listing_order(&a, &a), Ordering::Equal);
        assert!(a.cursor().admits(&b));
        assert!(!b.cursor().admits(&a));
        assert!(!a.cursor().admits(&a));
    }

    /// One `boards` page over `boards` exactly as L2-2 defines it, built
    /// from the contract's helpers (the store implements the same rule).
    fn boards_page(boards: &[BoardInfo], params: &BoardsParams) -> BoardsResult {
        let mut after: Vec<&BoardInfo> = boards
            .iter()
            .filter(|b| params.before.as_ref().is_none_or(|c| c.admits(b)))
            .collect();
        after.sort_by(|a, b| board_listing_order(a, b));
        let limit = params.limit.unwrap_or(BOARDS_DEFAULT);
        let has_more = after.len() > limit;
        let page: Vec<BoardInfo> = after.into_iter().take(limit).cloned().collect();
        BoardsResult {
            next_before: page.last().map(BoardInfo::cursor),
            boards: page,
            has_more,
        }
    }

    /// 450 boards, a third without posts, pairs at one instant spelled two
    /// ways, so every page edge can fall inside a tie.
    fn many_boards() -> Vec<BoardInfo> {
        (0..450_u64)
            .map(|i| {
                let second = i / 2;
                let stamp = format!(
                    "2026-11-26T{:02}:{:02}:{:02}{}Z",
                    10 + second / 3600,
                    second / 60 % 60,
                    second % 60,
                    if i.is_multiple_of(2) { "" } else { ".000" }
                );
                if i.is_multiple_of(3) {
                    board_info(i, None, &stamp)
                } else {
                    board_info(i, Some(&stamp), "2026-01-01T00:00:00Z")
                }
            })
            .collect()
    }

    /// Pages from the newest until `hasMore` is false; `between` runs
    /// before each later page.
    fn page_through(
        boards: &mut [BoardInfo],
        limit: usize,
        mut between: impl FnMut(&mut [BoardInfo], usize),
    ) -> Vec<String> {
        let mut seen = Vec::new();
        let mut params = BoardsParams {
            before: None,
            limit: Some(limit),
        };
        for page in 0.. {
            if page > 0 {
                between(boards, page);
            }
            let result = boards_page(boards, &params);
            seen.extend(result.boards.iter().map(|b| b.board_id.clone()));
            if !result.has_more {
                break;
            }
            params.before = result.next_before;
            assert!(params.before.is_some(), "hasMore with no cursor");
        }
        seen
    }

    // Second review: 201+ boards are all reachable, each exactly once.
    #[test]
    fn boards_pages_reach_every_board_once() {
        let mut boards = many_boards();
        let mut expected: Vec<&BoardInfo> = boards.iter().collect();
        expected.sort_by(|a, b| board_listing_order(a, b));
        let expected: Vec<String> = expected.iter().map(|b| b.board_id.clone()).collect();
        for limit in [BOARDS_MAX, 7, 1] {
            let seen = page_through(&mut boards, limit, |_, _| {});
            assert_eq!(seen, expected, "limit {limit}");
        }
    }

    // A board's key only grows, so a pass never returns a board twice and
    // returns every board whose key is unchanged exactly once. One whose key
    // grows during the pass may be skipped by it (it leads a fresh pass), as
    // here, or, when the new key still falls after the cursor (an equal
    // instant and a larger boardId), appear once later in the same pass.
    #[test]
    fn boards_paging_survives_activity_during_the_pass() {
        let mut boards = many_boards();
        let mut sorted: Vec<&BoardInfo> = boards.iter().collect();
        sorted.sort_by(|a, b| board_listing_order(a, b));
        let returned_early = sorted[3].board_id.clone();
        let not_yet_reached = sorted[400].board_id.clone();
        let moved = [returned_early.clone(), not_yet_reached.clone()];
        let seen = page_through(&mut boards, BOARDS_MAX, |boards, page| {
            if page == 1 {
                for board in boards.iter_mut().filter(|b| moved.contains(&b.board_id)) {
                    board.last_activity_at = Some("2026-11-26T12:00:00.25Z".into());
                }
            }
        });
        let mut unique = seen.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), seen.len(), "a board was returned twice");
        assert_eq!(seen.iter().filter(|id| **id == returned_early).count(), 1);
        assert!(!seen.contains(&not_yet_reached));
        for board in &boards {
            if !moved.contains(&board.board_id) {
                assert!(seen.contains(&board.board_id), "{} missed", board.board_id);
            }
        }
        let next_pass = boards_page(&boards, &BoardsParams::default());
        let leaders: Vec<&str> = next_pass.boards[..2]
            .iter()
            .map(|b| b.board_id.as_str())
            .collect();
        let mut moved_sorted = moved.clone();
        moved_sorted.sort();
        assert_eq!(leaders, moved_sorted);
    }

    // Review of the cursor: after a clock correction a board's first post
    // can record a `createdAt` earlier than the board's own. The key is
    // max(createdAt, lastActivityAt), so it does not move backwards and the
    // board is not listed again on a later page.
    #[test]
    fn boards_key_ignores_posts_older_than_the_board() {
        let created = "2026-11-26T12:00:00Z";
        let mut a = board_info(1000, None, created);
        assert_eq!(a.activity_at(), created);
        a.last_activity_at = Some("2026-11-26T10:00:00Z".into());
        assert_eq!(a.activity_at(), created);
        // Equal instants spelled two ways: either spelling is the same key.
        a.last_activity_at = Some("2026-11-26T12:00:00.000Z".into());
        assert_eq!(instant_key(a.activity_at()), instant_key(created));
        a.last_activity_at = Some("2026-11-26T12:00:00.5Z".into());
        assert_eq!(a.activity_at(), "2026-11-26T12:00:00.5Z");

        // A (created 12:00, newest) is listed on the first page; its first
        // post, stamped 10:00, lands between pages. With the key read from
        // lastActivityAt alone, A would fall among the 10:00 boards still
        // ahead and be listed a second time.
        let mut boards = many_boards();
        boards.push(board_info(1000, None, created));
        let a_id = boards.last().unwrap().board_id.clone();
        let mut expected: Vec<&BoardInfo> = boards.iter().collect();
        expected.sort_by(|a, b| board_listing_order(a, b));
        let expected: Vec<String> = expected.iter().map(|b| b.board_id.clone()).collect();
        assert_eq!(expected[0], a_id);
        let seen = page_through(&mut boards, 7, |boards, page| {
            if page == 1 {
                let a = boards.iter_mut().find(|b| b.board_id == a_id).unwrap();
                a.last_activity_at = Some("2026-11-26T10:00:00Z".into());
            }
        });
        assert_eq!(seen, expected);
    }

    #[test]
    fn parse_register_run() {
        let params = json!({
            "requestId": "ctl-0123456789abcdef",
            "orchestratorPaneId": "2",
            "workerPaneId": "5",
            "publishPolicy": { "owner": "worker" },
            "taskRef": { "kind": "local", "id": "t-1" },
        });
        let call = TimelineCall::parse(METHOD_REGISTER_RUN, params.clone())
            .unwrap()
            .unwrap();
        assert_eq!(call.scope(), Scope::Control);
        assert!(call.is_write());
        assert_eq!(call.request_id(), Some("ctl-0123456789abcdef"));
        let TimelineCall::RegisterRun(p) = call else {
            panic!("registerRun")
        };
        assert_eq!(p.worker_pane_id, Some(WireId(5)));
        assert_eq!(p.publish_policy.owner, PublishOwner::Worker);
        let mut bad = params.clone();
        bad["boardId"] = json!("../x");
        assert_eq!(
            kind_of(TimelineCall::parse(METHOD_REGISTER_RUN, bad)),
            TimelineErrorKind::InvalidParams
        );
        let mut bad = params;
        bad["taskRef"]["kind"] = json!("jira");
        assert_eq!(
            kind_of(TimelineCall::parse(METHOD_REGISTER_RUN, bad)),
            TimelineErrorKind::InvalidParams
        );
    }

    #[test]
    fn parse_set_publish_policy() {
        let params = json!({
            "boardId": BOARD, "runId": RUN, "requestId": "r",
            "publishPolicy": { "owner": "manual", "producerId": "human:me" },
        });
        let call = TimelineCall::parse(METHOD_SET_PUBLISH_POLICY, params)
            .unwrap()
            .unwrap();
        assert_eq!(call.scope(), Scope::Control);
        assert_eq!(call.request_id(), Some("r"));
        assert_eq!(
            kind_of(TimelineCall::parse(
                METHOD_SET_PUBLISH_POLICY,
                json!({ "boardId": BOARD, "runId": "rn-x", "requestId": "r",
                        "publishPolicy": { "owner": "manual" } })
            )),
            TimelineErrorKind::InvalidParams
        );
    }

    #[test]
    fn parse_post() {
        let call = TimelineCall::parse(METHOD_POST, post_params(input()))
            .unwrap()
            .unwrap();
        assert_eq!(call.scope(), Scope::Control);
        assert!(call.is_write());
        assert_eq!(call.request_id(), Some("req-1"));
        assert_eq!(call.method(), METHOD_POST);
        // No run: only a result file or a manual post.
        let mut no_run = post_params(input());
        no_run["runId"] = Value::Null;
        assert_eq!(
            kind_of(TimelineCall::parse(METHOD_POST, no_run.clone())),
            TimelineErrorKind::InvalidParams
        );
        no_run["post"]["sourceRef"]["namespace"] = json!("result-file");
        assert!(TimelineCall::parse(METHOD_POST, no_run).is_ok());
        let mut version = input();
        version["schemaVersion"] = json!(2);
        assert_eq!(
            kind_of(TimelineCall::parse(METHOD_POST, post_params(version))),
            TimelineErrorKind::Unsupported
        );
    }

    #[test]
    fn parse_list() {
        let call = TimelineCall::parse(
            METHOD_LIST,
            json!({ "boardId": BOARD, "beforeSeq": "5000", "limit": 50,
                    "filter": { "kinds": ["blocker"] }, "query": "parser" }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(call.scope(), Scope::Read);
        let TimelineCall::List(p) = call else {
            panic!("list")
        };
        assert_eq!(p.before_seq, Some(Seq(5000)));
        assert_eq!(p.filter.unwrap().kinds, vec![PostKind::Blocker]);
        for bad in [
            json!({ "boardId": BOARD, "limit": 51 }),
            json!({ "boardId": BOARD, "limit": 0 }),
            json!({ "boardId": BOARD, "query": "x".repeat(MAX_QUERY_CHARS + 1) }),
            json!({ "boardId": BOARD, "filter": { "workerIds": ["w"] } }),
            json!({ "boardId": "bd-1" }),
        ] {
            assert_eq!(
                kind_of(TimelineCall::parse(METHOD_LIST, bad)),
                TimelineErrorKind::InvalidParams
            );
        }
    }

    #[test]
    fn parse_get() {
        let call =
            TimelineCall::parse(METHOD_GET, json!({ "boardId": BOARD, "postId": POST })).unwrap();
        assert_eq!(
            call,
            Some(TimelineCall::Get(GetParams {
                board_id: BOARD.to_string(),
                post_id: POST.to_string()
            }))
        );
        assert_eq!(
            kind_of(TimelineCall::parse(
                METHOD_GET,
                json!({ "boardId": BOARD, "postId": RUN })
            )),
            TimelineErrorKind::InvalidParams
        );
    }

    #[test]
    fn parse_changes() {
        let call =
            TimelineCall::parse(METHOD_CHANGES, json!({ "boardId": BOARD, "afterSeq": "0" }))
                .unwrap()
                .unwrap();
        assert_eq!(call.scope(), Scope::Read);
        assert_eq!(
            kind_of(TimelineCall::parse(
                METHOD_CHANGES,
                json!({ "boardId": BOARD, "afterSeq": "0", "limit": CHANGES_MAX + 1 })
            )),
            TimelineErrorKind::InvalidParams
        );
        assert_eq!(
            kind_of(TimelineCall::parse(
                METHOD_CHANGES,
                json!({ "boardId": BOARD })
            )),
            TimelineErrorKind::InvalidParams
        );
    }

    #[test]
    fn parse_unknown_method_is_none() {
        assert_eq!(
            TimelineCall::parse("noa.timeline.delete", json!({})).unwrap(),
            None
        );
        assert_eq!(
            TimelineCall::parse("noa.listPanels", json!({})).unwrap(),
            None
        );
    }

    #[test]
    fn limits_rejected_not_truncated() {
        let refused = |edit: &dyn Fn(&mut Value)| {
            let mut value = input();
            edit(&mut value);
            let post: PostInput = serde_json::from_value(value).unwrap();
            post.validate().expect_err("over a limit").kind
        };
        let ok = |edit: &dyn Fn(&mut Value)| {
            let mut value = input();
            edit(&mut value);
            let post: PostInput = serde_json::from_value(value).unwrap();
            post.validate()
        };
        assert_eq!(
            refused(&|v| v["title"] = json!("あ".repeat(121))),
            TimelineErrorKind::InvalidParams
        );
        assert!(ok(&|v| v["title"] = json!("あ".repeat(120))).is_ok());
        assert_eq!(
            refused(&|v| v["summary"] = json!("x".repeat(401))),
            TimelineErrorKind::InvalidParams
        );
        assert!(ok(&|v| v["summary"] = json!("x".repeat(400))).is_ok());
        assert_eq!(
            refused(&|v| v["title"] = json!(" \t\u{3000}")),
            TimelineErrorKind::InvalidParams
        );
        assert_eq!(
            refused(&|v| v["summary"] = json!("")),
            TimelineErrorKind::InvalidParams
        );
        assert_eq!(
            refused(&|v| v["bodyMarkdown"] = json!("b".repeat(MAX_BODY_BYTES + 1))),
            TimelineErrorKind::InvalidParams
        );
        assert!(ok(&|v| v["bodyMarkdown"] = json!("b".repeat(MAX_BODY_BYTES))).is_ok());
        assert_eq!(
            refused(&|v| v["sourceRef"]["eventId"] = json!("has space")),
            TimelineErrorKind::InvalidParams
        );
        assert_eq!(
            refused(&|v| v["supersedesPostId"] = json!("po-1")),
            TimelineErrorKind::InvalidParams
        );
        assert_eq!(
            refused(&|v| v["artifacts"] = json!([{ "kind": "blob", "label": "x", "ref": "y" }])),
            TimelineErrorKind::InvalidParams
        );
        assert!(ok(&|v| v["occurredAt"] = json!("2026-11-26T09:30:00.123Z")).is_ok());
        for bad in [
            "2026-11-26T09:30:00+09:00",
            "2026-13-01T00:00:00Z",
            "2026-11-26 09:30:00Z",
        ] {
            assert_eq!(
                refused(&|v| v["occurredAt"] = json!(bad)),
                TimelineErrorKind::InvalidParams
            );
        }
    }

    // Review defect 5: a seq param is a decimal string only.
    #[test]
    fn seq_params_must_be_strings() {
        for (method, params) in [
            (METHOD_LIST, json!({ "boardId": BOARD, "beforeSeq": 5000 })),
            (METHOD_CHANGES, json!({ "boardId": BOARD, "afterSeq": 0 })),
        ] {
            assert_eq!(
                kind_of(TimelineCall::parse(method, params)),
                TimelineErrorKind::InvalidParams,
                "{method}"
            );
        }
        assert!(
            TimelineCall::parse(METHOD_CHANGES, json!({ "boardId": BOARD, "afterSeq": "0" }))
                .is_ok()
        );
    }

    // Review defect 8: serde reads an array as a struct's fields in order,
    // so only an object (or no params) is taken.
    #[test]
    fn params_must_be_an_object() {
        for params in [json!([1]), json!(1), json!("x"), json!(true)] {
            assert_eq!(
                kind_of(TimelineCall::parse(METHOD_BOARDS, params.clone())),
                TimelineErrorKind::InvalidParams,
                "{params}"
            );
        }
        assert_eq!(
            kind_of(TimelineCall::parse(METHOD_GET, json!([BOARD, POST]))),
            TimelineErrorKind::InvalidParams
        );
        assert!(TimelineCall::parse(METHOD_BOARDS, Value::Null).is_ok());
        assert!(TimelineCall::parse(METHOD_BOARDS, json!({})).is_ok());
        // Not a timeline method: not this parser's to refuse.
        assert_eq!(
            TimelineCall::parse("noa.listPanels", json!([1])).unwrap(),
            None
        );
    }

    // Review defect 7: a newer post is `unsupported` even when it carries a
    // field this version would refuse as unknown.
    #[test]
    fn a_newer_schema_version_is_unsupported_before_unknown_fields() {
        let mut newer = input();
        newer["schemaVersion"] = json!(2);
        newer["newField"] = json!("from version 2");
        assert_eq!(
            kind_of(TimelineCall::parse(METHOD_POST, post_params(newer))),
            TimelineErrorKind::Unsupported
        );
        // Version 1 with an unknown field is malformed.
        let mut typo = input();
        typo["newField"] = json!("typo");
        assert_eq!(
            kind_of(TimelineCall::parse(METHOD_POST, post_params(typo))),
            TimelineErrorKind::InvalidParams
        );
    }

    // Review defect 3: one-character items expand under normalisation; the
    // item caps keep the largest post a `get` can return.
    #[test]
    fn list_caps_keep_the_largest_post_inside_a_get_response() {
        let issues = |n: usize| Value::Array(vec![json!("x"); n]);
        let mut many = input();
        many["openIssues"] = issues(9_000);
        let many = post_params(many);
        assert!(serde_json::to_vec(&many).unwrap().len() < MAX_POST_PARAMS_BYTES);
        assert_eq!(
            kind_of(TimelineCall::parse(METHOD_POST, many)),
            TimelineErrorKind::InvalidParams
        );
        for field in [
            "openIssues",
            "nextActions",
            "checks",
            "artifacts",
            "basedOnPostIds",
        ] {
            let item = match field {
                "checks" => json!({ "name": "c", "reportedBy": "r", "result": "passed" }),
                "artifacts" => json!({ "kind": "file", "label": "l", "ref": "r" }),
                "basedOnPostIds" => json!(POST),
                _ => json!("x"),
            };
            let mut over = input();
            over[field] = Value::Array(vec![item.clone(); MAX_LIST_ITEMS + 1]);
            assert_eq!(
                kind_of(TimelineCall::parse(METHOD_POST, post_params(over))),
                TimelineErrorKind::InvalidParams,
                "{field}"
            );
            let mut full = input();
            full[field] = Value::Array(vec![item; MAX_LIST_ITEMS]);
            assert!(
                TimelineCall::parse(METHOD_POST, post_params(full)).is_ok(),
                "{field}"
            );
        }

        // The most normalisation can add: every list full of the items that
        // grow most, every option absent, the rest of 64 KiB in the body
        // and one artifact's ref.
        let mut worst = input();
        worst["openIssues"] = json!(vec![""; MAX_LIST_ITEMS]);
        worst["nextActions"] = json!(vec![""; MAX_LIST_ITEMS]);
        worst["checks"] = Value::Array(vec![
            json!({ "name": "", "reportedBy": "", "result": "passed" });
            MAX_LIST_ITEMS
        ]);
        worst["artifacts"] = Value::Array(vec![
            json!({ "kind": "file", "label": "", "ref": "" });
            MAX_LIST_ITEMS
        ]);
        worst["basedOnPostIds"] = json!(vec![POST; MAX_LIST_ITEMS]);
        worst["taskRef"] = json!({ "kind": "local", "id": "t" });
        worst["bodyMarkdown"] = json!("b".repeat(MAX_BODY_BYTES));
        let mut params = post_params(worst);
        params["producerId"] = json!("p".repeat(MAX_TOKEN_BYTES));
        params["requestId"] = json!("q".repeat(MAX_TOKEN_BYTES));
        let room = MAX_POST_PARAMS_BYTES - serde_json::to_vec(&params).unwrap().len();
        params["post"]["artifacts"][0]["ref"] = json!("r".repeat(room));
        assert_eq!(
            serde_json::to_vec(&params).unwrap().len(),
            MAX_POST_PARAMS_BYTES
        );
        let Some(TimelineCall::Post(parsed)) = TimelineCall::parse(METHOD_POST, params).unwrap()
        else {
            panic!("post")
        };
        let normalized = parsed.post.normalized();
        let input_bytes = serde_json::to_vec(&normalized).unwrap().len();
        assert!(input_bytes < 75 * 1024, "{input_bytes}");
        let long = "l".repeat(MAX_TOKEN_BYTES);
        let get = GetResult {
            post: Post {
                author: Author {
                    agent_kind: Some(long.clone()),
                    label: Some(long.clone()),
                    role: Some(long.clone()),
                    worker_id: Some("wk-0123456789abcdef".to_string()),
                },
                board_id: BOARD.to_string(),
                created_at: "2026-11-26T00:00:00.123456789Z".to_string(),
                post_id: POST.to_string(),
                producer: Producer {
                    id: parsed.producer_id,
                    kind: ProducerKind::Runner,
                },
                provenance: Provenance::Registered,
                request_id: parsed.request_id,
                run_id: Some(RUN.to_string()),
                schema_version: SCHEMA_VERSION,
                seq: Seq(u64::MAX),
                worker_id: Some("wk-0123456789abcdef".to_string()),
                input: normalized,
            },
            superseded_by: Vec::new(),
        };
        let response = serde_json::to_vec(&get).unwrap().len();
        assert!(response < MAX_RESPONSE_BYTES / 2, "{response}");
    }

    // Review defect 11: an artifact's label is at most MAX_TOKEN_BYTES.
    #[test]
    fn artifact_label_is_capped() {
        let with_label = |label: String| {
            let mut value = input();
            value["artifacts"] = json!([{ "kind": "file", "label": label, "ref": "a.md" }]);
            serde_json::from_value::<PostInput>(value)
                .unwrap()
                .validate()
        };
        assert!(with_label("l".repeat(MAX_TOKEN_BYTES)).is_ok());
        assert_eq!(
            with_label("l".repeat(MAX_TOKEN_BYTES + 1))
                .unwrap_err()
                .kind,
            TimelineErrorKind::InvalidParams
        );
        // Bytes, not characters: 43 × 3 bytes.
        assert_eq!(
            with_label("あ".repeat(43)).unwrap_err().kind,
            TimelineErrorKind::InvalidParams
        );
    }

    // Review defect 9: the day must exist in its month.
    #[test]
    fn occurred_at_is_a_real_date() {
        for good in [
            "2024-02-29T10:00:00Z",
            "2000-02-29T10:00:00Z",
            "2026-04-30T23:59:59Z",
            "2026-12-31T00:00:00.5Z",
        ] {
            assert!(valid_utc_timestamp(good), "{good}");
        }
        for bad in [
            "2025-02-29T10:00:00Z",
            "1900-02-29T10:00:00Z",
            "2026-02-30T10:00:00Z",
            "2026-04-31T10:00:00Z",
            "2026-06-31T10:00:00Z",
            "2026-09-31T10:00:00Z",
            "2026-11-31T10:00:00Z",
            "2026-01-00T10:00:00Z",
            "2026-01-32T10:00:00Z",
        ] {
            assert!(!valid_utc_timestamp(bad), "{bad}");
        }
        let mut value = input();
        value["occurredAt"] = json!("2025-02-29T10:00:00Z");
        assert_eq!(
            kind_of(TimelineCall::parse(METHOD_POST, post_params(value))),
            TimelineErrorKind::InvalidParams
        );
    }

    // Review defect 10: only owner `worker` has a default producer.
    #[test]
    fn runner_and_manual_policies_name_their_producer() {
        let register = |policy: Value| {
            TimelineCall::parse(
                METHOD_REGISTER_RUN,
                json!({ "requestId": "review-s1-run", "orchestratorPaneId": "2",
                        "publishPolicy": policy }),
            )
        };
        for owner in ["runner", "manual"] {
            assert_eq!(
                kind_of(register(json!({ "owner": owner }))),
                TimelineErrorKind::InvalidParams,
                "{owner}"
            );
            assert!(register(json!({ "owner": owner, "producerId": "review:s1" })).is_ok());
        }
        assert!(register(json!({ "owner": "worker" })).is_ok());
        assert_eq!(
            kind_of(TimelineCall::parse(
                METHOD_SET_PUBLISH_POLICY,
                json!({ "boardId": BOARD, "runId": RUN, "requestId": "r",
                        "publishPolicy": { "owner": "manual" } })
            )),
            TimelineErrorKind::InvalidParams
        );
    }

    #[test]
    fn error_kinds_map_to_codes_and_data() {
        for kind in TimelineErrorKind::ALL {
            let code = kind.code().code();
            assert_eq!(TimelineErrorKind::from_code(code), Some(kind));
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(kind.as_str()));
        }
        assert_eq!(TimelineErrorKind::Conflict.code().code(), -32012);
        assert_eq!(TimelineErrorKind::NotFound.code().code(), -32013);
        assert_eq!(TimelineErrorKind::Busy.code().code(), -32014);
        assert_eq!(TimelineErrorKind::StorageFull.code().code(), -32015);
        assert_eq!(TimelineErrorKind::IoError.code().code(), -32016);
        assert_eq!(TimelineErrorKind::CorruptStore.code().code(), -32017);
        assert_eq!(TimelineErrorKind::Unsupported.code().code(), -32018);
        assert_eq!(TimelineErrorKind::from_code(-32003), None);
        let mut error = TimelineError::new(TimelineErrorKind::CorruptStore, "line 3");
        error.line = Some(3);
        assert_eq!(error.data(), json!({ "kind": "corrupt_store", "line": 3 }));
        assert_eq!(error.to_string(), "line 3");
        assert_eq!(
            TimelineError::unsupported().data(),
            json!({ "kind": "unsupported" })
        );
    }
}
