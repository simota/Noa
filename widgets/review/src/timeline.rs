//! The review's Board Timeline adapter (`docs/specs/board-timeline.md`
//! AC-5): what a review posts to its board's timeline, built from the
//! lead's accepted output (never from what the screen shows), and the
//! publisher that sends it through `noa-ipc`'s outbox.
//!
//! Closing a session with an accepted verdict produces one post for that
//! round (`verdict-r<round>`). A later verdict supersedes it. The session
//! and round determine its request id and source reference, so a restart
//! rebuilds the same keys and the server collapses a re-send.
//!
//! The publisher stages each envelope in the outbox
//! (`<data>/noa/timeline-outbox/review/<session>/`) before sending it and
//! re-sends a staged envelope as it was stored. It never reads or writes
//! the session's own files: a failed post changes nothing of the review.

use std::io;
use std::path::{Component, Path, PathBuf};

use noa_ipc::timeline::{
    Check, CheckResult, Item, MAX_BODY_BYTES, MAX_LIST_ITEMS, MAX_POST_PARAMS_BYTES,
    MAX_SUMMARY_CHARS, MAX_TITLE_CHARS, Outcome, PostInput, PostKind, PublishOwner, PublishPolicy,
    RegisterRunParams, RegisterRunResult, SCHEMA_VERSION, SourceRef, valid_token,
};
use noa_ipc::timeline_outbox::{
    self, Envelope, EnvelopeKey, Outbox, PostSink, PublishState, RunBinding,
};
use noa_ipc::{Client, ClientError, Scope, ScopeSet, WireId};

use crate::model::export;
use crate::model::types::{Decision, FindingStatus, RoundNo, Session};

/// `sourceRef.namespace`, the producer prefix and the outbox folder.
pub const NAMESPACE: &str = "review";

/// One post a confirmed event makes, before the publisher gives it its
/// board, run and the post it supersedes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// `verdict-r<round>`: unique within the session.
    pub event_id: String,
    pub post: PostInput,
    /// It corrects the candidate before it: sent with that one's post id
    /// as `supersedesPostId`, and only once that one is published.
    pub supersedes_previous: bool,
}

/// The posts the review's confirmed events make, oldest first: one per
/// round whose accepted lead output gives a verdict.
pub fn candidates(session: &Session) -> Vec<Candidate> {
    session
        .rounds
        .iter()
        .filter_map(|round| round.verdict_post.as_deref())
        .enumerate()
        .map(|(index, post)| Candidate {
            event_id: post.source_ref.event_id.clone(),
            post: post.clone(),
            supersedes_previous: index > 0,
        })
        .collect()
}

/// Snapshots the round's verdict and report when its session closes.
pub(crate) fn verdict_post(session: &Session, round: RoundNo) -> Option<PostInput> {
    let output = session.round(round)?.outcome.as_ref()?;
    let verdict = output.verdict.as_ref()?;
    let title = format!(
        "レビュー r{}: {} — {}",
        round,
        verdict.decision.label(),
        session.manifest.label
    );
    let summary =
        first_text(&[&output.summary, &verdict.rationale]).unwrap_or(verdict.decision.label());
    let result = match verdict.decision {
        Decision::Approve | Decision::ApproveWithFixes => CheckResult::Passed,
        Decision::RequestChanges | Decision::Reject => CheckResult::Failed,
    };
    let checks = vec![Check {
        evidence_ref: None,
        name: format!("review verdict: {}", verdict.decision.name()),
        reported_by: producer_id(session.id.as_str()),
        result,
    }];
    let open_issues = session
        .findings
        .iter()
        .filter(|f| matches!(f.status, FindingStatus::Open | FindingStatus::Agreed))
        .take(MAX_LIST_ITEMS)
        .map(|f| Item {
            r#ref: f.location.as_ref().map(|l| l.path.clone()),
            text: clip(&format!("{}: {}", f.id, f.claim), ITEM_CHARS),
        })
        .collect();
    let post = PostInput {
        artifacts: Vec::new(),
        author: None,
        based_on_post_ids: Vec::new(),
        body_markdown: export::report_markdown(session),
        checks,
        kind: PostKind::Summary,
        next_actions: Vec::new(),
        occurred_at: None,
        open_issues,
        // A review that reached a verdict did its work; the verdict is
        // the check (open question Q6).
        outcome: Outcome::Succeeded,
        retry_of_run_id: None,
        schema_version: SCHEMA_VERSION,
        source_ref: SourceRef {
            event_id: format!("verdict-r{round}"),
            locator: None,
            namespace: NAMESPACE.to_owned(),
            run_id: Some(session.id.as_str().to_owned()),
            session_id: session.id.as_str().to_owned(),
        },
        summary: clip(summary, MAX_SUMMARY_CHARS),
        supersedes_post_id: None,
        task_ref: None,
        title: clip(&title, MAX_TITLE_CHARS),
    };
    Some(fit(post, BODY_CUT_NOTE))
}

