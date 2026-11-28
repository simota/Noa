//! Explicit, immutable handoffs to Workflow's deferred start form.
//!
//! Opening a window is not evidence that a Workflow run was created. This
//! version intentionally does not invent a Workflow identity or track its run.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::model::{MAX_TEXT_BYTES, Run, RunStatus, Scope, valid_id};
use crate::store;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedHandoff {
    pub path: PathBuf,
    pub repo: PathBuf,
    pub title: String,
    pub objective: String,
}

#[derive(Serialize)]
struct Handoff<'a> {
    schema_version: u32,
    origin_kind: &'static str,
    ember_run_id: &'a str,
    repo: &'a Path,
    title: &'a str,
    objective: &'a str,
    objective_digest: String,
}

fn repository(path: &Path) -> Result<PathBuf, String> {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-C",
        ])
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .env_remove("NOA_EMBER_OPENAI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, _) in std::env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .spawn()
        .map_err(|_| "Gitリポジトリを確認できません")?;
    let output = child.stdout.take().ok_or("Gitの応答を取得できません")?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        output.take(4097).read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err("Gitリポジトリの確認を完了できませんでした".into());
            }
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| "Gitの応答を確認できません")?
        .map_err(|_| "Gitの応答を読み取れません")?;
    if !status.success() || bytes.len() > 4096 {
        return Err("選択したプロジェクトはGitリポジトリではありません".into());
    }
    let value = std::str::from_utf8(&bytes)
        .map_err(|_| "プロジェクトのパスはUTF-8である必要があります")?
        .trim_end_matches(['\n', '\r']);
    if value.chars().any(char::is_control) {
        return Err("プロジェクトのパスに制御文字があります".into());
    }
    std::fs::canonicalize(value).map_err(|_| "プロジェクトの保存先を確認できません".into())
}

pub fn prepare(root: &Path, run: &Run) -> Result<PreparedHandoff, String> {
    if run.status != RunStatus::Completed || !valid_id(&run.id) {
        return Err("完了済みの依頼を選択してください".into());
    }
    let Scope::Project { root: project } = &run.scope else {
        return Err("Workflowに引き継ぐにはプロジェクトを選択して依頼してください".into());
    };
    let repo = repository(Path::new(project))?;
    let answer = run.answer.as_deref().ok_or("引き継ぐ結果がありません")?;
    let objective = format!(
        "エンバーから引き継いだ依頼\n\n## ユーザーの依頼\n{}\n\n## 参考となる回答\n{}\n\n開始前に目的と対象リポジトリを確認してください。参考となる回答は未検証の下書きです。",
        run.objective, answer
    );
    if objective.len() > MAX_TEXT_BYTES {
        return Err("引き継ぐ内容が64KiBを超えています。要約した依頼を作成してください".into());
    }
    let title: String = run
        .objective
        .lines()
        .next()
        .unwrap_or("エンバーの依頼")
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    let handoff = Handoff {
        schema_version: 1,
        origin_kind: "ember",
        ember_run_id: &run.id,
        repo: &repo,
        title: &title,
        objective: &objective,
        objective_digest: store::digest(objective.as_bytes()),
    };
    let bytes = serde_json::to_vec(&handoff).map_err(|e| e.to_string())?;
    let hash = store::digest(&bytes);
    let directory = root.join("handoffs");
    store::private_directory(&directory)?;
    let path = directory.join(format!("{}-{hash}.json", run.id));
    match store::read(&path, 128 * 1024)? {
        Some(existing) if existing != bytes => {
            return Err("保存済みの引き継ぎ内容が一致しません".into());
        }
        Some(_) => (),
        None => store::atomic_write(&path, &bytes)?,
    }
    Ok(PreparedHandoff {
        path,
        repo,
        title,
        objective,
    })
}

