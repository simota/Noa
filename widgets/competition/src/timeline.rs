use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};

use competition::model::{export, types::Session};
use noa_ipc::timeline::*;
use noa_ipc::timeline_outbox::{
    Entry, Envelope, EnvelopeKey, Outbox, PostSink, PublishState, RunBinding, publish,
};
use noa_ipc::{Client, ScopeSet, WireId};

const NAMESPACE: &str = "competition";
const FULL_RESULT_NOTE: &str = "\n\n（全文は「結果をコピー」で確認できます）";

/// Why a closed session's post waits: no run is saved and no board pane
/// names the board to register one on (a run registered with neither gets
/// a board of its own, with no root, unrelated to the session).
pub(crate) const NOT_REGISTERED: &str = "ボードに登録されていないため送れません";

/// What the header shows of a session's post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// Not sent, and not sendable until what `why` says changes.
    Waiting {
        why: String,
    },
    Publish(PublishState),
}

/// Why there is no connection to post through.
#[derive(Debug)]
enum Unreachable {
    /// The Noa has no timeline: its server is off, or it lacks
    /// `boardTimelineV1`. Nothing is shown and nothing is stored.
    NoTimeline,
    Failed(String),
}

impl From<&str> for Unreachable {
    fn from(message: &str) -> Self {
        Self::Failed(message.into())
    }
}

/// A connection, or `None` when the Noa has no timeline.
fn reach<C>(connect: impl FnOnce() -> Result<C, Unreachable>) -> Result<Option<C>, String> {
    match connect() {
        Ok(client) => Ok(Some(client)),
        Err(Unreachable::NoTimeline) => Ok(None),
        Err(Unreachable::Failed(message)) => Err(message),
    }
}

fn excerpt_summary(mut text: String) -> String {
    let mut chars = text.char_indices().skip(MAX_SUMMARY_CHARS - 1);
    if let Some((end, _)) = chars.next()
        && chars.next().is_some()
    {
        text.truncate(end);
        text.push('…');
    }
    text
}

fn excerpt_body(mut text: String) -> String {
    if text.len() > MAX_BODY_BYTES {
        text.truncate(text.floor_char_boundary(MAX_BODY_BYTES - FULL_RESULT_NOTE.len()));
        text.push_str(FULL_RESULT_NOTE);
    }
    text
}

fn participants(
    session: &Session,
) -> impl Iterator<Item = &competition::model::types::Participant> {
    session.judges().chain(&session.contestants)
}

fn members(session: &Session) -> impl Iterator<Item = (u64, String)> + '_ {
    participants(session)
        .filter_map(|p| Some((p.pane_id?, format!("comp:{}:{}", session.id, p.id))))
}

fn candidates(session: &Session, close: usize) -> Vec<PostInput> {
    if !session.closed {
        return vec![];
    }
    let id = session.id.as_str();
    let last = session.rounds.iter().rev().find_map(|r| r.outcome.as_ref());
    let summary = session
        .final_verdict()
        .map(str::to_owned)
        .or_else(|| session.winner.as_ref().map(|p| format!("優勝: {p}")))
        .or_else(|| last.map(|o| o.overview.clone()))
        .unwrap_or_else(|| "採点が確定する前にコンペを終了しました".into());
    let checks = session
        .contestant_ids()
        .map(|p| {
            let score = session.latest_score(p);
            Check {
                name: match score {
                    Some(n) => format!("{p} 採点: {n}/100"),
                    None => format!("{p} 採点"),
                },
                result: if score.is_some() {
                    CheckResult::Passed
                } else {
                    CheckResult::NotRun
                },
                reported_by: session.judge.id.to_string(),
                evidence_ref: None,
            }
        })
        .collect();
    vec![PostInput {
        schema_version: SCHEMA_VERSION,
        kind: PostKind::Summary,
        outcome: if session.winner.is_some() {
            Outcome::Succeeded
        } else {
            Outcome::Unknown
        },
        title: format!("コンペの結果 · {id}"),
        summary: excerpt_summary(summary),
        body_markdown: excerpt_body(export::result_markdown(session)),
        open_issues: vec![],
        next_actions: vec![],
        checks,
        source_ref: SourceRef {
            namespace: NAMESPACE.into(),
            session_id: id.into(),
            run_id: Some(id.into()),
            event_id: format!("closed-{close}"),
            locator: None,
        },
        artifacts: vec![],
        author: None,
        based_on_post_ids: vec![],
        occurred_at: None,
        retry_of_run_id: None,
        supersedes_post_id: None,
        task_ref: None,
    }]
}

fn request_id(id: &str, event: &str) -> String {
    format!("{NAMESPACE}-{id}-{event}")
}

fn registration(id: &str, board: Option<String>, pane: Option<WireId>) -> RegisterRunParams {
    RegisterRunParams {
        request_id: request_id(id, "run"),
        board_id: board,
        board_name: None,
        orchestrator_pane_id: pane,
        worker_pane_id: None,
        task_ref: None,
        retry_of_run_id: None,
        publish_policy: PublishPolicy {
            owner: PublishOwner::Runner,
            producer_id: Some(format!("{NAMESPACE}:{id}")),
        },
        role: None,
        label: None,
    }
}