/// How many confirmed points `session` has (rounds whose accepted lead
/// output gives a verdict): cheap, so the runner renders
/// [`candidates`] only when this grew.
pub fn confirmed(session: &Session) -> usize {
    session
        .rounds
        .iter()
        .filter(|round| round.verdict_post.is_some())
        .count()
}

/// What a cut report ends with: the client cuts explicitly, the server
/// never truncates.
const BODY_CUT_NOTE: &str = "\n\n（以下省略: 全文は report.md）";

// ---------------------------------------------------------------------------
// Publisher: the same text in widgets/tasks, widgets/review and widgets/verify
// (each widget is its own crate; `docs/specs/board-timeline.md` V17).

/// The longest open issue or next action, in characters.
const ITEM_CHARS: usize = 200;

/// `<ns>:<session>`: who posts for the session's runner.
pub fn producer_id(session: &str) -> String {
    format!("{NAMESPACE}:{session}")
}

/// `<ns>-<session>-<eventId>`: the post's request id (key A).
pub fn request_id(session: &str, event_id: &str) -> String {
    format!("{NAMESPACE}-{session}-{event_id}")
}

/// `<ns>-<session>-run`: the run registration's request id.
pub fn run_request_id(session: &str) -> String {
    format!("{NAMESPACE}-{session}-run")
}

/// The outbox of `session` under the outbox root
/// (`noa_config::timeline_outbox_dir()`). `None` unless `session` is one
/// plain path component: a session id from a log edited by hand
/// (`/elsewhere`, `..`, `a/b`) never names a folder outside the root.
pub fn outbox_dir(root: &Path, session: &str) -> Option<PathBuf> {
    let mut parts = Path::new(session).components();
    matches!((parts.next(), parts.next()), (Some(Component::Normal(part)), None) if part == session)
        .then(|| root.join(NAMESPACE).join(session))
}

/// What the publisher needs of Noa: post (the outbox's [`PostSink`]),
/// register a run, and whether the server has the timeline at all.
pub trait TimelineApi: PostSink {
    /// `noa.hello` listed `boardTimelineV1`.
    fn supports(&self) -> bool;
    fn register_run(&mut self, p: &RegisterRunParams) -> Result<RegisterRunResult, ClientError>;
}

impl TimelineApi for Client {
    fn supports(&self) -> bool {
        self.supports_board_timeline()
    }

    fn register_run(&mut self, p: &RegisterRunParams) -> Result<RegisterRunResult, ClientError> {
        self.timeline_register_run(p)
    }
}

