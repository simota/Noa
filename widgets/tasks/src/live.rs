//! The live [`Feed`]: local task files, the run log, Noa's panes (over
//! [`ipc`]) and the GitHub source, each read off the UI thread; a new
//! [`View`] whenever any of them changed.
//!
//! One worker thread owns the files, the run log and the link to Noa: it
//! takes the intents that write or reach Noa, looks at `.noa/tasks/`,
//! `noa.listPanels` and the runs' result files at most every `POLL_MS`, and
//! sends what it sees. It also keeps the link store (`links.rs`): the
//! `Ran` backfill on its first look, the changes a run produced once it
//! ends, and the 関連成果 of the task whose detail is open, read when it
//! opens, every [`RELATED_EVERY`] and on 再取得. The GitHub source ([`IssueFeed`]) runs `gh` on its
//! own threads and never blocks, so the UI thread asks it directly.

pub mod ipc;
pub mod linking;
pub mod start;

use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tasks::github::{GhFeed, IssueFeed};
use tasks::links::{self, Edit, LinkId, Nav};
use tasks::local::{self, LocalFile, Scan};
use tasks::model::types::{
    AgentKind, AgentState, Liveness, Millis, POLL_MS, PaneFact, Report, ResultFact, Run, RunEvent,
    RunId, RunView, State, Task, TaskRef,
};
use tasks::{code, runs, store, worktree};

use crate::installed::{self, Installed};
use crate::ui::{Feed, Intent, LinkKind, RelatedView, View};

use ipc::{Connect, IpcConnect, Noa, NoaError, PaneView};
use linking::Places;
use start::RunRequest;

/// How often the worker looks at its clock between intents.
const TICK: Duration = Duration::from_millis(100);
const MINUTE: Millis = 60_000;
/// How often the code's markers are read again (`git grep`), besides 再取得.
const CODE_EVERY: Millis = MINUTE;
/// How often an open detail's 関連成果 is read again (it runs a diff).
const RELATED_EVERY: Millis = 10_000;

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
    /// Read the code's markers again now (再取得).
    ScanCode,
    /// The task whose detail is open: its 関連成果 is read now and every
    /// [`RELATED_EVERY`].
    Watch(Option<TaskRef>),
    /// Read the open detail's 関連成果 again now (再取得).
    RefreshRelated,
    Link {
        task: TaskRef,
        title: String,
        kind: LinkKind,
        id: String,
    },
    Unlink(Vec<LinkId>),
}

/// What the worker sends back.
enum Up {
    Seen {
        tasks: Vec<Task>,
        runs: Vec<RunView>,
    },
    Notice(String),
    Related(Box<RelatedView>),
}

/// Starts the program a [`Nav`] names; `Err` is the notice.
type Launcher = Box<dyn FnMut(&Nav) -> Result<(), String>>;

/// The live feed of one repository.
pub struct LiveFeed<C> {
    repo: PathBuf,
    issues: Box<dyn IssueFeed>,
    clock: Arc<dyn Fn() -> Millis + Send + Sync>,
    jobs: Sender<Job>,
    ups: Receiver<Up>,
    tasks: Vec<Task>,
    runs: Vec<RunView>,
    notice: Option<String>,
    agents: Option<Vec<AgentKind>>,
    installed: Option<Receiver<Installed>>,
    /// The last View handed out.
    shown: Option<View>,
    /// The 関連成果 last read, and whether the screen has it yet.
    related: Option<RelatedView>,
    related_new: bool,
    /// Starts the program a [`Nav`] names (`links::launch`; a test's records).
    launcher: Launcher,
    /// Tells the screen to poll: for what `act` says itself.
    wake: Arc<dyn Fn() + Send + Sync>,
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
            Arc::new(system_now),
            wake.clone(),
        );
        feed.installed = Some(installed::spawn(move || wake()));
        Ok(feed)
    }
}

