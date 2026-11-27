//! The live [`Feed`]: local task files, the run log, Noa's panes (over
//! [`ipc`]) and the GitHub source, each read off the UI thread; a new
//! [`View`] whenever any of them changed.
//!
//! One worker thread owns the files, the run log and the link to Noa: it
//! takes the intents that write or reach Noa, looks at `.noa/tasks/`,
//! `noa.listPanels` and the runs' result files at most every `POLL_MS`, and
//! sends what it sees. The GitHub source ([`IssueFeed`]) runs `gh` on its
//! own threads and never blocks, so the UI thread asks it directly.
//!
//! The worker also keeps the task links (`docs/specs/task-links.md`): it
//! names the repository in the link log, records each run's head as it
//! moves (the change poll, every `CHANGE_POLL_MS`), writes the links the
//! person adds or removes, and folds the log into each task's 関連成果.

pub mod ipc;
pub mod start;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tasks::github::{GhFeed, IssueFeed};
use tasks::local::{self, LocalFile, Scan};
use tasks::model::types::{
    AgentKind, Liveness, Millis, POLL_MS, PaneFact, Report, ResultFact, Run, RunEvent, RunId,
    RunView, State, Task, TaskRef,
};
use tasks::related::{self, LinkScan, Related};
use tasks::sibling::{self, Launcher};
use tasks::task_links::{self, CHANGE_POLL_MS, LinkEvent, SessionKind, TaskKey};
use tasks::timeline::{self, Publisher, RunSpec, Share, Status, TimelineApi};
use tasks::workflow_links;
use tasks::{code, runs, store, worktree};

use noa_ipc::timeline::Outcome;

use crate::installed::{self, Installed};
use crate::ui::{Feed, Intent, View};

use ipc::{Connect, IpcConnect, Noa, NoaError, PaneView};
use start::RunRequest;

/// How often the worker looks at its clock between intents.
const TICK: Duration = Duration::from_millis(100);
const MINUTE: Millis = 60_000;
/// How often the code's markers are read again (`git grep`), besides 再取得.
const CODE_EVERY: Millis = MINUTE;
const WORKFLOW_HELPER: sibling::Helper = sibling::Helper {
    bundled: "Noa Workflow",
    crate_name: "workflow",
    title: "Workflow",
};

/// What the UI thread hands the worker.
enum Job {
    Create(LocalFile),
    Edit {
        id: String,
        title: String,
        body: String,
        labels: Vec<String>,
    },
    SetState {
        id: String,
        state: State,
    },
    Run(Box<RunRequest>),
    OpenPane(RunId),
    ClosePane(RunId),
    OpenWorktree(RunId),
    Open(String),
    /// Read the code's markers again now, and look for moved heads (再取得).
    ScanCode,
    OpenSession {
        kind: SessionKind,
        session: String,
    },
    CheckVersion {
        task: TaskRef,
        kind: SessionKind,
        run: RunId,
        commit: String,
    },
    OpenWorkflow(Task),
    OpenWorkflowRun {
        task: TaskRef,
        run: String,
    },
    /// A PR's URL, parsed already.
    AddLink {
        task: TaskRef,
        url: String,
    },
    RemoveLink {
        task: TaskRef,
        id: String,
    },
    /// Where the runs are registered and shared from now on.
    Timeline(Box<TimelineLink>),
    /// 共有: posts the run's result with `outcome`; `None` sends the
    /// staged share again (再試行).
    Share {
        run: RunId,
        title: Option<String>,
        outcome: Option<Outcome>,
    },
}

/// What the worker sends back.
enum Up {
    Seen {
        tasks: Vec<Task>,
        runs: Vec<RunView>,
        related: BTreeMap<TaskRef, Related>,
    },
    Notice(String),
    /// Where each shared run's post stands.
    Timeline(BTreeMap<RunId, Status>),
}

/// How the worker reaches the Board Timeline: the outbox root
/// (`noa_config::timeline_outbox_dir()`) and a connection of its own (the
/// [`Connect`] link knows nothing of the timeline). Made on the worker.
pub struct TimelineLink {
    pub outbox_root: PathBuf,
    pub connect: Box<dyn FnMut() -> Result<Box<dyn TimelineApi>, String> + Send>,
}

impl TimelineLink {
    /// The running Noa's control server.
    pub fn system(outbox_root: PathBuf) -> Self {
        Self {
            outbox_root,
            connect: Box::new(|| {
                timeline::connect().map(|client| Box::new(client) as Box<dyn TimelineApi>)
            }),
        }
    }
}

/// The worker's side of the timeline.
struct Timeline {
    link: TimelineLink,
    /// The repository key as a request-id token ([`timeline::session_key`]).
    session: String,
    /// Its connection, dropped after a failure.
    api: Option<Box<dyn TimelineApi>>,
    /// Where each run shared (or failed to be) stands.
    states: BTreeMap<RunId, Status>,
    /// The outcome a share asked for, for a 再試行 of one whose share could
    /// not be saved (a saved share keeps its own).
    asked: HashMap<RunId, Outcome>,
}

impl Timeline {
    fn api(&mut self) -> Result<&mut Box<dyn TimelineApi>, String> {
        if self.api.is_none() {
            self.api = Some((self.link.connect)()?);
        }
        Ok(self.api.as_mut().expect("connected above"))
    }

    fn publisher(&self, run: &Run, live: bool) -> Result<Publisher, String> {
        Publisher::open(
            &self.link.outbox_root,
            &self.session,
            run.run.as_str(),
            RunSpec {
                worker_pane: run.pane.filter(|_| live),
                task_ref: Some(timeline::task_ref(&run.task)),
            },
        )
        .map_err(|e| format!("outbox: {e}"))
    }
}

/// The live feed of one repository.
pub struct LiveFeed<C> {
    repo: PathBuf,
    workspace_file: PathBuf,
    issues: Box<dyn IssueFeed>,
    clock: Arc<dyn Fn() -> Millis + Send + Sync>,
    jobs: Sender<Job>,
    ups: Receiver<Up>,
    tasks: Vec<Task>,
    runs: Vec<RunView>,
    related: BTreeMap<TaskRef, Related>,
    /// `related` changed since the last View handed out.
    related_new: bool,
    notice: Option<String>,
    agents: Option<Vec<AgentKind>>,
    installed: Option<Receiver<Installed>>,
    /// The last View handed out.
    shown: Option<View>,
    /// Tells the screen to poll: for what `act` says itself.
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Where each shared run's post stands, as the worker last said.
    timeline: BTreeMap<RunId, Status>,
    /// `timeline` changed since the last View handed out.
    timeline_new: bool,
    /// The connector lives on the worker.
    connect: PhantomData<fn() -> C>,
}

/// Milliseconds since the Unix epoch, now.
fn system_now() -> Millis {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as Millis)
}

/// Opens a URL or a directory with the desktop's opener, not waiting for it.
fn system_open(target: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    match std::process::Command::new(program).arg(target).spawn() {
        // Reaped off the worker.
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => eprintln!("tasks: {program} {target}: {e}"),
    }
}

impl LiveFeed<IpcConnect> {
    /// The feed of `repo` on the running Noa, `gh` and the widget's data dir.
    pub fn start(repo: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        // `--repo` is the focused pane's directory: the tasks are the work
        // tree's, wherever in it that is. Outside any work tree (Noa opens
        // the widget in $HOME, without `--repo`, when the focused pane's
        // directory is unknown) nothing is listed or written: the window
        // says which directory it was.
        let repo = worktree::toplevel(&repo).map_err(|why| {
            format!(
                "{} は git のリポジトリではありません（リポジトリ内のペインから開いてください）: {why}",
                repo.display()
            )
        })?;
        let root =
            store::default_root().ok_or_else(|| "データの保存先がわかりません".to_owned())?;
        let key = store::repo_key(&repo);
        let issues = GhFeed::start(repo.clone(), store::gh_cache(&root, &key), wake.clone());
        let mut feed = LiveFeed::with(
            repo,
            root,
            Box::new(issues),
            IpcConnect,
            Box::new(system_open),
            Box::new(sibling::System),
            Arc::new(system_now),
            wake.clone(),
        );
        feed.installed = Some(installed::spawn(move || wake()));
        if let Some(outbox) = noa_config::timeline_outbox_dir() {
            feed.send(Job::Timeline(Box::new(TimelineLink::system(outbox))));
        }
        Ok(feed)
    }
}

impl<C: Connect + Send + 'static> LiveFeed<C> {
    /// `opener` opens a URL or a directory (`open` / `xdg-open` in
    /// `start`); `launcher` starts Review or Verify on a linked session
    /// (`sibling::System` in `start`).
    #[allow(clippy::too_many_arguments)]
    pub fn with(
        repo: PathBuf,
        root: PathBuf,
        issues: Box<dyn IssueFeed>,
        connect: C,
        opener: Box<dyn FnMut(&str) + Send>,
        launcher: Box<dyn Launcher + Send>,
        clock: Arc<dyn Fn() -> Millis + Send + Sync>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let (jobs, job_rx) = mpsc::channel();
        let (up_tx, ups) = mpsc::channel();
        let workspace_file = root
            .join(store::repo_key(&repo))
            .join(tasks::workspace::FILE_NAME);
        let log = store::runs_log(&root, &store::repo_key(&repo));
        let links = start::links_of(&log);
        let (worker_repo, worker_clock, worker_wake) = (repo.clone(), clock.clone(), wake.clone());
        // Built on its thread: a connection need not be `Send`.
        let spawned = std::thread::Builder::new()
            .name("tasks-live".into())
            .spawn(move || {
                Worker {
                    scan: Scan::new(worker_repo.clone()),
                    repo: worker_repo,
                    root,
                    log,
                    link_scan: LinkScan::new(links.clone()),
                    links,
                    events: Vec::new(),
                    related: BTreeMap::new(),
                    workflows: BTreeMap::new(),
                    workflow_at: None,
                    fold_stale: true,
                    change_at: None,
                    here_live: HashSet::new(),
                    here_done: HashSet::new(),
                    link_failure: None,
                    launcher,
                    connect,
                    conn: None,
                    board: None,
                    runs: Vec::new(),
                    tasks: Vec::new(),
                    panes: Vec::new(),
                    facts: Vec::new(),
                    results: HashMap::new(),
                    panes_at: None,
                    results_at: None,
                    code: Vec::new(),
                    code_at: None,
                    code_failure: None,
                    brief_titles: HashMap::new(),
                    runs_seen: None,
                    opener,
                    clock: worker_clock,
                    wake: worker_wake,
                    up: up_tx,
                    sent: None,
                    timeline: None,
                }
                .run(job_rx)
            });
        let notice = spawned
            .err()
            .map(|e| format!("スレッドを開始できません: {e}"));
        LiveFeed {
            repo,
            workspace_file,
            issues,
            clock,
            jobs,
            ups,
            tasks: Vec::new(),
            runs: Vec::new(),
            related: BTreeMap::new(),
            related_new: false,
            notice,
            agents: None,
            installed: None,
            shown: None,
            wake,
            timeline: BTreeMap::new(),
            timeline_new: false,
            connect: PhantomData,
        }
    }

    /// A notice from the UI thread: shown on the next poll, which it asks for.
    fn say(&mut self, notice: impl Into<String>) {
        self.notice = Some(notice.into());
        (self.wake)();
    }

    fn send(&mut self, job: Job) {
        if self.jobs.send(job).is_err() {
            self.say("タスクの処理が止まっています");
        }
    }

    /// The task `r` names, as a run's brief needs it: an issue with its
    /// body once loaded.
    fn task(&self, r: &TaskRef) -> Option<Task> {
        match r {
            TaskRef::Local(_) | TaskRef::Code { .. } => {
                self.tasks.iter().find(|t| &t.r#ref == r).cloned()
            }
            TaskRef::GitHub { number, .. } => {
                let gh = self.issues.view();
                let mut task = gh.tasks.into_iter().find(|t| &t.r#ref == r)?;
                if let Some(Ok(detail)) = gh.details.get(number) {
                    task.body = Some(detail.body.clone());
                }
                Some(task)
            }
        }
    }
}

/// What `title`, `body` and `labels` typed in a form become in a file:
/// one-line header values, a body ending its last line.
fn file_fields(title: &str, body: &str, labels: &[String]) -> (String, String, Vec<String>) {
    let line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut body = body.to_owned();
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    let labels = labels
        .iter()
        .flat_map(|l| l.split(','))
        .map(line)
        .filter(|l| !l.is_empty())
        .collect();
    (line(title), body, labels)
}

/// Whether `a` and `b` differ in more than the clock.
fn same_view(a: &View, b: &View) -> bool {
    a.now / MINUTE == b.now / MINUTE
        && View {
            now: b.now,
            ..a.clone()
        } == *b
}

impl<C: Connect + Send + 'static> Feed for LiveFeed<C> {
    fn workspace_path(&self) -> Option<PathBuf> {
        Some(self.workspace_file.clone())
    }
    fn poll(&mut self) -> Option<View> {
        while let Ok(up) = self.ups.try_recv() {
            match up {
                Up::Seen {
                    tasks,
                    runs,
                    related,
                } => {
                    self.tasks = tasks;
                    self.runs = runs;
                    if related != self.related {
                        self.related = related;
                        self.related_new = true;
                    }
                }
                Up::Notice(notice) => self.notice = Some(notice),
                Up::Timeline(states) => {
                    if states != self.timeline {
                        self.timeline = states;
                        self.timeline_new = true;
                    }
                }
            }
        }
        if let Some(rx) = &self.installed
            && let Ok(found) = rx.try_recv()
        {
            self.installed = None;
            self.agents = Some(match found {
                Installed::Found(kinds) => kinds,
                // Offered all, as verify does: the shell may still find them.
                Installed::Unknown(why) => {
                    self.notice = Some(format!(
                        "インストール済みのエージェントを確認できません（{why}）"
                    ));
                    AgentKind::ALL.to_vec()
                }
            });
        }
        let view = View {
            repo: self.repo.clone(),
            tasks: self.tasks.clone(),
            runs: self.runs.clone(),
            github: self.issues.view(),
            agents: self.agents.clone(),
            notice: self.notice.clone(),
            now: (self.clock)(),
        };
        let related_new = std::mem::replace(&mut self.related_new, false)
            | std::mem::replace(&mut self.timeline_new, false);
        if !related_new
            && self
                .shown
                .as_ref()
                .is_some_and(|shown| same_view(shown, &view))
        {
            return None;
        }
        self.shown = Some(view.clone());
        Some(view)
    }

    fn timeline(&self) -> Option<&BTreeMap<RunId, Status>> {
        Some(&self.timeline)
    }

    fn related(&self) -> Option<&BTreeMap<TaskRef, Related>> {
        Some(&self.related)
    }

    fn waker(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        Some(self.wake.clone())
    }

    fn act(&mut self, intent: Intent) {
        match intent {
            Intent::Create {
                title,
                body,
                labels,
            } => {
                let (title, body, labels) = file_fields(&title, &body, &labels);
                self.send(Job::Create(LocalFile {
                    title,
                    state: State::Open,
                    labels,
                    extra: Vec::new(),
                    body,
                }));
            }
            Intent::Edit {
                id,
                title,
                body,
                labels,
            } => {
                let (title, body, labels) = file_fields(&title, &body, &labels);
                self.send(Job::Edit {
                    id,
                    title,
                    body,
                    labels,
                });
            }
            Intent::SetState { id, state } => self.send(Job::SetState { id, state }),
            Intent::Run {
                task,
                agent,
                isolation,
                auto,
            } => match self.task(&task) {
                Some(task) => {
                    let req = RunRequest {
                        task,
                        repo: self.repo.clone(),
                        agent,
                        isolation,
                        auto,
                    };
                    self.send(Job::Run(Box::new(req)));
                }
                None => self.say("そのタスクが見つかりません"),
            },
            Intent::OpenPane(run) => self.send(Job::OpenPane(run)),
            Intent::ClosePane(run) => self.send(Job::ClosePane(run)),
            Intent::OpenWorktree(run) => self.send(Job::OpenWorktree(run)),
            Intent::OpenUrl(url) => self.send(Job::Open(url)),
            Intent::Refresh => {
                self.issues.refresh();
                self.send(Job::ScanCode);
            }
            Intent::LoadDetail(number) => self.issues.load_detail(number),
            Intent::OpenSession { kind, session } => self.send(Job::OpenSession { kind, session }),
            Intent::CheckVersion {
                task,
                kind,
                run,
                commit,
            } => {
                self.send(Job::CheckVersion {
                    task,
                    kind,
                    run,
                    commit,
                });
            }
            Intent::OpenWorkflow(task) => match self.task(&task) {
                Some(task) if task.body.is_some() => self.send(Job::OpenWorkflow(task)),
                Some(_) => self.say("タスク本文の読み込み後にもう一度開いてください"),
                None => self.say("そのタスクが見つかりません"),
            },
            Intent::OpenWorkflowRun { task, run } => self.send(Job::OpenWorkflowRun { task, run }),
            Intent::AddLink { task, input } => {
                let gh = self.issues.view();
                match related::parse_pr(&input, gh.repo.as_deref()) {
                    Ok(url) => self.send(Job::AddLink { task, url }),
                    Err(why) => self.say(why),
                }
            }
            Intent::RemoveLink { task, id } => self.send(Job::RemoveLink { task, id }),
            Intent::Share { run, outcome } => {
                let title = self
                    .runs
                    .iter()
                    .find(|v| v.run.run == run)
                    .and_then(|v| self.task(&v.run.task))
                    .map(|task| task.title);
                self.send(Job::Share {
                    run,
                    title,
                    outcome,
                });
            }
        }
    }
}

