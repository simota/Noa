//! What the Tasks widget records in the link store (`links.rs`,
//! `docs/specs/artifact-links.md` L1-2, L1-6) and reads back for a task's
//! 関連成果: the run-start links, the `Ran` backfill, the changes a run
//! produced, and the links a person adds or removes by hand.
//!
//! Every git call and file stat here runs on the caller's thread (the
//! worker, or `start`), before [`links::append`] takes the store's lock.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tasks::links::{
    self, ChangeId, Edit, Info, LinkId, LinkSet, Node, Origin, Outcome, Rel, TaskKey,
};
use tasks::local;
use tasks::model::types::{AgentKind, Millis, Run, RunId, Task, TaskRef};

use crate::ui::{LinkKind, RelatedView, check_link_id};

/// How many commits of one run are recorded (`git rev-list` cap).
const COMMITS_CAP: usize = 50;

/// The link store's key for `task` (the same serde shape as `TaskRef`).
pub fn task_key(task: &TaskRef) -> TaskKey {
    match task {
        TaskRef::Local(id) => TaskKey::Local(id.clone()),
        TaskRef::GitHub { repo, number } => TaskKey::GitHub {
            repo: repo.clone(),
            number: *number,
        },
        TaskRef::Code { path, key } => TaskKey::Code {
            path: path.clone(),
            key: key.clone(),
        },
    }
}

/// The file whose existence makes `task` found: a local task's file, a
/// marker's source file; `None` for an issue.
fn task_record(repo: &Path, task: &TaskRef) -> Option<PathBuf> {
    match task {
        TaskRef::Local(id) => Some(local::tasks_dir(repo).join(format!("{id}.md"))),
        TaskRef::Code { path, .. } => Some(repo.join(path)),
        TaskRef::GitHub { .. } => None,
    }
}

/// `task` observed: its title and the file that makes it found (`repo`:
/// the work tree it was read in), so a back-link names it. An empty
/// `title` (the screen did not find the task) keeps the one recorded.
pub fn observe_task(repo: &Path, task: &TaskRef, title: &str) -> Edit {
    Edit::Observe {
        node: Node::Task(task_key(task)),
        info: Info {
            label: (!title.is_empty()).then(|| title.to_owned()),
            record: task_record(repo, task),
            ..Info::default()
        },
    }
}

