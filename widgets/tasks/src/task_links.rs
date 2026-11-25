//! Task links (`docs/specs/task-links.md`): the link log that ties a task
//! to what its runs produced (change versions, Review and Verify sessions,
//! PRs), the way Review and Verify find the task run their target belongs
//! to, and the way back from a session to its task.
//!
//! ```text
//! <data dir>/noa/tasks/<repo-key>/
//!   runs.jsonl         Tasks' run log (read here, RunStarted lines only)
//!   links.jsonl        the link log (append-only LinkEvent lines)
//!   links.jsonl.lock   held around every append: three processes write
//! ```
//!
//! This file is byte-identical in `widgets/tasks`, `widgets/review` and
//! `widgets/verify` (a test compares each copy with the Tasks one), so it
//! names nothing of its crate.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub type Millis = u64;
pub const LINKS_FILE: &str = "links.jsonl";
pub const RUNS_FILE: &str = "runs.jsonl";
/// How often Tasks looks for a run's head having moved.
pub const CHANGE_POLL_MS: Millis = 10_000;

const REVIEWS_DIR: &str = "reviews";
const VERIFICATIONS_DIR: &str = "verifications";
const SESSION_LOG: &str = "events.jsonl";

/// `<data dir>/noa/tasks` (`noa_config::session_state_path()`'s parent +
/// `tasks`), where Tasks keeps one key dir per repository.
pub fn default_root() -> Option<PathBuf> {
    let state = noa_config::session_state_path()?;
    Some(state.parent()?.join("tasks"))
}

/// The key dirs under `root` that hold a runs.jsonl or a links.jsonl,
/// sorted. A link named like a key dir is not followed.
pub fn key_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .map(|entry| entry.path())
        .filter(|dir| dir.join(RUNS_FILE).is_file() || dir.join(LINKS_FILE).is_file())
        .collect();
    dirs.sort();
    dirs
}

/// `<key dir>/links.jsonl`.
pub fn links_log(key_dir: &Path) -> PathBuf {
    key_dir.join(LINKS_FILE)
}

/// Tasks' `TaskRef` as JSON (`{"local":"t-1"}`,
/// `{"github":{"repo":"o/n","number":12}}`,
/// `{"code":{"path":"src/a.rs","key":"…"}}`): opaque outside Tasks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskKey(pub serde_json::Value);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    Review,
    Verify,
}

/// Which rule of [`match_run`] linked a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchedBy {
    Worktree,
    Branch,
    Commit,
}

/// One line of the link log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum LinkEvent {
    /// The repository Tasks opened on this key dir.
    Repo { path: PathBuf, at: Millis },
    /// The commit a run started from.
    ChangeBase {
        run: String,
        commit: String,
        at: Millis,
    },
    /// A run's head, seen to have moved.
    ChangeSeen {
        run: String,
        commit: String,
        at: Millis,
    },
    /// A Review or Verify session whose target matched a run.
    SessionLinked {
        kind: SessionKind,
        session: String,
        task: TaskKey,
        run: String,
        commit: Option<String>,
        uncommitted: bool,
        by: MatchedBy,
        at: Millis,
    },
    /// A linked session's verdict or ended state, as it changes.
    SessionOutcome {
        kind: SessionKind,
        session: String,
        decision: Option<String>,
        closed: bool,
        at: Millis,
    },
    /// A PR the person linked by hand.
    PrLinked {
        id: String,
        task: TaskKey,
        url: String,
        at: Millis,
    },
    /// A PR or session link the person removed (final for that id).
    LinkRemoved {
        task: TaskKey,
        id: String,
        at: Millis,
    },
}

/// Holds `links.jsonl.lock` beside `log` until dropped. Opened
/// `O_NOFOLLOW`: a link in its place is refused, not followed.
fn lock(log: &Path) -> io::Result<fs::File> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut name = log.as_os_str().to_owned();
    name.push(".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW)
        .open(PathBuf::from(name))?;
    // SAFETY: flock on a descriptor this function owns.
    while unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
    Ok(file)
}

/// Appends one event to the link log under its lock. A torn last line (a
/// writer died mid-write) is ended first, so this event is a line of its
/// own; the line goes out in one write and is synced.
pub fn append(log: &Path, event: &LinkEvent) -> io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _};
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir)?;
    }
    let _lock = lock(log)?;
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(log)?;
    let len = file.metadata()?.len();
    let mut last = [0u8; 1];
    let torn = len > 0 && {
        file.read_exact_at(&mut last, len - 1)?;
        last[0] != b'\n'
    };
    let mut line = if torn { b"\n".to_vec() } else { Vec::new() };
    serde_json::to_writer(&mut line, event).map_err(io::Error::other)?;
    line.push(b'\n');
    (&file).write_all(&line)?;
    file.sync_data()
}

/// The events of the link log in file order. A missing log is empty; a
/// line that does not parse (torn, or an event of a newer writer) is
/// skipped.
pub fn read(log: &Path) -> io::Result<Vec<LinkEvent>> {
    let text = match fs::read(log) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(text
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect())
}

/// A PR link id no event uses: `pr-<now>`, then `pr-<now>-2`, `-3` …
/// (ids are never reused, removed ones included).
pub fn new_pr_id(now: Millis, events: &[LinkEvent]) -> String {
    let taken = |id: &str| {
        events.iter().any(|event| match event {
            LinkEvent::PrLinked { id: used, .. } | LinkEvent::LinkRemoved { id: used, .. } => {
                used == id
            }
            _ => false,
        })
    };
    (1u32..)
        .map(|n| match n {
            1 => format!("pr-{now}"),
            n => format!("pr-{now}-{n}"),
        })
        .find(|id| !taken(id))
        .expect("an unbounded range has a free id")
}

