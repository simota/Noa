//! The tasks widget's Board Timeline adapter
//! (`docs/specs/board-timeline.md` AC-5): a run's result posted to its
//! board's timeline when the person shares it (共有), and the publisher that
//! sends it through `noa-ipc`'s outbox.
//!
//! Each run is registered once its pane is on the board (the board tab as
//! orchestrator, the run's float as worker, the widget as the publishing
//! runner). Nothing is posted until the person shares the run's result:
//! one post per run (`result-<run>`), built from `result.md` as it is then,
//! with the outcome the person picked (`unknown` unless they say
//! otherwise). Request id and source reference come from the repository
//! key and the run id alone, so a re-send is recognised by the server.
//!
//! The publisher stages each envelope in the outbox
//! (`<data>/noa/timeline-outbox/tasks/<repo>/<run>/`) before sending it and
//! re-sends a staged envelope as it was stored. What a share posts is saved
//! there (`share.json`) before anything is sent, so a retry or a restart
//! sends what the person shared, not the result file as it is by then. It
//! never writes the run log or the result file.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use noa_ipc::timeline::{
    Artifact, Item, MAX_BODY_BYTES, MAX_POST_PARAMS_BYTES, MAX_SUMMARY_CHARS, MAX_TITLE_CHARS,
    Outcome, PostInput, PostKind, PublishOwner, PublishPolicy, RegisterRunParams,
    RegisterRunResult, SCHEMA_VERSION, SourceRef, TaskRef as TimelineTaskRef, valid_token,
};
use noa_ipc::timeline_outbox::{
    self, Envelope, EnvelopeKey, Outbox, PostSink, PublishState, RunBinding,
};
use noa_ipc::{Client, ClientError, Scope, ScopeSet, WireId};

use crate::model::types::{RunId, TaskRef};

/// `sourceRef.namespace`, the producer prefix and the outbox folder.
pub const NAMESPACE: &str = "tasks";

/// One post a confirmed event makes, before the publisher gives it its
/// board, run and the post it supersedes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// `result-<run>`: unique within the repository.
    pub event_id: String,
    pub post: PostInput,
    /// It corrects the candidate before it: sent with that one's post id
    /// as `supersedesPostId`, and only once that one is published.
    pub supersedes_previous: bool,
}

/// `result-<run>`: the share's event id.
pub fn event_id(run: &str) -> String {
    format!("result-{run}")
}

/// The repository key (`store::repo_key`, `<dir name>-<hash>`) as a
/// request-id token: characters outside `[A-Za-z0-9._-]` become `_` and the
/// name is shortened; the hash keeps it unique.
pub fn session_key(repo_key: &str) -> String {
    let (name, hash) = repo_key.rsplit_once('-').unwrap_or(("repo", repo_key));
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect();
    format!("{name}-{hash}")
}

/// The timeline's reference to `task`: a local task by its id, an issue
/// as `owner/name#N` with its URL, a code marker as `path:key`. No title:
/// a run's registration is compared field by field, and a title changes.
pub fn task_ref(task: &TaskRef) -> TimelineTaskRef {
    match task {
        TaskRef::Local(id) => TimelineTaskRef {
            id: id.clone(),
            kind: "local".into(),
            title: None,
            url: None,
        },
        TaskRef::GitHub { repo, number } => TimelineTaskRef {
            id: format!("{repo}#{number}"),
            kind: "github".into(),
            title: None,
            url: Some(format!("https://github.com/{repo}/issues/{number}")),
        },
        TaskRef::Code { path, key } => TimelineTaskRef {
            id: format!("{path}:{key}"),
            kind: "other".into(),
            title: None,
            url: None,
        },
    }
}

/// What the run's agent wrote in its result file `path` (under the run's
/// work tree `base`), read for a share: through no link anywhere below
/// `base` (`fsx::open_read`), a regular file only (no FIFO wait), at most
/// [`MAX_BODY_BYTES`] and one byte; `true` when it was longer.
pub fn read_result(base: &Path, path: &Path) -> io::Result<(String, bool)> {
    let file = crate::fsx::open_read(base, path)?;
    let mut bytes = Vec::new();
    file.take(MAX_BODY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let longer = bytes.len() > MAX_BODY_BYTES;
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        // Cut inside a character: up to the last whole one.
        Err(e) if longer && e.utf8_error().error_len().is_none() => {
            let valid = e.utf8_error().valid_up_to();
            let mut bytes = e.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).expect("valid up to here")
        }
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    Ok((text, longer))
}