/// `git -C dir args…` without the person's `GIT_*` variables; its stdout.
fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"GIT_") {
            cmd.env_remove(name);
        }
    }
    let out = cmd
        .output()
        .map_err(|e| format!("git を起動できません: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// The commit `dir`'s `HEAD` is at (`None` on an unborn `HEAD`).
fn head_of(dir: &Path) -> Option<String> {
    let out = git(dir, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]).ok()?;
    let sha = String::from_utf8(out).ok()?.trim().to_owned();
    (!sha.is_empty()).then_some(sha)
}

/// The notice for the edits the store refused, or `None` when it took
/// (or skipped) all of them.
fn refused(what: &str, outcomes: &[Outcome]) -> Option<String> {
    let why: Vec<&str> = outcomes
        .iter()
        .filter_map(|o| match o {
            Outcome::Refused(why) => Some(why.as_str()),
            _ => None,
        })
        .collect();
    (!why.is_empty()).then(|| format!("{what}のリンクを記録できません: {}", why.join("、")))
}

/// What a run is observed as: its label, its runs log, work tree, branch
/// and (when known) the commit it started on.
fn run_info(
    agent: AgentKind,
    run: &RunId,
    log: &Path,
    workdir: &Path,
    branch: Option<&str>,
    head: Option<String>,
) -> Info {
    Info {
        label: Some(format!("{} · {}", agent.command(), tasks::runs::label(run))),
        record: Some(log.to_owned()),
        workdir: Some(workdir.to_owned()),
        branch: branch.map(str::to_owned),
        head,
        outcome: None,
    }
}

/// Right after `RunStarted`: `Ran(task → run)`, the run observed (its
/// runs log, work tree, branch, start commit) and the task observed (its
/// title and file). `root` is the stores' root (`<data dir>/noa/links`).
/// A failure is the notice to show; the run goes ahead regardless.
#[allow(clippy::too_many_arguments)]
pub fn record_start(
    root: &Path,
    repo: &Path,
    task: &Task,
    agent: AgentKind,
    run: &RunId,
    workdir: &Path,
    branch: Option<&str>,
    log: &Path,
    now: Millis,
) -> Option<String> {
    let what = format!("実行 {run} ");
    let store = match links::path(root, repo) {
        Ok(store) => store,
        Err(why) => return Some(format!("{what}のリンクを記録できません: {why}")),
    };
    let key = task_key(&task.r#ref);
    let edits = [
        Edit::Link {
            from: Node::Task(key),
            to: Node::Run(run.as_str().to_owned()),
            rel: Rel::Ran,
            origin: Origin::Auto,
        },
        Edit::Observe {
            node: Node::Run(run.as_str().to_owned()),
            info: run_info(agent, run, log, workdir, branch, head_of(workdir)),
        },
        observe_task(repo, &task.r#ref, &task.title),
    ];
    match links::append(&store, &edits, now) {
        Ok(outcomes) => refused(&what, &outcomes),
        Err(e) => Some(format!("{what}のリンクを記録できません: {e}")),
    }
}

/// `Ran` for every run of `runs` (logged in `log`) and the run observed;
/// the store skips a link it ever had, so a removed one stays removed.
pub fn backfill(store: &Path, runs: &[Run], log: &Path, now: Millis) -> Option<String> {
    if runs.is_empty() {
        return None;
    }
    let edits: Vec<Edit> = runs
        .iter()
        .flat_map(|run| {
            let node = Node::Run(run.run.as_str().to_owned());
            [
                Edit::Link {
                    from: Node::Task(task_key(&run.task)),
                    to: node.clone(),
                    rel: Rel::Ran,
                    origin: Origin::Auto,
                },
                Edit::Observe {
                    node,
                    info: run_info(
                        run.agent,
                        &run.run,
                        log,
                        &run.workdir,
                        run.branch.as_deref(),
                        None,
                    ),
                },
            ]
        })
        .collect();
    match links::append(store, &edits, now) {
        Ok(outcomes) => refused("以前の実行", &outcomes),
        Err(e) => Some(format!("以前の実行のリンクを記録できません: {e}")),
    }
}

/// What `run` (work tree `workdir`) produced, read now: `Produced(run →
/// commit)` for each commit since the one it started on (oldest first, at
/// most [`COMMITS_CAP`]), and `Produced(run → snapshot)` with the
/// snapshot observed when the tree is dirty. Runs git: off the UI thread.
pub fn produced(set: &LinkSet, run: &str, workdir: &Path) -> Vec<Edit> {
    let node = Node::Run(run.to_owned());
    let mut edits = Vec::new();
    let link = |change: ChangeId| Edit::Link {
        from: node.clone(),
        to: Node::Change(change),
        rel: Rel::Produced,
        origin: Origin::Auto,
    };
    // A run with no start commit recorded (logged before links existed)
    // has no range: everything before it would count as its own.
    if let Some(base) = set.info_of(&node).head {
        let range = format!("{base}..HEAD");
        let cap = format!("--max-count={COMMITS_CAP}");
        if let Ok(out) = git(workdir, &["rev-list", "--reverse", &cap, &range]) {
            for sha in String::from_utf8_lossy(&out).split_whitespace() {
                edits.push(link(ChangeId::Commit(sha.to_owned())));
            }
        }
    }
    // A clean tree is its HEAD, already listed above when the run made it;
    // a state with no id (too large, nested changes) gets no link.
    if let Ok(change @ ChangeId::Snapshot(_)) = links::snapshot_id(workdir) {
        edits.push(Edit::Observe {
            node: Node::Change(change.clone()),
            info: Info {
                workdir: Some(workdir.to_owned()),
                head: change.head().map(str::to_owned),
                ..Info::default()
            },
        });
        edits.push(link(change));
    }
    edits
}

/// [`produced`] for every run of `runs` whose work tree is still there,
/// appended in one write; the notice when that fails.
pub fn record_produced(store: &Path, runs: &[(String, PathBuf)], now: Millis) -> Option<String> {
    let set = match links::replay(store) {
        Ok(set) => set,
        Err(e) => return Some(format!("リンクの記録を読めません: {e}")),
    };
    let edits: Vec<Edit> = runs
        .iter()
        .filter(|(_, workdir)| workdir.is_dir())
        .flat_map(|(run, workdir)| produced(&set, run, workdir))
        .collect();
    if edits.is_empty() {
        return None;
    }
    match links::append(store, &edits, now) {
        Ok(outcomes) => refused("実行の成果", &outcomes),
        Err(e) => Some(format!("実行の成果のリンクを記録できません: {e}")),
    }
}

/// Whether `run` has a work tree of its own (a branch) and a recorded
/// start commit: what that tree holds since is the run's alone, so it is
/// read after the run ended too. An `Isolation::Here` run (no branch)
/// shares the repository with other work and is not.
pub fn own_tree(set: &LinkSet, run: &str) -> bool {
    let info = set.info_of(&Node::Run(run.to_owned()));
    info.branch.is_some() && info.head.is_some()
}

/// The task's runs that have a work tree recorded: `(run, workdir)`.
pub fn runs_of(set: &LinkSet, task: &TaskKey) -> Vec<(String, PathBuf)> {
    set.links_from(&Node::Task(task.clone()), Rel::Ran)
        .filter_map(|l| match &l.to {
            Node::Run(run) => Some((run.clone(), set.info.get(&l.to)?.workdir.clone()?)),
            _ => None,
        })
        .collect()
}

/// Where a task's 関連成果 is read from and checked against.
pub struct Places<'a> {
    /// The store (`Err`: why there is none).
    pub store: &'a Result<PathBuf, String>,
    /// `<data dir>/noa`: the reviews and verifications.
    pub data: &'a Path,
    /// The main worktree: commits and task files.
    pub main: &'a Path,
}

/// The 関連成果 of `task` as the store and the disk are now. Runs git and
/// stats: off the UI thread.
pub fn related_view(places: &Places<'_>, task: &TaskRef, now: Millis) -> RelatedView {
    let key = task_key(task);
    let read = places.store.clone().and_then(|store| {
        links::replay(&store).map_err(|e| format!("リンクの記録を読めません: {e}"))
    });
    let (related, current, presence) = match read {
        Ok(set) => {
            let current = links::task_current(&set, &key, places.main);
            let related = links::related(&set, &key, current.as_ref());
            let presence = links::presences(&set, places.data, places.main);
            (Ok(related), current, presence)
        }
        Err(why) => (Err(why), None, BTreeMap::new()),
    };
    RelatedView {
        task: task.clone(),
        related,
        current,
        presence,
        data: places.data.to_owned(),
        repo: places.main.to_owned(),
        at: now,
    }
}

/// The edits a manual リンクを追加 of `kind` `id` on `task` makes, once
/// `id` is checked: it has its kind's form and is found (the session's
/// log, the commit in `main`, the run in the store or `runs`). `Err` is
/// why it is refused.
pub fn manual(
    places: &Places<'_>,
    set: &LinkSet,
    runs: &[Run],
    log: &Path,
    task: &TaskRef,
    kind: LinkKind,
    id: &str,
) -> Result<Vec<Edit>, String> {
    let id = check_link_id(kind, id)?;
    let me = Node::Task(task_key(task));
    let about = |node: Node| Edit::Link {
        from: node,
        to: me.clone(),
        rel: Rel::About,
        origin: Origin::Manual,
    };
    Ok(match kind {
        LinkKind::Review | LinkKind::Verify => {
            let (dir, node) = match kind {
                LinkKind::Review => (
                    links::review_dir(places.data, &id),
                    Node::Review(id.clone()),
                ),
                _ => (
                    links::verify_dir(places.data, &id),
                    Node::Verify(id.clone()),
                ),
            };
            let found = dir.is_some_and(|dir| dir.join("events.jsonl").is_file());
            if !found {
                return Err(format!("{id} の記録が見つかりません"));
            }
            vec![about(node)]
        }
        LinkKind::Commit => {
            let spec = format!("{id}^{{commit}}");
            let out =
                git(places.main, &["rev-parse", "--verify", "--quiet", &spec]).map_err(|_| {
                    format!("{id} に当たるコミットが見つからないか、一つに決まりません")
                })?;
            let sha = String::from_utf8_lossy(&out).trim().to_owned();
            vec![Edit::Link {
                from: me,
                to: Node::Change(ChangeId::Commit(sha)),
                rel: Rel::Produced,
                origin: Origin::Manual,
            }]
        }
        LinkKind::Run => {
            let node = Node::Run(id.clone());
            let logged = runs.iter().find(|r| r.run.as_str() == id);
            if logged.is_none() && !set.nodes().contains(&node) {
                return Err(format!("{id} の記録が見つかりません"));
            }
            let mut edits = vec![Edit::Link {
                from: me,
                to: node.clone(),
                rel: Rel::Ran,
                origin: Origin::Manual,
            }];
            if let Some(run) = logged {
                edits.push(Edit::Observe {
                    node,
                    info: run_info(
                        run.agent,
                        &run.run,
                        log,
                        &run.workdir,
                        run.branch.as_deref(),
                        None,
                    ),
                });
            }
            edits
        }
    })
}

/// What a manual add did, as a notice.
pub fn added(kind: LinkKind, id: &str, outcomes: &[Outcome]) -> String {
    match outcomes.first() {
        Some(Outcome::Linked(_)) => format!("{} {id} をリンクしました", kind.label()),
        Some(Outcome::Refused(why)) => format!("リンクできません: {why}"),
        _ => format!("リンクできません: {}", links::ALREADY_LINKED),
    }
}

/// What a リンクを外す did, as a notice.
pub fn removed(ids: &[LinkId], outcomes: &[Outcome]) -> String {
    let done = outcomes.iter().filter(|o| **o == Outcome::Unlinked).count();
    if done == 0 {
        "外すリンクがありません（既に外されています）".to_owned()
    } else {
        format!("リンクを外しました（{done}/{} 件）", ids.len())
    }
}