/// A run as the link log's readers need it: its `RunStarted` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRecord {
    pub key_dir: PathBuf,
    pub run: String,
    pub task: TaskKey,
    pub workdir: PathBuf,
    pub branch: Option<String>,
    pub started: Millis,
}

/// The fields of a `RunStarted` line read here; Tasks may add others.
#[derive(Deserialize)]
struct RunStartedLine {
    run: String,
    task: TaskKey,
    workdir: PathBuf,
    #[serde(default)]
    branch: Option<String>,
    at: Millis,
}

/// The runs of `key_dir`'s run log, in log order: its `RunStarted` lines
/// (the first per run), every other line ignored.
pub fn read_runs(key_dir: &Path) -> Vec<RunRecord> {
    let Ok(text) = fs::read(key_dir.join(RUNS_FILE)) else {
        return Vec::new();
    };
    let mut runs: Vec<RunRecord> = Vec::new();
    for line in text.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("event").and_then(|e| e.as_str()) != Some("RunStarted") {
            continue;
        }
        let Ok(started) = serde_json::from_value::<RunStartedLine>(value) else {
            continue;
        };
        if runs.iter().any(|r| r.run == started.run) {
            continue;
        }
        runs.push(RunRecord {
            key_dir: key_dir.to_path_buf(),
            run: started.run,
            task: started.task,
            workdir: started.workdir,
            branch: started.branch,
            started: started.at,
        });
    }
    runs
}

/// What a Review or Verify session checks, as far as matching goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub toplevel: PathBuf,
    pub head_ref: Option<String>,
    pub head_sha: Option<String>,
}

/// The run a target belongs to, and the rule that said so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Matched {
    pub run: RunRecord,
    pub by: MatchedBy,
}

/// One path, compared canonicalized when both exist (`/tmp` is
/// `/private/tmp` on macOS), else as written.
fn same_path(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.components().eq(b.components()),
    }
}

/// The repository `path` is in: its git common dir, canonicalized (one for
/// a repository and all its worktrees, another for each clone).
fn repository_of(path: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    fs::canonicalize(text.trim_end_matches('\n')).ok()
}

/// The repository of the last `Repo` line of `key_dir`'s link log.
fn key_repository(key_dir: &Path) -> Option<PathBuf> {
    let path = read(&links_log(key_dir))
        .ok()?
        .into_iter()
        .rev()
        .find_map(|event| match event {
            LinkEvent::Repo { path, .. } => Some(path),
            _ => None,
        })?;
    repository_of(&path)
}

/// The task run `target` belongs to, over every key dir under `root`: the
/// first rule that matches, newest run first:
/// - (a) worktree: the run has a branch (a new worktree), its workdir is
///   the target's top level, and the target has no ref or the run's branch;
/// - (b) branch: the target's ref is the run's branch;
/// - (c) commit: the target's commit is one the run's key dir recorded for
///   the run (`ChangeSeen`).
///
/// (b) and (c) take only key dirs of the target's repository (the git
/// common dir of their last `Repo`): another clone may hold the same
/// branch name or commit.
///
/// A run without a branch works in the repository itself, shared with
/// every other session, so (a) never applies to it.
pub fn match_run(root: &Path, target: &Target) -> Option<Matched> {
    let dirs = key_dirs(root);
    let mut runs: Vec<(usize, RunRecord)> = dirs
        .iter()
        .flat_map(|dir| read_runs(dir))
        .enumerate()
        .collect();
    // Newest first; of two started in one millisecond, the later line.
    runs.sort_by(|(ia, a), (ib, b)| b.started.cmp(&a.started).then(ib.cmp(ia)));
    let runs: Vec<RunRecord> = runs.into_iter().map(|(_, run)| run).collect();
    let found = |by: MatchedBy, rule: &dyn Fn(&RunRecord) -> bool| {
        runs.iter().find(|run| rule(run)).map(|run| Matched {
            run: run.clone(),
            by,
        })
    };
    let head_ref = target
        .head_ref
        .as_deref()
        .map(|r| r.strip_prefix("refs/heads/").unwrap_or(r));
    // The work tree itself, or the run's own branch in it: another ref
    // there (`HEAD~1`, `main`) is not the run's head.
    let worktree = |run: &RunRecord| {
        run.branch.is_some()
            && same_path(&run.workdir, &target.toplevel)
            && head_ref.is_none_or(|r| run.branch.as_deref() == Some(r))
    };
    let mine = repository_of(&target.toplevel);
    let same_repo: Vec<&PathBuf> = dirs
        .iter()
        .filter(|dir| mine.is_some() && key_repository(dir) == mine)
        .collect();
    let in_repo = |run: &RunRecord| same_repo.contains(&&run.key_dir);
    let branch =
        |run: &RunRecord| head_ref.is_some() && run.branch.as_deref() == head_ref && in_repo(run);
    found(MatchedBy::Worktree, &worktree)
        .or_else(|| found(MatchedBy::Branch, &branch))
        .or_else(|| {
            let sha = target.head_sha.as_deref()?;
            let seen: Vec<(PathBuf, String)> = same_repo
                .iter()
                .copied()
                .flat_map(|dir| {
                    read(&links_log(dir))
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(move |event| match event {
                            LinkEvent::ChangeSeen { run, commit, .. } if commit == sha => {
                                Some((dir.clone(), run))
                            }
                            _ => None,
                        })
                })
                .collect();
            let commit = |run: &RunRecord| {
                seen.iter()
                    .any(|(dir, id)| *dir == run.key_dir && *id == run.run)
            };
            found(MatchedBy::Commit, &commit)
        })
}

