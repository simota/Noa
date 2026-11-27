//! Versioned handoffs and result summaries shared with the Workflow helper.
//! Results remain Workflow's reports, not Review/Verify sessions or verdicts.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::fsx;
use crate::model::types::{Millis, Task, TaskRef};
use crate::store;
use crate::task_links::{self, TaskKey};

const MAX_BYTES: u64 = 128 * 1024;
const MAX_RESULTS: usize = 500;

#[derive(Debug, Serialize)]
pub struct Handoff<'a> {
    pub schema_version: u32,
    pub repo: &'a Path,
    pub task: String,
    pub task_key: String,
    pub title: &'a str,
    pub objective: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultLink {
    pub schema_version: u32,
    pub repo: PathBuf,
    pub task: String,
    pub task_key: String,
    pub run_id: String,
    pub title: String,
    pub state: String,
    pub target_digest: Option<String>,
    pub definition_digest: String,
    pub summary: String,
    pub pr_url: Option<String>,
    pub updated_at: Millis,
    pub source: String,
}

pub fn select(task: &TaskRef) -> Option<String> {
    task_links::select_arg(&TaskKey(serde_json::to_value(task).ok()?))
}

pub fn directory(root: &Path, repo: &Path) -> PathBuf {
    root.join(store::repo_key(repo)).join("workflow-results")
}

pub fn valid_run(run: &str) -> bool {
    run.starts_with("run-")
        && run.len() <= 128
        && run
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

/// A private, immutable request. Reopening its form cannot change a live run.
pub fn handoff(root: &Path, repo: &Path, task: &Task, now: Millis) -> Result<PathBuf, String> {
    let select = select(&task.r#ref).ok_or("タスクを特定できません")?;
    let request = Handoff {
        schema_version: 1,
        repo,
        task: select,
        task_key: serde_json::to_string(&task.r#ref).map_err(|e| e.to_string())?,
        title: &task.title,
        objective: format!(
            "# {}\n\n{}",
            task.title,
            task.body.as_deref().unwrap_or_default()
        ),
    };
    if request.task_key.len() > 8192 {
        return Err("Workflow に引き継ぐタスク ID が長すぎます".into());
    }
    if request.title.len() > 4096 || request.objective.len() > 65536 {
        return Err("Workflow への引継ぎはタイトル 4 KiB、作業指示 64 KiB までです。内容を整理してから開いてください".into());
    }
    let bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Workflow への作業指示が 128 KiB を超えています".into());
    }
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let dir = root.join(store::repo_key(repo)).join("workflow-handoffs");
    let opened = fsx::open_dir(root, &dir, true).map_err(|e| e.to_string())?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    for _ in 0..32 {
        let name = OsString::from(format!(
            "{now}-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match write_private(&opened, &name, &bytes) {
            Ok(()) => return Ok(dir.join(name)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("作業指示を保存できません: {e}")),
        }
    }
    Err("作業指示の保存先が競合しました。もう一度開いてください".into())
}

fn write_private(dir: &OwnedFd, name: &std::ffi::OsStr, bytes: &[u8]) -> io::Result<()> {
    let c = std::ffi::CString::new(name.as_bytes()).map_err(io::Error::other)?;
    // SAFETY: dir is an open directory and c is a single generated filename.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this newly opened descriptor has one owner.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        fsx::remove_if_same_at(dir, name, &file)?;
        return Err(error);
    }
    Ok(())
}

fn read(root: &Path, repo: &Path, path: &Path) -> Option<ResultLink> {
    let (bytes, over) = fsx::read_capped(fsx::open_read(root, path).ok()?, MAX_BYTES).ok()?;
    if over {
        return None;
    }
    let result: ResultLink = serde_json::from_slice(&bytes).ok()?;
    let task_ref: TaskRef = serde_json::from_str(&result.task_key).ok()?;
    if result.schema_version != 1
        || result.repo != repo
        || !valid_run(&result.run_id)
        || result.source != "workflow_agent_reports"
        || path.file_stem()?.to_str()? != result.run_id
        || result.task.len() > 4096
        || result.title.len() > 16384
        || select(&task_ref).as_deref() != Some(result.task.as_str())
    {
        return None;
    }
    Some(result)
}

/// Reads only bounded local summaries. The full evidence remains in Workflow.
pub fn scan(root: &Path, repo: &Path) -> BTreeMap<String, Vec<ResultLink>> {
    let dir = directory(root, repo);
    if fsx::check_no_links(root, &dir).is_err() {
        return BTreeMap::new();
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return BTreeMap::new();
    };
    let mut paths: Vec<_> = entries
        .take(5000)
        .filter_map(Result::ok)
        .filter_map(|e| {
            let metadata = e.metadata().ok()?;
            (metadata.is_file() && e.path().extension().is_some_and(|v| v == "json"))
                .then_some((metadata.modified().ok(), e.path()))
        })
        .collect();
    paths.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut by_task: BTreeMap<String, Vec<ResultLink>> = BTreeMap::new();
    for (_, path) in paths.into_iter().take(MAX_RESULTS) {
        if let Some(result) = read(root, repo, &path) {
            by_task.entry(result.task.clone()).or_default().push(result);
        }
    }
    for results in by_task.values_mut() {
        results.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
    }
    by_task
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    #[test]
    fn handoff_is_immutable_and_keeps_the_whole_task() {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("noa-workflow-handoff-{}", std::process::id()));
        let repo = root.join("repo with spaces");
        std::fs::create_dir_all(&repo).unwrap();
        let task = testkit::local("t-1", "Plan & implement");
        let a = handoff(&root, &repo, &task, 1).unwrap();
        let b = handoff(&root, &repo, &task, 1).unwrap();
        assert_ne!(a, b);
        assert_eq!(
            std::fs::metadata(&a).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(a).unwrap()).unwrap();
        assert_eq!(value["task"], "t-1");
        assert_eq!(value["title"], task.title);
        assert!(value["objective"].as_str().unwrap().contains(&task.title));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_reader_refuses_other_repositories_unknown_sources_and_symlinks() {
        let root =
            std::env::temp_dir().join(format!("noa-workflow-results-{}", std::process::id()));
        let repo = root.join("repo");
        let dir = directory(&root, &repo);
        std::fs::create_dir_all(&dir).unwrap();
        let mut result = ResultLink {
            schema_version: 1,
            repo: repo.clone(),
            task: "t-1".into(),
            task_key: "{\"local\":\"t-1\"}".into(),
            run_id: "run-1".into(),
            title: "A".into(),
            state: "succeeded".into(),
            target_digest: Some("target".into()),
            definition_digest: "definition".into(),
            summary: "done".into(),
            pr_url: None,
            updated_at: 1,
            source: "workflow_agent_reports".into(),
        };
        let path = dir.join("run-1.json");
        std::fs::write(&path, serde_json::to_vec(&result).unwrap()).unwrap();
        assert_eq!(scan(&root, &repo)["t-1"].len(), 1);
        result.source = "review_session".into();
        std::fs::write(&path, serde_json::to_vec(&result).unwrap()).unwrap();
        assert!(scan(&root, &repo).is_empty());
        result.source = "workflow_agent_reports".into();
        result.repo = root.join("other");
        std::fs::write(&path, serde_json::to_vec(&result).unwrap()).unwrap();
        assert!(scan(&root, &repo).is_empty());
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/dev/null", &path).unwrap();
        assert!(scan(&root, &repo).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