fn connect() -> Result<Client, Unreachable> {
    let (config, _) = noa_config::load_startup_config(Default::default())
        .map_err(|_| "Noa の接続設定を読めません")?;
    if !config.server_enable {
        return Err(Unreachable::NoTimeline);
    }
    let token = config
        .server_token
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::fs::read_to_string(noa_config::server_token_path()?).ok())
        .ok_or("Noa の認証トークンを読めません")?;
    let ip = match config
        .server_bind
        .parse()
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
    {
        IpAddr::V4(v) if v.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v) if v.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    let url = format!("ws://{}/", SocketAddr::new(ip, config.server_port));
    let client = Client::connect(
        &url,
        token.trim(),
        ScopeSet::from_strings(["read", "control"]),
    )
    .map_err(|_| "Noa のタイムラインに接続できません")?;
    if !client.supports_board_timeline() {
        return Err(Unreachable::NoTimeline);
    }
    Ok(client)
}

trait TimelineApi: PostSink {
    fn board(&mut self, session: &Session) -> Result<Option<WireId>, String>;
    fn register_run(&mut self, params: &RegisterRunParams) -> Result<RegisterRunResult, String>;
}

impl TimelineApi for Client {
    fn board(&mut self, session: &Session) -> Result<Option<WireId>, String> {
        let panels = self
            .list_panels()
            .map_err(|_| "Noa のペインを取得できません")?;
        Ok(members(session).find_map(|(id, label)| {
            panels
                .iter()
                .find(|p| p.pane_id.0 == id && p.label.as_deref() == Some(&label))?
                .parent_pane_id
        }))
    }
    fn register_run(&mut self, params: &RegisterRunParams) -> Result<RegisterRunResult, String> {
        self.timeline_register_run(params).map_err(|e| match e {
            noa_ipc::ClientError::Rpc { message, .. } => message,
            _ => "タイムラインへの実行登録を確認できません".into(),
        })
    }
}

/// The run the session posts under: registered again with the saved board
/// when one is saved, else only from a board pane. `None` for a closed
/// session with neither ([`NOT_REGISTERED`]).
fn register(
    client: &mut dyn TimelineApi,
    session: &Session,
    saved: Option<RunBinding>,
) -> Result<Option<RunBinding>, String> {
    let board = client.board(session)?;
    if board.is_none() && saved.is_none() {
        if session.closed {
            return Ok(None);
        }
        return Err("このセッションのボードペインが見つかりません".into());
    }
    let params = registration(
        session.id.as_str(),
        saved.as_ref().map(|r| r.board_id.clone()),
        board,
    );
    let run = client.register_run(&params)?;
    let binding = RunBinding {
        board_id: run.board_id,
        run_id: run.run_id,
        producer_id: run.producer_id,
    };
    if saved.as_ref().is_some_and(|previous| *previous != binding) {
        return Err("保存済みの実行と登録結果が一致しません".into());
    }
    Ok(Some(binding))
}

fn stage_closed(
    outbox: &Outbox,
    binding: &RunBinding,
    mut post: PostInput,
    previous: Option<String>,
) -> Result<Entry, String> {
    let id = &post.source_ref.session_id;
    let key = EnvelopeKey {
        board_id: binding.board_id.clone(),
        producer_id: binding.producer_id.clone(),
        request_id: request_id(id, &post.source_ref.event_id),
    };
    if let Some(entry) = outbox.get(&key).map_err(|e| e.to_string())? {
        return Ok(entry);
    }
    // Older widgets staged the first close under the unnumbered request ID.
    if previous.is_none()
        && let Some(legacy) = outbox
            .get(&EnvelopeKey {
                request_id: request_id(id, "closed"),
                ..key.clone()
            })
            .map_err(|e| e.to_string())?
        && legacy.envelope.post.validate().is_ok()
        && wire_bytes(&legacy.envelope)? <= MAX_POST_PARAMS_BYTES
    {
        return Ok(legacy);
    }
    if !valid_token(&key.request_id) {
        return Err("終了イベントのリクエスト ID が無効です".into());
    }
    post.supersedes_post_id = previous;
    post.validate().map_err(|e| e.to_string())?;
    let mut envelope = Envelope {
        board_id: key.board_id,
        producer_id: key.producer_id,
        request_id: key.request_id,
        run_id: Some(binding.run_id.clone()),
        post,
    };
    // Escaping quotes or control characters can exceed the wire cap even with a 32 KiB body.
    while wire_bytes(&envelope)? > MAX_POST_PARAMS_BYTES {
        let body = &mut envelope.post.body_markdown;
        let prefix = body.strip_suffix(FULL_RESULT_NOTE).unwrap_or(body);
        if prefix.is_empty() {
            return Err("本文以外の投稿内容が送信サイズの上限を超えています".into());
        }
        body.truncate(prefix.floor_char_boundary(prefix.len() / 2));
        body.push_str(FULL_RESULT_NOTE);
    }
    outbox.stage(&envelope).map_err(|e| e.to_string())
}