/// Records a session linked to the matched run, in the key dir the run was
/// found in.
pub fn link_session(
    m: &Matched,
    kind: SessionKind,
    session: &str,
    commit: Option<&str>,
    uncommitted: bool,
    now: Millis,
) -> io::Result<()> {
    append(
        &links_log(&m.run.key_dir),
        &LinkEvent::SessionLinked {
            kind,
            session: session.to_owned(),
            task: m.run.task.clone(),
            run: m.run.run.clone(),
            commit: commit.map(str::to_owned),
            uncommitted,
            by: m.by,
            at: now,
        },
    )
}

/// The task (and run) a session was linked to, and the repository Tasks
/// last opened on that key dir (its last `Repo`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub key_dir: PathBuf,
    pub tasks_repo: Option<PathBuf>,
    pub task: TaskKey,
    pub run: String,
}

/// The `SessionLinked` of (kind, session) in any key dir, unless a
/// `LinkRemoved` names it.
pub fn find_origin(root: &Path, kind: SessionKind, session: &str) -> Option<Origin> {
    key_dirs(root).into_iter().find_map(|key_dir| {
        let events = read(&links_log(&key_dir)).ok()?;
        let (task, run) = events.iter().find_map(|event| match event {
            LinkEvent::SessionLinked {
                kind: k,
                session: s,
                task,
                run,
                ..
            } if *k == kind && s == session => Some((task.clone(), run.clone())),
            _ => None,
        })?;
        let removed = events.iter().any(|event| {
            matches!(event, LinkEvent::LinkRemoved { task: t, id, .. } if *t == task && id == session)
        });
        if removed {
            return None;
        }
        let tasks_repo = events.iter().rev().find_map(|event| match event {
            LinkEvent::Repo { path, .. } => Some(path.clone()),
            _ => None,
        });
        Some(Origin {
            key_dir,
            tasks_repo,
            task,
            run,
        })
    })
}

/// Records a linked session's verdict (`decision`, its name) and whether
/// it ended, in its origin's key dir.
pub fn record_outcome(
    origin: &Origin,
    kind: SessionKind,
    session: &str,
    decision: Option<&str>,
    closed: bool,
    now: Millis,
) -> io::Result<()> {
    append(
        &links_log(&origin.key_dir),
        &LinkEvent::SessionOutcome {
            kind,
            session: session.to_owned(),
            decision: decision.map(str::to_owned),
            closed,
            at: now,
        },
    )
}

/// What Tasks' `--select=` takes for `task`: `t-…` for a local task, `#N`
/// for an issue, `code:<key>:<path>` for a code marker. The key alone is not
/// enough (markers with the same text in two files share it); it holds no
/// `:`, so the first `:` after `code:` ends it and the rest is the path.
pub fn select_arg(task: &TaskKey) -> Option<String> {
    let object = task.0.as_object()?;
    if let Some(id) = object.get("local") {
        return id.as_str().map(str::to_owned);
    }
    if let Some(issue) = object.get("github") {
        return issue.get("number")?.as_u64().map(|n| format!("#{n}"));
    }
    if let Some(marker) = object.get("code") {
        let key = marker.get("key")?.as_str()?;
        let path = marker.get("path")?.as_str()?;
        return Some(format!("code:{key}:{path}"));
    }
    None
}

/// Tasks' arguments to open `origin`'s task: `--repo=<tasks repo>` and
/// `--select=<task>`. `Err` says why not, for the button.
pub fn tasks_args(origin: &Origin) -> Result<Vec<OsString>, String> {
    let repo = origin
        .tasks_repo
        .as_ref()
        .ok_or_else(|| "タスクのリポジトリがわかりません".to_owned())?;
    if !repo.is_dir() {
        return Err(format!("{} はもうありません", repo.display()));
    }
    let select = select_arg(&origin.task).ok_or_else(|| "タスクを特定できません".to_owned())?;
    let mut repo_arg = OsString::from("--repo=");
    repo_arg.push(repo);
    Ok(vec![repo_arg, OsString::from(format!("--select={select}"))])
}

