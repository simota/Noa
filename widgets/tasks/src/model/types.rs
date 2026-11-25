//! Every type the units share (L2-2; frozen once the skeleton lands).
//! Behaviour on them goes in `impl` blocks of the module that needs it.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Wall-clock time in milliseconds since the Unix epoch. Always injected by
/// the caller, so the model never reads a clock itself.
pub type Millis = u64;

/// A run without a pane this long after it started is gone (its spawn
/// failed, or the widget died between `RunStarted` and `RunPaneAssigned`).
pub const START_TIMEOUT_MS: Millis = 90_000;
/// How often `.noa/tasks/` is scanned, `noa.listPanels` asked and result
/// files `lstat`'ed, at most.
pub const POLL_MS: Millis = 2_000;
/// 完了 shows the tasks updated in the last this many days.
pub const DONE_DAYS: u64 = 7;

// ---------------------------------------------------------------- tasks

/// Which task: a local file's id (`t-<millis>[-n]`), an issue of a
/// GitHub repository, or a marker (`TODO:`, `FIXME:`, `#TODO(agent):`) in
/// a tracked file, known by its path and its text (`key`, a digest), not
/// its line, which moves as the file is edited.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskRef {
    Local(String),
    GitHub { repo: String, number: u64 },
    Code { path: String, key: String },
}

/// Where a task lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Local,
    GitHub,
    Code,
}

/// The only state a source stores (GitHub has no more).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    #[default]
    Open,
    Closed,
}

/// One unit of work, from either source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub r#ref: TaskRef,
    pub title: String,
    /// `None` for an issue until its detail loads.
    #[serde(default)]
    pub body: Option<String>,
    pub state: State,
    #[serde(default)]
    pub labels: Vec<String>,
    /// When the source last changed it (a file's mtime, `updatedAt`).
    pub updated: Millis,
    #[serde(default)]
    pub url: Option<String>,
    /// When it was closed, where the source says (`closedAt`); 完了 counts
    /// its 7 days from this, else from `updated`.
    #[serde(default)]
    pub closed: Option<Millis>,
    /// Why a local file does not parse: shown on its card, which is never
    /// dropped.
    #[serde(default)]
    pub problem: Option<String>,
}

impl TaskRef {
    pub fn source(&self) -> Source {
        match self {
            TaskRef::Local(_) => Source::Local,
            TaskRef::GitHub { .. } => Source::GitHub,
            TaskRef::Code { .. } => Source::Code,
        }
    }

    /// The card's source badge: ローカル, the issue's `#N`, or the
    /// marker's file name.
    pub fn badge(&self) -> String {
        match self {
            TaskRef::Local(_) => "ローカル".to_owned(),
            TaskRef::GitHub { number, .. } => format!("#{number}"),
            TaskRef::Code { path, .. } => path.rsplit('/').next().unwrap_or(path).to_owned(),
        }
    }
}

/// The four columns, derived from a task's state and its last run
/// ([`super::column::column`]), never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Column {
    Todo,
    Running,
    Review,
    Done,
}

impl Column {
    /// In the order the screen shows them.
    pub const ALL: [Column; 4] = [Column::Todo, Column::Running, Column::Review, Column::Done];

    pub fn label(self) -> &'static str {
        match self {
            Column::Todo => "未着手",
            Column::Running => "実行中",
            Column::Review => "確認待ち",
            Column::Done => "完了",
        }
    }
}

// ---------------------------------------------------------------- agents and runs

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
    Agy,
}

impl AgentKind {
    pub const ALL: [AgentKind; 3] = [AgentKind::Claude, AgentKind::Codex, AgentKind::Agy];

    /// The command that starts the agent interactively, and its display name.
    pub fn command(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Agy => "agy",
        }
    }
}

/// Where a run's agent works.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    /// 新しい worktree: `task-<run-id>` on a new branch (the default, so
    /// two runs never share a work tree).
    #[default]
    Worktree,
    /// この作業ツリー: the repository itself.
    Here,
}

macro_rules! string_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_id!(
    /// One run: `run-<millis>[-n]`. Its pane's label is `task:<run-id>`.
    RunId
);