/// The worker thread's state: everything that reads files or talks to Noa.
struct Worker<C: Connect> {
    repo: PathBuf,
    /// The data root (`<data dir>/noa/tasks`): sessions are beside it.
    root: PathBuf,
    log: PathBuf,
    /// The link log of this repository's key dir, and its events as last
    /// read.
    links: PathBuf,
    link_scan: LinkScan,
    events: Vec<LinkEvent>,
    /// `events` folded over `runs`, again when either changed
    /// (`fold_stale`).
    related: BTreeMap<TaskRef, Related>,
    fold_stale: bool,
    workflows: BTreeMap<String, Vec<workflow_links::ResultLink>>,
    workflow_at: Option<Millis>,
    /// When the change poll last ran.
    change_at: Option<Millis>,
    /// この作業ツリー runs seen live, and those looked at once more since
    /// they ended.
    here_live: HashSet<RunId>,
    here_done: HashSet<RunId>,
    /// Why the change poll last failed, said once until it changes.
    link_failure: Option<String>,
    launcher: Box<dyn Launcher + Send>,
    scan: Scan,
    connect: C,
    conn: Option<C::Conn>,
    /// The board tab the repository's runs float on, as learned on `conn`.
    board: Option<u64>,
    runs: Vec<Run>,
    tasks: Vec<Task>,
    /// The last `noa.listPanels` on `conn` (empty once it drops: a pane id
    /// means nothing to another Noa), and what runs make of the last one
    /// seen (kept on screen over a lost link).
    panes: Vec<PaneView>,
    facts: Vec<PaneFact>,
    /// Each run's result file as last seen, and its report.
    results: HashMap<RunId, (ResultFact, Option<Report>)>,
    panes_at: Option<Millis>,
    results_at: Option<Millis>,
    /// The code's markers (`code::scan`), read every [`CODE_EVERY`].
    code: Vec<Task>,
    code_at: Option<Millis>,
    /// Why the last scan failed, said once until it changes.
    code_failure: Option<String>,
    /// The title in each marker run's brief, read once: a marker its run
    /// removed is listed by it until the run is closed.
    brief_titles: HashMap<RunId, String>,
    /// The run log's (length, mtime) when last read: another window on the
    /// repository appends to it too.
    runs_seen: Option<(u64, std::time::SystemTime)>,
    opener: Box<dyn FnMut(&str) + Send>,
    clock: Arc<dyn Fn() -> Millis + Send + Sync>,
    wake: Arc<dyn Fn() + Send + Sync>,
    up: Sender<Up>,
    sent: Option<Seen>,
    /// The Board Timeline, once [`Job::Timeline`] gave it.
    timeline: Option<Timeline>,
}

/// What the worker last sent.
type Seen = (Vec<Task>, Vec<RunView>, BTreeMap<TaskRef, Related>);

/// The `TaskKey` the link log names `task` by.
fn task_key(task: &TaskRef) -> TaskKey {
    TaskKey(serde_json::to_value(task).expect("a task ref is JSON"))
}

/// The commit each run's head was last recorded at in `events`, as
/// `related::fold` takes the head: its last `ChangeSeen`, or a commit one of
/// its sessions named first (a new version of the run's task), else its
/// first `ChangeBase`.
fn recorded_heads<'a>(runs: &[Run], events: &'a [LinkEvent]) -> HashMap<&'a str, &'a str> {
    let task_of: HashMap<&str, &TaskRef> = runs.iter().map(|r| (r.run.as_str(), &r.task)).collect();
    let mut bases: HashMap<&str, &str> = HashMap::new();
    for event in events {
        if let LinkEvent::ChangeBase { run, commit, .. } = event {
            bases.entry(run.as_str()).or_insert(commit.as_str());
        }
    }
    let mut heads: HashMap<&str, &str> = bases.clone();
    let mut named: HashSet<(&TaskRef, &str)> = HashSet::new();
    let mut sessions: HashSet<(SessionKind, &str)> = HashSet::new();
    for event in events {
        match event {
            LinkEvent::ChangeSeen { run, commit, .. } => {
                heads.insert(run.as_str(), commit.as_str());
                if let Some(task) = task_of.get(run.as_str())
                    && bases.get(run.as_str()) != Some(&commit.as_str())
                {
                    named.insert((task, commit.as_str()));
                }
            }
            LinkEvent::SessionLinked {
                kind,
                session,
                run,
                commit: Some(commit),
                ..
            } => {
                if !sessions.insert((*kind, session.as_str())) {
                    continue;
                }
                let Some(task) = task_of.get(run.as_str()) else {
                    continue;
                };
                if bases.get(run.as_str()) != Some(&commit.as_str())
                    && named.insert((task, commit.as_str()))
                {
                    heads.insert(run.as_str(), commit.as_str());
                }
            }
            _ => {}
        }
    }
    heads
}

/// The head of every `task-run-*` branch of `repo`, by branch name: one
/// `git for-each-ref`, read from the repository (not a worktree), so a
/// removed worktree's branch is still read.
fn run_branch_heads(repo: &Path) -> Result<HashMap<String, String>, String> {
    let out = start::git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:lstrip=2)%00%(objectname)",
            "refs/heads/task-run-*",
        ],
    )?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|line| line.split_once('\0'))
        .map(|(name, sha)| (name.to_owned(), sha.trim().to_owned()))
        .collect())
}

/// The commit `branch` of `repo` was created at: the oldest entry of its
/// reflog ("branch: Created from HEAD"), if git kept one.
fn branch_origin(repo: &Path, branch: &str) -> Option<String> {
    let name = format!("refs/heads/{branch}");
    let out = start::git(repo, &["reflog", "show", "--format=%H", &name, "--"]).ok()?;
    let oldest = String::from_utf8_lossy(&out)
        .lines()
        .last()?
        .trim()
        .to_owned();
    (!oldest.is_empty()).then_some(oldest)
}

/// Whether `last` is at least `POLL_MS` before `now` (or never).
fn due(last: Option<Millis>, now: Millis) -> bool {
    last.is_none_or(|last| now >= last.saturating_add(POLL_MS))
}

