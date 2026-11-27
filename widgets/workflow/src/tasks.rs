//! Versioned, read-only handoffs and derived result references for Tasks.
//! These are Workflow's reports, never Review/Verify widget sessions.

use crate::adapters::{fs, git};
use crate::model::{ArtifactKind, Run, digest_bytes};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The stable Tasks identity, kept independent of its display selector.
#[derive(Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
enum TaskReference {
    Local(String),
    Github { repo: String, number: u64 },
    Code { path: String, key: String },
}

fn task_selector(key: &str) -> Option<String> {
    let reference: TaskReference = serde_json::from_str(key).ok()?;
    let plain = |value: &str| !value.is_empty() && !value.chars().any(char::is_control);
    match reference {
        TaskReference::Local(id)
            if plain(&id) && id.starts_with("t-") && id.len() > 2 && !id.contains('/') =>
        {
            Some(id)
        }
        TaskReference::Github { repo, number }
            if plain(&repo)
                && repo.split_once('/').is_some_and(|(owner, name)| {
                    plain(owner) && plain(name) && !name.contains('/')
                })
                && number > 0 =>
        {
            Some(format!("#{number}"))
        }
        TaskReference::Code { path, key } if plain(&path) && plain(&key) && !key.contains(':') => {
            Some(format!("code:{key}:{path}"))
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskOrigin {
    pub repo: PathBuf,
    pub task: String,
    pub task_key: String,
    pub title: String,
    pub objective_digest: String,
}

impl TaskOrigin {
    pub fn validate(&self, repository: &Path, objective: &str) -> Result<(), String> {
        if self.repo != repository
            || self.task.is_empty()
            || self.task.len() > 1024
            || self.task.chars().any(char::is_control)
            || self.title.trim().is_empty()
            || self.task_key.len() > 8192
            || task_selector(&self.task_key).as_deref() != Some(self.task.as_str())
            || self.title.len() > 4096
            || self.title.chars().any(char::is_control)
            || self.objective_digest != digest_bytes(objective.as_bytes())
        {
            return Err("元タスクとリポジトリ・目的の対応を確認できません".into());
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Handoff {
    schema_version: u32,
    repo: PathBuf,
    task: String,
    task_key: String,
    title: String,
    objective: String,
}

/// Opening a handoff only fills the start form. It does not start a run.
pub fn read_handoff(path: &Path, repository: &Path) -> Result<(TaskOrigin, String), String> {
    let bytes = fs::read(path, 128 * 1024)?.ok_or("元タスクの引継ぎファイルがありません")?;
    let handoff: Handoff =
        serde_json::from_slice(&bytes).map_err(|e| format!("元タスクの形式が不正です: {e}"))?;
    if handoff.schema_version != 1
        || handoff.objective.trim().is_empty()
        || handoff.objective.len() > 65_536
        || handoff
            .objective
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r'))
    {
        return Err("元タスクの目的は空でない64KiB以内のテキストが必要です".into());
    }
    let origin = TaskOrigin {
        repo: handoff.repo,
        task: handoff.task,
        task_key: handoff.task_key,
        title: handoff.title,
        objective_digest: digest_bytes(handoff.objective.as_bytes()),
    };
    origin.validate(&git::repository(repository)?, &handoff.objective)?;
    Ok((origin, handoff.objective))
}

/// Matches Tasks' public <directory name>-<FNV-1a64(canonical path)> contract.
pub fn repo_key(repo: &Path) -> String {
    let repo: PathBuf = repo.components().collect();
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let hash = repo
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, b| {
            (hash ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    format!("{name}-{hash:016x}")
}

#[derive(Debug, Serialize)]
struct ResultReference<'a> {
    schema_version: u32,
    repo: &'a Path,
    task: &'a str,
    task_key: &'a str,
    run_id: &'a str,
    title: &'a str,
    state: crate::model::NodeState,
    target_digest: String,
    definition_digest: &'a str,
    summary: String,
    pr_url: Option<&'a str>,
    updated_at: u64,
    source: &'static str,
}

pub fn publish(run: &Run) -> Result<(), String> {
    if run.origin_task.is_none() {
        return Ok(());
    }
    let state = noa_config::session_state_path().ok_or("Tasksの保存先が見つかりません")?;
    let root = state
        .parent()
        .ok_or("Tasksの保存先が不正です")?
        .join("tasks");
    publish_in(&root, run)
}

fn publish_in(tasks_root: &Path, run: &Run) -> Result<(), String> {
    let Some(origin) = &run.origin_task else {
        return Ok(());
    };
    origin.validate(&run.repository, &run.objective)?;
    crate::engine::validate_run(run).map_err(|e| e.to_string())?;
    let outputs: Vec<_> = run.nodes.values().flat_map(|n| n.outputs.iter()).collect();
    let pr_url = outputs
        .iter()
        .find(|a| a.kind == ArtifactKind::PullRequest)
        .and_then(|a| a.data.get("url"))
        .and_then(serde_json::Value::as_str);
    let reference = ResultReference {
        schema_version: 1,
        repo: &run.repository,
        task: &origin.task,
        run_id: &run.id,
        task_key: &origin.task_key,
        title: &origin.title,
        state: run.state,
        target_digest: run.target.digest(),
        definition_digest: &run.definition_digest,
        summary: format!(
            "Workflow: {} · {} / {} 工程完了",
            run.state.label(),
            run.nodes
                .values()
                .filter(|n| n.state == crate::model::NodeState::Succeeded)
                .count(),
            run.nodes.len()
        ),
        pr_url,
        updated_at: run.updated_at,
        source: "workflow_agent_reports",
    };
    fs::write_json(
        &tasks_root
            .join(repo_key(&run.repository))
            .join("workflow-results")
            .join(format!("{}.json", run.id)),
        &reference,
    )
}

/// Resolve only the bundled Tasks sibling or its known development build.
/// Values are separate argv entries; no shell or PATH command is constructed.
pub fn open_origin(origin: &TaskOrigin) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let parent = executable
        .parent()
        .ok_or("Tasksの実行ファイルが見つかりません")?;
    let mut candidates = vec![parent.join("Noa Tasks"), parent.join("tasks")];
    for ancestor in parent.ancestors() {
        if ancestor.file_name().is_some_and(|n| n == "widgets") {
            candidates.push(ancestor.join("tasks/target/debug/tasks"));
            candidates.push(ancestor.join("tasks/target/release/tasks"));
            break;
        }
    }
    let program = candidates
        .into_iter()
        .find(|p| p.is_file())
        .ok_or("TasksをビルドするかNoaの配布版から開いてください")?;
    let mut child = Command::new(program)
        .arg(format!("--repo={}", origin.repo.display()))
        .arg(format!("--select={}", origin.task))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Tasksを開けません: {e}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(temp.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        temp
    }

    fn handoff(repo: &Path) -> serde_json::Value {
        serde_json::json!({"schema_version":1,"repo":repo,"task":"t-1","task_key":"{\"local\":\"t-1\"}","title":"A task","objective":"Improve widgets\n\nVerify keyboard use."})
    }

    #[test]
    fn handoff_is_bounded_repo_scoped_and_does_not_follow_links() {
        let repo = repository();
        let root = std::fs::canonicalize(repo.path()).unwrap();
        let path = root.join("handoff.json");
        fs::write_json(&path, &handoff(&root)).unwrap();
        let (origin, objective) = read_handoff(&path, &root).unwrap();
        assert_eq!(origin.task, "t-1");
        assert_eq!(origin.objective_digest, digest_bytes(objective.as_bytes()));
        let another = repository();
        assert!(read_handoff(&path, another.path()).is_err());
        let link = root.join("link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_handoff(&link, &root).is_err());
        let mut oversized = handoff(&root);
        oversized["objective"] = serde_json::Value::String("a".repeat(65_537));
        fs::write_json(&path, &oversized).unwrap();
        assert!(read_handoff(&path, &root).is_err());
        let mut unrelated = handoff(&root);
        unrelated["schema_version"] = serde_json::json!(2);
        fs::write_json(&path, &unrelated).unwrap();
        assert!(read_handoff(&path, &root).is_err());
    }

    #[test]
    fn task_key_matches_the_existing_tasks_storage_contract() {
        assert_eq!(
            repo_key(Path::new("/src/noa/")),
            repo_key(Path::new("/src/noa"))
        );
        assert_eq!(repo_key(Path::new("a")), "a-af63dc4c8601ec8c");
    }

    #[test]
    fn handoff_task_identity_must_match_its_return_selector() {
        let repo = repository();
        let root = std::fs::canonicalize(repo.path()).unwrap();
        let path = root.join("handoff.json");
        for key in [
            "null",
            "{\"local\":\"other\"}",
            "{\"github\":{\"repo\":\"o/n\",\"number\":12}}",
            "{\"code\":{\"key\":\"k\"}}",
            "{\"local\":\"t-1\",\"github\":{\"repo\":\"o/n\",\"number\":12}}",
        ] {
            let mut value = handoff(&root);
            value["task_key"] = key.into();
            fs::write_json(&path, &value).unwrap();
            assert!(read_handoff(&path, &root).is_err(), "{key}");
        }
        for (selector, key) in [
            ("t-1", "{\"local\":\"t-1\"}"),
            ("#12", "{\"github\":{\"repo\":\"o/n\",\"number\":12}}"),
            (
                "code:k:src/a.rs",
                "{\"code\":{\"path\":\"src/a.rs\",\"key\":\"k\"}}",
            ),
        ] {
            let mut value = handoff(&root);
            value["task"] = selector.into();
            value["task_key"] = key.into();
            fs::write_json(&path, &value).unwrap();
            assert!(read_handoff(&path, &root).is_ok(), "{key}");
        }
    }

    #[test]
    fn result_reference_keeps_its_provenance_and_target() {
        let temp = tempfile::tempdir().unwrap();
        let mut run = Run::demo();
        run.origin_task = Some(TaskOrigin {
            repo: run.repository.clone(),
            task: "#12".into(),
            task_key: "{\"github\":{\"repo\":\"simota/Noa\",\"number\":12}}".into(),
            title: "Improve widgets".into(),
            objective_digest: digest_bytes(run.objective.as_bytes()),
        });
        publish_in(temp.path(), &run).unwrap();
        let path = temp
            .path()
            .join(repo_key(&run.repository))
            .join("workflow-results")
            .join(format!("{}.json", run.id));
        let json: serde_json::Value = fs::read_json(&path).unwrap().unwrap();
        assert_eq!(json["task"], "#12");
        assert_eq!(json["source"], "workflow_agent_reports");
        assert_eq!(json["target_digest"], run.target.digest());
        assert!(json["pr_url"].is_null());
    }
}