/// An agent's last report about itself (`Panel.agent.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    Running,
    Permission,
    Input,
    Finished,
    Error,
}

/// One line of `runs.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum RunEvent {
    /// Written before the spawn; `label` is the pane's.
    RunStarted {
        run: RunId,
        task: TaskRef,
        agent: AgentKind,
        workdir: PathBuf,
        branch: Option<String>,
        label: String,
        at: Millis,
    },
    /// Written after the spawn: the float and the board tab it is on.
    RunPaneAssigned {
        run: RunId,
        pane: u64,
        board: u64,
        at: Millis,
    },
    /// The person closed its pane from the widget.
    RunClosed { run: RunId, at: Millis },
}

/// A run, folded from its events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub run: RunId,
    pub task: TaskRef,
    pub agent: AgentKind,
    pub workdir: PathBuf,
    pub branch: Option<String>,
    pub pane: Option<u64>,
    pub board: Option<u64>,
    pub started: Millis,
    pub closed: Option<Millis>,
}

/// A pane, as far as runs care (from `noa.listPanels`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneFact {
    pub pane_id: u64,
    /// The board it is stowed on, for a float.
    pub parent: Option<u64>,
    pub label: Option<String>,
    pub agent: Option<AgentState>,
    /// Its child exited and the pane is held open.
    pub exited: bool,
}

/// Whether a run's pane is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness {
    /// Started, no pane yet, within [`START_TIMEOUT_MS`].
    Starting,
    Alive {
        agent: Option<AgentState>,
    },
    Gone,
}

/// The run's result file (`NOA_RESULT`), as `lstat` saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultFact {
    Missing,
    Written { bytes: u64, modified: Millis },
}

/// A result file's text, capped at 16 KiB / 200 lines (`cut` when it was).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub text: String,
    pub cut: bool,
}

/// A run with what is known of it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunView {
    pub run: Run,
    pub liveness: Liveness,
    pub result: ResultFact,
    pub report: Option<Report>,
}

// ---------------------------------------------------------------- GitHub

/// Why the GitHub source has nothing new.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GhFailure {
    NoGh,
    NotAuthenticated,
    NotGitHub,
    Timeout,
    Failed(String),
}

