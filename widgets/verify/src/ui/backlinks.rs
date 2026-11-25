//! Where the verification on screen came from (spec `artifact-links`
//! L1-5): its task and the exact change it judged, read from the links
//! store ([`links::backlinks`]) when the session is shown and again at
//! most once a second while it stays shown, and turned into the target
//! card's rows ([`rows`], pure).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use verify::links::{self, BackLinks, LinkSet, Nav, Node, Presence, TaskKey};
use verify::model::types::Session;

/// The shortest time between two reads of the store for the session on
/// screen (as a stored view re-reads its log).
const RECHECK: Duration = Duration::from_secs(1);

/// The text of a row with no link to a task.
pub(super) const NO_TASK: &str = "関連するタスクはありません";

/// One row of the target card's links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    /// `タスク` or `対象の版`.
    pub kind: &'static str,
    /// The task's kind, drawn as a tag.
    pub badge: Option<&'static str>,
    pub text: String,
    /// 開く: its accessible name and what it does; `None` for a record
    /// that cannot be found or no link.
    pub open: Option<(&'static str, Nav)>,
}

/// How a task's kind is badged.
fn badge(task: &TaskKey) -> &'static str {
    match task {
        TaskKey::Local(_) => "ローカル",
        TaskKey::GitHub { .. } => "GitHub",
        TaskKey::Code { .. } => "コード",
    }
}

fn missing(what: &str) -> String {
    format!("記録が見つからない（{what}）")
}

/// The rows for `back`: one per task (or [`NO_TASK`]), then the judged
/// change. A record `presence` says is missing has no button. `repo` is
/// the main worktree Tasks is opened on.
pub(crate) fn rows(
    back: &BackLinks,
    presence: &dyn Fn(&Node) -> Presence,
    repo: &Path,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for task in &back.tasks {
        let node = Node::Task(task.task.clone());
        let row = match presence(&node) {
            Presence::Missing { what } => Row {
                kind: "タスク",
                badge: Some(badge(&task.task)),
                text: missing(&what),
                open: None,
            },
            Presence::Found | Presence::Unknown => Row {
                kind: "タスク",
                badge: Some(badge(&task.task)),
                text: task
                    .info
                    .label
                    .clone()
                    .unwrap_or_else(|| task.task.select()),
                open: Some(("タスクを開く", Nav::task(repo, &task.task))),
            },
        };
        rows.push(row);
    }
    if back.tasks.is_empty() {
        rows.push(Row {
            kind: "タスク",
            badge: None,
            text: NO_TASK.to_owned(),
            open: None,
        });
    }
    for change in &back.changes {
        let node = Node::Change(change.change.clone());
        let row = match presence(&node) {
            Presence::Missing { what } => Row {
                kind: "対象の版",
                badge: None,
                text: missing(&what),
                open: None,
            },
            Presence::Found | Presence::Unknown => Row {
                kind: "対象の版",
                badge: None,
                text: change.change.short(),
                open: Some((
                    "版を開く",
                    change_nav(&change.change, &change.tasks, presence, repo),
                )),
            },
        };
        rows.push(row);
    }
    rows
}

/// 版を開く: the first task whose 関連成果 lists the change and whose record
/// can be found (as a task row's 開く: one `presence` says is missing is
/// passed over), else a copy of its id.
fn change_nav(
    change: &links::ChangeId,
    tasks: &[TaskKey],
    presence: &dyn Fn(&Node) -> Presence,
    repo: &Path,
) -> Nav {
    let found = tasks.iter().find(|task| {
        !matches!(
            presence(&Node::Task((*task).clone())),
            Presence::Missing { .. }
        )
    });
    match found {
        Some(task) => Nav::task(repo, task),
        None => Nav::Copy(change.as_str().to_owned()),
    }
}

/// The rows of verification `id` in `set`, with presence looked up on
/// disk and in git (`data`: `<data dir>/noa`; `repo`: the main worktree).
fn read_rows(set: &LinkSet, id: &str, data: &Path, repo: &Path) -> Vec<Row> {
    let back = links::backlinks(set, &Node::Verify(id.to_owned()));
    rows(&back, &|node| links::presence(set, node, data, repo), repo)
}

/// What a look at the store found.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shown {
    session: String,
    stamp: Option<(u64, SystemTime)>,
    rows: Vec<Row>,
}

/// Reads the links of the session on screen.
pub(super) struct Reader {
    /// `<data dir>/noa/links`; `None`: no store (the demo).
    root: Option<PathBuf>,
    /// `<data dir>/noa`.
    data: Option<PathBuf>,
    shown: Option<Shown>,
    checked: Option<Instant>,
}

impl Reader {
    pub(super) fn new(root: Option<PathBuf>, data: Option<PathBuf>) -> Self {
        Self {
            root,
            data,
            shown: None,
            checked: None,
        }
    }

    /// The person's own store.
    pub(super) fn default_store() -> Self {
        Self::new(links::default_root(), links::default_data())
    }

    /// No store: nothing is shown.
    pub(super) fn none() -> Self {
        Self::new(None, None)
    }

    /// Forgets what was read: the next look reads again.
    pub(super) fn forget(&mut self) {
        self.shown = None;
        self.checked = None;
    }

    /// The rows for `session`, read first when another session was read
    /// last.
    pub(super) fn rows(&mut self, session: &Session) -> &[Row] {
        if self
            .shown
            .as_ref()
            .is_none_or(|s| s.session != session.id.as_str())
        {
            self.read(session, Instant::now());
        }
        self.shown.as_ref().map_or(&[], |s| &s.rows)
    }

    /// Reads the store again when [`RECHECK`] has passed (or another
    /// session is on screen); whether the rows changed.
    pub(super) fn refresh(&mut self, session: &Session, now: Instant) -> bool {
        let same = self
            .shown
            .as_ref()
            .is_some_and(|s| s.session == session.id.as_str());
        if same
            && self
                .checked
                .is_some_and(|at| now.duration_since(at) < RECHECK)
        {
            return false;
        }
        self.read(session, now)
    }

    fn read(&mut self, session: &Session, now: Instant) -> bool {
        self.checked = Some(now);
        let id = session.id.as_str();
        let (Some(root), Some(data)) = (&self.root, &self.data) else {
            let next = Shown {
                session: id.to_owned(),
                stamp: None,
                rows: Vec::new(),
            };
            return self.replace(next);
        };
        let repo = &session.main_worktree;
        let log = root.join(links::repo_key(repo)).join(links::LINKS_FILE);
        let stamp = stamp(&log);
        if let Some(shown) = &self.shown
            && shown.session == id
            && shown.stamp == stamp
        {
            return false;
        }
        let rows = match links::replay(&log) {
            Ok(set) => read_rows(&set, id, data, repo),
            Err(e) => vec![Row {
                kind: "タスク",
                badge: None,
                text: format!("リンクを読めません: {e}"),
                open: None,
            }],
        };
        self.replace(Shown {
            session: id.to_owned(),
            stamp,
            rows,
        })
    }

    fn replace(&mut self, next: Shown) -> bool {
        let changed = self.shown.as_ref().is_none_or(|s| s.rows != next.rows);
        self.shown = Some(next);
        changed
    }
}

/// The store file's size and time of last change; `None` when there is
/// none.
fn stamp(log: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(log).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// How [`Nav`]s that open another program are carried out: the sibling
/// started ([`links::launch`]), or a test's recorder.
pub(crate) type Launcher = Box<dyn FnMut(&Nav) -> Result<(), String>>;

pub(super) fn system_launcher() -> Launcher {
    Box::new(links::launch)
}