/// A connection for the timeline: the running Noa's control server, found
/// from the config as the widget's own link finds it, with read and
/// control scopes (posting needs control).
pub fn connect() -> Result<Client, String> {
    let (config, _diagnostics) =
        noa_config::load_startup_config(noa_config::ConfigOverrides::default())
            .map_err(|err| format!("設定を読めません: {err:#}"))?;
    if !config.server_enable {
        return Err("設定でサーバーが無効です".into());
    }
    let token = match config.server_token.filter(|token| !token.trim().is_empty()) {
        Some(token) => token,
        None => {
            let path = noa_config::server_token_path()
                .ok_or_else(|| "トークンの場所がわかりません".to_owned())?;
            std::fs::read_to_string(&path)
                .map(|token| token.trim().to_owned())
                .ok()
                .filter(|token| !token.is_empty())
                .ok_or_else(|| "トークンがありません".to_owned())?
        }
    };
    let url = control_url(&config.server_bind, config.server_port);
    let scopes = ScopeSet::from_strings(["read", "control"]);
    let client = Client::connect(&url, &token, scopes).map_err(|err| match err {
        ClientError::Rpc { message, .. } => message,
        // Transport errors can name the URL; say only that it failed.
        _ => "Noa に接続できません".to_owned(),
    })?;
    if !client.granted_scopes().contains(Scope::Control) {
        return Err("タイムラインへの投稿には control スコープが要ります".into());
    }
    Ok(client)
}

fn control_url(bind: &str, port: u16) -> String {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    let ip = match bind.parse().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)) {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    format!("ws://{}/", SocketAddr::new(ip, port))
}

/// Where the session's posts stand, for its header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Nothing to post (or the server has no timeline).
    Idle,
    /// Staged or being sent, not acknowledged yet.
    Pending,
    /// Not sent, and not sendable until what `why` says changes.
    Waiting {
        why: String,
    },
    /// The server stored the last of them as `post_id`, numbered `seq`.
    Published {
        seq: u64,
        post_id: String,
    },
    Failed {
        message: String,
    },
}

impl Status {
    /// `タイムライン: 投稿待ち / 投稿済み #seq / 投稿失敗（再試行）`; `None`
    /// when there is nothing to show.
    pub fn label(&self) -> Option<String> {
        match self {
            Status::Idle => None,
            Status::Pending | Status::Waiting { .. } => Some("タイムライン: 投稿待ち".into()),
            Status::Published { seq, .. } => Some(format!("タイムライン: 投稿済み #{seq}")),
            Status::Failed { .. } => Some("タイムライン: 投稿失敗（再試行）".into()),
        }
    }

    /// What goes with [`Status::label`]: why the post failed or waits, or
    /// the id the server stored it under.
    pub fn detail(&self) -> Option<String> {
        match self {
            Status::Idle | Status::Pending => None,
            Status::Waiting { why } => Some(why.clone()),
            Status::Published { post_id, .. } => Some(format!("postId {post_id}")),
            Status::Failed { message } => Some(message.clone()),
        }
    }

    pub fn failed(&self) -> bool {
        matches!(self, Status::Failed { .. })
    }

    /// Not on the timeline yet: a 再試行 sends it.
    pub fn unsent(&self) -> bool {
        matches!(
            self,
            Status::Pending | Status::Waiting { .. } | Status::Failed { .. }
        )
    }
}

/// Why a post waits: no run is saved and no board pane names the board to
/// register one on (a run registered without one gets a board of its own,
/// with no root).
pub const NOT_REGISTERED: &str = "ボードに登録されていないため送れません";

/// The run registration a publisher makes, beyond its request id: the
/// same on every attempt (the server compares it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSpec {
    /// The pane the run's agent works in, when the run has one.
    pub worker_pane: Option<u64>,
    pub task_ref: Option<noa_ipc::timeline::TaskRef>,
}

