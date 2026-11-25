//! Shared test helpers for the library's units: temporary directories
//! (from `std::env::temp_dir`, removed on drop) and builders for tasks and
//! runs. Nothing here is a fake of an outside program; those belong to the
//! unit that needs them.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::model::types::{
    AgentKind, Liveness, Millis, ResultFact, Run, RunId, RunView, State, Task, TaskRef,
};

/// The repository the issue builders name.
pub const REPO: &str = "noa-dev/noa";

/// A directory under the system temp dir, unique to this process and
/// call, removed with everything in it when dropped.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(name: &str) -> TempDir {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("noa-tasks-{name}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        // The canonical path, so it compares equal to what git prints
        // (macOS's /var is a link to /private/var).
        TempDir(path.canonicalize().unwrap())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An open local task `id` titled `title`, with a body, no labels.
pub fn local(id: &str, title: &str) -> Task {
    Task {
        r#ref: TaskRef::Local(id.to_owned()),
        title: title.to_owned(),
        body: Some(format!("{title} の本文")),
        state: State::Open,
        labels: Vec::new(),
        updated: 1_000,
        url: None,
        closed: None,
        problem: None,
    }
}

/// An open issue `#number` of [`REPO`] titled `title`, its body not loaded.
pub fn issue(number: u64, title: &str) -> Task {
    Task {
        r#ref: TaskRef::GitHub {
            repo: REPO.to_owned(),
            number,
        },
        title: title.to_owned(),
        body: None,
        state: State::Open,
        labels: Vec::new(),
        updated: 1_000,
        url: Some(format!("https://github.com/{REPO}/issues/{number}")),
        closed: None,
        problem: None,
    }
}

/// `task` with `labels`.
pub fn labelled(mut task: Task, labels: &[&str]) -> Task {
    task.labels = labels.iter().map(|l| (*l).to_owned()).collect();
    task
}

/// `task`, closed.
pub fn closed(mut task: Task) -> Task {
    task.state = State::Closed;
    task
}

/// A claude run `run-<started>` on `task` in `/repo`, never closed.
pub fn run(task: &TaskRef, started: Millis) -> Run {
    Run {
        run: RunId::new(format!("run-{started}")),
        task: task.clone(),
        agent: AgentKind::Claude,
        workdir: PathBuf::from("/repo"),
        branch: None,
        pane: None,
        board: None,
        started,
        closed: None,
    }
}

/// [`run`] seen as `liveness` with `result`, no report read.
pub fn run_view(task: &TaskRef, liveness: Liveness, result: ResultFact) -> RunView {
    RunView {
        run: run(task, 1_000),
        liveness,
        result,
        report: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_temp_dir_is_fresh_per_call_and_removed_on_drop() {
        let a = TempDir::new("kit");
        let b = TempDir::new("kit");
        assert_ne!(a.path(), b.path());
        std::fs::write(a.path().join("f"), "x").unwrap();
        let gone = a.path().to_owned();
        drop(a);
        assert!(!gone.exists());
        assert!(b.path().is_dir());
    }

    #[test]
    fn builders_make_open_tasks_of_each_source() {
        assert_eq!(local("t-1", "a").r#ref, TaskRef::Local("t-1".into()));
        assert_eq!(issue(12, "b").r#ref.badge(), "#12");
        assert_eq!(closed(local("t-1", "a")).state, State::Closed);
        assert_eq!(labelled(issue(1, "c"), &["bug"]).labels, ["bug"]);
    }
}