/// Resolve a bundled helper or a known development build; never a shell string.
pub fn open(prepared: &PreparedHandoff) -> Result<(), String> {
    validate_prepared(prepared)?;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let parent = executable
        .parent()
        .ok_or("Workflowの実行ファイルが見つかりません")?;
    let mut candidates = vec![parent.join("Noa Workflow"), parent.join("workflow")];
    for ancestor in parent.ancestors() {
        if ancestor.file_name().is_some_and(|n| n == "widgets") {
            candidates.push(ancestor.join("workflow/target/debug/workflow"));
            candidates.push(ancestor.join("workflow/target/release/workflow"));
            break;
        }
    }
    let program = candidates
        .into_iter()
        .find(|p| p.is_file())
        .ok_or("WorkflowをビルドするかNoaの配布版から開いてください")?;
    launch(&program, prepared)
}

fn validate_prepared(prepared: &PreparedHandoff) -> Result<(), String> {
    let bytes = store::read(&prepared.path, 128 * 1024)?
        .ok_or("引き継ぎが削除されています。依頼を選び直してください")?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "引き継ぎ内容を確認できません")?;
    if value["schema_version"] != 1
        || value["origin_kind"] != "ember"
        || value["repo"] != serde_json::to_value(&prepared.repo).map_err(|e| e.to_string())?
        || value["title"] != prepared.title
        || value["objective"] != prepared.objective
        || value["objective_digest"] != store::digest(prepared.objective.as_bytes())
    {
        return Err(
            "引き継ぎ内容がプレビューから変更されています。もう一度準備してください".into(),
        );
    }
    Ok(())
}

fn launch(program: &Path, prepared: &PreparedHandoff) -> Result<(), String> {
    let mut child = Command::new(program)
        .arg(format!("--repo={}", prepared.repo.display()))
        .arg(format!("--ember-origin={}", prepared.path.display()))
        .env_remove("NOA_EMBER_OPENAI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Workflowを開けません: {e}"))?;
    // A missing/old helper often exits immediately. A still-running process is
    // only a launch request, not confirmation that a window or run exists.
    let deadline = Instant::now() + Duration::from_millis(400);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if !status.success() => {
                return Err("Workflowを起動できませんでした。最新版をビルドし、引き継ぎ内容を準備し直してください".into());
            }
            Ok(Some(_)) => return Ok(()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            Ok(None) => break,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Workflowの起動を確認できません: {error}"));
            }
        }
    }
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_is_an_immutable_repo_scoped_draft() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        let mut run = Run {
            id: "run-1".into(),
            scope: Scope::Project {
                root: root.to_string_lossy().into_owned(),
            },
            objective: "入力を検証する".into(),
            answer: Some("境界条件をテストする。".into()),
            status: RunStatus::Completed,
            ..Run::default()
        };
        let a = prepare(&root.join("store"), &run).unwrap();
        let b = prepare(&root.join("store"), &run).unwrap();
        assert_eq!(a.path, b.path);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&a.path).unwrap()).unwrap();
        assert_eq!(value["origin_kind"], "ember");
        assert_eq!(value["repo"], root.to_string_lossy().as_ref());
        assert!(value.get("task_key").is_none());
        assert!(value.get("workflow_run_id").is_none());
        assert_eq!(
            value["objective_digest"],
            store::digest(a.objective.as_bytes())
        );
        validate_prepared(&a).unwrap();
        std::fs::remove_file(&a.path).unwrap();
        assert!(validate_prepared(&a).is_err());
        run.scope = Scope::Global;
        assert!(prepare(&root.join("store"), &run).is_err());
        run.status = RunStatus::Running;
        assert!(prepare(&root.join("store"), &run).is_err());
    }

    #[test]
    fn a_helper_that_rejects_the_handoff_reports_failure() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let program = root.join("old-workflow");
        std::fs::write(&program, "#!/bin/sh\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let draft = PreparedHandoff {
            path: root.join("draft.json"),
            repo: root,
            title: "draft".into(),
            objective: "draft".into(),
        };
        assert!(launch(&program, &draft).is_err());
    }
}