/// One session's publisher: its outbox, the run it registered and the
/// outcome of its last attempt.
#[derive(Debug)]
pub struct Publisher {
    outbox: Outbox,
    session: String,
    spec: RunSpec,
    binding: Option<RunBinding>,
    /// The board pane the run was last registered from in this process: a
    /// new one (a resumed session's new board tab) registers again, which
    /// rebinds it as the board's root.
    bound_pane: Option<u64>,
    /// Why the last attempt failed where the outbox recorded nothing: before
    /// any envelope was staged, or the staging itself.
    error: Option<String>,
}

impl Publisher {
    /// Opens the outbox of `session` under `root`. Refused for a session id
    /// that cannot name a folder under it or a request (a log edited by
    /// hand).
    pub fn open(root: &Path, session: &str, spec: RunSpec) -> io::Result<Self> {
        let Some(dir) = outbox_dir(root, session) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session id cannot name a timeline outbox",
            ));
        };
        if !valid_token(&request_id(session, "run")) || !valid_token(&producer_id(session)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session id cannot name a timeline request",
            ));
        }
        let outbox = Outbox::open(dir, true)?;
        let binding = outbox.load_run()?;
        Ok(Self {
            outbox,
            session: session.to_owned(),
            spec,
            binding,
            bound_pane: None,
            error: None,
        })
    }

    /// [`Publisher::open`] on an outbox that exists (its run was registered
    /// or a post tried); `None`, creating nothing, when there is none, as
    /// for a session id that names no folder under the root (nothing is
    /// looked at for it).
    pub fn existing(root: &Path, session: &str) -> io::Result<Option<Self>> {
        if !outbox_dir(root, session).is_some_and(|dir| dir.is_dir()) {
            return Ok(None);
        }
        Self::open(root, session, RunSpec::default()).map(Some)
    }

    /// The board and run saved by a registration (this process or an
    /// earlier one).
    pub fn binding(&self) -> Option<&RunBinding> {
        self.binding.as_ref()
    }

    /// Registers the run (again, with the saved board, when one is saved)
    /// unless it was already registered from `board` in this process. With
    /// no run saved, only from a board pane: the error is what the post
    /// shows ([`Status::Waiting`] or [`Status::Failed`]).
    fn ensure_run(
        &mut self,
        api: &mut dyn TimelineApi,
        board: Option<u64>,
    ) -> Result<RunBinding, Status> {
        if let Some(binding) = &self.binding
            && (board.is_none() || self.bound_pane == board)
        {
            return Ok(binding.clone());
        }
        if board.is_none() {
            return Err(Status::Waiting {
                why: NOT_REGISTERED.into(),
            });
        }
        let params = RegisterRunParams {
            board_id: self.binding.as_ref().map(|b| b.board_id.clone()),
            board_name: None,
            label: None,
            orchestrator_pane_id: board.map(WireId),
            publish_policy: PublishPolicy {
                owner: PublishOwner::Runner,
                producer_id: Some(producer_id(&self.session)),
            },
            request_id: run_request_id(&self.session),
            retry_of_run_id: None,
            role: None,
            task_ref: self.spec.task_ref.clone(),
            worker_pane_id: self.spec.worker_pane.map(WireId),
        };
        match api.register_run(&params) {
            Ok(registered) => {
                let binding = RunBinding {
                    board_id: registered.board_id,
                    producer_id: registered.producer_id,
                    run_id: registered.run_id,
                };
                if self.binding.as_ref() != Some(&binding) {
                    self.outbox.save_run(&binding).map_err(|e| Status::Failed {
                        message: format!("outbox: {e}"),
                    })?;
                }
                self.binding = Some(binding.clone());
                self.bound_pane = board;
                Ok(binding)
            }
            Err(err) => Err(Status::Failed {
                message: describe(&err),
            }),
        }
    }

    /// Registers the run from the board pane `board` before anything is
    /// posted, so it exists from the session's start: [`Status::Idle`] once
    /// it is (or when the server has no timeline), else why not.
    pub fn register(&mut self, api: &mut dyn TimelineApi, board: Option<u64>) -> Status {
        if !api.supports() {
            return Status::Idle;
        }
        match self.ensure_run(api, board) {
            Ok(_) => Status::Idle,
            Err(status) => status,
        }
    }

    /// Sends what is not yet published, in order: a staged envelope as it
    /// was stored, a new candidate once the one it supersedes is published.
    /// A candidate waits behind an earlier one that is not published.
    /// `board` is the pane of the session's board tab, when it has one.
    pub fn sync(
        &mut self,
        api: &mut dyn TimelineApi,
        board: Option<u64>,
        candidates: &[Candidate],
    ) -> Status {
        if !api.supports() {
            return Status::Idle;
        }
        if candidates.is_empty() {
            return self.status(candidates);
        }
        let binding = match self.ensure_run(api, board) {
            Ok(binding) => binding,
            Err(Status::Failed { message }) => {
                self.error = Some(message);
                return self.status(candidates);
            }
            // It waits for a run; `status` says why.
            Err(_) => {
                self.error = None;
                return self.status(candidates);
            }
        };
        self.error = None;
        let mut previous: Option<String> = None;
        for candidate in candidates {
            let key = self.key(&binding, candidate);
            let state = match self.outbox.get(&key) {
                Ok(Some(entry)) if matches!(entry.state, PublishState::Published { .. }) => {
                    entry.state
                }
                Ok(Some(entry)) => timeline_outbox::publish(api, &self.outbox, entry.envelope),
                Ok(None) => {
                    let supersedes = if candidate.supersedes_previous {
                        match &previous {
                            Some(post) => Some(post.clone()),
                            None => break,
                        }
                    } else {
                        None
                    };
                    let mut post = candidate.post.clone();
                    post.supersedes_post_id = supersedes;
                    let env = Envelope {
                        board_id: binding.board_id.clone(),
                        post,
                        producer_id: binding.producer_id.clone(),
                        request_id: key.request_id.clone(),
                        run_id: Some(binding.run_id.clone()),
                    };
                    timeline_outbox::publish(api, &self.outbox, env)
                }
                Err(e) => {
                    self.error = Some(format!("outbox: {e}"));
                    break;
                }
            };
            // A failure the outbox could not record (the staging refused, the
            // outcome not written) is only in what `publish` returned.
            if let PublishState::Failed { message, .. } = &state
                && !matches!(self.outbox.get(&key), Ok(Some(entry)) if entry.state == state)
            {
                self.error = Some(message.clone());
            }
            match state {
                PublishState::Published { post_id, .. } => previous = Some(post_id),
                _ => break,
            }
        }
        self.status(candidates)
    }

    /// Where `candidates` stand, from the outbox alone (no request).
    pub fn status(&self, candidates: &[Candidate]) -> Status {
        if candidates.is_empty() {
            return Status::Idle;
        }
        if let Some(why) = &self.error {
            return Status::Failed {
                message: why.clone(),
            };
        }
        let Some(binding) = &self.binding else {
            return Status::Waiting {
                why: NOT_REGISTERED.into(),
            };
        };
        let mut last = None;
        for candidate in candidates {
            match self.outbox.get(&self.key(binding, candidate)) {
                Ok(Some(entry)) => match entry.state {
                    PublishState::Published { seq, post_id } => last = Some((seq.0, post_id)),
                    PublishState::Failed { message, .. } => return Status::Failed { message },
                    PublishState::Pending => return Status::Pending,
                },
                Ok(None) => return Status::Pending,
                Err(e) => {
                    return Status::Failed {
                        message: format!("outbox: {e}"),
                    };
                }
            }
        }
        last.map_or(Status::Pending, |(seq, post_id)| Status::Published {
            seq,
            post_id,
        })
    }

    fn key(&self, binding: &RunBinding, candidate: &Candidate) -> EnvelopeKey {
        EnvelopeKey {
            board_id: binding.board_id.clone(),
            producer_id: binding.producer_id.clone(),
            request_id: request_id(&self.session, &candidate.event_id),
        }
    }
}