/// What the person shares of one run.
#[derive(Debug, Clone, Copy)]
pub struct Share<'a> {
    pub run: &'a RunId,
    pub task: &'a TaskRef,
    /// The task's title, when it is known.
    pub title: Option<&'a str>,
    pub result_path: &'a Path,
    /// The result file's text as [`read_result`] gave it.
    pub result: &'a str,
    /// The file goes on past [`MAX_BODY_BYTES`].
    pub longer: bool,
    pub outcome: Outcome,
}

/// What a share posts (`session`: [`session_key`]).
pub fn candidate(session: &str, share: &Share<'_>) -> Candidate {
    let Share {
        run,
        task,
        title,
        result_path,
        result,
        longer,
        outcome,
    } = *share;
    let event_id = event_id(run.as_str());
    let title = match title.map(str::trim).filter(|t| !t.is_empty()) {
        Some(title) => format!("タスク結果: {title}"),
        None => format!("タスク結果: {run}"),
    };
    let summary = result
        .lines()
        .map(|line| line.trim().trim_start_matches(['#', '-', '*', '>']).trim())
        .find(|line| !line.is_empty())
        .map_or_else(|| format!("{run} の結果"), str::to_owned);
    let body = if longer || result.len() > MAX_BODY_BYTES {
        // Even when what was read fits: the file goes on past it.
        excerpt(result, MAX_BODY_BYTES, BODY_CUT_NOTE)
    } else {
        result.to_owned()
    };
    let path = result_path.display().to_string();
    let post = PostInput {
        artifacts: vec![Artifact {
            digest: None,
            kind: "file".into(),
            label: "result.md".into(),
            r#ref: path,
        }],
        author: None,
        based_on_post_ids: Vec::new(),
        body_markdown: body,
        checks: Vec::new(),
        kind: PostKind::Summary,
        next_actions: Vec::<Item>::new(),
        occurred_at: None,
        open_issues: Vec::new(),
        outcome,
        retry_of_run_id: None,
        schema_version: SCHEMA_VERSION,
        source_ref: SourceRef {
            event_id: event_id.clone(),
            locator: None,
            namespace: NAMESPACE.to_owned(),
            run_id: Some(run.as_str().to_owned()),
            session_id: session.to_owned(),
        },
        summary: clip(&summary, MAX_SUMMARY_CHARS),
        supersedes_post_id: None,
        task_ref: Some(task_ref(task)),
        title: clip(&title, MAX_TITLE_CHARS),
    };
    Candidate {
        event_id,
        post: fit(post, BODY_CUT_NOTE),
        supersedes_previous: false,
    }
}

/// What a cut result ends with: the client cuts explicitly, the server
/// never truncates.
const BODY_CUT_NOTE: &str = "\n\n（全文は result.md）";

// ---------------------------------------------------------------------------
// Publisher: as in widgets/review and widgets/verify (each widget is its own
// crate; `docs/specs/board-timeline.md` V17), with one run registered per
// task run instead of one per session.

/// `<ns>:<session>`: who posts for the repository's runs.
pub fn producer_id(session: &str) -> String {
    format!("{NAMESPACE}:{session}")
}

/// `<ns>-<session>-<eventId>`: the post's request id (key A).
pub fn request_id(session: &str, event_id: &str) -> String {
    format!("{NAMESPACE}-{session}-{event_id}")
}

/// `<ns>-<session>-run-<run>`: the registration's request id of `run`.
pub fn run_request_id(session: &str, run: &str) -> String {
    format!("{NAMESPACE}-{session}-run-{run}")
}