impl<C: Connect> Worker<C> {
    fn run(mut self, jobs: Receiver<Job>) {
        self.reload_runs();
        self.name_repo();
        loop {
            self.look();
            self.publish();
            match jobs.recv_timeout(TICK) {
                Ok(job) => self.handle(job),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn say(&self, notice: impl Into<String>) {
        let _ = self.up.send(Up::Notice(notice.into()));
        (self.wake)();
    }

    fn reload_runs(&mut self) {
        match runs::replay(&self.log) {
            Ok(runs) => self.runs = runs,
            Err(e) => self.say(format!("実行の記録を読めません: {e}")),
        }
        self.fold_stale = true;
    }

    /// `Repo` in the link log, unless its last `Repo` names this
    /// repository already (L1-1).
    fn name_repo(&mut self) {
        let named = task_links::read(&self.links).map(|events| {
            events.iter().rev().find_map(|event| match event {
                LinkEvent::Repo { path, .. } => Some(path.clone()),
                _ => None,
            })
        });
        let written = match named {
            Ok(Some(path)) if path == self.repo => return,
            Ok(_) => self.append_link(&LinkEvent::Repo {
                path: self.repo.clone(),
                at: (self.clock)(),
            }),
            Err(e) => Err(e.to_string()),
        };
        if let Err(why) = written {
            self.say(format!("関連の記録に書けません: {why}"));
        }
    }

    /// Appends to the link log and reads it again at the next look.
    fn append_link(&mut self, event: &LinkEvent) -> Result<(), String> {
        let written = task_links::append(&self.links, event).map_err(|e| e.to_string());
        self.link_scan = LinkScan::new(self.links.clone());
        written
    }

    /// The change poll (L1-3): each run's head, recorded as `ChangeSeen`
    /// when it differs from the commit last recorded for it. Runs on a
    /// branch are read from their `task-run-…` ref; この作業ツリー runs
    /// from the repository's `HEAD` while live and once after they end.
    fn poll_changes(&mut self, now: Millis) {
        let events = match task_links::read(&self.links) {
            Ok(events) => events,
            Err(e) => return self.change_failed(format!("関連の記録を読めません: {e}")),
        };
        let recorded: HashMap<String, String> = recorded_heads(&self.runs, &events)
            .into_iter()
            .map(|(run, commit)| (run.to_owned(), commit.to_owned()))
            .collect();
        let based: HashSet<String> = events
            .iter()
            .filter_map(|event| match event {
                LinkEvent::ChangeBase { run, .. } => Some(run.clone()),
                _ => None,
            })
            .collect();
        // Each run's head, with its branch when it has one.
        let mut heads: Vec<(RunId, Option<String>, Result<String, String>)> = Vec::new();
        let mut failure = None;
        if self.runs.iter().any(|r| r.branch.is_some()) {
            match run_branch_heads(&self.repo) {
                Ok(branches) => {
                    for run in &self.runs {
                        // A deleted branch adds nothing; its versions stay.
                        if let Some((branch, sha)) = run
                            .branch
                            .as_ref()
                            .and_then(|b| Some((b, branches.get(b)?)))
                        {
                            heads.push((run.run.clone(), Some(branch.clone()), Ok(sha.clone())));
                        }
                    }
                }
                Err(why) => failure = Some(format!("変更を確かめられません: {why}")),
            }
        }
        for view in self.views(now) {
            let run = view.run;
            if run.branch.is_some() {
                continue;
            }
            let look = match view.liveness {
                Liveness::Starting | Liveness::Alive { .. } => {
                    self.here_live.insert(run.run.clone());
                    true
                }
                // Marked done below only once read and written.
                Liveness::Gone => {
                    self.here_live.contains(&run.run) && !self.here_done.contains(&run.run)
                }
            };
            if look {
                heads.push((run.run.clone(), None, start::head_of(&run.workdir)));
            }
        }
        let ended: HashSet<RunId> = self
            .views(now)
            .into_iter()
            .filter(|view| view.run.branch.is_none() && view.liveness == Liveness::Gone)
            .map(|view| view.run.run)
            .collect();
        for (run, branch, head) in heads {
            let last_read = ended.contains(&run);
            let commit = match head {
                Ok(commit) => commit,
                Err(why) => {
                    failure = Some(format!("変更を確かめられません: {why}"));
                    continue;
                }
            };
            if commit.is_empty() {
                if last_read {
                    self.here_done.insert(run);
                }
                continue;
            }
            let mut last = recorded.get(run.as_str()).cloned();
            if !based.contains(run.as_str()) {
                // A run started before the link log (or whose base could
                // not be written): its base is where its branch was created,
                // else the head seen now, never a version.
                let base = branch
                    .as_deref()
                    .and_then(|b| branch_origin(&self.repo, b))
                    .unwrap_or_else(|| commit.clone());
                let event = LinkEvent::ChangeBase {
                    run: run.to_string(),
                    commit: base.clone(),
                    at: now,
                };
                if let Err(why) = self.append_link(&event) {
                    failure = Some(format!("関連の記録に書けません: {why}"));
                    continue;
                }
                last.get_or_insert(base);
            }
            if last.as_ref() != Some(&commit) {
                let seen = LinkEvent::ChangeSeen {
                    run: run.to_string(),
                    commit,
                    at: now,
                };
                if let Err(why) = self.append_link(&seen) {
                    failure = Some(format!("関連の記録に書けません: {why}"));
                    continue;
                }
            }
            if last_read {
                self.here_done.insert(run);
            }
        }
        match failure {
            Some(why) => self.change_failed(why),
            None => self.link_failure = None,
        }
    }

    fn change_failed(&mut self, why: String) {
        if self.link_failure.as_ref() != Some(&why) {
            self.say(why.clone());
            self.link_failure = Some(why);
        }
    }

    /// The task's links as the log says now (read again: another window
    /// may have written since the last look).
    fn related_now(&mut self, task: &TaskRef) -> Result<(Vec<LinkEvent>, Related), String> {
        let events =
            task_links::read(&self.links).map_err(|e| format!("関連の記録を読めません: {e}"))?;
        let related = related::fold(&self.runs, &events)
            .remove(task)
            .unwrap_or_default();
        Ok((events, related))
    }

    fn add_link(&mut self, task: &TaskRef, url: &str) -> Result<String, String> {
        let (events, related) = self.related_now(task)?;
        if related.prs.iter().any(|pr| pr.url == url) {
            return Err("この PR はもう関連づいています".to_owned());
        }
        let now = (self.clock)();
        self.append_link(&LinkEvent::PrLinked {
            id: task_links::new_pr_id(now, &events),
            task: task_key(task),
            url: url.to_owned(),
            at: now,
        })
        .map_err(|why| format!("関連の記録に書けません: {why}"))?;
        Ok(format!("{url} を関連づけました"))
    }

    fn remove_link(&mut self, task: &TaskRef, id: &str) -> Result<String, String> {
        let (_, related) = self.related_now(task)?;
        let linked = related.prs.iter().any(|pr| pr.id == id)
            || related
                .reviews
                .iter()
                .chain(&related.verifications)
                .any(|c| c.session == id);
        if !linked {
            return Err(format!("{id} はこのタスクに関連づいていません"));
        }
        self.append_link(&LinkEvent::LinkRemoved {
            task: task_key(task),
            id: id.to_owned(),
            at: (self.clock)(),
        })
        .map_err(|why| format!("関連の記録に書けません: {why}"))?;
        Ok(format!("{id} の関連を外しました"))
    }

    /// Review's or Verify's own window on a stored session
    /// (`--session=<its folder>`).
    fn open_session(&mut self, kind: SessionKind, session: &str) -> Result<String, String> {
        let store = task_links::session_store(&self.root, kind, session)
            .ok_or_else(|| format!("{session} の記録が見つかりません"))?;
        let helper = match kind {
            SessionKind::Review => sibling::REVIEW,
            SessionKind::Verify => sibling::VERIFY,
        };
        let mut arg = OsString::from("--session=");
        arg.push(&store);
        self.launcher.launch(helper, vec![arg])?;
        Ok(format!("{session} を開きました"))
    }

    fn check_version(
        &mut self,
        task: &TaskRef,
        kind: SessionKind,
        run: &RunId,
        commit: &str,
    ) -> Result<String, String> {
        let (events, related) = self.related_now(task)?;
        if !related
            .current()
            .is_some_and(|version| &version.run == run && version.commit == commit)
        {
            return Err("表示後に変更の版が変わりました。最新の関連成果を確認してください".into());
        }
        let base = events
            .iter()
            .find_map(|event| match event {
                LinkEvent::ChangeBase {
                    run: id, commit, ..
                } if id == run.as_str() => Some(commit.as_str()),
                _ => None,
            })
            .ok_or("着手時点のコミットが不明です。Review / Verify で対象を指定してください")?;
        for sha in [base, commit] {
            if !matches!(sha.len(), 40 | 64) || !sha.bytes().all(|c| c.is_ascii_hexdigit()) {
                return Err("記録されたコミット ID が不正です".into());
            }
            start::git(
                &self.repo,
                &["cat-file", "-e", &format!("{sha}^{{commit}}")],
            )
            .map_err(|_| {
                format!("コミット {sha} が見つかりません。リポジトリを確認してください")
            })?;
        }
        let select = task_links::select_arg(&task_key(task)).ok_or("元のタスクを特定できません")?;
        let mut repo = OsString::from("--repo=");
        repo.push(&self.repo);
        let mut origin = OsString::from("--origin-repo=");
        origin.push(&self.repo);
        let helper = match kind {
            SessionKind::Review => sibling::REVIEW,
            SessionKind::Verify => sibling::VERIFY,
        };
        self.launcher.launch(
            helper,
            vec![
                repo,
                format!("--base={base}").into(),
                format!("--revision={commit}").into(),
                origin,
                format!("--origin-task={select}").into(),
                format!("--origin-run={run}").into(),
            ],
        )?;
        Ok(format!(
            "{} の開始フォームを開きました。対象の版を確認して開始してください",
            helper.title
        ))
    }

    fn open_workflow(&mut self, task: &Task) -> Result<String, String> {
        let request = workflow_links::handoff(&self.root, &self.repo, task, (self.clock)())?;
        let mut repo = OsString::from("--repo=");
        repo.push(&self.repo);
        let mut origin = OsString::from("--task-origin=");
        origin.push(request);
        self.launcher.launch(WORKFLOW_HELPER, vec![repo, origin])?;
        Ok("Workflow の開始フォームを開きました。作業指示を確認して開始してください".into())
    }

    fn open_workflow_run(&mut self, task: &TaskRef, run: &str) -> Result<String, String> {
        let results = workflow_links::scan(&self.root, &self.repo);
        let select = workflow_links::select(task).ok_or("元のタスクを特定できません")?;
        let expected = serde_json::to_string(task).map_err(|e| e.to_string())?;
        if !results.get(&select).is_some_and(|runs| {
            runs.iter()
                .any(|r| r.run_id == run && r.task_key == expected)
        }) {
            return Err("このタスクの Workflow 実行が見つかりません".into());
        }
        let mut repo = OsString::from("--repo=");
        repo.push(&self.repo);
        self.launcher
            .launch(WORKFLOW_HELPER, vec![repo, format!("--run={run}").into()])?;
        Ok(format!("Workflow {run} を開きました"))
    }

    /// The link to Noa, connecting when there is none.
    fn conn(&mut self) -> Result<&mut C::Conn, String> {
        if self.conn.is_none() {
            self.conn = Some(self.connect.connect()?);
        }
        Ok(self.conn.as_mut().expect("just connected"))
    }

    /// Drops a connection that is gone, so the next call reconnects, and
    /// the board and pane ids learned on it: Noa mints them from 1 again
    /// when it restarts, so the same id may be another tab there.
    fn check_link<T>(&mut self, result: Result<T, NoaError>) -> Result<T, NoaError> {
        if matches!(result, Err(NoaError::Link(_))) {
            self.conn = None;
            self.board = None;
            self.panes.clear();
        }
        result
    }

    /// Looks at what is due: the task files, the code's markers, the
    /// panes, the result files.
    fn look(&mut self) {
        let now = (self.clock)();
        if let Some(tasks) = self.scan.poll(now) {
            self.tasks = tasks;
        }
        if self
            .code_at
            .is_none_or(|at| now.saturating_sub(at) >= CODE_EVERY)
        {
            self.code_at = Some(now);
            match code::scan(&self.repo) {
                Ok(code) => {
                    self.code = code;
                    self.code_failure = None;
                }
                // The markers last read stay.
                Err(why) => {
                    if self.code_failure.as_ref() != Some(&why) {
                        self.say(format!("コードのマーカーを読めません: {why}"));
                        self.code_failure = Some(why);
                    }
                }
            }
        }
        if due(self.panes_at, now) {
            self.panes_at = Some(now);
            self.list_panes();
        }
        if self
            .change_at
            .is_none_or(|at| now.saturating_sub(at) >= CHANGE_POLL_MS)
        {
            self.change_at = Some(now);
            self.poll_changes(now);
        }
        if let Some(events) = self.link_scan.poll(now) {
            self.events = events;
            self.fold_stale = true;
        }
        if due(self.results_at, now) {
            self.results_at = Some(now);
            // Runs another window started (or closed) show here too.
            let stamp = std::fs::metadata(&self.log)
                .ok()
                .and_then(|m| Some((m.len(), m.modified().ok()?)));
            if stamp != self.runs_seen {
                self.runs_seen = stamp;
                self.reload_runs();
            }
            for run in &self.runs {
                let fact = runs::result_fact(&runs::result_file(&run.workdir, &run.run));
                let known = self.results.get(&run.run);
                if known.is_some_and(|(seen, _)| *seen == fact) {
                    continue;
                }
                let report = match fact {
                    ResultFact::Written { .. } => {
                        runs::read_report(&runs::result_file(&run.workdir, &run.run))
                    }
                    ResultFact::Missing => None,
                };
                self.results.insert(run.run.clone(), (fact, report));
            }
        }
        self.note_here_live(now);
    }

    /// Notes the この作業ツリー runs live now, at every look and not only at
    /// the change poll, so one that ends between two polls still gets its
    /// last head read once (L1-3).
    fn note_here_live(&mut self, now: Millis) {
        let live: Vec<RunId> = self
            .views(now)
            .into_iter()
            .filter(|view| {
                view.run.branch.is_none()
                    && matches!(view.liveness, Liveness::Starting | Liveness::Alive { .. })
            })
            .map(|view| view.run.run)
            .collect();
        self.here_live.extend(live);
    }

    /// `noa.listPanels`; a failure keeps the panes last seen, so a lost
    /// link does not end the runs on screen.
    fn list_panes(&mut self) {
        let listed = match self.conn() {
            Ok(conn) => conn.panes(),
            Err(_) => return,
        };
        if let Ok(panes) = self.check_link(listed) {
            self.see_panes(panes);
        }
    }

    /// `noa.listPanels` now, on the current link or, when that one is gone
    /// (Noa restarted), once more on a new one; `Err` when Noa cannot be
    /// reached.
    fn refresh_panes(&mut self) -> Result<(), String> {
        let mut retried = false;
        loop {
            let listed = self.conn()?.panes();
            match self.check_link(listed) {
                Ok(panes) => {
                    self.see_panes(panes);
                    return Ok(());
                }
                // `check_link` dropped it: the next `conn` reconnects.
                Err(NoaError::Link(_)) if !retried => retried = true,
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    /// What `panes` says of the runs, and the board their live floats are
    /// on (or the one kept: learned on this same link).
    fn see_panes(&mut self, panes: Vec<PaneView>) {
        self.facts = panes.iter().map(ipc::pane_fact).collect();
        self.panes = panes;
        self.board = start::board_of(&self.panes, &self.runs).or(self.board);
    }

    fn views(&self, now: Millis) -> Vec<RunView> {
        self.runs
            .iter()
            .map(|run| {
                let (fact, report) = self
                    .results
                    .get(&run.run)
                    .cloned()
                    .unwrap_or((ResultFact::Missing, None));
                runs::view(run, &self.facts, fact, report, now)
            })
            .collect()
    }

    /// The marker tasks no longer in the code (a run in this work tree
    /// removed its marker, as the brief says) whose run is not closed yet:
    /// listed until it is, so the person sees its report in 確認待ち.
    fn markers_gone(&mut self) -> Vec<Task> {
        let mut gone: Vec<Task> = Vec::new();
        for run in &self.runs {
            if run.closed.is_some()
                || !matches!(run.task, TaskRef::Code { .. })
                || self.code.iter().any(|t| t.r#ref == run.task)
                || gone.iter().any(|t| t.r#ref == run.task)
            {
                continue;
            }
            let title = self
                .brief_titles
                .entry(run.run.clone())
                .or_insert_with(|| {
                    // The brief is in the agent's work tree: read as the
                    // other files there are (no link followed, no blocking
                    // open, a regular file, a bounded read).
                    tasks::fsx::open_read(&run.workdir, &runs::task_file(&run.workdir, &run.run))
                        .and_then(|file| tasks::fsx::read_capped(file, 4096))
                        .ok()
                        .and_then(|(bytes, _)| {
                            String::from_utf8_lossy(&bytes)
                                .lines()
                                .next()
                                .and_then(|l| l.strip_prefix("# "))
                                .map(str::to_owned)
                        })
                        .unwrap_or_else(|| run.task.badge())
                })
                .clone();
            gone.push(Task {
                r#ref: run.task.clone(),
                title,
                body: Some(
                    "（マーカーはコードから消えています。実行を閉じると一覧から消えます）".into(),
                ),
                state: State::Open,
                labels: Vec::new(),
                updated: run.started,
                url: None,
                closed: None,
                problem: None,
            });
        }
        gone
    }

    /// Sends what is seen when it changed.
    fn publish(&mut self) {
        let now = (self.clock)();
        if due(self.workflow_at, now) {
            self.workflow_at = Some(now);
            let workflows = workflow_links::scan(&self.root, &self.repo);
            if workflows != self.workflows {
                self.workflows = workflows;
                self.fold_stale = true;
            }
        }
        let gone = self.markers_gone();
        let tasks = self
            .tasks
            .iter()
            .chain(&self.code)
            .chain(&gone)
            .cloned()
            .collect();
        if std::mem::replace(&mut self.fold_stale, false) {
            self.related = related::fold(&self.runs, &self.events);
            for result in self.workflows.values().flatten() {
                if let Ok(task) = serde_json::from_str::<TaskRef>(&result.task_key) {
                    self.related
                        .entry(task)
                        .or_default()
                        .workflows
                        .push(result.clone());
                }
            }
        }
        let seen = (tasks, self.views((self.clock)()), self.related.clone());
        if self.sent.as_ref() == Some(&seen) {
            return;
        }
        let _ = self.up.send(Up::Seen {
            tasks: seen.0.clone(),
            runs: seen.1.clone(),
            related: seen.2.clone(),
        });
        self.sent = Some(seen);
        (self.wake)();
    }

    /// Looks again at once: after a write or a pane change.
    fn look_now(&mut self) {
        self.scan = Scan::new(self.repo.clone());
        self.panes_at = None;
        self.results_at = None;
        self.workflow_at = None;
    }

    fn handle(&mut self, job: Job) {
        let outcome = match job {
            Job::ScanCode => {
                self.code_at = None;
                self.change_at = None;
                return;
            }
            Job::Create(file) => self.create(file),
            Job::Edit {
                id,
                title,
                body,
                labels,
            } => self.edit(&id, title, body, labels),
            Job::SetState { id, state } => local::set_state(&self.repo, &id, state)
                .map(|()| match state {
                    State::Closed => format!("{id} を完了にしました"),
                    State::Open => format!("{id} を再開しました"),
                })
                .map_err(|e| format!("{id} を書き換えられません: {e}")),
            Job::Run(req) => self.start(&req),
            Job::OpenPane(run) => self.open_pane(&run),
            Job::ClosePane(run) => self.close_pane(&run),
            Job::OpenWorktree(run) => match self.runs.iter().find(|r| r.run == run) {
                Some(found) if !found.workdir.exists() => Err(format!(
                    "{} はもうありません（worktree は削除済み）",
                    found.workdir.display()
                )),
                Some(found) => {
                    let dir = found.workdir.display().to_string();
                    (self.opener)(&dir);
                    Ok(format!("{dir} を開きました"))
                }
                None => Err(format!("{run} の記録がありません")),
            },
            Job::Open(url) => {
                (self.opener)(&url);
                Ok(format!("{url} を開きました"))
            }
            Job::OpenSession { kind, session } => self.open_session(kind, &session),
            Job::CheckVersion {
                task,
                kind,
                run,
                commit,
            } => self.check_version(&task, kind, &run, &commit),
            Job::OpenWorkflow(task) => self.open_workflow(&task),
            Job::OpenWorkflowRun { task, run } => self.open_workflow_run(&task, &run),
            Job::AddLink { task, url } => self.add_link(&task, &url),
            Job::RemoveLink { task, id } => self.remove_link(&task, &id),
            Job::Timeline(link) => {
                self.use_timeline(*link);
                return;
            }
            Job::Share {
                run,
                title,
                outcome,
            } => self.share(&run, title.as_deref(), outcome),
        };
        // What the intent changed is shown before what is said about it.
        self.look_now();
        self.look();
        self.publish();
        match outcome {
            Ok(notice) | Err(notice) => self.say(notice),
        }
    }

    fn create(&mut self, file: LocalFile) -> Result<String, String> {
        if file.title.is_empty() {
            return Err("タイトルが空です".to_owned());
        }
        store::prepare_workdir(&self.repo).map_err(|e| e.to_string())?;
        let now = (self.clock)();
        local::create(&self.repo, &file, now)
            .map(|id| format!("{id} を作りました"))
            .map_err(|e| format!("タスクを作れません: {e}"))
    }

    fn edit(
        &mut self,
        id: &str,
        title: String,
        body: String,
        labels: Vec<String>,
    ) -> Result<String, String> {
        if title.is_empty() {
            return Err("タイトルが空です".to_owned());
        }
        let text = local::read(&self.repo, id).map_err(|e| format!("{id} を読めません: {e}"))?;
        let old = local::parse(&text).map_err(|why| format!("{id} を読めません: {why}"))?;
        let file = LocalFile {
            title,
            body,
            labels,
            ..old
        };
        local::write(&self.repo, id, &file)
            .map(|()| format!("{id} を保存しました"))
            .map_err(|e| format!("{id} を書けません: {e}"))
    }

    fn start(&mut self, req: &RunRequest) -> Result<String, String> {
        // Another window on this repository may be starting the same task:
        // from reading the log to logging RunStarted is one step for all
        // of them. Held until this run is logged and spawned.
        let _lock = runs::lock(&self.log).map_err(|e| format!("実行の記録を開けません: {e}"))?;
        // What runs and floats there are now, on a link that answers,
        // before a work tree or a log line is made: the panes last seen
        // may be from before a failed connect or a Noa since restarted.
        self.reload_runs();
        self.refresh_panes()
            .map_err(|why| format!("Noa に接続できないため起動できません: {why}"))?;
        let now = (self.clock)();
        let last = self
            .views(now)
            .into_iter()
            .filter(|v| v.run.task == req.task.r#ref)
            .max_by_key(|v| v.run.started);
        runs::may_start(&req.task, last.as_ref())?;
        let log = self.log.clone();
        let mut board = self.board;
        let conn = self
            .conn()
            .map_err(|why| format!("Noa に接続できないため起動できません: {why}"))?;
        let started = start::start(conn, &mut board, &log, req, now);
        self.board = board;
        self.reload_runs();
        if let Ok((run, _)) = &started {
            self.register_on_timeline(run);
        }
        // Its base is in the link log now.
        self.link_scan = LinkScan::new(self.links.clone());
        started.map(|(run, base)| {
            let started = format!(
                "{} を {} で始めました（{run}）",
                req.task.title,
                req.agent.command()
            );
            match base {
                Some(why) => format!("{started}。着手時点を記録できません: {why}"),
                None => started,
            }
        })
    }

    /// Takes up the Board Timeline: where the runs shared before stand,
    /// from their outboxes, and a share not on the timeline yet (saved
    /// before an exit, or its answer lost) is sent again at once.
    fn use_timeline(&mut self, link: TimelineLink) {
        let mut tl = Timeline {
            session: timeline::session_key(&store::repo_key(&self.repo)),
            link,
            api: None,
            states: BTreeMap::new(),
            asked: HashMap::new(),
        };
        for run in &self.runs {
            // A run id that cannot name a folder under the root (a log
            // edited by hand) has no outbox: nothing is looked at for it.
            let Some(dir) =
                timeline::outbox_dir(&tl.link.outbox_root, &tl.session, run.run.as_str())
            else {
                continue;
            };
            if !dir.is_dir() {
                continue;
            }
            if let Some(state) = tl.publisher(run, false).ok().and_then(|p| p.shared()) {
                tl.states.insert(run.run.clone(), state);
            }
        }
        let unsent: Vec<RunId> = tl
            .states
            .iter()
            .filter(|(_, state)| state.unsent())
            .map(|(run, _)| run.clone())
            .collect();
        let _ = self.up.send(Up::Timeline(tl.states.clone()));
        (self.wake)();
        self.timeline = Some(tl);
        for run in unsent {
            // As 再試行 would: the share as it was saved.
            let _ = self.share(&run, None, None);
        }
    }

    /// Registers a run just started on the timeline, its float as the
    /// worker on its board: the result is shared later, by the person. A
    /// failure is retried by the share.
    fn register_on_timeline(&mut self, run: &RunId) {
        let Some(tl) = &mut self.timeline else {
            return;
        };
        let Some(found) = self.runs.iter().find(|r| &r.run == run) else {
            return;
        };
        if found.pane.is_none() {
            return;
        }
        let registered = tl.publisher(found, true).and_then(|mut publisher| {
            let api = tl.api()?;
            Ok(publisher.register(api.as_mut(), found.board))
        });
        if !matches!(registered, Ok(Status::Idle)) {
            tl.api = None;
        }
    }

    /// 共有: the run's result file as it is now, posted with `outcome`,
    /// saved in the run's outbox before anything is sent; `None` (再試行)
    /// sends what was saved, without reading the result file again. The
    /// run log and the result file are only read.
    fn share(
        &mut self,
        run: &RunId,
        title: Option<&str>,
        outcome: Option<Outcome>,
    ) -> Result<String, String> {
        let now = (self.clock)();
        let live = self
            .views(now)
            .iter()
            .any(|v| &v.run.run == run && matches!(v.liveness, Liveness::Alive { .. }));
        let Some(tl) = &mut self.timeline else {
            return Err("タイムラインに投稿できません（保存先がわかりません）".into());
        };
        let found = self
            .runs
            .iter()
            .find(|r| &r.run == run)
            .ok_or_else(|| format!("{run} の記録がありません"))?;
        let (up, wake) = (&self.up, &self.wake);
        let status = (|| -> Result<Status, String> {
            let mut publisher = tl.publisher(found, live)?;
            let saved = publisher
                .saved_share()
                .map_err(|e| format!("outbox: {e}"))?;
            let candidate = match saved {
                // Shared before: sent as it was shared, whatever the result
                // file says now (or if it is gone with its work tree).
                Some(candidate) => candidate,
                None => {
                    let outcome = outcome
                        .or_else(|| tl.asked.get(run).copied())
                        .unwrap_or(Outcome::Unknown);
                    tl.asked.insert(run.clone(), outcome);
                    let path = runs::result_file(&found.workdir, run);
                    let (text, longer) = timeline::read_result(&found.workdir, &path)
                        .map_err(|e| format!("{} を読めません: {e}", path.display()))?;
                    let candidate = timeline::candidate(
                        &tl.session,
                        &Share {
                            run,
                            task: &found.task,
                            title,
                            result_path: &path,
                            result: &text,
                            longer,
                            outcome,
                        },
                    );
                    publisher
                        .save_share(&candidate)
                        .map_err(|e| format!("outbox: {e}"))?;
                    candidate
                }
            };
            // Pending while it is on its way.
            if !matches!(publisher.shared(), Some(Status::Published { .. })) {
                tl.states.insert(run.clone(), Status::Pending);
                let _ = up.send(Up::Timeline(tl.states.clone()));
                wake();
            }
            // The board is named only while the run's float is on it: an
            // id from before a restart may be another pane now.
            let board = found.board.filter(|_| live);
            let api = tl.api()?;
            Ok(publisher.sync(api.as_mut(), board, &[candidate]))
        })()
        .unwrap_or_else(|message| Status::Failed { message });
        if status.failed() {
            tl.api = None;
        }
        let notice = match &status {
            Status::Published { seq, .. } => Ok(format!(
                "{run} の結果をタイムラインに投稿しました（#{seq}）"
            )),
            Status::Idle => Err("この Noa はタイムラインに対応していません".to_owned()),
            Status::Pending => Ok(format!("{run} の結果は投稿待ちです")),
            Status::Waiting { why } => Err(format!("{run} の結果は投稿待ちです: {why}")),
            Status::Failed { message } => Err(format!("タイムラインに投稿できません: {message}")),
        };
        if status == Status::Idle {
            tl.states.remove(run);
        } else {
            tl.states.insert(run.clone(), status);
        }
        let _ = self.up.send(Up::Timeline(tl.states.clone()));
        // Woken here, not only by the notice: a share sent again at start
        // has none, and its outcome would wait for the next periodic wake.
        (self.wake)();
        notice
    }

    /// The pane of `run`, by its label, in the last listing: also one
    /// whose agent ended (Noa keeps an exited float open), so 閉じる can
    /// close it and 開く show how it ended.
    fn pane_of(&self, run: &RunId) -> Option<u64> {
        let label = runs::label(run);
        self.panes
            .iter()
            .find(|p| p.label.as_deref() == Some(label.as_str()))
            .map(|p| p.pane_id)
    }

    /// The live pane of `run` in a listing taken now on a link that
    /// answers: the panes last seen may be from a Noa since restarted,
    /// where the same id is another tab, or from before a failed connect.
    fn listed_pane_of(&mut self, run: &RunId) -> Result<Option<u64>, String> {
        self.refresh_panes()
            .map_err(|why| format!("Noa に接続できません: {why}"))?;
        Ok(self.pane_of(run))
    }

    fn open_pane(&mut self, run: &RunId) -> Result<String, String> {
        let pane = self
            .listed_pane_of(run)?
            .ok_or_else(|| format!("{run} のペインが見つかりません"))?;
        let conn = self
            .conn()
            .map_err(|why| format!("Noa に接続できません: {why}"))?;
        let focused = conn.focus(pane);
        self.check_link(focused)
            .map(|()| format!("{run} のペインを開きました"))
            .map_err(|e| format!("{run} のペインを開けません: {e}"))
    }

    fn close_pane(&mut self, run: &RunId) -> Result<String, String> {
        if !self.runs.iter().any(|r| &r.run == run) {
            return Err(format!("{run} の記録がありません"));
        }
        let notice = match self.listed_pane_of(run)? {
            Some(pane) => {
                let conn = self
                    .conn()
                    .map_err(|why| format!("Noa に接続できません: {why}"))?;
                let closed = conn.close_pane(pane);
                match self.check_link(closed) {
                    Ok(()) | Err(NoaError::PaneGone) => format!("{run} のペインを閉じました"),
                    Err(e) => return Err(format!("{run} のペインを閉じられません: {e}")),
                }
            }
            None => {
                let now = (self.clock)();
                let starting = self
                    .views(now)
                    .iter()
                    .any(|v| &v.run.run == run && v.liveness == Liveness::Starting);
                // Its float may still come: closing it now would leave
                // the agent running with its card gone.
                if starting {
                    return Err(format!(
                        "{run} は起動中で、ペインがまだ見つかりません。少し待ってからもう一度閉じてください"
                    ));
                }
                format!("{run} のペインが見つかりません。実行を閉じました")
            }
        };
        let at = (self.clock)();
        runs::append(
            &self.log,
            &RunEvent::RunClosed {
                run: run.clone(),
                at,
            },
        )
        .map_err(|e| format!("実行の記録に書けません: {e}"))?;
        self.reload_runs();
        Ok(notice)
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A Noa held in memory, shared with the test that made it.

    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use super::ipc::{Connect, Noa, NoaError, PaneSpec, PaneView, Placement};

    #[derive(Debug, Default)]
    pub struct State {
        pub panes: Vec<PaneView>,
        /// Each spawn, with the run log's text at the moment it came.
        pub spawned: Vec<(Placement, PaneSpec, String)>,
        pub focused: Vec<u64>,
        pub closed: Vec<u64>,
        pub next: u64,
        pub no_board_tab: bool,
        /// Floats on these boards are refused as gone.
        pub gone_boards: Vec<u64>,
        pub connects: usize,
        /// `connect` fails (Noa not running).
        pub refuse_connects: bool,
        /// Connections from the first this many connects are dead (Noa
        /// restarted): every call on them is `NoaError::Link`.
        pub dead_before: usize,
        /// `panes` calls, dead connections' included.
        pub lists: usize,
    }

    /// A pane that is a plain tab of someone else's: no label, no parent.
    pub fn tab(pane_id: u64) -> PaneView {
        PaneView {
            parent_pane_id: None,
            label: None,
            ..float(pane_id, 0, "")
        }
    }

    /// A pane that is a float on `parent`, labelled.
    pub fn float(pane_id: u64, parent: u64, label: &str) -> PaneView {
        PaneView {
            pane_id,
            parent_pane_id: Some(parent),
            label: Some(label.to_owned()),
            agent_state: None,
            agent_seq: 0,
            agent_blocked: None,
            agent_kind: None,
            exited: false,
            agent_detail: None,
        }
    }

    #[derive(Clone, Default)]
    pub struct FakeNoa {
        pub state: Arc<Mutex<State>>,
        /// The run log a spawn snapshots.
        pub log: Option<PathBuf>,
        /// Which connect made this connection (0: the test's own handle).
        pub conn: usize,
    }

    impl FakeNoa {
        pub fn new(log: Option<PathBuf>) -> FakeNoa {
            let fake = FakeNoa {
                state: Arc::default(),
                log,
                conn: 0,
            };
            fake.state.lock().unwrap().next = 100;
            fake
        }

        pub fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
            f(&mut self.state.lock().unwrap())
        }

        /// `Err(Link)` on a connection Noa has dropped since.
        fn alive(&self) -> Result<(), NoaError> {
            match self.with(|s| self.conn != 0 && self.conn <= s.dead_before) {
                true => Err(NoaError::Link("connection reset".into())),
                false => Ok(()),
            }
        }
    }

    impl Noa for FakeNoa {
        fn panes(&mut self) -> Result<Vec<PaneView>, NoaError> {
            self.with(|s| s.lists += 1);
            self.alive()?;
            Ok(self.with(|s| s.panes.clone()))
        }

        fn spawn(&mut self, placement: Placement, spec: &PaneSpec) -> Result<u64, NoaError> {
            self.alive()?;
            let log = self
                .log
                .as_ref()
                .map(|log| std::fs::read_to_string(log).unwrap_or_default())
                .unwrap_or_default();
            self.with(|s| {
                if let Placement::Float(board) = placement
                    && s.gone_boards.contains(&board)
                {
                    return Err(NoaError::PaneGone);
                }
                s.next += 1;
                let id = s.next;
                s.spawned.push((placement, spec.clone(), log));
                if let Placement::Float(board) = placement {
                    s.panes.push(float(id, board, &spec.label));
                }
                Ok(id)
            })
        }

        fn focus(&mut self, pane: u64) -> Result<(), NoaError> {
            self.alive()?;
            self.with(|s| s.focused.push(pane));
            Ok(())
        }

        fn close_pane(&mut self, pane: u64) -> Result<(), NoaError> {
            self.alive()?;
            self.with(|s| {
                s.closed.push(pane);
                s.panes.retain(|p| p.pane_id != pane);
            });
            Ok(())
        }

        fn server_version(&self) -> String {
            "0.9.15".to_owned()
        }

        fn supports_board_tab(&self) -> bool {
            self.with(|s| !s.no_board_tab)
        }
    }

    impl Connect for FakeNoa {
        type Conn = FakeNoa;

        fn connect(&mut self) -> Result<FakeNoa, String> {
            let conn = self.with(|s| {
                if s.refuse_connects {
                    return Err("connection refused".to_owned());
                }
                s.connects += 1;
                Ok(s.connects)
            })?;
            Ok(FakeNoa {
                conn,
                ..self.clone()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    use tasks::model::column::column;
    use tasks::model::types::{Column, GitHubView, Isolation, Liveness};

    use super::fake::{FakeNoa, float, tab};
    use super::start::tests::{Temp, repo_at};
    use super::*;

    /// Noa opens the widget in `$HOME` without `--repo` when the focused
    /// pane's directory is unknown: a directory outside any git work tree
    /// is refused with why, not taken as the repository.
    #[test]
    fn a_directory_outside_any_repository_is_refused_not_listed() {
        let tmp = Temp::new("live-not-a-repo");
        let err = match LiveFeed::start(tmp.0.to_path_buf(), Arc::new(|| {})) {
            Ok(_) => panic!("a directory outside any repository was opened"),
            Err(why) => why,
        };
        assert!(err.contains(&tmp.0.display().to_string()), "{err}");
        assert!(!tmp.0.join(".noa").exists());
    }

    /// An IssueFeed that holds a fixed view and records what it was asked.
    struct Issues {
        view: GitHubView,
        asked: Arc<Mutex<Vec<String>>>,
    }

    impl IssueFeed for Issues {
        fn view(&self) -> GitHubView {
            self.view.clone()
        }

        fn refresh(&mut self) {
            self.asked.lock().unwrap().push("refresh".into());
        }

        fn load_detail(&mut self, number: u64) {
            self.asked.lock().unwrap().push(format!("detail {number}"));
        }
    }

    struct Rig {
        _tmp: Temp,
        repo: PathBuf,
        /// The data root (`<data dir>/noa/tasks`).
        root: PathBuf,
        log: PathBuf,
        noa: FakeNoa,
        clock: Arc<AtomicU64>,
        opened: Arc<Mutex<Vec<String>>>,
        /// What the launcher was asked to start.
        launched: Launched,
        asked: Arc<Mutex<Vec<String>>>,
        /// The thread of each wake.
        wakes: Arc<Mutex<Vec<std::thread::ThreadId>>>,
        feed: LiveFeed<FakeNoa>,
    }

    const START: Millis = 1_800_000_000_000;

    /// A feed on a fresh repository; `before` runs on the data root and
    /// the fake Noa before the feed starts.
    fn rig(name: &str, before: impl FnOnce(&Path, &Path, &FakeNoa)) -> Rig {
        rig_with(name, before, false)
    }

    /// Which widgets a launcher started, with their arguments.
    type Launched = Arc<Mutex<Vec<(sibling::Helper, Vec<OsString>)>>>;

    /// A launcher that records what it is asked and starts nothing.
    struct Recording(Launched);

    impl Launcher for Recording {
        fn launch(&mut self, helper: sibling::Helper, args: Vec<OsString>) -> Result<(), String> {
            self.0.lock().unwrap().push((helper, args));
            Ok(())
        }
    }

    /// [`rig`]; with `record`, its launcher records (else it is
    /// `sibling::Offline`).
    fn rig_with(name: &str, before: impl FnOnce(&Path, &Path, &FakeNoa), record: bool) -> Rig {
        let tmp = Temp::new(name);
        let repo = tmp.0.join("app");
        repo_at(&repo);
        let root = tmp.0.join("data");
        let log = store::runs_log(&root, &store::repo_key(&repo));
        let noa = FakeNoa::new(None);
        before(&repo, &log, &noa);
        let clock = Arc::new(AtomicU64::new(START));
        let opened = Arc::new(Mutex::new(Vec::new()));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let wakes = Arc::new(Mutex::new(Vec::new()));
        let launched: Launched = Arc::default();
        let launcher: Box<dyn Launcher + Send> = if record {
            Box::new(Recording(launched.clone()))
        } else {
            Box::new(sibling::Offline("テストでは起動しません"))
        };
        let (c, o, w) = (clock.clone(), opened.clone(), wakes.clone());
        let feed = LiveFeed::with(
            repo.clone(),
            root.clone(),
            Box::new(Issues {
                view: GitHubView::default(),
                asked: asked.clone(),
            }),
            noa.clone(),
            Box::new(move |target: &str| o.lock().unwrap().push(target.to_owned())),
            launcher,
            Arc::new(move || c.load(Ordering::SeqCst)),
            Arc::new(move || w.lock().unwrap().push(std::thread::current().id())),
        );
        Rig {
            _tmp: tmp,
            repo,
            root,
            log,
            noa,
            clock,
            opened,
            launched,
            asked,
            wakes,
            feed,
        }
    }

    impl Rig {
        /// Polls until `done` holds of the latest View (10 s at most).
        fn until(&mut self, what: &str, done: impl Fn(&View) -> bool) -> View {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut last = None;
            loop {
                if let Some(view) = self.feed.poll() {
                    last = Some(view);
                }
                if let Some(view) = &last
                    && done(view)
                {
                    return view.clone();
                }
                assert!(Instant::now() < deadline, "{what}: {last:#?}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        /// Moves the clock on past the next look.
        fn tick(&self) {
            self.clock.fetch_add(POLL_MS, Ordering::SeqCst);
        }
    }

    fn col(view: &View, r: &TaskRef) -> Option<Column> {
        let task = view.tasks.iter().find(|t| &t.r#ref == r)?;
        let last = view
            .runs
            .iter()
            .filter(|v| &v.run.task == r)
            .max_by_key(|v| v.run.started);
        Some(column(task, last))
    }

    /// A second window on `rig`'s repository: the same data root, run
    /// log and Noa, its own worker and a clock 7 ms off (its run ids
    /// differ, as another window's would).
    fn second_window(rig: &Rig) -> LiveFeed<FakeNoa> {
        second_window_waking(rig, Arc::new(|| {}))
    }

    /// [`second_window`], waking its screen with `wake`.
    fn second_window_waking(rig: &Rig, wake: Arc<dyn Fn() + Send + Sync>) -> LiveFeed<FakeNoa> {
        let root = rig.log.parent().unwrap().parent().unwrap().to_path_buf();
        let c = rig.clock.clone();
        LiveFeed::with(
            rig.repo.clone(),
            root,
            Box::new(Issues {
                view: GitHubView::default(),
                asked: rig.asked.clone(),
            }),
            rig.noa.clone(),
            Box::new(|_: &str| {}),
            Box::new(sibling::Offline("テストでは起動しません")),
            Arc::new(move || c.load(Ordering::SeqCst) + 7),
            wake,
        )
    }

    /// Polls `feed` until `done` holds of its latest View (10 s at most).
    fn until_on(feed: &mut LiveFeed<FakeNoa>, what: &str, done: impl Fn(&View) -> bool) -> View {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last = None;
        loop {
            if let Some(view) = feed.poll() {
                last = Some(view);
            }
            if let Some(view) = &last
                && done(view)
            {
                return view.clone();
            }
            assert!(Instant::now() < deadline, "{what}: {last:#?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A run another window on the repository started shows here too, by
    /// the next look: the run log is the record they share.
    #[test]
    fn a_run_another_window_started_shows_in_this_one() {
        let mut a = rig("other-window-run", |repo, _, _| {
            write_task(repo, "t-1", "one")
        });
        let mut b = second_window(&a);
        until_on(&mut b, "the task", |v| v.tasks.len() == 1);
        a.until("the task", |v| v.tasks.len() == 1);
        let r = TaskRef::Local("t-1".into());
        a.feed.act(Intent::Run {
            task: r.clone(),
            agent: AgentKind::Claude,
            isolation: Isolation::Here,
            auto: None,
        });
        a.until("実行中 in the first window", |v| {
            col(v, &r) == Some(Column::Running)
        });
        a.tick();
        until_on(&mut b, "実行中 in the second window", |v| {
            col(v, &r) == Some(Column::Running)
        });
    }

    /// The code's markers in tracked files are tasks of the feed; one
    /// removed from the code leaves the list at the next 再取得.
    #[test]
    fn the_codes_markers_are_tasks_until_they_are_removed() {
        let mut rig = rig("code-markers", |repo, _, _| {
            fs::write(repo.join("lib.rs"), "fn a() {} // TODO: make it fast\n").unwrap();
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["add", "lib.rs"])
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .status()
                .unwrap();
            assert!(ok.success());
        });
        let view = rig.until("the marker", |v| {
            v.tasks
                .iter()
                .any(|t| matches!(&t.r#ref, TaskRef::Code { path, .. } if path == "lib.rs"))
        });
        let marker = view
            .tasks
            .iter()
            .find(|t| t.r#ref.source() == tasks::model::types::Source::Code)
            .unwrap()
            .clone();
        assert_eq!(marker.title, "make it fast");
        assert!(marker.body.as_deref().unwrap().contains("lib.rs:1"));
        fs::write(rig.repo.join("lib.rs"), "fn a() {}\n").unwrap();
        rig.feed.act(Intent::Refresh);
        rig.until("the marker gone", |v| {
            !v.tasks.iter().any(|t| t.r#ref == marker.r#ref)
        });
    }

    /// A marker run in this work tree, whose agent removed the marker as
    /// its brief says, stays listed with its run and report until the run
    /// is closed: the person sees it in 確認待ち first.
    #[test]
    fn a_marker_removed_by_its_own_run_stays_until_the_run_is_closed() {
        let mut rig = rig("code-run-here", |repo, _, _| {
            fs::write(repo.join("lib.rs"), "fn a() {} // TODO: make it fast\n").unwrap();
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["add", "lib.rs"])
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .status()
                .unwrap();
            assert!(ok.success());
        });
        let view = rig.until("the marker", |v| {
            v.tasks
                .iter()
                .any(|t| matches!(t.r#ref, TaskRef::Code { .. }))
        });
        let marker = view
            .tasks
            .iter()
            .find(|t| matches!(t.r#ref, TaskRef::Code { .. }))
            .unwrap()
            .r#ref
            .clone();
        rig.feed.act(Intent::Run {
            task: marker.clone(),
            agent: AgentKind::Claude,
            isolation: Isolation::Here,
            auto: None,
        });
        let view = rig.until("実行中", |v| col(v, &marker) == Some(Column::Running));
        let run = view.runs[0].run.run.clone();
        // The agent does the work: removes the marker, reports, finishes.
        fs::write(rig.repo.join("lib.rs"), "fn a() {}\n").unwrap();
        fs::write(
            runs::result_file(&rig.repo, &run),
            "# 報告\n\n速くしました\n",
        )
        .unwrap();
        rig.noa.with(|s| {
            for pane in &mut s.panes {
                pane.agent_state = Some("finished".into());
            }
        });
        rig.feed.act(Intent::Refresh);
        rig.tick();
        let view = rig.until("確認待ち with its report", |v| {
            col(v, &marker) == Some(Column::Review)
                && v.runs
                    .iter()
                    .any(|r| r.run.run == run && r.report.is_some())
        });
        let task = view.tasks.iter().find(|t| t.r#ref == marker).unwrap();
        assert_eq!(task.title, "make it fast");
        // Closing the run lets it go.
        rig.feed.act(Intent::ClosePane(run));
        rig.tick();
        rig.until("gone once closed", |v| {
            !v.tasks.iter().any(|t| t.r#ref == marker)
        });
    }

    /// A marker run's brief replaced by a FIFO (the run's work tree is the
    /// agent's) never stops the worker: the gone marker is still listed.
    #[test]
    fn a_marker_runs_brief_that_is_a_fifo_does_not_stop_the_worker() {
        let mut rig = rig("code-fifo-brief", |repo, log, _| {
            let task = TaskRef::Code {
                path: "lib.rs".into(),
                key: "00".into(),
            };
            let run = RunId::new("run-1");
            let dir = runs::run_dir(repo, &run);
            fs::create_dir_all(&dir).unwrap();
            let made = std::process::Command::new("mkfifo")
                .arg(runs::task_file(repo, &run))
                .status()
                .unwrap();
            assert!(made.success());
            runs::append(
                log,
                &RunEvent::RunStarted {
                    run,
                    task,
                    agent: AgentKind::Codex,
                    workdir: repo.to_owned(),
                    branch: None,
                    label: "task:run-1".into(),
                    at: START - 1_000,
                    auto: None,
                },
            )
            .unwrap();
        });
        rig.until("the gone marker listed", |v| {
            v.tasks
                .iter()
                .any(|t| matches!(t.r#ref, TaskRef::Code { .. }))
        });
    }

    /// Two windows on one repository (two feeds, one run log) sent 実行 on
    /// the same task at once start one run: the check and the RunStarted
    /// it allows are one step across windows.
    #[test]
    fn two_windows_running_one_task_at_once_start_one_run() {
        let mut a = rig("two-windows", |_, _, _| {});
        let mut b = second_window(&a);
        let ids: Vec<String> = (0..12).map(|n| format!("t-{n}")).collect();
        for id in &ids {
            write_task(&a.repo, id, id);
        }
        a.tick();
        a.until("the tasks", |v| v.tasks.len() == ids.len());
        let deadline = Instant::now() + Duration::from_secs(10);
        while b.poll().is_none_or(|v| v.tasks.len() != ids.len()) {
            assert!(Instant::now() < deadline, "the second window's tasks");
            std::thread::sleep(Duration::from_millis(10));
        }
        for id in &ids {
            let run = Intent::Run {
                task: TaskRef::Local(id.clone()),
                agent: AgentKind::Claude,
                isolation: Isolation::Worktree,
                auto: None,
            };
            a.feed.act(run.clone());
            b.act(run);
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let runs = runs::replay(&a.log).unwrap();
            let settled = ids.iter().all(|id| {
                runs.iter()
                    .any(|r| r.task == TaskRef::Local(id.clone()) && r.pane.is_some())
            });
            if settled {
                // Give a second run that slipped through time to log.
                std::thread::sleep(Duration::from_millis(500));
                break;
            }
            assert!(Instant::now() < deadline, "runs never started: {runs:#?}");
            std::thread::sleep(Duration::from_millis(20));
        }
        let runs = runs::replay(&a.log).unwrap();
        for id in &ids {
            let n = runs
                .iter()
                .filter(|r| r.task == TaskRef::Local(id.clone()))
                .count();
            assert_eq!(n, 1, "{id} started {n} runs");
        }
    }

    fn write_task(repo: &Path, id: &str, title: &str) {
        fs::create_dir_all(local::tasks_dir(repo)).unwrap();
        fs::write(
            local::tasks_dir(repo).join(format!("{id}.md")),
            format!("---\ntitle: {title}\n---\n本文\n"),
        )
        .unwrap();
    }

    #[test]
    fn ac21_create_run_report_and_close_move_a_task_across_the_columns() {
        let mut rig = rig("e2e", |_, _, _| {});
        rig.until("the first view", |v| v.tasks.is_empty() && v.now == START);
        rig.feed.act(Intent::Create {
            title: "フォントを\n直す".into(),
            body: "手順".into(),
            labels: vec!["bug".into()],
        });
        let view = rig.until("the new task", |v| v.tasks.len() == 1);
        let task = view.tasks[0].clone();
        let id = format!("t-{START}");
        assert_eq!(task.r#ref, TaskRef::Local(id.clone()));
        assert_eq!(task.title, "フォントを 直す");
        assert_eq!(task.labels, ["bug"]);
        assert_eq!(col(&view, &task.r#ref), Some(Column::Todo));
        // The repository's .noa is gitignored before the file is written.
        assert_eq!(
            fs::read_to_string(rig.repo.join(".noa/.gitignore")).unwrap(),
            "*\n"
        );

        rig.feed.act(Intent::Run {
            task: task.r#ref.clone(),
            agent: AgentKind::Claude,
            isolation: Isolation::Here,
            auto: None,
        });
        let r = task.r#ref.clone();
        let view = rig.until("実行中", |v| {
            col(v, &r) == Some(Column::Running)
                && v.runs
                    .iter()
                    .any(|run| matches!(run.liveness, Liveness::Alive { .. }))
        });
        let run = view.runs[0].run.run.clone();
        assert!(
            view.notice.as_deref().unwrap().contains("claude"),
            "{view:?}"
        );
        // A second 実行 while it runs is refused.
        rig.feed.act(Intent::Run {
            task: r.clone(),
            agent: AgentKind::Claude,
            isolation: Isolation::Here,
            auto: None,
        });
        rig.until("refused", |v| {
            v.notice
                .as_deref()
                .is_some_and(|n| n.contains("実行中です"))
        });
        assert_eq!(runs::replay(&rig.log).unwrap().len(), 1);

        // The agent writes its report and says it finished.
        fs::write(runs::result_file(&rig.repo, &run), "# 報告\n\n直しました\n").unwrap();
        rig.noa.with(|s| {
            for pane in &mut s.panes {
                pane.agent_state = Some("finished".into());
            }
        });
        rig.tick();
        let view = rig.until("確認待ち", |v| {
            col(v, &r) == Some(Column::Review) && v.runs[0].report.is_some()
        });
        assert_eq!(
            view.runs[0].report.as_ref().unwrap().text,
            "# 報告\n\n直しました\n"
        );

        // `d`: 完了にする.
        rig.feed.act(Intent::SetState {
            id: id.clone(),
            state: State::Closed,
        });
        rig.until("完了", |v| col(v, &r) == Some(Column::Done));
        let text =
            fs::read_to_string(local::tasks_dir(&rig.repo).join(format!("{id}.md"))).unwrap();
        assert!(text.contains("state: closed\n"), "{text}");
        assert!(text.ends_with("---\n手順\n"), "{text}");
    }

    fn live_run(repo: &Path, log: &Path, noa: &FakeNoa) {
        write_task(repo, "t-1", "走っている");
        write_task(repo, "t-2", "次の");
        for event in [
            RunEvent::RunStarted {
                run: RunId::new("run-1"),
                task: TaskRef::Local("t-1".into()),
                agent: AgentKind::Codex,
                workdir: repo.to_owned(),
                branch: None,
                label: "task:run-1".into(),
                at: START - 60_000,
                auto: None,
            },
            RunEvent::RunPaneAssigned {
                run: RunId::new("run-1"),
                pane: 40,
                board: 7,
                at: START - 60_000,
            },
        ] {
            runs::append(log, &event).unwrap();
        }
        noa.with(|s| s.panes.push(float(40, 7, "task:run-1")));
    }

    #[test]
    fn ac17_a_reopened_window_finds_the_board_from_a_live_float_and_floats_the_next_run_on_it() {
        let mut rig = rig("reattach", live_run);
        let t1 = TaskRef::Local("t-1".into());
        let t2 = TaskRef::Local("t-2".into());
        rig.until("re-attached", |v| {
            col(v, &t1) == Some(Column::Running) && col(v, &t2) == Some(Column::Todo)
        });
        rig.feed.act(Intent::Run {
            task: t2.clone(),
            agent: AgentKind::Claude,
            isolation: Isolation::Here,
            auto: None,
        });
        rig.until("the second run", |v| col(v, &t2) == Some(Column::Running));
        let spawned = rig.noa.with(|s| s.spawned.clone());
        assert_eq!(spawned.len(), 1, "no new board tab: {spawned:?}");
        assert_eq!(spawned[0].0, ipc::Placement::Float(7));
    }

    #[test]
    fn ac18_run_actions_reach_noa_and_the_opener_and_leave_task_state_alone() {
        let mut rig = rig("actions", live_run);
        let t1 = TaskRef::Local("t-1".into());
        rig.until("live", |v| col(v, &t1) == Some(Column::Running));
        let file = local::tasks_dir(&rig.repo).join("t-1.md");
        let before = fs::read_to_string(&file).unwrap();
        let run = RunId::new("run-1");

        rig.feed.act(Intent::OpenPane(run.clone()));
        rig.until("focused", |v| {
            v.notice
                .as_deref()
                .is_some_and(|n| n.contains("開きました"))
        });
        assert_eq!(rig.noa.with(|s| s.focused.clone()), [40]);

        rig.feed.act(Intent::OpenWorktree(run.clone()));
        rig.feed
            .act(Intent::OpenUrl("https://github.com/o/n/issues/1".into()));
        rig.until("opened", |v| {
            v.notice.as_deref().is_some_and(|n| n.contains("issues/1"))
        });
        assert_eq!(
            *rig.opened.lock().unwrap(),
            [
                rig.repo.display().to_string(),
                "https://github.com/o/n/issues/1".to_owned()
            ]
        );

        rig.feed.act(Intent::ClosePane(run.clone()));
        let view = rig.until("closed", |v| col(v, &t1) == Some(Column::Review));
        assert_eq!(view.runs[0].liveness, Liveness::Gone);
        assert_eq!(rig.noa.with(|s| s.closed.clone()), [40]);
        assert!(
            runs::replay(&rig.log).unwrap()[0].closed.is_some(),
            "RunClosed logged"
        );

        // Refresh and LoadDetail go to the GitHub source.
        rig.feed.act(Intent::Refresh);
        rig.feed.act(Intent::LoadDetail(12));
        assert_eq!(*rig.asked.lock().unwrap(), ["refresh", "detail 12"]);
        // None of it touched the task.
        assert_eq!(fs::read_to_string(&file).unwrap(), before);
        assert!(view.tasks.iter().all(|t| t.state == State::Open));
    }

    #[test]
    fn a_notice_act_sets_itself_wakes_the_screen() {
        let mut rig = rig("notice-wake", |_, _, _| {});
        rig.until("the first view", |v| v.now == START);
        let me = std::thread::current().id();
        rig.wakes.lock().unwrap().clear();
        // The task is gone since the form opened.
        rig.feed.act(Intent::Run {
            task: TaskRef::Local("t-404".into()),
            agent: AgentKind::Claude,
            isolation: Isolation::Here,
            auto: None,
        });
        assert!(
            rig.wakes.lock().unwrap().contains(&me),
            "act woke the screen"
        );
        let view = rig.feed.poll().expect("a new view");
        assert_eq!(view.notice.as_deref(), Some("そのタスクが見つかりません"));
        // The worker has stopped.
        rig.wakes.lock().unwrap().clear();
        rig.feed.jobs = mpsc::channel().0;
        rig.feed.act(Intent::OpenUrl("https://example.com".into()));
        assert!(
            rig.wakes.lock().unwrap().contains(&me),
            "act woke the screen"
        );
        let view = rig.feed.poll().expect("a new view");
        assert_eq!(view.notice.as_deref(), Some("タスクの処理が止まっています"));
    }

    #[test]
    fn ac17_a_run_after_a_failed_first_connect_sees_the_live_run_and_floats_on_its_board() {
        let mut rig = rig("late-connect", |repo, log, noa| {
            live_run(repo, log, noa);
            noa.with(|s| s.refuse_connects = true);
        });
        let t1 = TaskRef::Local("t-1".into());
        let t2 = TaskRef::Local("t-2".into());
        // No panes seen: run-1 looks gone.
        rig.until("listed without Noa", |v| {
            v.tasks.len() == 2 && v.runs.len() == 1
        });
        rig.noa.with(|s| s.refuse_connects = false);
        // Noa is back: run-1 is live, so t-1 is refused …
        rig.feed.act(Intent::Run {
            task: t1.clone(),
            agent: AgentKind::Claude,
            isolation: Isolation::Here,
            auto: None,
        });
        rig.until("refused", |v| {
            v.notice
                .as_deref()
                .is_some_and(|n| n.contains("実行中です"))
        });
        assert!(rig.noa.with(|s| s.spawned.is_empty()));
        assert_eq!(runs::replay(&rig.log).unwrap().len(), 1);
        // … and t-2 floats on run-1's board, no new board tab.
        rig.feed.act(Intent::Run {
            task: t2.clone(),
            agent: AgentKind::Claude,
            isolation: Isolation::Here,
            auto: None,
        });
        rig.until("the second run", |v| col(v, &t2) == Some(Column::Running));
        let spawned = rig.noa.with(|s| s.spawned.clone());
        assert_eq!(spawned.len(), 1, "{spawned:?}");
        assert_eq!(spawned[0].0, ipc::Placement::Float(7));
    }

    /// The work trees `git worktree add` made beside `repo`, and its `task-*` branches.
    fn made_beside(repo: &Path) -> (bool, String) {
        let wt = repo.with_file_name(format!(
            "{}-wt",
            repo.file_name().unwrap().to_string_lossy()
        ));
        let branches = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["branch", "--list", "task-*"])
            .output()
            .unwrap();
        (wt.exists(), String::from_utf8(branches.stdout).unwrap())
    }

    #[test]
    fn a_run_reconnects_when_noa_restarted_since_the_last_look() {
        let mut rig = rig("restarted", |repo, _, _| write_task(repo, "t-2", "次の"));
        let t2 = TaskRef::Local("t-2".into());
        rig.until("listed", |v| col(v, &t2) == Some(Column::Todo));
        // The worker's link is from before the restart.
        rig.noa.with(|s| s.dead_before = s.connects);
        rig.feed.act(Intent::Run {
            task: t2.clone(),
            agent: AgentKind::Claude,
            isolation: Isolation::Worktree,
            auto: None,
        });
        rig.until("running", |v| col(v, &t2) == Some(Column::Running));
        assert_eq!(rig.noa.with(|s| s.connects), 2, "reconnected");
        let runs = runs::replay(&rig.log).unwrap();
        assert_eq!(runs.len(), 1);
        assert!(
            runs[0].closed.is_none() && runs[0].pane.is_some(),
            "{runs:?}"
        );
    }

    #[test]
    fn a_run_is_refused_before_any_worktree_when_noa_cannot_be_reached() {
        let mut rig = rig("unreachable", |repo, _, _| write_task(repo, "t-2", "次の"));
        let t2 = TaskRef::Local("t-2".into());
        rig.until("listed", |v| col(v, &t2) == Some(Column::Todo));
        // Noa quit: the old link is dead and a new one is refused.
        rig.noa.with(|s| {
            s.dead_before = s.connects;
            s.refuse_connects = true;
        });
        rig.feed.act(Intent::Run {
            task: t2.clone(),
            agent: AgentKind::Claude,
            isolation: Isolation::Worktree,
            auto: None,
        });
        let view = rig.until("refused", |v| {
            v.notice
                .as_deref()
                .is_some_and(|n| n.contains("Noa に接続できない"))
        });
        assert_eq!(col(&view, &t2), Some(Column::Todo));
        assert!(runs::replay(&rig.log).unwrap().is_empty(), "nothing logged");
        assert_eq!(
            made_beside(&rig.repo),
            (false, String::new()),
            "no worktree or branch"
        );
        assert!(rig.noa.with(|s| s.spawned.is_empty()));
    }

    /// Noa restarts: the worker's link is dead, and the new Noa holds
    /// only the person's own tabs, minted with the ids the old one had.
    fn restart(noa: &FakeNoa, ids: &[u64]) {
        noa.with(|s| {
            s.dead_before = s.connects;
            s.panes = ids.iter().map(|&id| tab(id)).collect();
        });
    }

    #[test]
    fn a_run_after_noa_restarted_opens_a_new_board_tab_not_a_float_on_the_old_boards_id() {
        let mut rig = rig("restart-board", live_run);
        let t1 = TaskRef::Local("t-1".into());
        let t2 = TaskRef::Local("t-2".into());
        // The board (7) is learned from run-1's float.
        rig.until("live", |v| col(v, &t1) == Some(Column::Running));
        // In the new Noa, 7 is the person's shell tab.
        restart(&rig.noa, &[7, 40]);
        rig.feed.act(Intent::Run {
            task: t2.clone(),
            agent: AgentKind::Claude,
            isolation: Isolation::Here,
            auto: None,
        });
        rig.until("the second run", |v| col(v, &t2) == Some(Column::Running));
        let spawned = rig.noa.with(|s| s.spawned.clone());
        assert_eq!(spawned.len(), 2, "{spawned:?}");
        assert_eq!(spawned[0].0, ipc::Placement::Board, "a new board tab");
        assert_eq!(spawned[1].0, ipc::Placement::Float(101), "on the new board");
        assert!(spawned[0].1.attended, "the board is not auto-approved");
    }

    #[test]
    fn open_and_close_after_noa_restarted_leave_the_new_noas_pane_with_the_old_id_alone() {
        let mut rig = rig("restart-close", live_run);
        let t1 = TaskRef::Local("t-1".into());
        rig.until("live", |v| col(v, &t1) == Some(Column::Running));
        // Noa quits: the next look finds the link dead and cannot reconnect …
        let lists = rig.noa.with(|s| {
            s.dead_before = s.connects;
            s.refuse_connects = true;
            s.lists
        });
        rig.tick();
        let deadline = Instant::now() + Duration::from_secs(10);
        while rig.noa.with(|s| s.lists) == lists {
            assert!(Instant::now() < deadline, "the look after the restart");
            std::thread::sleep(Duration::from_millis(10));
        }
        // … then the new Noa mints 40 for the person's own tab, before the next look.
        rig.noa.with(|s| {
            s.refuse_connects = false;
            s.panes = vec![tab(40)];
        });
        let run = RunId::new("run-1");
        rig.feed.act(Intent::OpenPane(run.clone()));
        rig.until("not found to open", |v| {
            v.notice.as_deref() == Some("run-1 のペインが見つかりません")
        });
        assert!(rig.noa.with(|s| s.focused.is_empty()), "nothing focused");
        rig.feed.act(Intent::ClosePane(run.clone()));
        rig.until("closed", |v| {
            v.notice
                .as_deref()
                .is_some_and(|n| n.contains("見つかりません。実行を閉じました"))
        });
        assert!(rig.noa.with(|s| s.closed.is_empty()), "nothing closed");
        assert_eq!(rig.noa.with(|s| s.panes.clone()), [tab(40)]);
        // Its pane is not live in this Noa: the run is closed.
        assert!(runs::replay(&rig.log).unwrap()[0].closed.is_some());
    }

    /// A run just started: `RunStarted` logged, no pane assigned yet.
    fn starting_run(repo: &Path, log: &Path) {
        write_task(repo, "t-1", "起動中の");
        runs::append(
            log,
            &RunEvent::RunStarted {
                run: RunId::new("run-1"),
                task: TaskRef::Local("t-1".into()),
                agent: AgentKind::Codex,
                workdir: repo.to_owned(),
                branch: None,
                label: "task:run-1".into(),
                at: START - 1_000,
                auto: None,
            },
        )
        .unwrap();
    }

    #[test]
    fn closing_a_starting_run_whose_float_was_never_listed_closes_the_float() {
        let mut rig = rig("starting-close", |repo, log, noa| {
            starting_run(repo, log);
            // Its float is up, but the first connect failed.
            noa.with(|s| {
                s.panes.push(float(40, 7, "task:run-1"));
                s.refuse_connects = true;
            });
        });
        rig.until("starting", |v| {
            v.runs.len() == 1 && v.runs[0].liveness == Liveness::Starting
        });
        rig.noa.with(|s| s.refuse_connects = false);
        rig.feed.act(Intent::ClosePane(RunId::new("run-1")));
        let view = rig.until("closed", |v| {
            v.notice.as_deref() == Some("run-1 のペインを閉じました")
        });
        assert_eq!(rig.noa.with(|s| s.closed.clone()), [40]);
        assert_eq!(view.runs[0].liveness, Liveness::Gone);
        assert!(runs::replay(&rig.log).unwrap()[0].closed.is_some());
    }

    /// An agent that ended leaves its float open (exited): 閉じる closes
    /// it in Noa, so ended floats do not pile up on the board.
    #[test]
    fn closing_a_run_whose_agent_ended_closes_its_float_too() {
        let mut rig = rig("ended-close", |repo, log, noa| {
            live_run(repo, log, noa);
            noa.with(|s| {
                for pane in &mut s.panes {
                    pane.exited = true;
                }
            });
        });
        rig.until("gone", |v| {
            v.runs.len() == 1 && v.runs[0].liveness == Liveness::Gone
        });
        rig.feed.act(Intent::ClosePane(RunId::new("run-1")));
        rig.until("closed", |v| {
            v.notice.as_deref() == Some("run-1 のペインを閉じました")
        });
        assert_eq!(rig.noa.with(|s| s.closed.clone()), [40]);
        assert!(runs::replay(&rig.log).unwrap()[0].closed.is_some());
    }

    #[test]
    fn closing_a_starting_run_with_no_float_yet_is_refused_with_a_notice() {
        let mut rig = rig("starting-refused", |repo, log, _| starting_run(repo, log));
        rig.until("starting", |v| {
            v.runs.len() == 1 && v.runs[0].liveness == Liveness::Starting
        });
        rig.feed.act(Intent::ClosePane(RunId::new("run-1")));
        let view = rig.until("refused", |v| {
            v.notice.as_deref().is_some_and(|n| n.contains("起動中"))
        });
        assert_eq!(view.runs[0].liveness, Liveness::Starting);
        assert!(rig.noa.with(|s| s.closed.is_empty()));
        assert!(
            runs::replay(&rig.log).unwrap()[0].closed.is_none(),
            "not logged closed"
        );
    }

    #[test]
    fn edit_keeps_the_files_other_keys_and_state() {
        let mut rig = rig("edit", |repo, _, _| {
            fs::create_dir_all(local::tasks_dir(repo)).unwrap();
            fs::write(
                local::tasks_dir(repo).join("t-1.md"),
                "---\ntitle: old\nstate: closed\nowner: me\n---\nold body\n",
            )
            .unwrap();
        });
        rig.until("listed", |v| v.tasks.len() == 1);
        rig.feed.act(Intent::Edit {
            id: "t-1".into(),
            title: "new".into(),
            body: "new body".into(),
            labels: vec!["a".into(), "b".into()],
        });
        let view = rig.until("edited", |v| v.tasks[0].title == "new");
        assert_eq!(view.tasks[0].state, State::Closed);
        assert_eq!(
            fs::read_to_string(local::tasks_dir(&rig.repo).join("t-1.md")).unwrap(),
            "---\ntitle: new\nstate: closed\nlabels: a, b\nowner: me\n---\nnew body\n"
        );
    }

    // ------------------------------------------------------------ task links

    impl Rig {
        /// Polls until `done` holds of `task`'s 関連成果 (10 s at most).
        fn until_related(
            &mut self,
            task: &TaskRef,
            what: &str,
            done: impl Fn(&Related) -> bool,
        ) -> Related {
            until_related_on(&mut self.feed, task, what, done)
        }

        /// Waits until every job sent so far is done and the worker has
        /// looked once more since: an opened URL's notice is the barrier.
        fn settle(&mut self) {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let mark = format!("barrier-{}", SEQ.fetch_add(1, Ordering::Relaxed));
            self.feed.act(Intent::OpenUrl(mark.clone()));
            self.until("the barrier", |v| {
                v.notice.as_deref().is_some_and(|n| n.contains(&mark))
            });
        }

        fn links(&self) -> Vec<LinkEvent> {
            task_links::read(&start::links_of(&self.log)).unwrap()
        }

        /// The ChangeSeen lines of the link log, as (run, commit).
        fn seen(&self) -> Vec<(String, String)> {
            self.links()
                .into_iter()
                .filter_map(|e| match e {
                    LinkEvent::ChangeSeen { run, commit, .. } => Some((run, commit)),
                    _ => None,
                })
                .collect()
        }
    }

    fn until_related_on(
        feed: &mut LiveFeed<FakeNoa>,
        task: &TaskRef,
        what: &str,
        done: impl Fn(&Related) -> bool,
    ) -> Related {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let _ = feed.poll();
            let related = feed
                .related()
                .and_then(|all| all.get(task))
                .cloned()
                .unwrap_or_default();
            if done(&related) {
                return related;
            }
            assert!(Instant::now() < deadline, "{what}: {related:#?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// `git -C dir <args>` that must succeed; its output, trimmed.
    fn git_ok(dir: &Path, args: &[&str]) -> String {
        let out = start::git(dir, args).unwrap_or_else(|why| panic!("git {args:?}: {why}"));
        String::from_utf8_lossy(&out).trim().to_owned()
    }

    /// An empty commit on `dir`'s checkout; its SHA.
    fn commit_in(dir: &Path, message: &str) -> String {
        git_ok(
            dir,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                message,
            ],
        );
        start::head_of(dir).unwrap()
    }

    /// A commit on top of `parent` in `repo`, without a checkout; its SHA.
    fn commit_on(repo: &Path, parent: &str, message: &str) -> String {
        let tree = format!("{parent}^{{tree}}");
        git_ok(
            repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "commit-tree",
                &tree,
                "-p",
                parent,
                "-m",
                message,
            ],
        )
    }

    fn local_ref(id: &str) -> TaskRef {
        TaskRef::Local(id.into())
    }

    /// Logs a run of `task` that started and ended before the window opened.
    fn logged_run(
        log: &Path,
        run: &str,
        task: &TaskRef,
        branch: Option<&str>,
        workdir: &Path,
        at: Millis,
    ) {
        runs::append(
            log,
            &RunEvent::RunStarted {
                run: RunId::new(run),
                task: task.clone(),
                agent: AgentKind::Claude,
                workdir: workdir.to_path_buf(),
                branch: branch.map(str::to_owned),
                label: runs::label(&RunId::new(run)),
                at,
                auto: None,
            },
        )
        .unwrap();
        runs::append(
            log,
            &RunEvent::RunClosed {
                run: RunId::new(run),
                at: at + 1,
            },
        )
        .unwrap();
    }

    /// Starts a run of t-1 (written by `rig`'s `before`) and waits until it
    /// is 実行中; the run.
    fn run_t1(rig: &mut Rig, isolation: Isolation) -> Run {
        let t = local_ref("t-1");
        rig.until("the task", |v| v.tasks.iter().any(|x| x.r#ref == t));
        rig.feed.act(Intent::Run {
            task: t.clone(),
            agent: AgentKind::Claude,
            isolation,
            auto: None,
        });
        let view = rig.until("実行中", |v| col(v, &t) == Some(Column::Running));
        view.runs[0].run.clone()
    }

    /// Auto-assign L1-7: 実行's `Intent::Run` with an `AutoRecord` reaches
    /// the run log's `RunStarted` (through `RunRequest.auto`), and the run
    /// replays as any other.
    #[test]
    fn an_auto_assigned_run_logs_its_record_on_run_started() {
        let mut rig = rig("auto-record", |repo, _, _| write_task(repo, "t-1", "one"));
        let t = local_ref("t-1");
        rig.until("the task", |v| v.tasks.iter().any(|x| x.r#ref == t));
        let auto = tasks::auto_assign::AutoRecord {
            planner: "claude (sonnet · medium)".into(),
            at: START,
            picks: vec![tasks::auto_assign::Pick {
                slot: "worker".into(),
                agent: "codex".into(),
                model: None,
                effort: None,
                reason: "小さな修正".into(),
            }],
            overridden: Vec::new(),
        };
        rig.feed.act(Intent::Run {
            task: t.clone(),
            agent: AgentKind::Codex,
            isolation: Isolation::Here,
            auto: Some(auto.clone()),
        });
        let view = rig.until("実行中", |v| col(v, &t) == Some(Column::Running));
        assert_eq!(view.runs[0].run.agent, AgentKind::Codex);
        let text = fs::read_to_string(&rig.log).unwrap();
        let logged: Vec<Option<tasks::auto_assign::AutoRecord>> = text
            .lines()
            .filter_map(|l| match serde_json::from_str::<RunEvent>(l).unwrap() {
                RunEvent::RunStarted { auto, .. } => Some(auto),
                _ => None,
            })
            .collect();
        assert_eq!(logged, [Some(auto)], "{text}");
    }

    /// AC-2: the 関連成果 of a task whose run committed twice and was
    /// reviewed read back the same in a new window after the run's pane is
    /// closed and its worktree removed: the link log names runs, commits
    /// and sessions, nothing in the work tree.
    #[test]
    fn links_ac2_related_items_survive_closing_the_pane_and_removing_the_worktree() {
        let mut rig = rig("links-ac2", |repo, _, _| write_task(repo, "t-1", "one"));
        let t = local_ref("t-1");
        let run = run_t1(&mut rig, Isolation::Worktree);
        commit_in(&run.workdir, "one");
        rig.feed.act(Intent::Refresh);
        rig.until_related(&t, "v1", |r| r.versions.len() == 1);
        let head = commit_in(&run.workdir, "two");
        rig.feed.act(Intent::Refresh);
        rig.until_related(&t, "v2", |r| r.versions.len() == 2);
        // A review of the worktree, linked as Review links it.
        let target = task_links::Target {
            toplevel: run.workdir.clone(),
            head_ref: None,
            head_sha: Some(head.clone()),
        };
        let matched = task_links::match_run(&rig.root, &target).expect("the run matches");
        assert_eq!(matched.by, task_links::MatchedBy::Worktree);
        task_links::link_session(
            &matched,
            SessionKind::Review,
            "rev-1",
            Some(&head),
            false,
            START,
        )
        .unwrap();
        let origin = task_links::find_origin(&rig.root, SessionKind::Review, "rev-1").unwrap();
        task_links::record_outcome(
            &origin,
            SessionKind::Review,
            "rev-1",
            Some("approve"),
            true,
            START,
        )
        .unwrap();
        rig.tick();
        let before = rig.until_related(&t, "the review", |r| {
            r.reviews.len() == 1
                && r.reviews[0].outcome == related::Outcome::Decided("approve".into())
        });
        assert_eq!(before.runs, std::slice::from_ref(&run.run));
        assert_eq!(
            before.reviews[0].target,
            related::TargetVersion::Version {
                no: 2,
                uncommitted: false
            }
        );

        rig.feed.act(Intent::ClosePane(run.run.clone()));
        rig.until("closed", |v| v.runs.iter().all(|r| r.run.closed.is_some()));
        git_ok(
            &rig.repo,
            &[
                "worktree",
                "remove",
                "--force",
                run.workdir.to_str().unwrap(),
            ],
        );
        assert!(!run.workdir.exists());
        let mut again = second_window(&rig);
        let after = until_related_on(&mut again, &t, "the same items in a new window", |r| {
            *r == before
        });
        assert_eq!(after, before);
    }

    /// AC-3 (Tasks): a run's start is still logged by `RunStarted` alone in
    /// the run log; its base is the worktree's HEAD; each commit on its
    /// branch is one ChangeSeen, an unmoved head none, and a deleted
    /// branch adds nothing and keeps the versions.
    #[test]
    fn links_ac3_a_worktree_runs_base_and_each_new_head_are_recorded_once() {
        let mut rig = rig("links-ac3-wt", |repo, _, _| write_task(repo, "t-1", "one"));
        let t = local_ref("t-1");
        let base = start::head_of(&rig.repo).unwrap();
        let run = run_t1(&mut rig, Isolation::Worktree);
        let branch = run.branch.clone().unwrap();
        // The run log holds run events only: its format is unchanged.
        let text = fs::read_to_string(&rig.log).unwrap();
        for line in text.lines() {
            let event: RunEvent =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}"));
            assert_eq!(serde_json::to_string(&event).unwrap(), line);
        }
        assert!(text.contains(r#""event":"RunStarted""#));
        let links = rig.links();
        assert_eq!(
            links[0],
            LinkEvent::Repo {
                path: rig.repo.clone(),
                at: START
            }
        );
        assert!(
            links.contains(&LinkEvent::ChangeBase {
                run: run.run.to_string(),
                commit: base.clone(),
                at: START
            }),
            "{links:?}"
        );
        assert!(rig.seen().is_empty(), "an unmoved head is no version");

        let c1 = commit_in(&run.workdir, "one");
        rig.feed.act(Intent::Refresh);
        rig.settle();
        assert_eq!(rig.seen(), [(run.run.to_string(), c1.clone())]);
        rig.feed.act(Intent::Refresh);
        rig.settle();
        assert_eq!(rig.seen().len(), 1, "nothing on an unmoved head");

        git_ok(
            &rig.repo,
            &[
                "worktree",
                "remove",
                "--force",
                run.workdir.to_str().unwrap(),
            ],
        );
        git_ok(&rig.repo, &["branch", "-D", &branch]);
        rig.feed.act(Intent::Refresh);
        rig.settle();
        assert_eq!(rig.seen().len(), 1, "a deleted branch adds nothing");
        let related = rig.until_related(&t, "v1 kept", |r| r.versions.len() == 1);
        assert_eq!(related.versions[0].commit, c1);
        // A second window adds no Repo line of its own for the same path.
        let mut other = second_window(&rig);
        until_related_on(&mut other, &t, "v1", |r| r.versions.len() == 1);
        let repos = rig
            .links()
            .iter()
            .filter(|e| matches!(e, LinkEvent::Repo { .. }))
            .count();
        assert_eq!(repos, 1);
    }

    /// AC-3 (Tasks): an この作業ツリー run's head is the repository's,
    /// recorded while the run is live and once more after it ends, never
    /// again.
    #[test]
    fn links_ac3_a_here_run_is_recorded_while_live_and_once_after() {
        let mut rig = rig("links-ac3-here", |repo, _, _| {
            write_task(repo, "t-1", "one")
        });
        let run = run_t1(&mut rig, Isolation::Here);
        assert_eq!(run.branch, None);
        let c1 = commit_in(&rig.repo, "one");
        rig.feed.act(Intent::Refresh);
        rig.settle();
        assert_eq!(rig.seen(), [(run.run.to_string(), c1)]);
        rig.feed.act(Intent::Refresh);
        rig.settle();
        assert_eq!(rig.seen().len(), 1, "nothing on an unmoved head");
        rig.feed.act(Intent::ClosePane(run.run.clone()));
        rig.until("closed", |v| v.runs.iter().all(|r| r.run.closed.is_some()));
        let c2 = commit_in(&rig.repo, "two");
        rig.feed.act(Intent::Refresh);
        rig.settle();
        assert_eq!(rig.seen().last(), Some(&(run.run.to_string(), c2)));
        assert_eq!(rig.seen().len(), 2, "once after it ended");
        commit_in(&rig.repo, "three");
        rig.feed.act(Intent::Refresh);
        rig.settle();
        assert_eq!(rig.seen().len(), 2, "not after that");
    }

    /// AC-3 (Tasks): an この作業ツリー run that commits and ends before any
    /// change poll saw it live still gets its last head once it is gone.
    #[test]
    fn links_ac3_a_here_run_ended_before_a_change_poll_is_recorded_once() {
        let mut rig = rig("links-ac3-here-short", |repo, _, _| {
            write_task(repo, "t-1", "one")
        });
        let run = run_t1(&mut rig, Isolation::Here);
        let c1 = commit_in(&rig.repo, "one");
        rig.feed.act(Intent::ClosePane(run.run.clone()));
        rig.until("closed", |v| v.runs.iter().all(|r| r.run.closed.is_some()));
        rig.feed.act(Intent::Refresh);
        rig.settle();
        assert_eq!(rig.seen(), [(run.run.to_string(), c1)]);
        commit_in(&rig.repo, "two");
        rig.feed.act(Intent::Refresh);
        rig.settle();
        assert_eq!(rig.seen().len(), 1, "not after that");
    }

    /// AC-3 (Tasks): a last read of an ended この作業ツリー run that could
    /// not be written is tried again at the next poll, once.
    #[test]
    fn links_ac3_a_here_runs_last_head_is_retried_after_a_failed_write() {
        let mut rig = rig("links-ac3-here-retry", |repo, _, _| {
            write_task(repo, "t-1", "one")
        });
        let run = run_t1(&mut rig, Isolation::Here);
        let c1 = commit_in(&rig.repo, "one");
        rig.feed.act(Intent::ClosePane(run.run.clone()));
        rig.until("closed", |v| v.runs.iter().all(|r| r.run.closed.is_some()));
        let mut lock = start::links_of(&rig.log).into_os_string();
        lock.push(".lock");
        let lock = PathBuf::from(lock);
        let _ = fs::remove_file(&lock);
        std::os::unix::fs::symlink(rig.root.join("elsewhere"), &lock).unwrap();
        rig.feed.act(Intent::Refresh);
        rig.settle();
        fs::remove_file(&lock).unwrap();
        assert!(rig.seen().is_empty(), "the write failed");
        rig.feed.act(Intent::Refresh);
        rig.settle();
        assert_eq!(rig.seen(), [(run.run.to_string(), c1)]);
        commit_in(&rig.repo, "two");
        rig.feed.act(Intent::Refresh);
        rig.settle();
        assert_eq!(rig.seen().len(), 1, "not after that");
    }

    /// AC-4 (Tasks): a commit a session named first is the head the poll
    /// compares with, so a reset back to the earlier version is seen.
    #[test]
    fn links_ac4_a_reset_from_a_version_a_session_named_first_is_seen() {
        let mut rig = rig("links-ac4-reset", |repo, _, _| {
            write_task(repo, "t-1", "one")
        });
        let t = local_ref("t-1");
        let run = run_t1(&mut rig, Isolation::Worktree);
        let a = commit_in(&run.workdir, "one");
        rig.feed.act(Intent::Refresh);
        rig.until_related(&t, "v1", |r| r.versions.len() == 1);
        let b = commit_in(&run.workdir, "two");
        let target = task_links::Target {
            toplevel: run.workdir.clone(),
            head_ref: None,
            head_sha: Some(b.clone()),
        };
        let matched = task_links::match_run(&rig.root, &target).expect("the run matches");
        task_links::link_session(
            &matched,
            SessionKind::Review,
            "rev-1",
            Some(&b),
            false,
            START,
        )
        .unwrap();
        git_ok(&run.workdir, &["reset", "-q", "--hard", &a]);
        rig.feed.act(Intent::Refresh);
        rig.settle();
        rig.tick();
        let r = rig.until_related(&t, "the review", |r| r.reviews.len() == 1);
        assert_eq!(
            r.current().map(|v| v.commit.as_str()),
            Some(a.as_str()),
            "{r:#?}"
        );
    }

    /// A run started before the link log (a `RunStarted`, no `ChangeBase`):
    /// the first poll records its base, not a version. Its base is where
    /// its branch was created, so a branch still there shows no version and
    /// one that moved before the upgrade shows its head as v1; the next
    /// commit after that is the next version.
    #[test]
    fn links_ac3_a_run_from_before_the_link_log_gets_a_base_not_a_version() {
        let mut rig = rig("links-ac3-old", |repo, log, _| {
            let base = start::head_of(repo).unwrap();
            git_ok(repo, &["branch", "task-run-1", &base]);
            git_ok(repo, &["branch", "task-run-2", &base]);
            let moved = commit_on(repo, &base, "before the upgrade");
            git_ok(repo, &["update-ref", "refs/heads/task-run-2", &moved]);
            for (run, task, at) in [("run-1", "t-1", START - 100), ("run-2", "t-2", START - 50)] {
                let workdir = repo.with_file_name(format!("gone-{run}"));
                let branch = format!("task-{run}");
                logged_run(log, run, &local_ref(task), Some(&branch), &workdir, at);
            }
        });
        let (t1, t2) = (local_ref("t-1"), local_ref("t-2"));
        let base = start::head_of(&rig.repo).unwrap();
        let moved = git_ok(&rig.repo, &["rev-parse", "refs/heads/task-run-2"]);
        rig.settle();
        rig.feed.act(Intent::Refresh);
        rig.settle();
        let bases: Vec<(String, String)> = rig
            .links()
            .into_iter()
            .filter_map(|e| match e {
                LinkEvent::ChangeBase { run, commit, .. } => Some((run, commit)),
                _ => None,
            })
            .collect();
        assert_eq!(
            bases,
            [
                ("run-1".to_owned(), base.clone()),
                ("run-2".to_owned(), base.clone())
            ]
        );
        assert_eq!(rig.seen(), [("run-2".to_owned(), moved.clone())]);
        let r1 = rig.until_related(&t1, "run-1", |r| r.runs.len() == 1);
        assert!(r1.versions.is_empty(), "{r1:#?}");
        let r2 = rig.until_related(&t2, "v1", |r| r.versions.len() == 1);
        assert_eq!(
            (r2.versions[0].no, r2.versions[0].commit.as_str()),
            (1, moved.as_str())
        );

        let c1 = commit_on(&rig.repo, &base, "one");
        git_ok(&rig.repo, &["update-ref", "refs/heads/task-run-1", &c1]);
        rig.feed.act(Intent::Refresh);
        let r1 = rig.until_related(&t1, "v1", |r| r.versions.len() == 1);
        assert_eq!(
            (r1.versions[0].no, r1.versions[0].commit.as_str()),
            (1, c1.as_str())
        );
        assert_eq!(
            rig.links()
                .iter()
                .filter(|e| matches!(e, LinkEvent::ChangeBase { .. }))
                .count(),
            2,
            "a base is recorded once"
        );
    }

    /// AC-5 (Tasks): 開く on a review or a verification starts Review or
    /// Verify on the stored session; a PR's 開く and worktree の場所を開く go
    /// to the opener, ペインを開く to Noa; a removed worktree is said to be
    /// gone instead of opened.
    #[test]
    fn links_ac5_open_buttons_reach_the_launcher_the_opener_and_noa() {
        let mut rig = rig_with(
            "links-ac5",
            |repo, _, _| write_task(repo, "t-1", "one"),
            true,
        );
        let data = rig.root.parent().unwrap().to_path_buf();
        for (dir, id) in [("reviews", "rev-1"), ("verifications", "ver-1")] {
            fs::create_dir_all(data.join(dir).join(id)).unwrap();
            fs::write(data.join(dir).join(id).join("events.jsonl"), "").unwrap();
        }
        let run = run_t1(&mut rig, Isolation::Worktree);

        rig.feed.act(Intent::OpenSession {
            kind: SessionKind::Review,
            session: "rev-1".into(),
        });
        rig.until("opened", |v| {
            v.notice.as_deref() == Some("rev-1 を開きました")
        });
        rig.feed.act(Intent::OpenSession {
            kind: SessionKind::Verify,
            session: "ver-1".into(),
        });
        rig.until("opened", |v| {
            v.notice.as_deref() == Some("ver-1 を開きました")
        });
        let session = |dir: &str, id: &str| {
            let mut arg = OsString::from("--session=");
            arg.push(data.join(dir).join(id));
            vec![arg]
        };
        assert_eq!(
            *rig.launched.lock().unwrap(),
            [
                (sibling::REVIEW, session("reviews", "rev-1")),
                (sibling::VERIFY, session("verifications", "ver-1")),
            ]
        );
        // A session with no stored folder (or a bad id) starts nothing.
        for bad in ["rev-none", "../rev-1"] {
            rig.feed.act(Intent::OpenSession {
                kind: SessionKind::Review,
                session: bad.into(),
            });
            rig.until("refused", |v| {
                v.notice.as_deref() == Some(&format!("{bad} の記録が見つかりません"))
            });
        }
        assert_eq!(rig.launched.lock().unwrap().len(), 2);

        let url = "https://github.com/o/n/pull/45";
        rig.feed.act(Intent::OpenUrl(url.into()));
        rig.feed.act(Intent::OpenPane(run.run.clone()));
        rig.until("focused", |v| {
            v.notice
                .as_deref()
                .is_some_and(|n| n.contains("ペインを開きました"))
        });
        assert_eq!(rig.noa.with(|s| s.focused.clone()).len(), 1);
        rig.feed.act(Intent::OpenWorktree(run.run.clone()));
        rig.settle();
        let workdir = run.workdir.display().to_string();
        {
            let opened = rig.opened.lock().unwrap();
            assert!(opened.contains(&url.to_owned()), "{opened:?}");
            assert!(opened.contains(&workdir), "{opened:?}");
        }
        git_ok(&rig.repo, &["worktree", "remove", "--force", &workdir]);
        let before = rig
            .opened
            .lock()
            .unwrap()
            .iter()
            .filter(|o| **o == workdir)
            .count();
        rig.feed.act(Intent::OpenWorktree(run.run.clone()));
        let gone = format!("{workdir} はもうありません（worktree は削除済み）");
        rig.until("gone", |v| v.notice.as_deref() == Some(gone.as_str()));
        let after = rig
            .opened
            .lock()
            .unwrap()
            .iter()
            .filter(|o| **o == workdir)
            .count();
        assert_eq!(after, before, "a removed worktree is not opened");
    }

    #[test]
    fn current_version_buttons_pin_base_revision_and_origin_and_refuse_a_stale_version() {
        let mut rig = rig_with(
            "version-handoff",
            |repo, _, _| write_task(repo, "t-1", "one"),
            true,
        );
        let task = local_ref("t-1");
        let base = start::head_of(&rig.repo).unwrap();
        let run = run_t1(&mut rig, Isolation::Worktree);
        // The fixture starts a board tab and the original agent float.
        // Opening either setup form must preserve that exact spawn record.
        let started_panes = rig.noa.with(|state| state.spawned.clone());
        assert!(
            matches!(
                started_panes.as_slice(),
                [
                    (ipc::Placement::Board, _, _),
                    (ipc::Placement::Float(_), _, _)
                ]
            ),
            "{started_panes:?}"
        );
        let first = commit_in(&run.workdir, "first change");
        rig.feed.act(Intent::Refresh);
        rig.until_related(&task, "the recorded revision", |related| {
            related
                .current()
                .is_some_and(|version| version.commit == first)
        });
        for kind in [SessionKind::Review, SessionKind::Verify] {
            rig.feed.act(Intent::CheckVersion {
                task: task.clone(),
                kind,
                run: run.run.clone(),
                commit: first.clone(),
            });
            rig.settle();
            assert_eq!(
                rig.noa.with(|state| state.spawned.clone()),
                started_panes,
                "opening the {kind:?} setup form must not start another pane"
            );
        }
        let launches = rig.launched.lock().unwrap().clone();
        assert_eq!(launches.len(), 2);
        assert_eq!(launches[0].0, sibling::REVIEW);
        assert_eq!(launches[1].0, sibling::VERIFY);
        for (_, args) in launches {
            assert_eq!(
                args,
                [
                    OsString::from(format!("--repo={}", rig.repo.display())),
                    format!("--base={base}").into(),
                    format!("--revision={first}").into(),
                    format!("--origin-repo={}", rig.repo.display()).into(),
                    "--origin-task=t-1".into(),
                    format!("--origin-run={}", run.run).into(),
                ]
            );
        }
        let second = commit_in(&run.workdir, "second change");
        rig.feed.act(Intent::Refresh);
        rig.until_related(&task, "the new revision", |related| {
            related
                .current()
                .is_some_and(|version| version.commit == second)
        });
        rig.feed.act(Intent::CheckVersion {
            task,
            kind: SessionKind::Review,
            run: run.run,
            commit: first,
        });
        rig.until("stale target refused", |view| {
            view.notice
                .as_deref()
                .is_some_and(|notice| notice.contains("版が変わりました"))
        });
        assert_eq!(rig.launched.lock().unwrap().len(), 2);
        assert_eq!(
            rig.noa.with(|state| state.spawned.clone()),
            started_panes,
            "refusing a stale version must not start another pane"
        );
    }

    #[test]
    fn workflow_handoff_preserves_task_identity_without_starting_a_run() {
        let mut rig = rig_with(
            "workflow-handoff",
            |repo, _, _| write_task(repo, "t-1", "one"),
            true,
        );
        let task = local_ref("t-1");
        rig.until("the task", |view| {
            view.tasks.iter().any(|item| item.r#ref == task)
        });
        rig.feed.act(Intent::OpenWorkflow(task.clone()));
        rig.until("workflow form", |view| {
            view.notice
                .as_deref()
                .is_some_and(|notice| notice.contains("Workflow の開始フォーム"))
        });
        let launches = rig.launched.lock().unwrap().clone();
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0].0, WORKFLOW_HELPER);
        assert_eq!(launches[0].1.len(), 2);
        let origin = launches[0].1[1]
            .to_str()
            .unwrap()
            .strip_prefix("--task-origin=")
            .unwrap();
        let request: serde_json::Value =
            serde_json::from_slice(&fs::read(origin).unwrap()).unwrap();
        assert_eq!(request["repo"], rig.repo.to_str().unwrap());
        assert_eq!(request["task"], "t-1");
        assert_eq!(
            serde_json::from_str::<TaskRef>(request["task_key"].as_str().unwrap()).unwrap(),
            task
        );
        assert!(request["objective"].as_str().unwrap().contains("one"));
        assert!(rig.noa.with(|state| state.spawned.is_empty()));
        assert!(runs::replay(&rig.log).unwrap().is_empty());
    }

    /// AC-6: 関連を追加 writes PrLinked and the PR shows; the same URL on the
    /// task again and a number without a known repository are refused;
    /// 外す writes LinkRemoved and the PR, or an automatic review link, is
    /// gone, also in a new window; a removed PR added again shows once.
    #[test]
    fn links_ac6_add_and_remove_links_are_written_to_the_log() {
        let mut rig = rig("links-ac6", |repo, log, _| {
            write_task(repo, "t-1", "one");
            logged_run(log, "run-1", &local_ref("t-1"), None, repo, START - 10);
            task_links::append(
                &start::links_of(log),
                &LinkEvent::SessionLinked {
                    kind: SessionKind::Review,
                    session: "rev-1".into(),
                    task: task_key(&local_ref("t-1")),
                    run: "run-1".into(),
                    commit: None,
                    uncommitted: false,
                    by: task_links::MatchedBy::Commit,
                    at: START - 5,
                },
            )
            .unwrap();
        });
        let t = local_ref("t-1");
        rig.until_related(&t, "the review", |r| r.reviews.len() == 1);

        rig.feed.act(Intent::AddLink {
            task: t.clone(),
            input: "#45".into(),
        });
        rig.until("refused", |v| {
            v.notice.as_deref() == Some("GitHub のリポジトリがわからないため番号では追加できません")
        });
        let url = "https://github.com/o/n/pull/45";
        rig.feed.act(Intent::AddLink {
            task: t.clone(),
            input: format!("{url}/files"),
        });
        let related = rig.until_related(&t, "the PR", |r| r.prs.len() == 1);
        assert_eq!(related.prs[0].url, url);
        assert_eq!(related.prs[0].number, Some(45));
        let id = related.prs[0].id.clone();
        assert!(rig.links().contains(&LinkEvent::PrLinked {
            id: id.clone(),
            task: task_key(&t),
            url: url.into(),
            at: START
        }));
        rig.feed.act(Intent::AddLink {
            task: t.clone(),
            input: url.into(),
        });
        rig.until("refused", |v| {
            v.notice.as_deref() == Some("この PR はもう関連づいています")
        });
        rig.feed.act(Intent::AddLink {
            task: t.clone(),
            input: "https://github.com/o/n/issues/45".into(),
        });
        rig.until("refused", |v| {
            v.notice
                .as_deref()
                .is_some_and(|n| n.starts_with("PR の URL"))
        });
        assert_eq!(rig.until_related(&t, "one PR", |_| true).prs.len(), 1);

        rig.feed.act(Intent::RemoveLink {
            task: t.clone(),
            id: id.clone(),
        });
        rig.until_related(&t, "the PR gone", |r| r.prs.is_empty());
        rig.feed.act(Intent::RemoveLink {
            task: t.clone(),
            id: "rev-1".into(),
        });
        rig.until_related(&t, "the review gone", |r| r.reviews.is_empty());
        assert!(rig.links().contains(&LinkEvent::LinkRemoved {
            task: task_key(&t),
            id: "rev-1".into(),
            at: START
        }));
        let mut other = second_window(&rig);
        until_related_on(&mut other, &t, "gone after a reload", |r| {
            r.prs.is_empty() && r.reviews.is_empty() && r.runs.len() == 1
        });

        rig.feed.act(Intent::AddLink {
            task: t.clone(),
            input: url.into(),
        });
        let related = rig.until_related(&t, "the PR again", |r| r.prs.len() == 1);
        assert_ne!(related.prs[0].id, id, "a new id");
        rig.settle();
        assert_eq!(rig.until_related(&t, "once", |_| true).prs.len(), 1);
    }

    /// AC-7: a task with two runs (two branches, three commits) lists both
    /// runs and v1…v3 across them; one PR on t-1 and #12 shows on both, and
    /// removing it from t-1 keeps it on #12.
    #[test]
    fn links_ac7_two_runs_share_the_versions_and_one_pr_shows_on_two_tasks() {
        let mut rig = rig("links-ac7", |repo, log, _| {
            let t = local_ref("t-1");
            let base = start::head_of(repo).unwrap();
            for (run, at) in [("run-1", START - 100), ("run-2", START - 50)] {
                let workdir = repo.with_file_name(format!("gone-{run}"));
                logged_run(log, run, &t, Some(&format!("task-{run}")), &workdir, at);
                task_links::append(
                    &start::links_of(log),
                    &LinkEvent::ChangeBase {
                        run: run.into(),
                        commit: base.clone(),
                        at,
                    },
                )
                .unwrap();
            }
        });
        let t = local_ref("t-1");
        let base = start::head_of(&rig.repo).unwrap();
        let c1 = commit_on(&rig.repo, &base, "one");
        git_ok(&rig.repo, &["update-ref", "refs/heads/task-run-1", &c1]);
        rig.feed.act(Intent::Refresh);
        rig.until_related(&t, "v1", |r| r.versions.len() == 1);
        let c2 = commit_on(&rig.repo, &c1, "two");
        git_ok(&rig.repo, &["update-ref", "refs/heads/task-run-1", &c2]);
        rig.feed.act(Intent::Refresh);
        rig.until_related(&t, "v2", |r| r.versions.len() == 2);
        let c3 = commit_on(&rig.repo, &c2, "three");
        git_ok(&rig.repo, &["update-ref", "refs/heads/task-run-2", &c3]);
        rig.feed.act(Intent::Refresh);
        let related = rig.until_related(&t, "v3", |r| r.versions.len() == 3);
        assert_eq!(related.runs, [RunId::new("run-2"), RunId::new("run-1")]);
        let versions: Vec<(u32, &str, &str)> = related
            .versions
            .iter()
            .map(|v| (v.no, v.commit.as_str(), v.run.as_str()))
            .collect();
        assert_eq!(
            versions,
            [
                (1, c1.as_str(), "run-1"),
                (2, c2.as_str(), "run-1"),
                (3, c3.as_str(), "run-2")
            ]
        );

        let issue = TaskRef::GitHub {
            repo: "o/n".into(),
            number: 12,
        };
        let url = "https://github.com/o/n/pull/45";
        for task in [&t, &issue] {
            rig.feed.act(Intent::AddLink {
                task: task.clone(),
                input: url.into(),
            });
            rig.until_related(task, "the PR", |r| r.prs.len() == 1);
        }
        let id = rig.until_related(&t, "the PR", |r| r.prs.len() == 1).prs[0]
            .id
            .clone();
        rig.feed.act(Intent::RemoveLink {
            task: t.clone(),
            id,
        });
        rig.until_related(&t, "gone from t-1", |r| r.prs.is_empty());
        let kept = rig.until_related(&issue, "kept on #12", |r| r.prs.len() == 1);
        assert_eq!(kept.prs[0].url, url);
    }

    mod timeline {
        use super::*;
        use noa_ipc::ClientError;
        use noa_ipc::timeline::{
            PostParams, PostResult, PublishOwner, RegisterRunParams, RegisterRunResult, Seq,
        };
        use noa_ipc::timeline_outbox::PostSink;

        #[derive(Default)]
        struct Posted {
            posts: Vec<PostParams>,
            runs: Vec<RegisterRunParams>,
            /// The seq each request was stored under (a re-send gets it back).
            stored: HashMap<String, u64>,
            /// The next post is stored and its answer lost.
            lose_next: bool,
            /// The next post is answered once this says so.
            gate: Option<std::sync::mpsc::Receiver<()>>,
        }

        #[derive(Clone, Default)]
        struct Tl(Arc<Mutex<Posted>>);

        impl PostSink for Tl {
            fn post(&mut self, p: &PostParams) -> Result<PostResult, ClientError> {
                let gate = {
                    let mut w = self.0.lock().unwrap();
                    w.posts.push(p.clone());
                    w.gate.take()
                };
                if let Some(gate) = gate {
                    let _ = gate.recv();
                }
                let mut w = self.0.lock().unwrap();
                let next = w.stored.len() as u64 + 1;
                let duplicate = w.stored.contains_key(&p.request_id);
                let seq = *w.stored.entry(p.request_id.clone()).or_insert(next);
                if std::mem::take(&mut w.lose_next) {
                    return Err(ClientError::Timeout("noa.timeline.post".into()));
                }
                Ok(PostResult {
                    created_at: "2026-11-26T00:00:00Z".into(),
                    duplicate,
                    post_id: format!("po-{seq:016x}"),
                    seq: Seq(seq),
                })
            }
        }

        fn published(seq: u64) -> Status {
            Status::Published {
                seq,
                post_id: format!("po-{seq:016x}"),
            }
        }

        /// The timeline `tl` behind a link with its outbox in `rig`'s data;
        /// `up` says whether Noa can be reached (always, when `None`).
        fn link_to(rig: &Rig, tl: &Tl, up: Option<Arc<std::sync::atomic::AtomicBool>>) -> Job {
            let shared = tl.clone();
            Job::Timeline(Box::new(TimelineLink {
                outbox_root: rig.root.join("outbox"),
                connect: Box::new(move || match &up {
                    Some(up) if !up.load(Ordering::SeqCst) => Err("Noa に接続できません".into()),
                    _ => Ok(Box::new(shared.clone()) as Box<dyn TimelineApi>),
                }),
            }))
        }

        /// Starts a run of the rig's task `t-1` and waits for its float.
        fn started(rig: &mut Rig) -> RunId {
            rig.until("the task", |v| v.tasks.len() == 1);
            rig.feed.act(Intent::Run {
                task: TaskRef::Local("t-1".into()),
                agent: AgentKind::Claude,
                isolation: Isolation::Here,
                auto: None,
            });
            let alive = |run: &&RunView| matches!(run.liveness, Liveness::Alive { .. });
            let view = rig.until("実行中", |v| v.runs.iter().any(|run| alive(&run)));
            view.runs.iter().find(alive).unwrap().run.run.clone()
        }

        impl TimelineApi for Tl {
            fn supports(&self) -> bool {
                true
            }

            fn register_run(
                &mut self,
                p: &RegisterRunParams,
            ) -> Result<RegisterRunResult, ClientError> {
                let mut w = self.0.lock().unwrap();
                w.runs.push(p.clone());
                Ok(RegisterRunResult {
                    board_id: "bd-0000000000000001".into(),
                    created: w.runs.len() == 1,
                    producer_id: p.publish_policy.producer_id.clone().unwrap(),
                    run_id: "rn-0000000000000001".into(),
                    worker_id: Some("wk-0000000000000001".into()),
                })
            }
        }

        /// Polls until `run`'s share state is one `done` takes.
        fn until_shared(rig: &mut Rig, run: &RunId, done: impl Fn(&Status) -> bool) -> Status {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                rig.feed.poll();
                if let Some(state) = rig.feed.timeline().and_then(|all| all.get(run))
                    && done(state)
                {
                    return state.clone();
                }
                assert!(Instant::now() < deadline, "no share state for {run}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        #[test]
        fn ac5_a_run_is_registered_at_start_and_posted_only_when_shared() {
            let mut rig = rig("timeline", |repo, _, _| write_task(repo, "t-1", "フォント"));
            let tl = Tl::default();
            let shared = tl.clone();
            rig.feed.send(Job::Timeline(Box::new(TimelineLink {
                outbox_root: rig.root.join("outbox"),
                connect: Box::new(move || Ok(Box::new(shared.clone()) as Box<dyn TimelineApi>)),
            })));
            rig.until("the task", |v| v.tasks.len() == 1);
            let task = TaskRef::Local("t-1".into());
            rig.feed.act(Intent::Run {
                task: task.clone(),
                agent: AgentKind::Claude,
                isolation: Isolation::Here,
                auto: None,
            });
            let view = rig.until("実行中", |v| {
                v.runs
                    .iter()
                    .any(|run| matches!(run.liveness, Liveness::Alive { .. }))
            });
            let run = view.runs[0].run.clone();
            {
                let posted = tl.0.lock().unwrap();
                assert_eq!(posted.runs.len(), 1, "registered once its float is up");
                let reg = &posted.runs[0];
                assert_eq!(reg.worker_pane_id.map(|p| p.0), run.pane);
                assert_eq!(reg.orchestrator_pane_id.map(|p| p.0), run.board);
                assert_eq!(reg.publish_policy.owner, PublishOwner::Runner);
                let key = tasks::timeline::session_key(&store::repo_key(&rig.repo));
                assert_eq!(reg.request_id, format!("tasks-{key}-run-{}", run.run));
                assert_eq!(
                    reg.publish_policy.producer_id.as_deref(),
                    Some(format!("tasks:{key}").as_str())
                );
                assert_eq!(reg.task_ref.as_ref().unwrap().id, "t-1");
                assert!(posted.posts.is_empty(), "nothing posted before 共有");
            }

            let result = runs::result_file(&rig.repo, &run.run);
            fs::write(&result, "# 報告\n\n直しました\n").unwrap();
            let log = fs::read(&rig.log).unwrap();
            rig.feed.act(Intent::Share {
                run: run.run.clone(),
                outcome: Some(Outcome::Succeeded),
            });
            let published = |s: &Status| matches!(s, Status::Published { .. });
            assert_eq!(
                until_shared(&mut rig, &run.run, published),
                self::published(1)
            );
            let posted = tl.0.lock().unwrap();
            assert_eq!(posted.posts.len(), 1);
            let post = &posted.posts[0];
            let key = tasks::timeline::session_key(&store::repo_key(&rig.repo));
            assert_eq!(post.request_id, format!("tasks-{key}-result-{}", run.run));
            assert_eq!(post.post.outcome, Outcome::Succeeded);
            assert_eq!(post.post.title, "タスク結果: フォント");
            assert_eq!(post.post.summary, "報告");
            assert_eq!(post.post.body_markdown, "# 報告\n\n直しました\n");
            assert_eq!(post.post.artifacts[0].r#ref, result.display().to_string());
            // Sharing reads the run's files and writes neither.
            assert_eq!(fs::read(&rig.log).unwrap(), log);
            assert_eq!(
                fs::read_to_string(&result).unwrap(),
                "# 報告\n\n直しました\n"
            );
        }

        #[test]
        fn ac5_a_share_that_cannot_reach_noa_fails_and_retries_with_its_outcome() {
            let mut rig = rig("timeline-down", |repo, _, _| {
                write_task(repo, "t-1", "フォント")
            });
            let tl = Tl::default();
            let up = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (shared, gate) = (tl.clone(), up.clone());
            rig.feed.send(Job::Timeline(Box::new(TimelineLink {
                outbox_root: rig.root.join("outbox"),
                connect: Box::new(move || {
                    if gate.load(Ordering::SeqCst) {
                        Ok(Box::new(shared.clone()) as Box<dyn TimelineApi>)
                    } else {
                        Err("Noa に接続できません".into())
                    }
                }),
            })));
            rig.until("the task", |v| v.tasks.len() == 1);
            rig.feed.act(Intent::Run {
                task: TaskRef::Local("t-1".into()),
                agent: AgentKind::Claude,
                isolation: Isolation::Here,
                auto: None,
            });
            let view = rig.until("実行中", |v| {
                v.runs
                    .iter()
                    .any(|run| matches!(run.liveness, Liveness::Alive { .. }))
            });
            let run = view.runs[0].run.run.clone();
            fs::write(runs::result_file(&rig.repo, &run), "結果\n").unwrap();
            rig.feed.act(Intent::Share {
                run: run.clone(),
                outcome: Some(Outcome::Succeeded),
            });
            let failed = until_shared(&mut rig, &run, Status::failed);
            assert_eq!(failed.detail().as_deref(), Some("Noa に接続できません"));
            // Written again after the share: the share keeps what it was.
            fs::write(runs::result_file(&rig.repo, &run), "あとで書き換え\n").unwrap();
            up.store(true, Ordering::SeqCst);
            rig.feed.act(Intent::Share {
                run: run.clone(),
                outcome: None,
            });
            let published = |s: &Status| matches!(s, Status::Published { .. });
            assert_eq!(until_shared(&mut rig, &run, published), self::published(1));
            let posted = tl.0.lock().unwrap();
            assert_eq!(
                posted.posts[0].post.outcome,
                Outcome::Succeeded,
                "the asked outcome"
            );
            assert_eq!(posted.posts[0].post.body_markdown, "結果\n", "as shared");
        }

        // Review defects 4 and 5: a share made while Noa was down is saved
        // with its outcome; a restart sends it, as it was shared, with no
        // press.
        #[test]
        fn ac5_a_share_saved_before_an_exit_goes_out_at_the_next_start() {
            let mut rig = rig("timeline-restart-saved", |repo, _, _| {
                write_task(repo, "t-1", "フォント")
            });
            let tl = Tl::default();
            let up = Arc::new(std::sync::atomic::AtomicBool::new(false));
            rig.feed.send(link_to(&rig, &tl, Some(up.clone())));
            let run = started(&mut rig);
            fs::write(runs::result_file(&rig.repo, &run), "共有した結果\n").unwrap();
            rig.feed.act(Intent::Share {
                run: run.clone(),
                outcome: Some(Outcome::Succeeded),
            });
            until_shared(&mut rig, &run, Status::failed);
            fs::write(runs::result_file(&rig.repo, &run), "書き換え\n").unwrap();
            // Quit and start again, Noa up now.
            up.store(true, Ordering::SeqCst);
            rig.feed = second_window(&rig);
            rig.feed.send(link_to(&rig, &tl, None));
            let published = |s: &Status| matches!(s, Status::Published { .. });
            assert_eq!(until_shared(&mut rig, &run, published), self::published(1));
            let posted = tl.0.lock().unwrap();
            assert_eq!(posted.posts.len(), 1);
            assert_eq!(posted.posts[0].post.outcome, Outcome::Succeeded);
            assert_eq!(posted.posts[0].post.body_markdown, "共有した結果\n");
        }

        // Review defects 4 and 6: a share whose answer was lost goes out
        // again at the next start, from the outbox alone (the work tree's
        // result file gone meanwhile), and the server keeps one.
        #[test]
        fn ac5_a_share_whose_answer_was_lost_is_resent_at_start_without_its_file() {
            let mut rig = rig("timeline-restart-lost", |repo, _, _| {
                write_task(repo, "t-1", "フォント")
            });
            let tl = Tl::default();
            tl.0.lock().unwrap().lose_next = true;
            rig.feed.send(link_to(&rig, &tl, None));
            let run = started(&mut rig);
            let result = runs::result_file(&rig.repo, &run);
            fs::write(&result, "結果\n").unwrap();
            rig.feed.act(Intent::Share {
                run: run.clone(),
                outcome: None,
            });
            let failed = until_shared(&mut rig, &run, Status::failed);
            assert_eq!(
                failed.detail().as_deref(),
                Some("timed out waiting for the post acknowledgement")
            );
            // The work tree is cleaned up.
            fs::remove_dir_all(result.parent().unwrap()).unwrap();
            rig.feed = second_window(&rig);
            rig.feed.send(link_to(&rig, &tl, None));
            let published = |s: &Status| matches!(s, Status::Published { .. });
            assert_eq!(until_shared(&mut rig, &run, published), self::published(1));
            {
                let posted = tl.0.lock().unwrap();
                assert_eq!(posted.posts.len(), 2);
                assert_eq!(posted.posts[0], posted.posts[1], "the stored envelope");
                assert_eq!(posted.stored.len(), 1, "stored once");
            }
        }

        // Review defect 6: 再試行 of a share whose post timed out does not
        // need the result file any more.
        #[test]
        fn ac5_a_retry_needs_only_the_outbox() {
            let mut rig = rig("timeline-retry-no-file", |repo, _, _| {
                write_task(repo, "t-1", "フォント")
            });
            let tl = Tl::default();
            tl.0.lock().unwrap().lose_next = true;
            rig.feed.send(link_to(&rig, &tl, None));
            let run = started(&mut rig);
            let result = runs::result_file(&rig.repo, &run);
            fs::write(&result, "結果\n").unwrap();
            rig.feed.act(Intent::Share {
                run: run.clone(),
                outcome: Some(Outcome::Partial),
            });
            until_shared(&mut rig, &run, Status::failed);
            fs::remove_dir_all(result.parent().unwrap()).unwrap();
            rig.feed.act(Intent::Share {
                run: run.clone(),
                outcome: None,
            });
            let published = |s: &Status| matches!(s, Status::Published { .. });
            assert_eq!(until_shared(&mut rig, &run, published), self::published(1));
            let posted = tl.0.lock().unwrap();
            assert_eq!(posted.posts[0], posted.posts[1]);
            assert_eq!(posted.posts[1].post.outcome, Outcome::Partial);
        }

        // Review defect 11: the share shows pending while its post is on
        // its way.
        #[test]
        fn ac5_a_share_in_flight_shows_pending() {
            let mut rig = rig("timeline-in-flight", |repo, _, _| {
                write_task(repo, "t-1", "フォント")
            });
            let tl = Tl::default();
            let (release, gate) = std::sync::mpsc::channel();
            tl.0.lock().unwrap().gate = Some(gate);
            rig.feed.send(link_to(&rig, &tl, None));
            let run = started(&mut rig);
            fs::write(runs::result_file(&rig.repo, &run), "結果\n").unwrap();
            rig.feed.act(Intent::Share {
                run: run.clone(),
                outcome: None,
            });
            assert_eq!(
                until_shared(&mut rig, &run, |s| *s == Status::Pending),
                Status::Pending
            );
            release.send(()).unwrap();
            let published = |s: &Status| matches!(s, Status::Published { .. });
            assert_eq!(until_shared(&mut rig, &run, published), self::published(1));
        }

        // Review defect 6: a share sent again at start wakes the screen
        // when its answer comes, as a share by hand does with its notice.
        #[test]
        fn ac5_a_share_resent_at_start_wakes_the_screen_when_answered() {
            let mut rig = rig("timeline-restart-wake", |repo, _, _| {
                write_task(repo, "t-1", "フォント")
            });
            let tl = Tl::default();
            let up = Arc::new(std::sync::atomic::AtomicBool::new(false));
            rig.feed.send(link_to(&rig, &tl, Some(up.clone())));
            let run = started(&mut rig);
            fs::write(runs::result_file(&rig.repo, &run), "結果\n").unwrap();
            rig.feed.act(Intent::Share {
                run: run.clone(),
                outcome: None,
            });
            until_shared(&mut rig, &run, Status::failed);
            up.store(true, Ordering::SeqCst);
            let (release, gate) = std::sync::mpsc::channel();
            tl.0.lock().unwrap().gate = Some(gate);
            let wakes = Arc::new(AtomicU64::new(0));
            let counted = wakes.clone();
            rig.feed = second_window_waking(
                &rig,
                Arc::new(move || {
                    counted.fetch_add(1, Ordering::SeqCst);
                }),
            );
            rig.feed.send(link_to(&rig, &tl, None));
            // The resend is in its post: the worker woke for 投稿待ち
            // before it and does nothing else until the answer.
            let deadline = Instant::now() + Duration::from_secs(10);
            while tl.0.lock().unwrap().gate.is_some() {
                assert!(Instant::now() < deadline, "the resend at start");
                std::thread::sleep(Duration::from_millis(10));
            }
            let before = wakes.load(Ordering::SeqCst);
            release.send(()).unwrap();
            let published = |s: &Status| matches!(s, Status::Published { .. });
            assert_eq!(until_shared(&mut rig, &run, published), self::published(1));
            let deadline = Instant::now() + Duration::from_secs(10);
            while wakes.load(Ordering::SeqCst) == before {
                assert!(Instant::now() < deadline, "the answer woke no one");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        // Review defect 2: a run log edited to hold a path for a run id
        // touches nothing outside the outbox root at start.
        #[test]
        fn ac5_a_run_id_that_is_a_path_is_skipped_at_start() {
            use std::os::unix::fs::PermissionsExt;
            let outside = Temp::new("timeline-outside");
            let target = outside.0.join("not-an-outbox");
            fs::create_dir_all(&target).unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
            let id = target.display().to_string();
            // What the request-id check alone let through.
            assert!(noa_ipc::timeline::valid_token(
                &tasks::timeline::run_request_id("k", &id)
            ));
            let mut rig = rig("timeline-path-id", |repo, log, _| {
                write_task(repo, "t-1", "フォント");
                runs::append(
                    log,
                    &RunEvent::RunStarted {
                        run: RunId::new(id.clone()),
                        task: TaskRef::Local("t-1".into()),
                        agent: AgentKind::Claude,
                        workdir: repo.to_owned(),
                        branch: None,
                        label: runs::label(&RunId::new(id.clone())),
                        at: 1,
                        auto: None,
                    },
                )
                .unwrap();
            });
            let tl = Tl::default();
            rig.feed.send(link_to(&rig, &tl, None));
            // A share of another run: the start was looked at before it.
            let run = started(&mut rig);
            fs::write(runs::result_file(&rig.repo, &run), "結果\n").unwrap();
            rig.feed.act(Intent::Share {
                run: run.clone(),
                outcome: None,
            });
            let published = |s: &Status| matches!(s, Status::Published { .. });
            until_shared(&mut rig, &run, published);
            let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o755, "left as it was");
            assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
            assert!(rig.feed.timeline().unwrap().get(&RunId::new(id)).is_none());
        }
    }
}