/// Where a finished session's posts stand, from its outbox alone (it has
/// no runner): [`Status::Idle`] when it has none.
pub fn stored_status(root: &Path, session: &Session) -> Status {
    match Publisher::existing(root, session.id.as_str()) {
        Ok(Some(publisher)) => publisher.status(&candidates(session)),
        Ok(None) => Status::Idle,
        Err(e) => Status::Failed {
            message: format!("outbox: {e}"),
        },
    }
}

/// 再試行 on a finished session (no runner, no board pane): its posts not
/// yet published, sent under the run saved in its outbox, a staged
/// envelope as it was stored.
pub fn resend_stored(root: &Path, session: &Session, api: &mut dyn TimelineApi) -> Status {
    match Publisher::existing(root, session.id.as_str()) {
        Ok(Some(mut publisher)) => publisher.sync(api, None, &candidates(session)),
        Ok(None) => Status::Idle,
        Err(e) => Status::Failed {
            message: format!("outbox: {e}"),
        },
    }
}

fn describe(err: &ClientError) -> String {
    match err {
        ClientError::Rpc { message, .. } => message.clone(),
        ClientError::Timeout(_) => "タイムラインが応答しません".into(),
        // Transport diagnostics can include the connection URL.
        _ => "タイムラインに届きません".into(),
    }
}