impl<C: Connect + Send + 'static> LiveFeed<C> {
    /// `opener` opens a URL or a directory (`open` / `xdg-open` in `start`).
    pub fn with(
        repo: PathBuf,
        root: PathBuf,
        issues: Box<dyn IssueFeed>,
        connect: C,
        opener: Box<dyn FnMut(&str) + Send>,
        clock: Arc<dyn Fn() -> Millis + Send + Sync>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let (jobs, job_rx) = mpsc::channel();
        let (up_tx, ups) = mpsc::channel();
        let log = store::runs_log(&root, &store::repo_key(&repo));
        // `<data dir>/noa`: the runs logs' root is `<data dir>/noa/tasks`.
        let data = root.parent().unwrap_or(&root).to_path_buf();
        let (worker_repo, worker_clock, worker_wake) = (repo.clone(), clock.clone(), wake.clone());
        // Built on its thread: a connection need not be `Send`.
        let spawned = std::thread::Builder::new()
            .name("tasks-live".into())
            .spawn(move || {
                // Git, so on this thread: the store is the main worktree's.
                let links_root = data.join("links");
                let store = links::path(&links_root, &worker_repo);
                let main =
                    links::main_worktree(&worker_repo).unwrap_or_else(|_| worker_repo.clone());
                Worker {
                    scan: Scan::new(worker_repo.clone()),
                    links_root,
                    store,
                    data,
                    main,
                    backfilled: false,
                    phases: HashMap::new(),
                    live_since: HashMap::new(),
                    watch: None,
                    related_at: None,
                    link_failure: None,
                    repo: worker_repo,
                    log,
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
                }
                .run(job_rx)
            });
        let notice = spawned
            .err()
            .map(|e| format!("スレッドを開始できません: {e}"));
        LiveFeed {
            repo,
            issues,
            clock,
            jobs,
            ups,
            tasks: Vec::new(),
            runs: Vec::new(),
            notice,
            agents: None,
            installed: None,
            shown: None,
            related: None,
            related_new: false,
            launcher: Box::new(links::launch),
            wake,
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
    fn poll(&mut self) -> Option<View> {
        while let Ok(up) = self.ups.try_recv() {
            match up {
                Up::Seen { tasks, runs } => {
                    self.tasks = tasks;
                    self.runs = runs;
                }
                Up::Notice(notice) => self.notice = Some(notice),
                Up::Related(related) => {
                    self.related = Some(*related);
                    self.related_new = true;
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
        if self
            .shown
            .as_ref()
            .is_some_and(|shown| same_view(shown, &view))
        {
            return None;
        }
        self.shown = Some(view.clone());
        Some(view)
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
            } => match self.task(&task) {
                Some(task) => {
                    let req = RunRequest {
                        task,
                        repo: self.repo.clone(),
                        agent,
                        isolation,
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
                self.send(Job::RefreshRelated);
            }
            Intent::LoadDetail(number) => self.issues.load_detail(number),
            Intent::Link {
                task,
                title,
                kind,
                id,
            } => self.send(Job::Link {
                task,
                title,
                kind,
                id,
            }),
            Intent::Unlink { links, .. } => self.send(Job::Unlink(links)),
            // Starting a program is quick; it is reaped on its own thread.
            Intent::Navigate(nav) => match (self.launcher)(&nav) {
                Ok(()) => {
                    let what = nav.sibling().map_or("", |s| s.title);
                    self.say(format!("{what} で開きました"));
                }
                Err(why) => self.say(why),
            },
        }
    }

    fn watch(&mut self, task: Option<&TaskRef>) {
        // What was read of another task is not this one's.
        if self.related.as_ref().map(|r| &r.task) != task {
            self.related = None;
            self.related_new = false;
        }
        self.send(Job::Watch(task.cloned()));
    }

    fn related(&mut self) -> Option<RelatedView> {
        // Ups are drained by `poll`, which the screen calls first.
        std::mem::replace(&mut self.related_new, false)
            .then(|| self.related.clone())
            .flatten()
    }
}

/// The worker thread's state: everything that reads files or talks to Noa.
struct Worker<C: Connect> {
    repo: PathBuf,
    log: PathBuf,
    /// The link stores' root (`<data dir>/noa/links`), this repository's
    /// store under it (`Err`: why there is none), `<data dir>/noa` and
    /// the main worktree.
    links_root: PathBuf,
    store: Result<PathBuf, String>,
    data: PathBuf,
    main: PathBuf,
    /// Whether the first look was made: the runs logged before the store
    /// knew them linked, the runs ended by then seen.
    backfilled: bool,
    /// Each run's phase at the last look: a run's changes are recorded
    /// while it is live and as it ends, never after.
    phases: HashMap<RunId, Phase>,
    /// The `HEAD` of a run's work tree as it went live again (a
    /// follow-up): a run with no tree of its own produced only what was
    /// committed since (other work commits in between).
    live_since: HashMap<RunId, String>,
    /// The task whose detail is open, and when its 関連成果 was read.
    watch: Option<TaskRef>,
    related_at: Option<Millis>,
    /// Why the last automatic link failed, said once until it changes.
    link_failure: Option<String>,
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
    sent: Option<(Vec<Task>, Vec<RunView>)>,
}

/// Where a run is, for recording what it produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Starting, or its agent at work (or not reported).
    Live,
    /// Its agent finished or failed; the pane is still there.
    Done,
    /// Its pane is gone; or it had ended before the first look, when its
    /// ending was not seen.
    Over,
}

impl Phase {
    fn of(liveness: &Liveness) -> Phase {
        match liveness {
            Liveness::Gone => Phase::Over,
            Liveness::Alive {
                agent: Some(AgentState::Finished | AgentState::Error),
            } => Phase::Done,
            Liveness::Starting | Liveness::Alive { .. } => Phase::Live,
        }
    }
}

/// Whether `last` is at least `POLL_MS` before `now` (or never).
fn due(last: Option<Millis>, now: Millis) -> bool {
    last.is_none_or(|last| now >= last.saturating_add(POLL_MS))
}

impl<C: Connect> Worker<C> {
    fn run(mut self, jobs: Receiver<Job>) {
        self.reload_runs();
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
        self.record_links(now);
    }

    /// The link store's share of a look: the `Ran` backfill once, the
    /// changes of each run seen ending, and the open detail's 関連成果
    /// when due. Git runs here, on the worker.
    fn record_links(&mut self, now: Millis) {
        let Ok(store) = self.store.clone() else {
            if let Some(task) = self.watch.clone()
                && self.related_at.is_none()
            {
                self.related_at = Some(now);
                self.send_related(&task, now);
            }
            return;
        };
        // A run's agent finished or failed (each time, after a follow-up
        // too), or its pane went: what its work tree holds now is what it
        // produced. Read as it ends, and a run with a tree of its own again
        // as its pane goes after that; not an `Isolation::Here` run, whose
        // tree (the repository itself) other tasks commit to meanwhile.
        // Those already ended at the first look did not end now: their
        // ending was not seen.
        let first = !self.backfilled;
        let mut ended: Vec<(String, PathBuf, Option<String>)> = Vec::new();
        // Gone after its agent finished: read again only for a run with a
        // tree of its own, which holds no other work (a last commit made
        // after it finished would be lost with the tree).
        let mut gone_done: Vec<(String, PathBuf, Option<String>)> = Vec::new();
        for v in self.views(now) {
            let before = self.phases.get(&v.run.run).copied();
            // An ending not seen stays over until the run is live again.
            let phase = match Phase::of(&v.liveness) {
                Phase::Live => Phase::Live,
                _ if first || before == Some(Phase::Over) => Phase::Over,
                phase => phase,
            };
            self.phases.insert(v.run.run.clone(), phase);
            if phase == Phase::Live && matches!(before, Some(Phase::Done | Phase::Over)) {
                match linking::head_of(&v.run.workdir) {
                    Some(head) => self.live_since.insert(v.run.run.clone(), head),
                    None => self.live_since.remove(&v.run.run),
                };
            }
            let since = self.live_since.get(&v.run.run).cloned();
            let was_live = matches!(before, None | Some(Phase::Live));
            if phase != Phase::Live && was_live && !first {
                ended.push((v.run.run.as_str().to_owned(), v.run.workdir, since));
            } else if phase == Phase::Over && before == Some(Phase::Done) && !first {
                gone_done.push((v.run.run.as_str().to_owned(), v.run.workdir, since));
            }
        }
        if !gone_done.is_empty()
            && let Ok(set) = links::replay(&store)
        {
            ended.extend(
                gone_done
                    .into_iter()
                    .filter(|(run, _, _)| linking::own_tree(&set, run)),
            );
        }
        if first {
            self.backfilled = true;
            let failed = linking::backfill(&store, &self.runs, &self.log, now);
            self.link_failed(failed);
            // Runs that ended while Tasks was closed: those with a work
            // tree of their own, still there, hold only their own work.
            let own: Vec<(String, PathBuf, Option<String>)> = match links::replay(&store) {
                Ok(set) => self
                    .runs
                    .iter()
                    .filter(|r| self.phases.get(&r.run) != Some(&Phase::Live))
                    .filter(|r| linking::own_tree(&set, r.run.as_str()))
                    .map(|r| (r.run.as_str().to_owned(), r.workdir.clone(), None))
                    .collect(),
                Err(_) => Vec::new(),
            };
            if !own.is_empty() {
                let failed = linking::record_produced(&store, &own, now);
                self.link_failed(failed);
            }
        } else if !ended.is_empty() {
            let failed = linking::record_produced(&store, &ended, now);
            self.link_failed(failed);
        }
        if let Some(task) = self.watch.clone()
            && self
                .related_at
                .is_none_or(|at| now.saturating_sub(at) >= RELATED_EVERY)
        {
            self.related_at = Some(now);
            // The task's live runs, and its ended runs with a work tree of
            // their own: an ended run's shared tree (the repository itself,
            // for an `Isolation::Here` run) moves on with other work, which
            // is not the run's.
            let runs: Vec<(String, PathBuf, Option<String>)> = links::replay(&store)
                .map(|set| {
                    linking::runs_of(&set, &linking::task_key(&task))
                        .into_iter()
                        .filter(|(run, _)| {
                            self.phases.get(&RunId::new(run.clone())) == Some(&Phase::Live)
                                || linking::own_tree(&set, run)
                        })
                        .map(|(run, workdir)| {
                            let since = self.live_since.get(&RunId::new(run.clone())).cloned();
                            (run, workdir, since)
                        })
                        .collect()
                })
                .unwrap_or_default();
            let failed = linking::record_produced(&store, &runs, now);
            self.link_failed(failed);
            self.send_related(&task, now);
        }
    }

    fn places(&self) -> Places<'_> {
        Places {
            store: &self.store,
            data: &self.data,
            main: &self.main,
        }
    }

    fn send_related(&self, task: &TaskRef, now: Millis) {
        let view = linking::related_view(&self.places(), task, now);
        let _ = self.up.send(Up::Related(Box::new(view)));
        (self.wake)();
    }

    /// Says why an automatic link was not recorded, once until it changes.
    fn link_failed(&mut self, why: Option<String>) {
        if let Some(why) = why
            && self.link_failure.as_ref() != Some(&why)
        {
            self.say(why.clone());
            self.link_failure = Some(why);
        }
    }

    /// リンクを追加: checked (its form, found), then appended `Manual`,
    /// with the task observed (its title and file) for the back-links.
    fn link(
        &mut self,
        task: &TaskRef,
        title: &str,
        kind: LinkKind,
        id: &str,
    ) -> Result<String, String> {
        let store = self.store.clone()?;
        let set = links::replay(&store).map_err(|e| format!("リンクの記録を読めません: {e}"))?;
        let mut edits =
            linking::manual(&self.places(), &set, &self.runs, &self.log, task, kind, id)
                .map_err(|why| format!("リンクできません: {why}"))?;
        edits.push(linking::observe_task(&self.repo, task, title));
        let now = (self.clock)();
        let outcomes =
            links::append(&store, &edits, now).map_err(|e| format!("リンクを書けません: {e}"))?;
        self.related_at = None;
        Ok(linking::added(kind, id.trim(), &outcomes))
    }

    /// リンクを外す, answered: an `Unlinked` for each.
    fn unlink(&mut self, ids: &[LinkId]) -> Result<String, String> {
        let store = self.store.clone()?;
        let edits: Vec<Edit> = ids.iter().cloned().map(Edit::Unlink).collect();
        let now = (self.clock)();
        let outcomes =
            links::append(&store, &edits, now).map_err(|e| format!("リンクを外せません: {e}"))?;
        self.related_at = None;
        Ok(linking::removed(ids, &outcomes))
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
        let gone = self.markers_gone();
        let tasks = self
            .tasks
            .iter()
            .chain(&self.code)
            .chain(&gone)
            .cloned()
            .collect();
        let seen = (tasks, self.views((self.clock)()));
        if self.sent.as_ref() == Some(&seen) {
            return;
        }
        let _ = self.up.send(Up::Seen {
            tasks: seen.0.clone(),
            runs: seen.1.clone(),
        });
        self.sent = Some(seen);
        (self.wake)();
    }

    /// Looks again at once: after a write or a pane change.
    fn look_now(&mut self) {
        self.scan = Scan::new(self.repo.clone());
        self.panes_at = None;
        self.results_at = None;
    }

    fn handle(&mut self, job: Job) {
        let outcome = match job {
            Job::ScanCode => {
                self.code_at = None;
                return;
            }
            Job::Watch(task) => {
                if task != self.watch {
                    self.watch = task;
                    self.related_at = None;
                }
                return;
            }
            Job::RefreshRelated => {
                self.related_at = None;
                return;
            }
            Job::Link {
                task,
                title,
                kind,
                id,
            } => self.link(&task, &title, kind, &id),
            Job::Unlink(ids) => self.unlink(&ids),
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
                Some(found) if !found.workdir.is_dir() => Err(format!(
                    "{} の作業ツリー {} はもうありません",
                    run,
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
        let links_root = self.links_root.clone();
        let mut board = self.board;
        let conn = self
            .conn()
            .map_err(|why| format!("Noa に接続できないため起動できません: {why}"))?;
        let started = start::start(conn, &mut board, &log, Some(&links_root), req, now);
        self.board = board;
        self.reload_runs();
        // Its links are recorded: the store sees it as this window does.
        self.related_at = None;
        started.map(|started| {
            let begun = format!(
                "{} を {} で始めました（{}）",
                req.task.title,
                req.agent.command(),
                started.run
            );
            match started.notice {
                Some(notice) => format!("{begun}。{notice}"),
                None => begun,
            }
        })
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
    use crate::ui::{LinkKind, RelatedView};

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
        log: PathBuf,
        noa: FakeNoa,
        clock: Arc<AtomicU64>,
        opened: Arc<Mutex<Vec<String>>>,
        asked: Arc<Mutex<Vec<String>>>,
        /// The thread of each wake.
        wakes: Arc<Mutex<Vec<std::thread::ThreadId>>>,
        feed: LiveFeed<FakeNoa>,
    }

    const START: Millis = 1_800_000_000_000;

    /// A feed on a fresh repository; `before` runs on the data root and
    /// the fake Noa before the feed starts.
    fn rig(name: &str, before: impl FnOnce(&Path, &Path, &FakeNoa)) -> Rig {
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
        let (c, o, w) = (clock.clone(), opened.clone(), wakes.clone());
        let feed = LiveFeed::with(
            repo.clone(),
            root,
            Box::new(Issues {
                view: GitHubView::default(),
                asked: asked.clone(),
            }),
            noa.clone(),
            Box::new(move |target: &str| o.lock().unwrap().push(target.to_owned())),
            Arc::new(move || c.load(Ordering::SeqCst)),
            Arc::new(move || w.lock().unwrap().push(std::thread::current().id())),
        );
        Rig {
            _tmp: tmp,
            repo,
            log,
            noa,
            clock,
            opened,
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
            Arc::new(move || c.load(Ordering::SeqCst) + 7),
            Arc::new(|| {}),
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

    // ------------------------------------------------- 関連成果 (#93)

    /// Polls `feed` until its 関連成果 satisfies `done` (10 s at most).
    fn related_until(
        feed: &mut LiveFeed<FakeNoa>,
        what: &str,
        done: impl Fn(&RelatedView) -> bool,
    ) -> RelatedView {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last = None;
        loop {
            feed.poll();
            if let Some(related) = feed.related() {
                last = Some(related);
            }
            if let Some(related) = &last
                && done(related)
            {
                return related.clone();
            }
            assert!(Instant::now() < deadline, "{what}: {last:#?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// `git -C dir args…` (a test identity), its stdout trimmed.
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// Polls `feed` until its notice says `what` (10 s at most): the View
    /// that carried it may have been taken by another helper's poll.
    fn said(feed: &mut LiveFeed<FakeNoa>, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !feed.notice.as_deref().is_some_and(|n| n.contains(what)) {
            feed.poll();
            assert!(Instant::now() < deadline, "{what}: {:?}", feed.notice);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn related_of(view: &RelatedView) -> &links::Related {
        view.related.as_ref().expect("the store is read")
    }

    /// `<data dir>/noa` of `rig`: the runs logs' root's parent.
    fn data_of(rig: &Rig) -> PathBuf {
        rig.log.ancestors().nth(3).unwrap().to_path_buf()
    }

    /// AC7 (feed): a link added by hand is checked (found, not linked
    /// yet), written to the store and seen by a new feed on the repository;
    /// a removed one is gone for it too.
    #[test]
    fn ac7_links_added_and_removed_by_hand_persist_across_a_new_feed() {
        let mut rig = rig("links-manual", |repo, _, _| write_task(repo, "t-1", "one"));
        rig.until("the task", |v| v.tasks.len() == 1);
        let data = data_of(&rig);
        fs::create_dir_all(data.join("reviews/rev-1")).unwrap();
        fs::write(data.join("reviews/rev-1/events.jsonl"), "").unwrap();
        let t1 = TaskRef::Local("t-1".into());
        rig.feed.watch(Some(&t1));
        related_until(&mut rig.feed, "nothing linked", |r| {
            related_of(r).loose.is_empty() && related_of(r).runs.is_empty()
        });

        rig.feed.act(Intent::Link {
            task: t1.clone(),
            title: "one".into(),
            kind: LinkKind::Review,
            id: "rev-1".into(),
        });
        let shown = related_until(&mut rig.feed, "the review", |r| {
            related_of(r)
                .loose
                .iter()
                .any(|e| e.node == links::Node::Review("rev-1".into()))
        });
        said(&mut rig.feed, "rev-1 をリンクしました");
        // Refused, with why: already linked; not found; no such commit.
        for (kind, id, why) in [
            (LinkKind::Review, "rev-1", links::ALREADY_LINKED),
            (LinkKind::Verify, "ver-9", "ver-9 の記録が見つかりません"),
            (LinkKind::Commit, "deadbeef", "コミットが見つからない"),
            (LinkKind::Run, "run-1", "run-1 の記録が見つかりません"),
        ] {
            rig.feed.act(Intent::Link {
                task: t1.clone(),
                title: "one".into(),
                kind,
                id: id.into(),
            });
            said(&mut rig.feed, why);
        }
        // A commit by an unambiguous prefix is linked by its full SHA.
        let head = git_in(&rig.repo, &["rev-parse", "HEAD"]);
        rig.feed.act(Intent::Link {
            task: t1.clone(),
            title: "one".into(),
            kind: LinkKind::Commit,
            id: head[..9].to_owned(),
        });
        related_until(&mut rig.feed, "the commit", |r| {
            related_of(r)
                .changes
                .iter()
                .any(|c| c.change.as_str() == head)
        });

        let mut other = second_window(&rig);
        other.watch(Some(&t1));
        related_until(&mut other, "another window sees both", |r| {
            related_of(r).loose.len() == 1 && related_of(r).changes.len() == 1
        });

        let own = shown.related.unwrap().loose[0].links.clone();
        assert_eq!(own.len(), 1, "the review's About the task");
        rig.feed.act(Intent::Unlink {
            task: t1.clone(),
            links: own,
        });
        related_until(&mut rig.feed, "removed", |r| related_of(r).loose.is_empty());
        let mut again = second_window(&rig);
        again.watch(Some(&t1));
        let seen = related_until(&mut again, "removed for a new feed", |r| {
            related_of(r).changes.len() == 1
        });
        assert!(related_of(&seen).loose.is_empty());
    }

    /// A worktree run started here records its start commit; its commits
    /// since are recorded when the detail is read (the newest current),
    /// and when the run ends.
    #[test]
    fn a_runs_commits_are_recorded_when_its_detail_is_read_and_when_it_ends() {
        let mut rig = rig("links-produced", |repo, _, _| {
            write_task(repo, "t-1", "one")
        });
        rig.until("the task", |v| v.tasks.len() == 1);
        let t1 = TaskRef::Local("t-1".into());
        rig.feed.act(Intent::Run {
            task: t1.clone(),
            agent: AgentKind::Claude,
            isolation: Isolation::Worktree,
        });
        let view = rig.until("started", |v| v.runs.len() == 1);
        let run = view.runs[0].run.clone();
        let v1 = {
            git_in(&run.workdir, &["commit", "-q", "--allow-empty", "-m", "v1"]);
            git_in(&run.workdir, &["rev-parse", "HEAD"])
        };
        rig.feed.watch(Some(&t1));
        let seen = related_until(&mut rig.feed, "v1 under the run", |r| {
            related_of(r).runs.first().is_some_and(|e| {
                e.changes.len() == 1 && e.changes[0].change.as_str() == v1 && e.changes[0].current
            })
        });
        assert_eq!(related_of(&seen).runs[0].run, run.run.as_str());

        // Edits on v1 are its snapshot; discarded, v1 is current again
        // although its link was made before the snapshot's.
        fs::write(run.workdir.join("draft.txt"), "draft\n").unwrap();
        rig.feed.watch(None);
        rig.feed.watch(Some(&t1));
        related_until(&mut rig.feed, "the snapshot on v1 current", |r| {
            related_of(r).runs.first().is_some_and(|e| {
                e.changes
                    .iter()
                    .any(|c| matches!(c.change, links::ChangeId::Snapshot(_)) && c.current)
            })
        });
        fs::remove_file(run.workdir.join("draft.txt")).unwrap();
        rig.feed.watch(None);
        rig.feed.watch(Some(&t1));
        related_until(&mut rig.feed, "v1 current again", |r| {
            related_of(r).runs.first().is_some_and(|e| {
                e.changes
                    .iter()
                    .any(|c| c.change.as_str() == v1 && c.current)
            })
        });

        // The detail closed; the run ends after a second commit: recorded
        // as it ends, not only when a detail is read.
        rig.feed.watch(None);
        git_in(&run.workdir, &["commit", "-q", "--allow-empty", "-m", "v2"]);
        let v2 = git_in(&run.workdir, &["rev-parse", "HEAD"]);
        rig.feed.act(Intent::ClosePane(run.run.clone()));
        rig.until("ended", |v| v.runs[0].liveness == Liveness::Gone);
        let store = links::path(&data_of(&rig).join("links"), &rig.repo).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let node = links::Node::Run(run.run.as_str().to_owned());
        let change = links::Node::Change(links::ChangeId::Commit(v2.clone()));
        while links::replay(&store)
            .unwrap()
            .live(&node, &change, links::Rel::Produced)
            .is_none()
        {
            assert!(Instant::now() < deadline, "v2 never recorded");
            rig.tick();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Runs logged before the store knew them get their `Ran` link on the
    /// first look; the uncommitted work in a run's worktree is recorded as
    /// a snapshot when the detail is read, and is its current change.
    #[test]
    fn runs_logged_before_links_are_backfilled_and_dirty_work_is_a_snapshot() {
        let mut rig = rig("links-backfill", |repo, log, _| {
            write_task(repo, "t-1", "one");
            let wt = repo.with_file_name("app-wt").join("task-run-1");
            git_in(
                repo,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "task-run-1",
                    wt.to_str().unwrap(),
                ],
            );
            fs::write(wt.join("fix.txt"), "work\n").unwrap();
            runs::append(
                log,
                &RunEvent::RunStarted {
                    run: RunId::new("run-1"),
                    task: TaskRef::Local("t-1".into()),
                    agent: AgentKind::Claude,
                    workdir: wt,
                    branch: Some("task-run-1".into()),
                    label: "task:run-1".into(),
                    at: START - 1000,
                },
            )
            .unwrap();
        });
        rig.until("the run", |v| v.runs.len() == 1);
        let t1 = TaskRef::Local("t-1".into());
        rig.feed.watch(Some(&t1));
        let seen = related_until(&mut rig.feed, "the snapshot", |r| {
            related_of(r).runs.first().is_some_and(|e| {
                e.changes
                    .iter()
                    .any(|c| matches!(c.change, links::ChangeId::Snapshot(_)) && c.current)
            })
        });
        let entry = &related_of(&seen).runs[0];
        assert_eq!(entry.run, "run-1");
        assert_eq!(entry.info.record.as_deref(), Some(rig.log.as_path()));
        assert_eq!(entry.changes.len(), 1, "no start commit: no commit range");
        assert_eq!(seen.current.as_ref().map(|c| c.run.as_str()), Some("run-1"));
    }

    /// Whether the store of `rig`'s repository has `Produced(run → sha)`.
    fn produced_by(rig: &Rig, run: &RunId, sha: &str) -> bool {
        let store = links::path(&data_of(rig).join("links"), &rig.repo).unwrap();
        links::replay(&store)
            .unwrap()
            .live(
                &links::Node::Run(run.as_str().to_owned()),
                &links::Node::Change(links::ChangeId::Commit(sha.to_owned())),
                links::Rel::Produced,
            )
            .is_some()
    }

    /// Moves the clock on and polls until `done` holds (10 s at most).
    fn ticking(rig: &mut Rig, what: &str, done: impl Fn(&Rig, &View) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last: Option<View> = None;
        loop {
            rig.tick();
            if let Some(view) = rig.feed.poll() {
                last = Some(view);
            }
            if last.as_ref().is_some_and(|v| done(rig, v)) {
                return;
            }
            assert!(Instant::now() < deadline, "{what}: {last:#?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Sets the agent state every pane reports.
    fn agent(rig: &Rig, state: &str) {
        rig.noa.with(|s| {
            for pane in &mut s.panes {
                pane.agent_state = Some(state.into());
            }
        });
    }

    /// An `Isolation::Here` run shares the repository: what another task
    /// commits there after the run ended is not the run's, and reading
    /// its task's detail (here, or in a window opened later) records none
    /// of it.
    #[test]
    fn an_ended_runs_detail_does_not_take_commits_made_after_it_ended() {
        let mut rig = rig("links-ended", |repo, _, _| write_task(repo, "t-1", "one"));
        rig.until("the task", |v| v.tasks.len() == 1);
        let t1 = TaskRef::Local("t-1".into());
        rig.feed.act(Intent::Run {
            task: t1.clone(),
            agent: AgentKind::Claude,
            isolation: Isolation::Here,
        });
        let run = rig.until("started", |v| v.runs.len() == 1).runs[0]
            .run
            .run
            .clone();
        git_in(&rig.repo, &["commit", "-q", "--allow-empty", "-m", "v1"]);
        let v1 = git_in(&rig.repo, &["rev-parse", "HEAD"]);
        rig.feed.act(Intent::ClosePane(run.clone()));
        ticking(&mut rig, "v1 recorded as it ends", |rig, _| {
            produced_by(rig, &run, &v1)
        });

        git_in(&rig.repo, &["commit", "-q", "--allow-empty", "-m", "other"]);
        let other = git_in(&rig.repo, &["rev-parse", "HEAD"]);
        rig.feed.watch(Some(&t1));
        let read = related_until(&mut rig.feed, "the detail", |r| {
            related_of(r).runs.len() == 1
        });
        // And read again on the 10 s refresh.
        rig.clock.fetch_add(RELATED_EVERY, Ordering::SeqCst);
        related_until(&mut rig.feed, "read again", |r| r.at > read.at);
        assert!(!produced_by(&rig, &run, &other), "a later commit taken");

        let mut later = second_window(&rig);
        later.watch(Some(&t1));
        related_until(&mut later, "a later window's detail", |r| {
            related_of(r).runs.len() == 1
        });
        assert!(!produced_by(&rig, &run, &other), "a later commit taken");
        assert!(produced_by(&rig, &run, &v1));
    }

    /// A pane given a follow-up after its agent finished: what it commits
    /// then is recorded when it finishes again, and what is there when
    /// its pane goes, at that.
    #[test]
    fn a_follow_up_in_the_same_pane_is_recorded_when_it_finishes_again_and_at_gone() {
        let mut rig = rig("links-again", |repo, _, _| write_task(repo, "t-1", "one"));
        rig.until("the task", |v| v.tasks.len() == 1);
        rig.feed.act(Intent::Run {
            task: TaskRef::Local("t-1".into()),
            agent: AgentKind::Claude,
            isolation: Isolation::Worktree,
        });
        let run = rig.until("started", |v| v.runs.len() == 1).runs[0]
            .run
            .clone();
        let commit = |what: &str| {
            git_in(&run.workdir, &["commit", "-q", "--allow-empty", "-m", what]);
            git_in(&run.workdir, &["rev-parse", "HEAD"])
        };
        let at_work = |_: &Rig, v: &View| {
            v.runs[0].liveness
                == Liveness::Alive {
                    agent: Some(AgentState::Running),
                }
        };
        agent(&rig, "running");
        ticking(&mut rig, "at work", at_work);
        let v1 = commit("v1");
        agent(&rig, "finished");
        ticking(&mut rig, "v1 as it finishes", |rig, _| {
            produced_by(rig, &run.run, &v1)
        });

        agent(&rig, "running");
        ticking(&mut rig, "at work again", at_work);
        let v2 = commit("v2");
        agent(&rig, "finished");
        ticking(&mut rig, "v2 as it finishes again", |rig, _| {
            produced_by(rig, &run.run, &v2)
        });

        // Committed after it finished, then its pane goes: its own work
        // tree holds only its work, so the pane going reads it again (the
        // tree may be removed next, and the commit with it).
        let v3 = commit("v3");
        rig.feed.act(Intent::ClosePane(run.run.clone()));
        // The look that saw it gone recorded what it would before showing it.
        rig.until("gone", |v| v.runs[0].liveness == Liveness::Gone);
        assert!(produced_by(&rig, &run.run, &v3), "not read as it went");
    }

    /// An `Isolation::Here` run given a follow-up after another task
    /// committed in the shared checkout: as it finishes again only what
    /// was committed while it was live again is its own.
    #[test]
    fn a_here_runs_follow_up_takes_only_the_commits_made_while_it_was_live() {
        let mut rig = rig("links-here-again", |repo, _, _| {
            write_task(repo, "t-1", "one")
        });
        rig.until("the task", |v| v.tasks.len() == 1);
        rig.feed.act(Intent::Run {
            task: TaskRef::Local("t-1".into()),
            agent: AgentKind::Claude,
            isolation: Isolation::Here,
        });
        let run = rig.until("started", |v| v.runs.len() == 1).runs[0]
            .run
            .run
            .clone();
        let commit = |rig: &Rig, what: &str| {
            git_in(&rig.repo, &["commit", "-q", "--allow-empty", "-m", what]);
            git_in(&rig.repo, &["rev-parse", "HEAD"])
        };
        let at_work = |_: &Rig, v: &View| {
            v.runs[0].liveness
                == Liveness::Alive {
                    agent: Some(AgentState::Running),
                }
        };
        agent(&rig, "running");
        ticking(&mut rig, "at work", at_work);
        let v1 = commit(&rig, "v1");
        agent(&rig, "finished");
        ticking(&mut rig, "v1 as it finishes", |rig, _| {
            produced_by(rig, &run, &v1)
        });

        // Task B commits in the shared checkout; then A's follow-up.
        let b = commit(&rig, "b");
        agent(&rig, "running");
        ticking(&mut rig, "at work again", at_work);
        let v2 = commit(&rig, "v2");
        agent(&rig, "finished");
        ticking(&mut rig, "v2 as it finishes again", |rig, _| {
            produced_by(rig, &run, &v2)
        });
        assert!(!produced_by(&rig, &run, &b), "B's commit taken as A's");
        assert!(produced_by(&rig, &run, &v1));
    }

    /// An `Isolation::Here` run whose agent finished is read once, as it
    /// finishes: what another task commits in the repository while its
    /// pane stays open is not taken when the pane goes.
    #[test]
    fn a_here_run_is_read_as_it_finishes_not_again_when_its_pane_goes() {
        let mut rig = rig("links-here-once", |repo, _, _| {
            write_task(repo, "t-1", "one")
        });
        rig.until("the task", |v| v.tasks.len() == 1);
        rig.feed.act(Intent::Run {
            task: TaskRef::Local("t-1".into()),
            agent: AgentKind::Claude,
            isolation: Isolation::Here,
        });
        let run = rig.until("started", |v| v.runs.len() == 1).runs[0]
            .run
            .run
            .clone();
        agent(&rig, "running");
        ticking(&mut rig, "at work", |_, v| {
            v.runs[0].liveness
                == Liveness::Alive {
                    agent: Some(AgentState::Running),
                }
        });
        git_in(&rig.repo, &["commit", "-q", "--allow-empty", "-m", "v1"]);
        let v1 = git_in(&rig.repo, &["rev-parse", "HEAD"]);
        agent(&rig, "finished");
        ticking(&mut rig, "v1 as it finishes", |rig, _| {
            produced_by(rig, &run, &v1)
        });

        // Task B commits in the shared checkout; then A's pane goes.
        git_in(&rig.repo, &["commit", "-q", "--allow-empty", "-m", "b"]);
        let b = git_in(&rig.repo, &["rev-parse", "HEAD"]);
        rig.feed.act(Intent::ClosePane(run.clone()));
        // The look that saw it gone recorded what it would before showing it.
        rig.until("gone", |v| v.runs[0].liveness == Liveness::Gone);
        assert!(!produced_by(&rig, &run, &b), "B's commit taken as A's");
        assert!(produced_by(&rig, &run, &v1));
    }

    /// A worktree run that ended while Tasks was closed is read on the
    /// first look: its own work tree is still there and holds only its
    /// work, which would be lost from the history once it is removed.
    #[test]
    fn a_worktree_run_that_ended_while_tasks_was_closed_is_read_on_the_first_look() {
        let mut rig = rig("links-unseen-end", |repo, _, _| {
            write_task(repo, "t-1", "one")
        });
        rig.until("the task", |v| v.tasks.len() == 1);
        rig.feed.act(Intent::Run {
            task: TaskRef::Local("t-1".into()),
            agent: AgentKind::Claude,
            isolation: Isolation::Worktree,
        });
        let run = rig.until("started", |v| v.runs.len() == 1).runs[0]
            .run
            .clone();
        git_in(&run.workdir, &["commit", "-q", "--allow-empty", "-m", "v1"]);
        let v1 = git_in(&run.workdir, &["rev-parse", "HEAD"]);
        // Its pane goes unseen: the clock stands still, so this window
        // lists no panes again (as if it were closed).
        rig.noa.with(|s| s.panes.clear());
        let mut later = second_window(&rig);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !produced_by(&rig, &run.run, &v1) {
            later.poll();
            assert!(Instant::now() < deadline, "v1 never recorded");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A review linked by hand to a task with no run observes the task
    /// (title, file), so the review's back-link names it.
    #[test]
    fn a_link_added_by_hand_observes_the_task() {
        let mut rig = rig("links-observe", |repo, _, _| write_task(repo, "t-1", "one"));
        rig.until("the task", |v| v.tasks.len() == 1);
        let data = data_of(&rig);
        fs::create_dir_all(data.join("reviews/rev-1")).unwrap();
        fs::write(data.join("reviews/rev-1/events.jsonl"), "").unwrap();
        rig.feed.act(Intent::Link {
            task: TaskRef::Local("t-1".into()),
            title: "one".into(),
            kind: LinkKind::Review,
            id: "rev-1".into(),
        });
        said(&mut rig.feed, "rev-1 をリンクしました");
        let store = links::path(&data.join("links"), &rig.repo).unwrap();
        let info = links::replay(&store)
            .unwrap()
            .info_of(&links::Node::Task(links::TaskKey::Local("t-1".into())));
        assert_eq!(info.label.as_deref(), Some("one"));
        assert_eq!(
            info.record,
            Some(local::tasks_dir(&rig.repo).join("t-1.md"))
        );
    }

    /// 開く on a run whose pane and work tree are both gone passes no
    /// path to the OS and says the work tree is gone.
    #[test]
    fn opening_a_removed_work_tree_says_so_and_opens_nothing() {
        let mut rig = rig("open-gone", |repo, _, _| write_task(repo, "t-1", "one"));
        rig.until("the task", |v| v.tasks.len() == 1);
        rig.feed.act(Intent::Run {
            task: TaskRef::Local("t-1".into()),
            agent: AgentKind::Claude,
            isolation: Isolation::Worktree,
        });
        let run = rig.until("started", |v| v.runs.len() == 1).runs[0]
            .run
            .clone();
        rig.feed.act(Intent::ClosePane(run.run.clone()));
        rig.until("ended", |v| v.runs[0].liveness == Liveness::Gone);
        fs::remove_dir_all(&run.workdir).unwrap();
        rig.feed.act(Intent::OpenWorktree(run.run.clone()));
        said(
            &mut rig.feed,
            &format!(
                "{} の作業ツリー {} はもうありません",
                run.run,
                run.workdir.display()
            ),
        );
        assert!(rig.opened.lock().unwrap().is_empty());
    }
}
