//! Draft PR publication is a separate, approved effect. Its marker, branch
//! and target SHA make a lost response observable without creating again.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::model::TargetSnapshot;

use super::{fs, git, process};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedPr {
    pub repository: String,
    pub push_url: String,
    pub branch: String,
    pub base: String,
    pub title: String,
    pub body: String,
    pub marker: String,
    pub target: TargetSnapshot,
}

fn gh(directory: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let executable = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"]
        .iter()
        .find(|path| Path::new(path).is_file())
        .copied()
        .unwrap_or("gh");
    process::output(
        Command::new(executable)
            .current_dir(directory)
            .args(args)
            .env("GH_HOST", "github.com")
            .env("GH_PROMPT_DISABLED", "1"),
        Duration::from_secs(25),
    )
}

fn repository(remote: &str) -> Result<String, String> {
    let value = remote
        .strip_prefix("https://github.com/")
        .or_else(|| remote.strip_prefix("git@github.com:"))
        .ok_or("PRの公開先は認証情報を含まないgithub.com originのみ対応しています")?;
    let value = value.trim_end_matches('/').trim_end_matches(".git");
    let parts: Vec<_> = value.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
    {
        return Err("originのリポジトリ名を安全に確認できません".into());
    }
    Ok(value.into())
}

pub fn prepare(
    directory: &Path,
    request_id: &str,
    objective: &str,
    target: &TargetSnapshot,
) -> Result<PreparedPr, String> {
    git::same_snapshot(directory, target)?;
    if !git::is_clean(target) {
        return Err(
            "PRの作成には、隔離worktreeの変更をコミットしてから再検証・再承認してください".into(),
        );
    }
    let repository = repository(&git::text(directory, &["remote", "get-url", "origin"])?)?;
    let push_url = git::text(directory, &["remote", "get-url", "--push", "origin"])?;
    if self::repository(&push_url)? != repository {
        return Err(
            "originの読取先とpush先が異なります。公開先を揃えてから確認してください".into(),
        );
    }
    let branch = git::branch(directory)?;
    let raw = gh(
        directory,
        &[
            "repo",
            "view",
            &repository,
            "--json",
            "nameWithOwner,defaultBranchRef",
        ],
    )?;
    let response: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|_| "GitHubのリポジトリ応答を確認できません")?;
    if response["nameWithOwner"]
        .as_str()
        .is_none_or(|name| !name.eq_ignore_ascii_case(&repository))
    {
        return Err("GitHubの応答が承認対象のリポジトリと一致しません".into());
    }
    let base = response["defaultBranchRef"]["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("PRのbaseブランチがありません")?
        .to_owned();
    ensure_base_contains_start(directory, &repository, &base, &target.base_sha)?;
    let title: String = objective
        .lines()
        .next()
        .unwrap_or("Workflow change")
        .trim()
        .chars()
        .take(120)
        .collect();
    let title = if title.is_empty() {
        "Workflow change".into()
    } else {
        title
    };
    let marker = format!("<!-- noa-workflow:{request_id} -->");
    let body = format!(
        "## Change\n\n{objective}\n\n## Validation\n\nThis draft was prepared by Noa Workflow after its Review and Verify nodes completed for target `{}`. Review the linked local evidence before merging.\n\n{marker}\n",
        target.head_sha
    );
    Ok(PreparedPr {
        repository,
        push_url,
        branch,
        base,
        title,
        body,
        marker,
        target: target.clone(),
    })
}