/// The first of `texts` that is not blank.
fn first_text<'a>(texts: &[&'a String]) -> Option<&'a str> {
    texts.iter().map(|t| t.trim()).find(|t| !t.is_empty())
}

/// `text` trimmed, cut to `max` characters (the last one an ellipsis) when
/// longer.
fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// What the body of a post whose structured items alone overflow its
/// request becomes: the body and the items left out are in the report.
const ITEMS_CUT_NOTE: &str = "（本文と一部の項目を省略: 全件は report.md）";

/// `post` with its body cut (at a character boundary, ending in `note`)
/// so the body stays within [`MAX_BODY_BYTES`] and the whole `post`
/// request within [`MAX_POST_PARAMS_BYTES`]. When the items alone are over
/// that, the longest list gives up items from its end and the body is
/// [`ITEMS_CUT_NOTE`].
fn fit(mut post: PostInput, note: &str) -> PostInput {
    // Room for the ids and keys `post`'s params add around the post.
    const ENVELOPE_BYTES: usize = 1024;
    let budget = MAX_POST_PARAMS_BYTES - ENVELOPE_BYTES;
    if post.body_markdown.len() > MAX_BODY_BYTES {
        post.body_markdown = cut(&post.body_markdown, MAX_BODY_BYTES, note);
    }
    let mut items_cut = false;
    loop {
        let size = serde_json::to_vec(&post).map_or(usize::MAX, |v| v.len());
        if size <= budget {
            return post;
        }
        if !items_cut && !post.body_markdown.is_empty() {
            let over = size - budget;
            let keep = post.body_markdown.len().saturating_sub(over + note.len());
            post.body_markdown = if keep == 0 {
                String::new()
            } else {
                cut(&post.body_markdown, keep + note.len(), note)
            };
            continue;
        }
        let (issues, checks, actions) = (
            post.open_issues.len(),
            post.checks.len(),
            post.next_actions.len(),
        );
        if issues == 0 && checks == 0 && actions == 0 {
            return post;
        }
        if issues >= checks.max(actions) {
            post.open_issues.pop();
        } else if checks >= actions {
            post.checks.pop();
        } else {
            post.next_actions.pop();
        }
        if !items_cut {
            items_cut = true;
            post.body_markdown = ITEMS_CUT_NOTE.to_owned();
        }
    }
}

