//! Opt-in outcome notifications. A notification failure never changes a run.

use crate::model::{Run, RunKind, RunStatus};

fn message(run: &Run) -> Result<&'static str, String> {
    if !matches!(
        run.status,
        RunStatus::Completed | RunStatus::Failed | RunStatus::Uncertain
    ) || run.kind == RunKind::Conversation
    {
        return Err("通知の対象となる完了済みの依頼がありません".into());
    }
    if run.status == RunStatus::Failed {
        return Ok("依頼を完了できませんでした。エンバーで詳細を確認できます。");
    }
    if run.status == RunStatus::Uncertain {
        return Ok("依頼の結果を確認できませんでした。エンバーで詳細を確認できます。");
    }
    Ok(match run.kind {
        RunKind::Document => "文書ができました。エンバーで確認できます。",
        RunKind::Research => "資料の調査が終わりました。エンバーで確認できます。",
        RunKind::Conversation => unreachable!(),
    })
}

/// The body contains no objective, filename, response, or other private context.
pub fn completed(run: &Run) -> Result<(), String> {
    let body = message(run)?;
    #[cfg(target_os = "macos")]
    {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let mut child = Command::new("/usr/bin/osascript")
            .args([
                "-e",
                "on run argv\ndisplay notification (item 1 of argv) with title \"Noa · エンバー\"\nend run",
                "--",
                body,
            ])
            .env_remove("NOA_EMBER_OPENAI_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "通知を送信できませんでした")?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return if status.success() {
                        Ok(())
                    } else {
                        Err("通知を送信できませんでした。macOSの通知設定を確認してください".into())
                    };
                }
                Ok(None) if started.elapsed() < Duration::from_secs(5) => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("通知の送信を確認できませんでした".into());
                }
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = body;
        Err("デスクトップ通知はmacOSで利用できます".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_do_not_include_private_context_or_unfinished_runs() {
        let mut run = Run {
            kind: RunKind::Document,
            status: RunStatus::Completed,
            objective: "private project name".into(),
            answer: Some("confidential text".into()),
            ..Run::default()
        };
        let body = message(&run).unwrap();
        assert!(!body.contains("private") && !body.contains("confidential"));
        run.status = RunStatus::Failed;
        run.error = Some("private provider response".into());
        assert_eq!(
            message(&run).unwrap(),
            "依頼を完了できませんでした。エンバーで詳細を確認できます。"
        );
        run.status = RunStatus::Running;
        assert!(message(&run).is_err());
        run.status = RunStatus::Completed;
        run.kind = RunKind::Conversation;
        assert!(message(&run).is_err());
    }
}