impl GhFailure {
    /// The GitHub source header's line.
    pub fn reason(&self) -> String {
        match self {
            GhFailure::NoGh => {
                "gh が見つかりません（GitHub CLI を入れると issue も表示されます）".to_owned()
            }
            GhFailure::NotAuthenticated => "gh にログインしていません（gh auth login）".to_owned(),
            GhFailure::NotGitHub => "GitHub のリポジトリではありません".to_owned(),
            GhFailure::Timeout => "gh が時間内に応答しませんでした".to_owned(),
            GhFailure::Failed(why) => format!("gh が失敗しました: {why}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub author: String,
    pub body: String,
    pub at: Millis,
}

/// What an issue's detail adds: its body and comments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueDetail {
    pub body: String,
    pub comments: Vec<Comment>,
}

/// The GitHub source, as the screen shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubView {
    /// `owner/name` once known.
    pub repo: Option<String>,
    /// The last listing, from the cache until a fetch.
    pub tasks: Vec<Task>,
    /// When that listing was fetched, for 「n 分前の取得」.
    pub fetched: Option<Millis>,
    pub refreshing: bool,
    /// The last refresh's; the tasks stay.
    pub failure: Option<GhFailure>,
    pub details: BTreeMap<u64, Result<IssueDetail, GhFailure>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(event: &RunEvent) -> String {
        serde_json::to_string(event).unwrap()
    }

    #[test]
    fn ac03_each_run_event_is_a_fixed_json_line_and_reads_back() {
        let started = RunEvent::RunStarted {
            run: RunId::new("run-5"),
            task: TaskRef::GitHub {
                repo: "noa-dev/noa".into(),
                number: 12,
            },
            agent: AgentKind::Codex,
            workdir: PathBuf::from("/src/noa-wt/task-run-5"),
            branch: Some("task-run-5".into()),
            label: "task:run-5".into(),
            at: 5,
        };
        let local = RunEvent::RunStarted {
            run: RunId::new("run-6"),
            task: TaskRef::Local("t-1".into()),
            agent: AgentKind::Claude,
            workdir: PathBuf::from("/src/noa"),
            branch: None,
            label: "task:run-6".into(),
            at: 6,
        };
        let assigned = RunEvent::RunPaneAssigned {
            run: RunId::new("run-5"),
            pane: 41,
            board: 7,
            at: 8,
        };
        let closed = RunEvent::RunClosed {
            run: RunId::new("run-5"),
            at: 9,
        };
        let expected = [
            (
                &started,
                r#"{"event":"RunStarted","run":"run-5","task":{"github":{"repo":"noa-dev/noa","number":12}},"agent":"codex","workdir":"/src/noa-wt/task-run-5","branch":"task-run-5","label":"task:run-5","at":5}"#,
            ),
            (
                &local,
                r#"{"event":"RunStarted","run":"run-6","task":{"local":"t-1"},"agent":"claude","workdir":"/src/noa","branch":null,"label":"task:run-6","at":6}"#,
            ),
            (
                &assigned,
                r#"{"event":"RunPaneAssigned","run":"run-5","pane":41,"board":7,"at":8}"#,
            ),
            (&closed, r#"{"event":"RunClosed","run":"run-5","at":9}"#),
        ];
        for (event, json) in expected {
            assert_eq!(line(event), json);
            assert!(!json.contains('\n'));
            assert_eq!(&serde_json::from_str::<RunEvent>(json).unwrap(), event);
        }
        assert!(
            serde_json::from_str::<RunEvent>(r#"{"event":"RunLost","run":"r","at":1}"#).is_err()
        );
    }

    #[test]
    fn ac03_a_task_round_trips() {
        let tasks = [
            Task {
                r#ref: TaskRef::Local("t-1700000000000-2".into()),
                title: "直す".into(),
                body: Some("本文\n".into()),
                state: State::Closed,
                labels: vec!["bug".into(), "ui".into()],
                updated: 1_700_000_000_000,
                url: None,
                closed: None,
                problem: Some("title がありません".into()),
            },
            Task {
                r#ref: TaskRef::GitHub {
                    repo: "noa-dev/noa".into(),
                    number: 12,
                },
                title: "Add search".into(),
                body: None,
                state: State::Open,
                labels: Vec::new(),
                updated: 3,
                url: Some("https://github.com/noa-dev/noa/issues/12".into()),
                closed: None,
                problem: None,
            },
        ];
        for task in &tasks {
            let json = serde_json::to_string(task).unwrap();
            assert!(json.starts_with(r#"{"ref":"#), "{json}");
            assert_eq!(&serde_json::from_str::<Task>(&json).unwrap(), task);
        }
        let json = serde_json::to_string(&tasks[1]).unwrap();
        assert!(json.contains(r#""state":"open""#), "{json}");
        // The GitHub view a cache keeps reads back too.
        let mut view = GitHubView {
            repo: Some("noa-dev/noa".into()),
            tasks: tasks.to_vec(),
            fetched: Some(4),
            refreshing: false,
            failure: Some(GhFailure::Failed("x".into())),
            details: BTreeMap::new(),
        };
        view.details.insert(
            12,
            Ok(IssueDetail {
                body: "b".into(),
                comments: vec![Comment {
                    author: "a".into(),
                    body: "c".into(),
                    at: 2,
                }],
            }),
        );
        view.details.insert(13, Err(GhFailure::Timeout));
        let json = serde_json::to_string(&view).unwrap();
        assert_eq!(serde_json::from_str::<GitHubView>(&json).unwrap(), view);
    }

    #[test]
    fn badges_and_sources_follow_the_ref() {
        let local = TaskRef::Local("t-1".into());
        let issue = TaskRef::GitHub {
            repo: "o/n".into(),
            number: 7,
        };
        assert_eq!(
            (local.source(), local.badge().as_str()),
            (Source::Local, "ローカル")
        );
        assert_eq!(
            (issue.source(), issue.badge().as_str()),
            (Source::GitHub, "#7")
        );
    }
}