fn encoded_reference(reference: &str) -> String {
    let mut encoded = String::new();
    for byte in reference.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(&mut encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn validate_base_comparison(response: &serde_json::Value, start: &str) -> Result<(), String> {
    // API direction is START...BASE: ahead means the chosen public base
    // contains the starting commit. A divergent feature branch would add
    // commits outside the task's captured START...HEAD approval diff.
    if !matches!(response["status"].as_str(), Some("ahead" | "identical"))
        || response["base_commit"]["sha"].as_str() != Some(start)
        || response["merge_base_commit"]["sha"].as_str() != Some(start)
    {
        return Err("開始時のcommitがPRのbaseに含まれていません。今回の変更以外を公開しないため、未マージのfeature commitを含む開始はPR公開対象外です".into());
    }
    Ok(())
}

fn ensure_base_contains_start(
    directory: &Path,
    repository: &str,
    base: &str,
    start: &str,
) -> Result<(), String> {
    let endpoint = format!(
        "repos/{repository}/compare/{start}...{}",
        encoded_reference(base)
    );
    let raw = gh(
        directory,
        &[
            "api",
            "--method",
            "GET",
            &endpoint,
            "--jq",
            "{status, base_commit: {sha: .base_commit.sha}, merge_base_commit: {sha: .merge_base_commit.sha}}",
        ],
    )
    .map_err(|message| {
        format!(
            "公開先baseが開始commitを含むことを確認できません。未pushの開始commit・認証・接続を確認してから照合してください: {message}"
        )
    })?;
    let response: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|_| "公開先baseの比較応答を確認できません")?;
    validate_base_comparison(&response, start)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    Absent,
    Present { url: String, number: u64 },
}

pub fn reconcile(directory: &Path, prepared: &PreparedPr) -> Result<Found, String> {
    git::same_snapshot(directory, &prepared.target)?;
    if repository(&git::text(directory, &["remote", "get-url", "origin"])?)? != prepared.repository
        || git::text(directory, &["remote", "get-url", "--push", "origin"])? != prepared.push_url
        || repository(&prepared.push_url)? != prepared.repository
        || git::branch(directory)? != prepared.branch
    {
        return Err("PR公開先またはブランチが保存済みの操作と一致しません".into());
    }
    let raw = gh(
        directory,
        &[
            "pr",
            "list",
            "--repo",
            &prepared.repository,
            "--state",
            "all",
            "--head",
            &prepared.branch,
            "--base",
            &prepared.base,
            "--limit",
            "100",
            "--json",
            "number,url,headRefName,headRefOid,body",
        ],
    )?;
    let entries: Vec<serde_json::Value> =
        serde_json::from_slice(&raw).map_err(|_| "PR照合の応答が不正です")?;
    let mut matched = Vec::new();
    for entry in entries {
        if entry["body"]
            .as_str()
            .is_some_and(|body| body.contains(&prepared.marker))
        {
            if entry["headRefOid"].as_str() != Some(&prepared.target.head_sha)
                || entry["headRefName"].as_str() != Some(&prepared.branch)
            {
                return Err("同じ操作のPRが別の変更版を指しています。自動で更新しません".into());
            }
            let url = entry["url"].as_str().ok_or("PR URLがありません")?;
            let prefix = format!("https://github.com/{}/pull/", prepared.repository);
            if !url.starts_with(&prefix) {
                return Err("PR URLの公開先が一致しません".into());
            }
            let number = entry["number"].as_u64().ok_or("PR番号がありません")?;
            matched.push(Found::Present {
                url: url.into(),
                number,
            });
        } else {
            return Err("同じheadブランチの別PRがあります。重複PRを作成しません".into());
        }
    }
    match matched.len() {
        0 => Ok(Found::Absent),
        1 => Ok(matched.remove(0)),
        _ => Err("同じ操作識別子のPRが複数あります。手動で確認してください".into()),
    }
}

pub fn publish(
    directory: &Path,
    receipt_dir: &Path,
    prepared: &PreparedPr,
) -> Result<Found, String> {
    if let found @ Found::Present { .. } = reconcile(directory, prepared)? {
        return Ok(found);
    }
    ensure_base_contains_start(
        directory,
        &prepared.repository,
        &prepared.base,
        &prepared.target.base_sha,
    )?;
    let current = git::snapshot(directory, &prepared.target.base_sha)?;
    if current != prepared.target {
        return Err("公開直前に対象が変わりました".into());
    }
    // A normal push is never forced. A lost acknowledgement remains
    // uncertain; callers must reconcile this prepared operation.
    let reference = format!(
        "{}:refs/heads/{}",
        prepared.target.head_sha, prepared.branch
    );
    let mut push = Command::new("git");
    push.args(["-c", "core.hooksPath=/dev/null", "-C"])
        .arg(directory)
        .args(["push", "--", &prepared.push_url, &reference]);
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"GIT_") {
            push.env_remove(name);
        }
    }
    process::output(&mut push, Duration::from_secs(25))?;
    let body: PathBuf = receipt_dir.join("pull-request.md");
    fs::atomic_write(&body, prepared.body.as_bytes())?;
    let body = body.to_str().ok_or("PR本文のパスが不正です")?;
    gh(
        directory,
        &[
            "pr",
            "create",
            "--repo",
            &prepared.repository,
            "--head",
            &prepared.branch,
            "--base",
            &prepared.base,
            "--title",
            &prepared.title,
            "--body-file",
            body,
            "--draft",
        ],
    )?;
    match reconcile(directory, prepared)? {
        Found::Absent => {
            Err("PR作成応答後も対象PRを確認できません。再作成せず照合してください".into())
        }
        found => Ok(found),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_routing_rejects_credentials_and_other_hosts() {
        assert_eq!(
            repository("git@github.com:simota/Noa.git").unwrap(),
            "simota/Noa"
        );
        assert_eq!(
            repository("https://github.com/simota/Noa.git").unwrap(),
            "simota/Noa"
        );
        for remote in [
            "https://token@github.com/simota/Noa",
            "https://example.com/simota/Noa",
            "https://github.com/a/b/extra",
        ] {
            assert!(repository(remote).is_err());
        }
    }

    #[test]
    fn publication_base_must_contain_the_exact_starting_commit() {
        let start = "a".repeat(40);
        let mut comparison = serde_json::json!({
            "status": "ahead",
            "base_commit": {"sha": start},
            "merge_base_commit": {"sha": start}
        });
        assert!(validate_base_comparison(&comparison, &start).is_ok());
        comparison["status"] = serde_json::json!("identical");
        assert!(validate_base_comparison(&comparison, &start).is_ok());
        for status in ["behind", "diverged", "unknown"] {
            comparison["status"] = serde_json::json!(status);
            assert!(validate_base_comparison(&comparison, &start).is_err());
        }
        comparison["status"] = serde_json::json!("ahead");
        comparison["merge_base_commit"]["sha"] = serde_json::json!("b".repeat(40));
        assert!(validate_base_comparison(&comparison, &start).is_err());
        comparison["merge_base_commit"]["sha"] = serde_json::json!(start);
        comparison["base_commit"]["sha"] = serde_json::json!("c".repeat(40));
        assert!(validate_base_comparison(&comparison, &start).is_err());
        assert_eq!(encoded_reference("release/1.0"), "release%2F1.0");
    }
}
