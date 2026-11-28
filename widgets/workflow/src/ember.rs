//! Ember can prefill a start form without creating or resuming a Workflow run.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::adapters::{fs, git};
use crate::model::{digest_bytes, valid_id};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Handoff {
    schema_version: u32,
    origin_kind: String,
    ember_run_id: String,
    repo: PathBuf,
    title: String,
    objective: String,
    objective_digest: String,
}

/// This validates a draft, not an execution receipt. No agent is started here.
pub fn read_handoff(path: &Path, repository: &Path) -> Result<String, String> {
    if !path.is_absolute() {
        return Err("エンバーの引き継ぎには絶対パスが必要です".into());
    }
    let bytes = fs::read(path, 128 * 1024)?.ok_or("エンバーの引き継ぎファイルがありません")?;
    let handoff: Handoff =
        serde_json::from_slice(&bytes).map_err(|_| "エンバーの引き継ぎ形式が不正です")?;
    if handoff.schema_version != 1
        || handoff.origin_kind != "ember"
        || !valid_id(&handoff.ember_run_id)
        || !handoff.repo.is_absolute()
        || handoff.repo != git::repository(repository)?
        || handoff.title.trim().is_empty()
        || handoff.title.len() > 4096
        || handoff.title.chars().any(char::is_control)
        || handoff.objective.trim().is_empty()
        || handoff.objective.len() > 65_536
        || handoff
            .objective
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r'))
        || handoff.objective_digest != digest_bytes(handoff.objective.as_bytes())
    {
        return Err("エンバーの依頼と対象リポジトリ・内容の対応を確認できません".into());
    }
    Ok(handoff.objective)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ember_drafts_require_the_matching_repository_and_exact_content() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        let path = root.join("ember.json");
        let objective = "Add input validation\n\nReview the proposed changes before starting.";
        let mut value = serde_json::json!({
            "schema_version": 1, "origin_kind": "ember", "ember_run_id": "run-1",
            "repo": root, "title": "Input validation", "objective": objective,
            "objective_digest": digest_bytes(objective.as_bytes())
        });
        fs::write_json(&path, &value).unwrap();
        assert_eq!(read_handoff(&path, &root).unwrap(), objective);
        value["objective"] = "Changed without updating the digest".into();
        fs::write_json(&path, &value).unwrap();
        assert!(read_handoff(&path, &root).is_err());
        value["objective"] = objective.into();
        value["repo"] = "/another/project".into();
        fs::write_json(&path, &value).unwrap();
        assert!(read_handoff(&path, &root).is_err());
        let link = root.join("link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_handoff(&link, &root).is_err());
        assert!(!root.join(".noa/workflow").exists());
    }
}