/// The outbox of `run` under the outbox root
/// (`noa_config::timeline_outbox_dir()`): one per run, since each holds
/// one registered run. `None` unless `session` and `run` are each one
/// plain path component: a run id from a log edited by hand (`/elsewhere`,
/// `..`, `a/b`) never names a folder outside the root.
pub fn outbox_dir(root: &Path, session: &str, run: &str) -> Option<PathBuf> {
    let one_part = |name: &str| {
        let mut parts = Path::new(name).components();
        matches!((parts.next(), parts.next()), (Some(Component::Normal(part)), None) if part == name)
    };
    (one_part(session) && one_part(run)).then(|| root.join(NAMESPACE).join(session).join(run))
}

/// Where a share's post is saved in its run's outbox before it is sent.
const SHARE_FILE: &str = "share.json";

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

/// Where a run's share stands, for its detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Nothing to post (or the server has no timeline).
    Idle,
    /// Saved or being sent, not acknowledged yet.
    Pending,
    /// Not sent, and not sendable until what `why` says changes.
    Waiting {
        why: String,
    },
    /// The server stored it as `post_id`, numbered `seq`.
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

/// Why a share waits: no run is saved, and the run's pane is not on its
/// board to register one from (a run registered without one gets a board
/// of its own, with no root).
pub const NOT_REGISTERED: &str =
    "ボードに登録されていないため送れません（実行のペインがボードにある間に再試行してください）";

/// The run registration a publisher makes, beyond its request id: the
/// same on every attempt (the server compares it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSpec {
    /// The pane the run's agent works in, when the run has one.
    pub worker_pane: Option<u64>,
    pub task_ref: Option<noa_ipc::timeline::TaskRef>,
}