/// `text` cut to at most `max` bytes, the last of them `note`.
fn cut(text: &str, max: usize, note: &str) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max.saturating_sub(note.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{note}", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_over_the_cap_is_cut_at_a_char_boundary_with_the_note() {
        let post = PostInput {
            body_markdown: "あ".repeat(MAX_BODY_BYTES),
            ..sample()
        };
        let fitted = fit(post, BODY_CUT_NOTE);
        assert!(fitted.body_markdown.len() <= MAX_BODY_BYTES);
        assert!(fitted.body_markdown.ends_with(BODY_CUT_NOTE));
        fitted.validate().unwrap();
    }

    #[test]
    fn a_post_whose_items_and_body_overflow_the_params_cap_fits() {
        let post = PostInput {
            body_markdown: "\"".repeat(MAX_BODY_BYTES),
            open_issues: (0..MAX_LIST_ITEMS)
                .map(|n| Item {
                    r#ref: None,
                    text: format!("{n}{}", "い".repeat(ITEM_CHARS)),
                })
                .collect(),
            ..sample()
        };
        let fitted = fit(post, BODY_CUT_NOTE);
        assert!(serde_json::to_vec(&fitted).unwrap().len() <= MAX_POST_PARAMS_BYTES - 1024);
        fitted.validate().unwrap();
    }

    #[test]
    fn a_post_whose_items_alone_overflow_the_params_cap_keeps_what_fits_and_says_so() {
        let issue = |n: usize| Item {
            r#ref: Some(format!("src/{}.rs", "p".repeat(75))),
            text: clip(&format!("F{n}: {}", "指".repeat(ITEM_CHARS)), ITEM_CHARS),
        };
        let post = PostInput {
            body_markdown: "本文".repeat(4000),
            checks: (0..MAX_LIST_ITEMS)
                .map(|n| Check {
                    evidence_ref: None,
                    name: clip(&format!("AC-{n}: {}", "基".repeat(ITEM_CHARS)), ITEM_CHARS),
                    reported_by: producer_id("s"),
                    result: CheckResult::Passed,
                })
                .collect(),
            open_issues: (0..MAX_LIST_ITEMS).map(issue).collect(),
            ..sample()
        };
        let fitted = fit(post, BODY_CUT_NOTE);
        assert!(serde_json::to_vec(&fitted).unwrap().len() <= MAX_POST_PARAMS_BYTES - 1024);
        assert_eq!(fitted.body_markdown, ITEMS_CUT_NOTE);
        assert!(fitted.open_issues.len() + fitted.checks.len() < 2 * MAX_LIST_ITEMS);
        // What is kept is the start of each list, in order.
        assert_eq!(fitted.open_issues[0], issue(0));
        assert!(fitted.checks[0].name.starts_with("AC-0: "));
        fitted.validate().unwrap();
    }

    #[test]
    fn clip_keeps_the_limit_in_characters() {
        let long = "日".repeat(MAX_TITLE_CHARS + 5);
        let clipped = clip(&long, MAX_TITLE_CHARS);
        assert_eq!(clipped.chars().count(), MAX_TITLE_CHARS);
        assert!(clipped.ends_with('…'));
        assert_eq!(clip("  短い  ", MAX_TITLE_CHARS), "短い");
    }

    fn sample() -> PostInput {
        PostInput {
            artifacts: Vec::new(),
            author: None,
            based_on_post_ids: Vec::new(),
            body_markdown: String::new(),
            checks: Vec::new(),
            kind: PostKind::Summary,
            next_actions: Vec::new(),
            occurred_at: None,
            open_issues: Vec::new(),
            outcome: Outcome::Unknown,
            retry_of_run_id: None,
            schema_version: SCHEMA_VERSION,
            source_ref: SourceRef {
                event_id: "e".into(),
                locator: None,
                namespace: NAMESPACE.into(),
                run_id: None,
                session_id: "s".into(),
            },
            summary: "s".into(),
            supersedes_post_id: None,
            task_ref: None,
            title: "t".into(),
        }
    }
}