/// `<tasks root>/../reviews/<id>` or `../verifications/<id>`; `None` for an
/// id that is not one plain path component starting `rev-` / `ver-`, or a
/// folder without `events.jsonl`.
pub fn session_store(tasks_root: &Path, kind: SessionKind, session: &str) -> Option<PathBuf> {
    let (prefix, dir) = match kind {
        SessionKind::Review => ("rev-", REVIEWS_DIR),
        SessionKind::Verify => ("ver-", VERIFICATIONS_DIR),
    };
    let plain = session.len() > prefix.len()
        && session.starts_with(prefix)
        && !session.contains(['/', '\0'])
        && Path::new(session).components().count() == 1;
    if !plain {
        return None;
    }
    let store = tasks_root.parent()?.join(dir).join(session);
    store.join(SESSION_LOG).is_file().then_some(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "task-links-{}-{name}-{}",
            env!("CARGO_PKG_NAME"),
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn key(json: &str) -> TaskKey {
        TaskKey(serde_json::from_str(json).unwrap())
    }

    fn local(id: &str) -> TaskKey {
        TaskKey(serde_json::json!({ "local": id }))
    }

    fn line(event: &LinkEvent) -> String {
        serde_json::to_string(event).unwrap()
    }

    /// Writes a `RunStarted` line as Tasks does (with the fields this
    /// module does not read).
    fn run_started(
        key_dir: &Path,
        run: &str,
        task: &str,
        workdir: &Path,
        branch: Option<&str>,
        at: Millis,
    ) {
        use std::io::Write as _;
        fs::create_dir_all(key_dir).unwrap();
        let line = serde_json::json!({
            "event": "RunStarted", "run": run, "task": serde_json::from_str::<serde_json::Value>(task).unwrap(),
            "agent": "codex", "workdir": workdir, "branch": branch,
            "label": format!("task:{run}"), "at": at,
        });
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(key_dir.join(RUNS_FILE))
            .unwrap();
        writeln!(file, "{line}").unwrap();
    }

    /// The spec's golden lines (L2-2), with one difference: a `TaskKey`
    /// is a `serde_json::Value`, whose object keys serde_json writes sorted
    /// (its `preserve_order` feature is off), so the issue task is written
    /// `{"number":12,"repo":"o/n"}`. Tasks' own order still reads back
    /// (`links_ac2_the_specs_task_key_order_reads_back`).
    fn golden() -> Vec<(LinkEvent, String)> {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        vec![
            (
                LinkEvent::Repo {
                    path: PathBuf::from("/src/noa"),
                    at: 1,
                },
                r#"{"event":"Repo","path":"/src/noa","at":1}"#.to_owned(),
            ),
            (
                LinkEvent::ChangeBase {
                    run: "run-5".into(),
                    commit: sha.into(),
                    at: 2,
                },
                format!(r#"{{"event":"ChangeBase","run":"run-5","commit":"{sha}","at":2}}"#),
            ),
            (
                LinkEvent::ChangeSeen {
                    run: "run-5".into(),
                    commit: sha.into(),
                    at: 3,
                },
                format!(r#"{{"event":"ChangeSeen","run":"run-5","commit":"{sha}","at":3}}"#),
            ),
            (
                LinkEvent::SessionLinked {
                    kind: SessionKind::Review,
                    session: "rev-9".into(),
                    task: key(r#"{"github":{"repo":"o/n","number":12}}"#),
                    run: "run-5".into(),
                    commit: Some(sha.into()),
                    uncommitted: false,
                    by: MatchedBy::Worktree,
                    at: 4,
                },
                format!(
                    r#"{{"event":"SessionLinked","kind":"review","session":"rev-9","task":{{"github":{{"number":12,"repo":"o/n"}}}},"run":"run-5","commit":"{sha}","uncommitted":false,"by":"worktree","at":4}}"#
                ),
            ),
            (
                LinkEvent::SessionOutcome {
                    kind: SessionKind::Review,
                    session: "rev-9".into(),
                    decision: Some("approve".into()),
                    closed: true,
                    at: 5,
                },
                r#"{"event":"SessionOutcome","kind":"review","session":"rev-9","decision":"approve","closed":true,"at":5}"#.to_owned(),
            ),
            (
                LinkEvent::PrLinked {
                    id: "pr-6".into(),
                    task: local("t-1"),
                    url: "https://github.com/o/n/pull/45".into(),
                    at: 6,
                },
                r#"{"event":"PrLinked","id":"pr-6","task":{"local":"t-1"},"url":"https://github.com/o/n/pull/45","at":6}"#.to_owned(),
            ),
            (
                LinkEvent::LinkRemoved {
                    task: local("t-1"),
                    id: "pr-6".into(),
                    at: 7,
                },
                r#"{"event":"LinkRemoved","task":{"local":"t-1"},"id":"pr-6","at":7}"#.to_owned(),
            ),
        ]
    }

    #[test]
    fn links_ac2_each_event_is_its_golden_line_and_round_trips_through_the_log() {
        for (event, json) in golden() {
            assert_eq!(line(&event), json);
            assert_eq!(serde_json::from_str::<LinkEvent>(&json).unwrap(), event);
        }
        let dir = scratch("golden");
        let log = links_log(&dir.join("noa-0123"));
        for (event, _) in golden() {
            append(&log, &event).unwrap();
        }
        let text = fs::read_to_string(&log).unwrap();
        let expected: Vec<String> = golden().into_iter().map(|(_, json)| json).collect();
        assert_eq!(text, expected.join("\n") + "\n");
        let events: Vec<LinkEvent> = golden().into_iter().map(|(event, _)| event).collect();
        assert_eq!(read(&log).unwrap(), events);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn links_ac2_the_specs_task_key_order_reads_back() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let spec = format!(
            r#"{{"event":"SessionLinked","kind":"review","session":"rev-9","task":{{"github":{{"repo":"o/n","number":12}}}},"run":"run-5","commit":"{sha}","uncommitted":false,"by":"worktree","at":4}}"#
        );
        let (event, written) = golden().swap_remove(3);
        assert_eq!(serde_json::from_str::<LinkEvent>(&spec).unwrap(), event);
        assert_eq!(serde_json::from_str::<LinkEvent>(&written).unwrap(), event);
    }

    #[test]
    fn links_ac2_a_missing_log_reads_empty() {
        let dir = scratch("missing");
        assert_eq!(read(&links_log(&dir)).unwrap(), Vec::new());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn links_ac2_a_torn_line_and_an_unknown_event_are_skipped() {
        let dir = scratch("torn");
        let log = links_log(&dir);
        let repo = LinkEvent::Repo {
            path: PathBuf::from("/src/noa"),
            at: 1,
        };
        let seen = LinkEvent::ChangeSeen {
            run: "run-5".into(),
            commit: SHA_A.into(),
            at: 3,
        };
        fs::write(
            &log,
            format!(
                "{}\n{}\n{{\"event\":\"ChangeBa",
                line(&repo),
                r#"{"event":"PrMerged","id":"pr-1","at":2}"#
            ),
        )
        .unwrap();
        assert_eq!(read(&log).unwrap(), vec![repo.clone()], "torn tail skipped");
        append(&log, &seen).unwrap();
        let text = fs::read_to_string(&log).unwrap();
        assert!(
            text.ends_with(&format!("{{\"event\":\"ChangeBa\n{}\n", line(&seen))),
            "the torn line is ended before the append: {text:?}"
        );
        assert_eq!(read(&log).unwrap(), vec![repo, seen]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn links_ac2_an_append_waits_for_the_lock() {
        use std::os::fd::AsRawFd as _;
        let dir = scratch("lock");
        let log = links_log(&dir);
        let held = lock(&log).unwrap();
        let event = LinkEvent::Repo {
            path: PathBuf::from("/src/noa"),
            at: 1,
        };
        let writer = {
            let (log, event) = (log.clone(), event.clone());
            std::thread::spawn(move || append(&log, &event))
        };
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(read(&log).unwrap(), Vec::new(), "the second appender waits");
        assert!(!writer.is_finished());
        // SAFETY: unlocking the descriptor this test holds.
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_UN) };
        drop(held);
        writer.join().unwrap().unwrap();
        assert_eq!(read(&log).unwrap(), vec![event]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn links_ac2_a_link_at_the_lock_or_the_log_is_refused() {
        let dir = scratch("nofollow");
        let elsewhere = dir.join("elsewhere");
        fs::write(&elsewhere, "").unwrap();
        let event = LinkEvent::Repo {
            path: PathBuf::from("/src/noa"),
            at: 1,
        };
        let a = dir.join("a");
        fs::create_dir_all(&a).unwrap();
        std::os::unix::fs::symlink(&elsewhere, a.join("links.jsonl.lock")).unwrap();
        assert!(append(&links_log(&a), &event).is_err());
        let b = dir.join("b");
        fs::create_dir_all(&b).unwrap();
        std::os::unix::fs::symlink(&elsewhere, links_log(&b)).unwrap();
        assert!(append(&links_log(&b), &event).is_err());
        assert_eq!(
            fs::read_to_string(&elsewhere).unwrap(),
            "",
            "nothing written"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn links_ac2_a_pr_id_is_never_reused() {
        assert_eq!(new_pr_id(6, &[]), "pr-6");
        let events = vec![
            LinkEvent::PrLinked {
                id: "pr-6".into(),
                task: local("t-1"),
                url: "https://github.com/o/n/pull/1".into(),
                at: 6,
            },
            LinkEvent::LinkRemoved {
                task: local("t-1"),
                id: "pr-6-2".into(),
                at: 6,
            },
        ];
        assert_eq!(new_pr_id(6, &events), "pr-6-3", "removed ids stay taken");
        assert_eq!(new_pr_id(7, &events), "pr-7");
    }

    #[test]
    fn links_ac3_read_runs_takes_run_started_lines_and_ignores_other_fields() {
        let dir = scratch("runs");
        let key_dir = dir.join("noa-1");
        fs::create_dir_all(&key_dir).unwrap();
        fs::write(
            key_dir.join(RUNS_FILE),
            concat!(
                r#"{"event":"RunStarted","run":"run-5","task":{"github":{"repo":"o/n","number":12}},"agent":"codex","workdir":"/src/noa-wt/task-run-5","branch":"task-run-5","label":"task:run-5","at":5,"future":[1]}"#,
                "\n",
                r#"{"event":"RunPaneAssigned","run":"run-5","pane":41,"board":7,"at":8}"#,
                "\n",
                r#"{"event":"RunStarted","run":"run-6","task":{"local":"t-1"},"agent":"claude","workdir":"/src/noa","branch":null,"label":"task:run-6","at":6}"#,
                "\n",
                r#"{"event":"RunStarted","run":"run-5","task":{"local":"t-9"},"workdir":"/x","at":9}"#,
                "\n",
                r#"{"event":"RunClosed","run":"run-5","at":9}"#,
                "\n",
                r#"{"event":"RunStar"#,
            ),
        )
        .unwrap();
        assert_eq!(
            read_runs(&key_dir),
            vec![
                RunRecord {
                    key_dir: key_dir.clone(),
                    run: "run-5".into(),
                    task: key(r#"{"github":{"repo":"o/n","number":12}}"#),
                    workdir: PathBuf::from("/src/noa-wt/task-run-5"),
                    branch: Some("task-run-5".into()),
                    started: 5,
                },
                RunRecord {
                    key_dir: key_dir.clone(),
                    run: "run-6".into(),
                    task: local("t-1"),
                    workdir: PathBuf::from("/src/noa"),
                    branch: None,
                    started: 6,
                },
            ]
        );
        assert_eq!(read_runs(&dir.join("none")), Vec::new());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn links_ac3_key_dirs_are_those_with_a_log_sorted() {
        let root = scratch("keydirs");
        fs::create_dir_all(root.join("b-2")).unwrap();
        fs::write(root.join("b-2").join(LINKS_FILE), "").unwrap();
        fs::create_dir_all(root.join("a-1")).unwrap();
        fs::write(root.join("a-1").join(RUNS_FILE), "").unwrap();
        fs::create_dir_all(root.join("c-3")).unwrap();
        fs::write(root.join("c-3").join("github.json"), "").unwrap();
        fs::write(root.join("loose.jsonl"), "").unwrap();
        assert_eq!(key_dirs(&root), vec![root.join("a-1"), root.join("b-2")]);
        assert_eq!(key_dirs(&root.join("absent")), Vec::<PathBuf>::new());
        fs::remove_dir_all(&root).unwrap();
    }

    /// Two key dirs: a worktree run and a newer one on the same task in
    /// `noa-1`, a この作業ツリー run in `other-2`.
    fn fixture(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = scratch(name);
        let one = root.join("noa-1");
        let two = root.join("other-2");
        let wt5 = root.join("wt/task-run-5");
        let wt7 = root.join("wt/task-run-7");
        fs::create_dir_all(&wt5).unwrap();
        fs::create_dir_all(&wt7).unwrap();
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        for dir in [&one, &two] {
            append(
                &links_log(dir),
                &LinkEvent::Repo {
                    path: repo.clone(),
                    at: 1,
                },
            )
            .unwrap();
        }
        run_started(
            &one,
            "run-5",
            r#"{"local":"t-1"}"#,
            &wt5,
            Some("task-run-5"),
            5,
        );
        run_started(
            &one,
            "run-7",
            r#"{"github":{"repo":"o/n","number":12}}"#,
            &wt7,
            Some("task-run-7"),
            7,
        );
        run_started(&two, "run-6", r#"{"local":"t-2"}"#, &repo, None, 6);
        (root, one, two)
    }

    #[test]
    fn links_ac3_match_run_rules_in_order_newest_run_first() {
        let (root, one, two) = fixture("match");
        let target = |toplevel: &Path, head_ref: Option<&str>, head_sha: Option<&str>| Target {
            toplevel: toplevel.to_path_buf(),
            head_ref: head_ref.map(str::to_owned),
            head_sha: head_sha.map(str::to_owned),
        };
        // (a): the run's worktree, also through a path that is not written
        // the way runs.jsonl has it.
        let via_dots = root.join("wt/../wt/task-run-5");
        let m = match_run(&root, &target(&via_dots, None, None)).unwrap();
        assert_eq!((m.run.run.as_str(), m.by), ("run-5", MatchedBy::Worktree));
        assert_eq!(m.run.key_dir, one);
        // Not (a): run-5's worktree on run-7's branch is not run-5's head
        // (and this worktree is no git work tree, so no other rule).
        assert_eq!(
            match_run(&root, &target(&via_dots, Some("task-run-7"), None)),
            None
        );
        // (b): the branch, as a ref name or a full ref.
        let repo = root.join("repo");
        let m = match_run(&root, &target(&repo, Some("task-run-7"), None)).unwrap();
        assert_eq!((m.run.run.as_str(), m.by), ("run-7", MatchedBy::Branch));
        let m = match_run(&root, &target(&repo, Some("refs/heads/task-run-5"), None)).unwrap();
        assert_eq!((m.run.run.as_str(), m.by), ("run-5", MatchedBy::Branch));
        // A この作業ツリー run is never matched by its workdir.
        assert_eq!(
            match_run(&root, &target(&repo, Some("main"), Some(SHA_A))),
            None
        );
        // (c): a recorded commit, newest run first; a base is not recorded.
        append(
            &links_log(&two),
            &LinkEvent::ChangeBase {
                run: "run-6".into(),
                commit: SHA_B.into(),
                at: 6,
            },
        )
        .unwrap();
        assert_eq!(match_run(&root, &target(&repo, None, Some(SHA_B))), None);
        append(
            &links_log(&two),
            &LinkEvent::ChangeSeen {
                run: "run-6".into(),
                commit: SHA_A.into(),
                at: 8,
            },
        )
        .unwrap();
        let m = match_run(&root, &target(&repo, Some("main"), Some(SHA_A))).unwrap();
        assert_eq!((m.run.run.as_str(), m.by), ("run-6", MatchedBy::Commit));
        assert_eq!(m.run.key_dir, two);
        append(
            &links_log(&one),
            &LinkEvent::ChangeSeen {
                run: "run-7".into(),
                commit: SHA_A.into(),
                at: 9,
            },
        )
        .unwrap();
        let m = match_run(&root, &target(&repo, None, Some(SHA_A))).unwrap();
        assert_eq!(
            (m.run.run.as_str(), m.by),
            ("run-7", MatchedBy::Commit),
            "newest run"
        );
        // A commit recorded for a run of another key dir does not count.
        append(
            &links_log(&two),
            &LinkEvent::ChangeSeen {
                run: "run-5".into(),
                commit: SHA_B.into(),
                at: 10,
            },
        )
        .unwrap();
        assert_eq!(match_run(&root, &target(&repo, None, Some(SHA_B))), None);
        // Nothing matches an unrelated repository.
        let other = root.join("unrelated");
        fs::create_dir_all(&other).unwrap();
        assert_eq!(
            match_run(&root, &target(&other, Some("feature"), Some("c0ffee"))),
            None
        );
        fs::remove_dir_all(&root).unwrap();
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@example.test"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// A worktree run's own work tree on another ref (`HEAD~1`) is not the
    /// run's head, so the worktree rule does not take it; a ref or a commit
    /// of another clone with the same branch name or SHA matches nothing.
    #[test]
    fn links_ac3_match_run_keeps_to_the_runs_head_and_repository() {
        let root = scratch("scope");
        let a = root.join("a");
        fs::create_dir_all(&a).unwrap();
        git(&a, &["init", "-q", "-b", "main"]);
        git(&a, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let wt = root.join("a-wt");
        git(
            &a,
            &["worktree", "add", "-qb", "task-run-5", wt.to_str().unwrap()],
        );
        git(&wt, &["commit", "-q", "--allow-empty", "-m", "one"]);
        let sha = git(&wt, &["rev-parse", "HEAD"]);
        let b = root.join("b");
        git(
            &root,
            &["clone", "-q", a.to_str().unwrap(), b.to_str().unwrap()],
        );
        git(&b, &["branch", "-q", "task-run-5", &sha]);
        let dir = root.join("tasks/a-1");
        run_started(
            &dir,
            "run-5",
            r#"{"local":"t-1"}"#,
            &wt,
            Some("task-run-5"),
            5,
        );
        for event in [
            LinkEvent::Repo {
                path: a.clone(),
                at: 1,
            },
            LinkEvent::ChangeSeen {
                run: "run-5".into(),
                commit: sha.clone(),
                at: 6,
            },
        ] {
            append(&links_log(&dir), &event).unwrap();
        }
        let tasks = root.join("tasks");
        let target = |toplevel: &Path, head_ref: Option<&str>, head_sha: Option<&str>| Target {
            toplevel: toplevel.to_path_buf(),
            head_ref: head_ref.map(str::to_owned),
            head_sha: head_sha.map(str::to_owned),
        };
        let by = |t: Target| match_run(&tasks, &t).map(|m| m.by);
        assert_eq!(by(target(&wt, None, Some(&sha))), Some(MatchedBy::Worktree));
        assert_eq!(
            by(target(&wt, Some("task-run-5"), Some(&sha))),
            Some(MatchedBy::Worktree)
        );
        assert_eq!(by(target(&wt, Some("HEAD~1"), Some("c1c1"))), None, "F3");
        assert_eq!(
            by(target(&a, Some("task-run-5"), Some(&sha))),
            Some(MatchedBy::Branch)
        );
        assert_eq!(
            by(target(&a, Some("main"), Some(&sha))),
            Some(MatchedBy::Commit)
        );
        assert_eq!(
            by(target(&b, Some("task-run-5"), Some(&sha))),
            None,
            "F5 branch"
        );
        assert_eq!(by(target(&b, Some("main"), Some(&sha))), None, "F5 commit");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn links_ac3_a_linked_session_and_its_outcome_go_to_the_runs_key_dir() {
        let (root, one, two) = fixture("link");
        let m = match_run(
            &root,
            &Target {
                toplevel: root.join("wt/task-run-7"),
                head_ref: None,
                head_sha: Some(SHA_A.into()),
            },
        )
        .unwrap();
        link_session(&m, SessionKind::Verify, "ver-1", Some(SHA_A), true, 20).unwrap();
        let origin = find_origin(&root, SessionKind::Verify, "ver-1").unwrap();
        record_outcome(
            &origin,
            SessionKind::Verify,
            "ver-1",
            Some("pass"),
            false,
            21,
        )
        .unwrap();
        let task = key(r#"{"github":{"repo":"o/n","number":12}}"#);
        // The fixture's `Repo` lines aside.
        let written = |dir: &Path| -> Vec<LinkEvent> {
            read(&links_log(dir))
                .unwrap()
                .into_iter()
                .filter(|e| !matches!(e, LinkEvent::Repo { .. }))
                .collect()
        };
        assert_eq!(
            written(&one),
            vec![
                LinkEvent::SessionLinked {
                    kind: SessionKind::Verify,
                    session: "ver-1".into(),
                    task,
                    run: "run-7".into(),
                    commit: Some(SHA_A.into()),
                    uncommitted: true,
                    by: MatchedBy::Worktree,
                    at: 20,
                },
                LinkEvent::SessionOutcome {
                    kind: SessionKind::Verify,
                    session: "ver-1".into(),
                    decision: Some("pass".into()),
                    closed: false,
                    at: 21,
                },
            ]
        );
        assert_eq!(written(&two), Vec::new());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn links_ac5_find_origin_names_the_task_and_the_last_repo_unless_removed() {
        let root = scratch("origin");
        let key_dir = root.join("noa-1");
        let log = links_log(&key_dir);
        let task = key(r#"{"code":{"path":"src/a.rs","key":"k1"}}"#);
        let linked = |session: &str| LinkEvent::SessionLinked {
            kind: SessionKind::Review,
            session: session.into(),
            task: task.clone(),
            run: "run-5".into(),
            commit: None,
            uncommitted: false,
            by: MatchedBy::Branch,
            at: 2,
        };
        for event in [
            LinkEvent::Repo {
                path: PathBuf::from("/src/old"),
                at: 1,
            },
            linked("rev-1"),
            linked("rev-2"),
            LinkEvent::Repo {
                path: PathBuf::from("/src/noa"),
                at: 3,
            },
            LinkEvent::LinkRemoved {
                task: task.clone(),
                id: "rev-2".into(),
                at: 4,
            },
        ] {
            append(&log, &event).unwrap();
        }
        assert_eq!(
            find_origin(&root, SessionKind::Review, "rev-1"),
            Some(Origin {
                key_dir: key_dir.clone(),
                tasks_repo: Some(PathBuf::from("/src/noa")),
                task: task.clone(),
                run: "run-5".into(),
            })
        );
        assert_eq!(
            find_origin(&root, SessionKind::Review, "rev-2"),
            None,
            "removed"
        );
        assert_eq!(
            find_origin(&root, SessionKind::Verify, "rev-1"),
            None,
            "other kind"
        );
        assert_eq!(find_origin(&root, SessionKind::Review, "rev-3"), None);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn links_ac5_select_arg_and_tasks_args() {
        assert_eq!(select_arg(&local("t-1")).as_deref(), Some("t-1"));
        assert_eq!(
            select_arg(&key(r#"{"github":{"repo":"o/n","number":12}}"#)).as_deref(),
            Some("#12")
        );
        assert_eq!(
            select_arg(&key(r#"{"code":{"path":"src/a.rs","key":"k1"}}"#)).as_deref(),
            Some("code:k1:src/a.rs")
        );
        // The same marker text in two files: same key, different arguments.
        let a = select_arg(&key(
            r#"{"code":{"path":"src/a.rs","key":"0123456789abcdef"}}"#,
        ));
        let b = select_arg(&key(
            r#"{"code":{"path":"src/b.rs","key":"0123456789abcdef"}}"#,
        ));
        assert_ne!(a, b);
        // A path holding `:` still splits back at the first `:` after `code:`.
        let arg = select_arg(&key(
            r#"{"code":{"path":"dir:x/a.rs","key":"0123456789abcdef-2"}}"#,
        ))
        .unwrap();
        assert_eq!(
            arg.strip_prefix("code:")
                .and_then(|rest| rest.split_once(':')),
            Some(("0123456789abcdef-2", "dir:x/a.rs"))
        );
        assert_eq!(
            select_arg(&key(r#"{"code":{"key":"k1"}}"#)),
            None,
            "a marker without its path cannot be selected"
        );
        assert_eq!(select_arg(&key(r#"{"jira":"X-1"}"#)), None);
        assert_eq!(select_arg(&key(r#""t-1""#)), None);

        let repo = scratch("tasks-args");
        let origin = |task: TaskKey, tasks_repo: Option<PathBuf>| Origin {
            key_dir: PathBuf::from("/data/noa/tasks/noa-1"),
            tasks_repo,
            task,
            run: "run-5".into(),
        };
        let repo_arg = {
            let mut arg = OsString::from("--repo=");
            arg.push(&repo);
            arg
        };
        for (task, select) in [
            (local("t-1"), "--select=t-1"),
            (
                key(r#"{"github":{"repo":"o/n","number":12}}"#),
                "--select=#12",
            ),
            (
                key(r#"{"code":{"path":"src/a.rs","key":"k1"}}"#),
                "--select=code:k1:src/a.rs",
            ),
        ] {
            assert_eq!(
                tasks_args(&origin(task, Some(repo.clone()))),
                Ok(vec![repo_arg.clone(), OsString::from(select)])
            );
        }
        assert_eq!(
            tasks_args(&origin(local("t-1"), None)),
            Err("タスクのリポジトリがわかりません".to_owned())
        );
        let gone = repo.join("gone");
        assert_eq!(
            tasks_args(&origin(local("t-1"), Some(gone.clone()))),
            Err(format!("{} はもうありません", gone.display()))
        );
        fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn links_ac5_session_store_takes_only_a_plain_stored_session() {
        let data = scratch("store");
        let tasks_root = data.join("tasks");
        fs::create_dir_all(&tasks_root).unwrap();
        let rev = data.join("reviews/rev-1");
        let ver = data.join("verifications/ver-1");
        for dir in [&rev, &ver] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("events.jsonl"), "").unwrap();
        }
        fs::create_dir_all(data.join("reviews/rev-empty")).unwrap();
        assert_eq!(
            session_store(&tasks_root, SessionKind::Review, "rev-1"),
            Some(rev)
        );
        assert_eq!(
            session_store(&tasks_root, SessionKind::Verify, "ver-1"),
            Some(ver)
        );
        for (kind, bad) in [
            (SessionKind::Review, "../x"),
            (SessionKind::Review, "a/b"),
            (SessionKind::Review, "rev-1/../rev-1"),
            (SessionKind::Review, "x-1"),
            (SessionKind::Review, "rev-"),
            (SessionKind::Review, ""),
            (SessionKind::Review, "ver-1"),
            (SessionKind::Verify, "rev-1"),
            (SessionKind::Review, "rev-empty"),
            (SessionKind::Review, "rev-absent"),
        ] {
            assert_eq!(
                session_store(&tasks_root, kind, bad),
                None,
                "{kind:?} {bad:?}"
            );
        }
        fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn links_copy_is_byte_identical_to_the_tasks_master() {
        assert!(
            include_str!("task_links.rs") == include_str!("../../tasks/src/task_links.rs"),
            "src/task_links.rs differs from widgets/tasks/src/task_links.rs: copy it over"
        );
    }
}