/// One task run's publisher: its outbox, the run it registered and the
/// outcome of its last attempt.
#[derive(Debug)]
pub struct Publisher {
    outbox: Outbox,
    /// The outbox's folder ([`outbox_dir`]).
    dir: PathBuf,
    session: String,
    run: String,
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
    /// Opens the outbox of `run` under `root`. Refused, before anything is
    /// made or changed on disk, for ids that cannot name its folder
    /// ([`outbox_dir`]) or a request (a log edited by hand).
    pub fn open(root: &Path, session: &str, run: &str, spec: RunSpec) -> io::Result<Self> {
        let Some(dir) = outbox_dir(root, session, run) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "run id cannot name a timeline outbox",
            ));
        };
        if !valid_token(&run_request_id(session, run))
            || !valid_token(&request_id(session, &event_id(run)))
            || !valid_token(&producer_id(session))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "run id cannot name a timeline request",
            ));
        }
        let outbox = Outbox::open(dir.clone(), true)?;
        let binding = outbox.load_run()?;
        Ok(Self {
            outbox,
            dir,
            session: session.to_owned(),
            run: run.to_owned(),
            spec,
            binding,
            bound_pane: None,
            error: None,
        })
    }

    /// The board and run saved by a registration (this process or an
    /// earlier one).
    pub fn binding(&self) -> Option<&RunBinding> {
        self.binding.as_ref()
    }

    /// Registers the run (again, with the saved board, when one is saved)
    /// unless it was already registered from `board` in this process. With
    /// no run saved, only from a board pane: the error is what the share
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
            request_id: run_request_id(&self.session, &self.run),
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

    /// Registers the run (its pane on `board`), so its pane is bound to
    /// it while it is open; a share later finds the run by its request id.
    pub fn register(&mut self, api: &mut dyn TimelineApi, board: Option<u64>) -> Status {
        if !api.supports() {
            return Status::Idle;
        }
        match self.ensure_run(api, board) {
            Ok(_) => {
                self.error = None;
                Status::Idle
            }
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

    /// Where the run's share stands, from the outbox alone: `None` when it
    /// was never shared (nothing saved, staged or failed).
    pub fn shared(&self) -> Option<Status> {
        if let Some(why) = &self.error {
            return Some(Status::Failed {
                message: why.clone(),
            });
        }
        match self.staged() {
            Ok(Some(entry)) => Some(match entry.state {
                PublishState::Pending => Status::Pending,
                PublishState::Published { seq, post_id } => Status::Published {
                    seq: seq.0,
                    post_id,
                },
                PublishState::Failed { message, .. } => Status::Failed { message },
            }),
            Ok(None) => match self.saved_share() {
                Ok(Some(_)) if self.binding.is_none() => Some(Status::Waiting {
                    why: NOT_REGISTERED.into(),
                }),
                Ok(Some(_)) => Some(Status::Pending),
                Ok(None) => None,
                Err(e) => Some(Status::Failed {
                    message: format!("outbox: {e}"),
                }),
            },
            Err(e) => Some(Status::Failed {
                message: format!("outbox: {e}"),
            }),
        }
    }

    /// The share's envelope in the outbox, staged under whichever board it
    /// was registered on.
    fn staged(&self) -> io::Result<Option<timeline_outbox::Entry>> {
        let request = request_id(&self.session, &event_id(&self.run));
        Ok(self
            .outbox
            .entries()?
            .into_iter()
            .find(|e| e.envelope.request_id == request))
    }

    /// Saves what the person shared before anything is sent (file mode
    /// 0600, written whole: temporary file, `sync_all`, rename, folder
    /// sync), so a retry or a restart sends it as it was shared.
    pub fn save_share(&self, candidate: &Candidate) -> io::Result<()> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let tmp = self.dir.join(format!(
            ".share.{}-{}.tmp",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let written = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.write_all(&serde_json::to_vec(&candidate.post)?)?;
            file.sync_all()?;
            fs::rename(&tmp, self.dir.join(SHARE_FILE))?;
            File::open(&self.dir)?.sync_all()
        })();
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written
    }

    /// What the person shared: as saved by [`Publisher::save_share`], else
    /// as staged (sent once already); `None` when nothing was shared.
    pub fn saved_share(&self) -> io::Result<Option<Candidate>> {
        let post: Option<PostInput> = match fs::read(self.dir.join(SHARE_FILE)) {
            Ok(bytes) => Some(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let post = match post {
            Some(post) => Some(post),
            None => self.staged()?.map(|entry| entry.envelope.post),
        };
        Ok(post.map(|post| Candidate {
            event_id: event_id(&self.run),
            post: PostInput {
                supersedes_post_id: None,
                ..post
            },
            supersedes_previous: false,
        }))
    }

    fn key(&self, binding: &RunBinding, candidate: &Candidate) -> EnvelopeKey {
        EnvelopeKey {
            board_id: binding.board_id.clone(),
            producer_id: binding.producer_id.clone(),
            request_id: request_id(&self.session, &candidate.event_id),
        }
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

/// `post` with its body cut (at a character boundary, ending in `note`)
/// so the body stays within [`MAX_BODY_BYTES`] and the whole `post`
/// request within [`MAX_POST_PARAMS_BYTES`].
fn fit(mut post: PostInput, note: &str) -> PostInput {
    // Room for the ids and keys `post`'s params add around the post.
    const ENVELOPE_BYTES: usize = 1024;
    let budget = MAX_POST_PARAMS_BYTES - ENVELOPE_BYTES;
    if post.body_markdown.len() > MAX_BODY_BYTES {
        post.body_markdown = cut(&post.body_markdown, MAX_BODY_BYTES, note);
    }
    loop {
        let size = serde_json::to_vec(&post).map_or(usize::MAX, |v| v.len());
        if size <= budget || post.body_markdown.is_empty() {
            return post;
        }
        let over = size - budget;
        let keep = post.body_markdown.len().saturating_sub(over + note.len());
        post.body_markdown = if keep == 0 {
            String::new()
        } else {
            cut(&post.body_markdown, keep + note.len(), note)
        };
    }
}

/// `text` cut to at most `max` bytes, the last of them `note`.
fn cut(text: &str, max: usize, note: &str) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    excerpt(text, max, note)
}

/// The start of `text` and then `note`, at most `max` bytes in all, cut at
/// a character boundary: also when `text` itself fits, since it is only
/// the start of something longer.
fn excerpt(text: &str, max: usize, note: &str) -> String {
    let mut end = max.saturating_sub(note.len()).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{note}", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    use noa_ipc::timeline::MAX_LIST_ITEMS;

    const ITEM_CHARS: usize = 200;

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