fn wire_bytes(envelope: &Envelope) -> Result<usize, String> {
    serde_json::to_vec(envelope)
        .map(|bytes| bytes.len())
        .map_err(|e| e.to_string())
}

fn outbox_path(root: &Path, id: &str) -> Result<std::path::PathBuf, String> {
    if !valid_token(&request_id(id, "run")) || Path::new(id).file_name() != Some(id.as_ref()) {
        return Err("セッション ID が無効です".into());
    }
    Ok(root.join(NAMESPACE).join(id))
}

fn logged_candidates(dir: &Path, snapshot: &Session) -> Result<Vec<PostInput>, String> {
    use competition::model::reduce::{self, Event};
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(dir.join(competition::store::EVENTS_FILE))
        .map_err(|e| e.to_string())?;
    let mut session = None;
    let mut posts = Vec::new();
    let mut revision = 0;
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let events: Vec<Event> = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        revision += 1;
        for batch in events.split_inclusive(|e| matches!(e, Event::SessionClosed)) {
            let model = match session.as_mut() {
                Some(model) => {
                    reduce::apply(model, batch).map_err(|e| e.to_string())?;
                    model
                }
                None => session.insert(reduce::create(batch).map_err(|e| e.to_string())?),
            };
            if model.id != snapshot.id {
                return Err("記録のセッション ID が変わりました".into());
            }
            if matches!(batch.last(), Some(Event::SessionClosed)) {
                posts.extend(candidates(model, posts.len() + 1));
            }
        }
        if revision == snapshot.rev {
            if session.is_some_and(|s| s.closed) && !posts.is_empty() {
                return Ok(posts);
            }
            break;
        }
    }
    Err("終了イベントが見つかりません".into())
}

fn sync<C: TimelineApi>(
    root: &Path,
    session: &Session,
    stored: Option<&Path>,
    connect: impl Fn() -> Result<C, Unreachable>,
) -> Result<Option<Status>, String> {
    let path = outbox_path(root, session.id.as_str())?;
    // The outbox is made only once a run is registered: a Noa without a
    // timeline, or a run that cannot be registered, leaves nothing behind.
    let opened = if path.try_exists().map_err(|e| e.to_string())? {
        Some(Outbox::open(path.clone(), true).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let saved = match &opened {
        Some(outbox) => outbox.load_run().map_err(|e| e.to_string())?,
        None => None,
    };
    let mut client = None;
    let (outbox, binding) = match (opened, saved) {
        (Some(outbox), Some(binding)) if session.closed => (outbox, binding),
        (opened, saved) => {
            let Some(mut conn) = reach(&connect)? else {
                return Ok(None);
            };
            let Some(binding) = register(&mut conn, session, saved)? else {
                return Ok(Some(Status::Waiting {
                    why: NOT_REGISTERED.into(),
                }));
            };
            let outbox = match opened {
                Some(outbox) => outbox,
                None => Outbox::open(path, true).map_err(|e| e.to_string())?,
            };
            outbox.save_run(&binding).map_err(|e| e.to_string())?;
            client = Some(conn);
            (outbox, binding)
        }
    };
    if !session.closed {
        return Ok(None);
    }
    let mut connection = || client.take().map_or_else(&connect, Ok);
    let Some(dir) = stored else {
        let entry = latest_entry(&outbox)?.ok_or("終了イベントの記録が見つかりません")?;
        return Ok(deliver(&outbox, entry, connection)?.map(Status::Publish));
    };
    let mut previous = None;
    let mut latest = None;
    for post in logged_candidates(dir, session)? {
        let entry = stage_closed(&outbox, &binding, post, previous.take())?;
        let Some(state) = deliver(&outbox, entry, &mut connection)? else {
            return Ok(None);
        };
        match &state {
            PublishState::Published { post_id, .. } => previous = Some(post_id.clone()),
            _ => return Ok(Some(Status::Publish(state))),
        }
        latest = Some(state);
    }
    Ok(latest.map(Status::Publish))
}

// A later close is staged only after the preceding close is acknowledged.
fn latest_entry(outbox: &Outbox) -> Result<Option<Entry>, String> {
    let entries = outbox.entries().map_err(|e| e.to_string())?;
    Ok(entries.into_iter().max_by_key(|entry| {
        entry
            .envelope
            .post
            .source_ref
            .event_id
            .strip_prefix("closed-")
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(0)
    }))
}

/// Sends `entry` unless it is published: `None` when the Noa has no
/// timeline (nothing is recorded).
fn deliver<C: PostSink>(
    outbox: &Outbox,
    entry: Entry,
    connect: impl FnOnce() -> Result<C, Unreachable>,
) -> Result<Option<PublishState>, String> {
    entry.envelope.post.validate().map_err(|e| e.to_string())?;
    if matches!(entry.state, PublishState::Published { .. }) {
        return Ok(Some(entry.state));
    }
    let mut client = match connect() {
        Ok(client) => client,
        Err(Unreachable::NoTimeline) => return Ok(None),
        Err(Unreachable::Failed(message)) => {
            let state = PublishState::Failed {
                kind: None,
                message,
            };
            outbox
                .record(&entry.envelope.key(), state.clone())
                .map_err(|e| e.to_string())?;
            return outbox
                .get(&entry.envelope.key())
                .map_err(|e| e.to_string())?
                .map(|entry| Some(entry.state))
                .ok_or_else(|| "保存済みの投稿が見つかりません".into());
        }
    };
    Ok(Some(publish(&mut client, outbox, entry.envelope)))
}

// A lost acknowledgement survives the widget closing, even if its session is never opened again.
fn restore<C: TimelineApi>(
    root: &Path,
    sessions: Option<&Path>,
    connect: &impl Fn() -> Result<C, Unreachable>,
    report: &impl Fn(&str, Option<Status>),
) {
    let Ok(dirs) = std::fs::read_dir(root.join(NAMESPACE)) else {
        return;
    };
    for dir in dirs.flatten() {
        if !dir.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(id) = dir.file_name().into_string() else {
            continue;
        };
        let result = (|| {
            let outbox = Outbox::open(outbox_path(root, &id)?, true).map_err(|e| e.to_string())?;
            if let Some(dir) = sessions.map(|p| p.join(&id))
                && let Ok(session) = competition::store::read_session(&dir)
                && session.closed
                && session.id.as_str() == id
            {
                return sync(root, &session, Some(&dir), connect);
            }
            let Some(entry) = latest_entry(&outbox)? else {
                return Ok(None);
            };
            if !matches!(entry.state, PublishState::Published { .. }) {
                report(&id, Some(Status::Publish(PublishState::Pending)));
            }
            Ok(deliver(&outbox, entry, connect)?.map(Status::Publish))
        })();
        match result {
            Ok(state) => report(&id, state),
            Err(message) => report(
                &id,
                Some(Status::Publish(PublishState::Failed {
                    kind: None,
                    message,
                })),
            ),
        }
    }
}

/// What the header shows after a job syncs `session`; `stored` when it was
/// reopened from history. Reopened with no outbox, it was never registered
/// and registers nothing now: it waits ([`NOT_REGISTERED`]) on a Noa with a
/// timeline and shows nothing on one without.
fn job<C: TimelineApi>(
    root: &Path,
    session: &Session,
    stored: bool,
    dir: Option<&Path>,
    connect: impl Fn() -> Result<C, Unreachable>,
    report: &impl Fn(&str, Option<Status>),
) -> Result<Option<Status>, String> {
    let id = session.id.as_str();
    if stored
        && !outbox_path(root, id)?
            .try_exists()
            .map_err(|e| e.to_string())?
    {
        return Ok(reach(connect)?.map(|_| Status::Waiting {
            why: NOT_REGISTERED.into(),
        }));
    }
    if session.closed {
        report(id, Some(Status::Publish(PublishState::Pending)));
    }
    sync(root, session, dir, connect)
}

enum Trigger {
    Confirmed(PathBuf),
    Stored(PathBuf),
    Retry,
}

#[derive(Clone)]
pub(crate) struct Timeline {
    jobs: mpsc::Sender<(Arc<Session>, Trigger)>,
    states: Arc<Mutex<HashMap<String, Status>>>,
}

impl Timeline {
    pub fn start(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (jobs, rx) = mpsc::channel::<(Arc<Session>, Trigger)>();
        let states = Arc::new(Mutex::new(HashMap::new()));
        let updates = Arc::clone(&states);
        let _runner = std::thread::Builder::new()
            .name(format!("{NAMESPACE}-timeline"))
            .spawn(move || {
                let root = noa_config::timeline_outbox_dir();
                // `None` clears what the header shows.
                let report = |id: &str, state: Option<Status>| {
                    let mut states = updates.lock().unwrap();
                    let changed = match state {
                        Some(state) => {
                            states.insert(id.to_owned(), state);
                            true
                        }
                        None => states.remove(id).is_some(),
                    };
                    drop(states);
                    if changed {
                        wake();
                    }
                };
                if let Some(root) = &root {
                    restore(
                        root,
                        competition::store::default_root().as_deref(),
                        &connect,
                        &report,
                    );
                }
                let mut failed_dirs = HashMap::new();
                for (session, trigger) in rx {
                    let id = session.id.as_str();
                    let stored = matches!(trigger, Trigger::Stored(_));
                    let dir = match trigger {
                        Trigger::Confirmed(dir) | Trigger::Stored(dir) => Some(dir),
                        Trigger::Retry => failed_dirs.remove(id),
                    };
                    let result = root
                        .as_deref()
                        .ok_or_else(|| "タイムラインの保存先が見つかりません".to_owned())
                        .and_then(|root| {
                            job(root, &session, stored, dir.as_deref(), connect, &report)
                        });
                    if matches!(
                        &result,
                        Err(_) | Ok(Some(Status::Publish(PublishState::Failed { .. })))
                    ) {
                        if let Some(dir) = dir {
                            failed_dirs.insert(id.to_owned(), dir);
                        }
                    } else {
                        failed_dirs.remove(id);
                    }
                    match result {
                        Ok(state) => report(id, state),
                        Err(message) => report(
                            id,
                            Some(Status::Publish(PublishState::Failed {
                                kind: None,
                                message,
                            })),
                        ),
                    }
                }
            });
        Self { jobs, states }
    }

    pub fn observe(&self, previous: Option<&Session>, next: &Arc<Session>, log_dir: PathBuf) {
        let changed = if next.closed {
            previous.is_none_or(|s| !s.closed)
        } else {
            fn panes(s: &Session) -> impl Iterator<Item = Option<u64>> + '_ {
                participants(s).map(|p| p.pane_id)
            }
            participants(next).any(|p| p.pane_id.is_some())
                && previous.is_none_or(|s| !panes(s).eq(panes(next)))
        };
        if changed {
            self.schedule(next, Trigger::Confirmed(log_dir));
        }
    }

    pub fn recover(&self, session: &Arc<Session>, dir: &Path) {
        if session.closed {
            self.schedule(session, Trigger::Stored(dir.to_owned()));
        }
    }

    pub fn retry(&self, session: &Arc<Session>) {
        self.schedule(session, Trigger::Retry);
    }

    fn schedule(&self, session: &Arc<Session>, trigger: Trigger) {
        if self.jobs.send((session.clone(), trigger)).is_err() {
            self.states.lock().unwrap().insert(
                session.id.to_string(),
                Status::Publish(PublishState::Failed {
                    kind: None,
                    message: "投稿スレッドが停止しています。ウィジェットを再起動してください"
                        .into(),
                }),
            );
        }
    }

    pub fn status(&self, session: &Session) -> Option<Status> {
        self.states
            .lock()
            .unwrap()
            .get(session.id.as_str())
            .cloned()
    }
}

pub(crate) fn label(state: &PublishState) -> String {
    match state {
        PublishState::Pending => "タイムライン: 投稿待ち".into(),
        PublishState::Published { seq, .. } => format!("タイムライン: 投稿済み #{}", seq.0),
        PublishState::Failed { .. } => "タイムライン: 投稿失敗（再試行）".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use competition::model::reduce::{self, Event};
    use competition::store::{EVENTS_FILE, Store};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    fn batches() -> Vec<Vec<Event>> {
        include_str!("model/testdata/single-judge-v0.9.15.events.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    struct Fixture {
        root: std::path::PathBuf,
        dir: std::path::PathBuf,
        session: Session,
    }

    impl Fixture {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "noa-{NAMESPACE}-timeline-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let batches = batches();
            let session = reduce::replay(&batches).unwrap();
            let dir = root.join("sessions").join(session.id.as_str());
            let mut log = Store::create(&dir, &batches[0]).unwrap();
            log.append_all(&batches[1..]).unwrap();
            Self { session, root, dir }
        }
        fn reopen(&self) -> Outbox {
            Outbox::open(
                outbox_path(&self.root, self.session.id.as_str()).unwrap(),
                true,
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[derive(Default, Clone)]
    struct Api {
        panes_closed: Rc<Cell<bool>>,
        lose_registration_ack: Rc<Cell<bool>>,
        registrations: Rc<RefCell<Vec<RegisterRunParams>>>,
        posts: Rc<RefCell<Vec<PostParams>>>,
        attempts: Rc<Cell<usize>>,
    }
    impl TimelineApi for Api {
        fn board(&mut self, _: &Session) -> Result<Option<WireId>, String> {
            Ok((!self.panes_closed.get()).then_some(WireId(42)))
        }
        fn register_run(&mut self, p: &RegisterRunParams) -> Result<RegisterRunResult, String> {
            self.registrations.borrow_mut().push(p.clone());
            if self.lose_registration_ack.replace(false) {
                return Err("登録の応答がタイムアウトしました".into());
            }
            Ok(RegisterRunResult {
                board_id: "bd-0000000000000001".into(),
                run_id: "rn-0000000000000002".into(),
                producer_id: p.publish_policy.producer_id.clone().unwrap(),
                worker_id: None,
                created: p.board_id.is_none(),
            })
        }
    }
    impl PostSink for Api {
        fn post(&mut self, p: &PostParams) -> Result<PostResult, noa_ipc::ClientError> {
            self.attempts.set(self.attempts.get() + 1);
            let mut posts = self.posts.borrow_mut();
            if let Some(index) = posts
                .iter()
                .position(|previous| previous.request_id == p.request_id)
            {
                assert_eq!(&posts[index], p);
                let seq = index as u64 + 1;
                Ok(PostResult {
                    post_id: format!("po-{seq:016x}"),
                    seq: Seq(seq),
                    created_at: "2026-11-26T00:00:00Z".into(),
                    duplicate: true,
                })
            } else {
                posts.push(p.clone());
                Err(noa_ipc::ClientError::Timeout("ack lost".into()))
            }
        }
    }

    #[test]
    fn ac5_candidates_from_confirmed_events_only() {
        let events = batches();
        let mut session = reduce::create(&events[0]).unwrap();
        for batch in &events[1..] {
            assert!(candidates(&session, 1).is_empty());
            reduce::apply(&mut session, batch).unwrap();
        }
        let posts = candidates(&session, 1);
        assert_eq!(posts.len(), 1);
        let post = &posts[0];
        post.validate().unwrap();
        assert_eq!(post.body_markdown, export::result_markdown(&session));
        assert_eq!(post.kind, PostKind::Summary);
        assert_eq!(Some(post.summary.as_str()), session.final_verdict());
        assert_eq!(post.checks.len(), 2);
        assert!(
            post.checks
                .iter()
                .any(|c| c.name.contains("claude") && c.name.contains("88/100"))
        );
    }

    #[test]
    fn ac5_excerpts_long_results_before_staging() {
        let f = Fixture::new();
        let mut session = f.session.clone();
        let outcome = session.rounds.last_mut().unwrap().outcome.as_mut().unwrap();
        outcome.final_verdict = Some("決".repeat(MAX_SUMMARY_CHARS + 1));
        session.request = "あ🦀".repeat(MAX_BODY_BYTES);
        let post = candidates(&session, 1).pop().unwrap();
        post.validate().unwrap();
        assert_eq!(
            post.summary,
            format!("{}…", "決".repeat(MAX_SUMMARY_CHARS - 1))
        );
        let prefix = post
            .body_markdown
            .strip_suffix("\n\n（全文は「結果をコピー」で確認できます）")
            .unwrap();
        assert!(export::result_markdown(&session).starts_with(prefix));
        assert!((MAX_BODY_BYTES - 3..=MAX_BODY_BYTES).contains(&post.body_markdown.len()));
    }

    #[test]
    fn ac5_escaped_body_fits_the_wire_request() {
        let f = Fixture::new();
        let outbox = f.reopen();
        let run = register(&mut Api::default(), &f.session, None)
            .unwrap()
            .unwrap();
        let mut post = candidates(&f.session, 1).pop().unwrap();
        post.body_markdown = "\x01".repeat(MAX_BODY_BYTES);
        let entry = stage_closed(&outbox, &run, post, None).unwrap();
        let api = Api::default();
        deliver(&outbox, entry, || Ok(api.clone())).unwrap();
        let posts = api.posts.borrow();
        assert!(serde_json::to_vec(&posts[0]).unwrap().len() <= MAX_POST_PARAMS_BYTES);
        assert!(posts[0].post.body_markdown.ends_with(FULL_RESULT_NOTE));
    }

    #[test]
    fn ac5_invalid_candidate_is_not_staged() {
        let f = Fixture::new();
        let outbox = f.reopen();
        let run = register(&mut Api::default(), &f.session, None)
            .unwrap()
            .unwrap();
        let mut post = candidates(&f.session, 1).pop().unwrap();
        post.summary.clear();
        assert!(stage_closed(&outbox, &run, post, None).is_err());
        assert!(outbox.entries().unwrap().is_empty());
    }

    #[test]
    fn ac5_registration_retry_after_panes_closed() {
        let f = Fixture::new();
        let api = Api::default();
        let open = reduce::create(&batches()[0]).unwrap();
        api.lose_registration_ack.set(true);
        assert!(sync(&f.root, &open, None, || Ok(api.clone())).is_err());
        api.panes_closed.set(true);
        // No saved run and no board pane: a run registered now would get a
        // rootless board of its own, unrelated to the session.
        let waiting = sync(&f.root, &f.session, Some(&f.dir), || Ok(api.clone()));
        assert_eq!(api.registrations.borrow().len(), 1);
        assert!(api.posts.borrow().is_empty());
        assert!(
            !outbox_path(&f.root, f.session.id.as_str())
                .unwrap()
                .exists()
        );
        assert_eq!(
            waiting,
            Ok(Some(Status::Waiting {
                why: NOT_REGISTERED.into()
            }))
        );
    }

    #[test]
    fn ac5_saved_run_resends_after_panes_closed() {
        let f = Fixture::new();
        let api = Api::default();
        let open = reduce::create(&batches()[0]).unwrap();
        sync(&f.root, &open, None, || Ok(api.clone())).unwrap();
        let binding = f.reopen().load_run().unwrap().unwrap();
        api.panes_closed.set(true);
        sync(&f.root, &f.session, Some(&f.dir), || Ok(api.clone())).unwrap();
        let ack = sync(&f.root, &f.session, Some(&f.dir), || Ok(api.clone())).unwrap();
        assert!(
            matches!(ack, Some(Status::Publish(PublishState::Published { .. }))),
            "{ack:?}"
        );
        assert_eq!(api.registrations.borrow().len(), 1);
        let posts = api.posts.borrow();
        assert_eq!(posts[0].board_id, binding.board_id);
        assert_eq!(posts[0].run_id.as_ref(), Some(&binding.run_id));
    }

    #[test]
    fn ac5_no_timeline_shows_and_creates_nothing() {
        let f = Fixture::new();
        let open = reduce::create(&batches()[0]).unwrap();
        for session in [&open, &f.session] {
            let state = sync::<Api>(&f.root, session, Some(&f.dir), || {
                Err(Unreachable::NoTimeline)
            });
            assert!(matches!(state, Ok(None)), "{state:?}");
        }
        assert!(!f.root.join(NAMESPACE).exists());
    }

    #[test]
    fn ac5_reopened_unregistered_session_still_waits() {
        let f = Fixture::new();
        let api = Api::default();
        api.panes_closed.set(true);
        let report = |_: &str, _: Option<Status>| {};
        let waiting = Ok(Some(Status::Waiting {
            why: NOT_REGISTERED.into(),
        }));
        // Closed with no saved run and no board pane, then reopened from history.
        for stored in [false, true] {
            let state = job(
                &f.root,
                &f.session,
                stored,
                Some(&f.dir),
                || Ok(api.clone()),
                &report,
            );
            assert_eq!(state, waiting, "stored: {stored}");
        }
        let state = job::<Api>(
            &f.root,
            &f.session,
            true,
            Some(&f.dir),
            || Err(Unreachable::NoTimeline),
            &report,
        );
        assert_eq!(state, Ok(None));
        assert!(api.registrations.borrow().is_empty());
        assert!(api.posts.borrow().is_empty());
        assert!(!f.root.join(NAMESPACE).exists());
    }

    #[test]
    fn ac5_request_ids_stable_across_replay() {
        let f = Fixture::new();
        let api = Api::default();
        let open = reduce::create(&batches()[0]).unwrap();
        sync(&f.root, &open, None, || Ok(api.clone())).unwrap();
        sync(&f.root, &open, None, || Ok(api.clone())).unwrap();
        let requests = api.registrations.borrow();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].request_id,
            format!("{NAMESPACE}-{}-run", open.id)
        );
        assert_eq!(requests[0].request_id, requests[1].request_id);
        assert_eq!(requests[0].orchestrator_pane_id, Some(WireId(42)));
        assert_eq!(requests[0].worker_pane_id, None);
        assert_eq!(requests[0].publish_policy.owner, PublishOwner::Runner);
        assert_eq!(requests[1].board_id.as_deref(), Some("bd-0000000000000001"));
        let run = f.reopen().load_run().unwrap().unwrap();
        let rebuilt = competition::store::read_session(&f.dir).unwrap();
        let first = stage_closed(
            &f.reopen(),
            &run,
            logged_candidates(&f.dir, &f.session)
                .unwrap()
                .pop()
                .unwrap(),
            None,
        )
        .unwrap()
        .envelope;
        assert_eq!(first.post, candidates(&rebuilt, 1)[0]);
        assert_eq!(
            first.request_id,
            format!("{NAMESPACE}-{}-closed-1", open.id)
        );
        assert_eq!(
            first.post.source_ref,
            SourceRef {
                namespace: NAMESPACE.into(),
                session_id: open.id.to_string(),
                run_id: Some(open.id.to_string()),
                event_id: "closed-1".into(),
                locator: None
            }
        );
        assert_eq!(first.producer_id, format!("{NAMESPACE}:{}", open.id));
    }

    #[test]
    fn ac5_restart_resends_exactly_once() {
        let f = Fixture::new();
        let api = Api::default();
        let failed = sync(&f.root, &f.session, Some(&f.dir), || Ok(api.clone())).unwrap();
        assert!(matches!(
            failed,
            Some(Status::Publish(PublishState::Failed { .. }))
        ));
        // Without a session log the staged envelope is still sufficient for a retry.
        let mut later = f.session.clone();
        later.request.push_str(" extended");
        let restored = RefCell::new(Vec::new());
        restore(&f.root, None, &|| Ok(api.clone()), &|id, state| {
            assert_eq!(id, f.session.id.as_str());
            restored.borrow_mut().push(state);
        });
        let ack = restored.borrow_mut().pop().flatten();
        assert_eq!(
            *restored.borrow(),
            vec![Some(Status::Publish(PublishState::Pending))]
        );
        let Some(Status::Publish(ack @ PublishState::Published { seq: Seq(1), .. })) = ack else {
            panic!("{ack:?}");
        };
        assert_eq!(f.reopen().entries().unwrap()[0].state, ack);
        assert_eq!(
            sync::<Api>(&f.root, &later, None, || panic!("ack already saved")).unwrap(),
            Some(Status::Publish(ack))
        );
        assert_eq!(api.attempts.get(), 2);
        assert_eq!(api.posts.borrow().len(), 1);
    }

    #[test]
    fn ac5_reclose_supersedes_the_acknowledged_close_after_restart() {
        let f = Fixture::new();
        let api = Api::default();
        let send = || Ok(api.clone());
        let first = sync(&f.root, &f.session, Some(&f.dir), send).unwrap();
        assert!(matches!(
            first,
            Some(Status::Publish(PublishState::Failed { .. }))
        ));

        let (mut log, session) = Store::open(&f.dir).unwrap();
        let last = session.rounds.last().unwrap();
        let round = last.no + 1;
        let mut output = last.outcome.clone().unwrap();
        output.final_verdict = Some("改訂後の結論".into());
        output.ranking.reverse();
        output.winner = output.ranking.first().cloned();
        output
            .evaluations
            .get_mut(output.winner.as_ref().unwrap())
            .unwrap()
            .score = 97;
        output.amendments.clear();
        let event = Event::JudgeAccepted { round, output };
        log.append(&[Event::RoundsExtended {
            max_rounds: round,
            at: 4,
        }])
        .unwrap();
        log.append(&[Event::RoundStarted {
            no: round,
            kind: last.kind,
            at: 5,
        }])
        .unwrap();
        log.append(&[event, Event::SessionClosed]).unwrap();
        drop(log);

        let sessions = f.dir.parent();
        let states = RefCell::new(Vec::new());
        let report = |_: &str, state: Option<Status>| states.borrow_mut().push(state);
        restore(&f.root, sessions, &send, &report);
        assert!(matches!(
            states.borrow().last(),
            Some(Some(Status::Publish(PublishState::Failed { .. })))
        ));
        restore(&f.root, sessions, &send, &report);
        assert!(matches!(
            states.borrow().last(),
            Some(Some(Status::Publish(PublishState::Published {
                seq: Seq(2),
                ..
            })))
        ));
        restore::<Api>(
            &f.root,
            None,
            &|| panic!("both acknowledgements saved"),
            &report,
        );
        assert!(matches!(
            states.borrow().last(),
            Some(Some(Status::Publish(PublishState::Published {
                seq: Seq(2),
                ..
            })))
        ));
        let posts = api.posts.borrow();
        assert_eq!(posts.len(), 2);
        assert_eq!(api.attempts.get(), 4);
        assert_eq!(posts[0].post, candidates(&f.session, 1)[0]);
        assert_eq!(
            posts[1].request_id,
            request_id(f.session.id.as_str(), "closed-2")
        );
        let revised = competition::store::read_session(&f.dir).unwrap();
        let mut expected = candidates(&revised, 2).pop().unwrap();
        expected.supersedes_post_id = Some("po-0000000000000001".into());
        assert_eq!(posts[1].post, expected);
        assert_eq!(posts[1].post.summary, "改訂後の結論");
    }

    #[test]
    fn ac5_reuses_a_valid_legacy_close_and_rebuilds_an_invalid_one() {
        for invalid in [false, true] {
            let f = Fixture::new();
            let outbox = f.reopen();
            let api = Api::default();
            let run = register(&mut api.clone(), &f.session, None)
                .unwrap()
                .unwrap();
            let mut post = candidates(&f.session, 1).pop().unwrap();
            post.source_ref.event_id = "closed".into();
            if invalid {
                post.summary = "字".repeat(MAX_SUMMARY_CHARS + 1);
            }
            outbox
                .stage(&Envelope {
                    board_id: run.board_id,
                    producer_id: run.producer_id,
                    request_id: request_id(f.session.id.as_str(), "closed"),
                    run_id: Some(run.run_id),
                    post,
                })
                .unwrap();
            let send = || Ok(api.clone());
            sync(&f.root, &f.session, Some(&f.dir), send).unwrap();
            let ack = sync(&f.root, &f.session, Some(&f.dir), send).unwrap();
            assert!(matches!(
                ack,
                Some(Status::Publish(PublishState::Published { seq: Seq(1), .. }))
            ));
            let posts = api.posts.borrow();
            assert_eq!(posts.len(), 1);
            assert_eq!(
                posts[0].post.source_ref.event_id,
                if invalid { "closed-1" } else { "closed" }
            );
        }
    }

    #[test]
    fn ac5_missing_envelope_is_built_from_first_logged_close() {
        let f = Fixture::new();
        let api = Api::default();
        let mut later = f.session.clone();
        later.request = "changed since the first close".into();
        sync(&f.root, &later, Some(&f.dir), || Ok(api.clone())).unwrap();
        let entry = f.reopen().entries().unwrap().pop().unwrap();
        assert_eq!(entry.envelope.post, candidates(&f.session, 1)[0]);
        assert_ne!(
            entry.envelope.post.body_markdown,
            export::result_markdown(&later)
        );
    }

    #[test]
    fn ac5_failure_keeps_source_result() {
        let f = Fixture::new();
        let path = f.dir.join(EVENTS_FILE);
        let before = std::fs::read(&path).unwrap();
        let api = Api::default();
        let state = sync(&f.root, &f.session, Some(&f.dir), || Ok(api.clone())).unwrap();
        let Some(Status::Publish(state @ PublishState::Failed { .. })) = state else {
            panic!("{state:?}");
        };
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert_eq!(competition::store::read_session(&f.dir).unwrap(), f.session);
        assert_eq!(f.reopen().entries().unwrap()[0].state, state);
    }

    #[test]
    fn ac5_no_post_before_session_closed() {
        let f = Fixture::new();
        let api = Api::default();
        let events = batches();
        for interval in [0, 1] {
            let mut session = reduce::create(&events[0]).unwrap();
            session.reveal_interval = interval;
            for event in events[1..].iter().flatten() {
                if matches!(event, Event::SessionClosed) {
                    break;
                }
                reduce::apply(&mut session, std::slice::from_ref(event)).unwrap();
                assert!(candidates(&session, 1).is_empty());
                sync(&f.root, &session, None, || Ok(api.clone())).unwrap();
                assert!(f.reopen().entries().unwrap().is_empty());
            }
            assert!(session.rounds.iter().any(|r| !r.submissions.is_empty()));
            assert!(session.rounds.last().unwrap().outcome.is_some());
            assert_eq!(api.attempts.get(), 0);
        }
    }
}
