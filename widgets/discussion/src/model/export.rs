//! A discussion as Markdown, for people and for an agent: the result (to
//! copy), the minutes with every message (to save as a file), and the
//! implementation request an agent is started with.

use std::fmt::Write as _;
use std::path::Path;

use super::types::{Author, IssueState, MinuteSection, MsgKind, Position, Session};

fn state_name(state: IssueState) -> &'static str {
    match state {
        IssueState::Open => "未決",
        IssueState::Contested => "対立",
        IssueState::Agreed => "合意",
        IssueState::Conditional => "条件付き合意",
        IssueState::Deferred => "保留",
    }
}

fn position_name(pos: Position) -> &'static str {
    match pos {
        Position::For => "賛成",
        Position::Against => "反対",
        Position::Neutral => "中立",
        Position::Unknown => "未表明",
    }
}

fn section_name(section: MinuteSection) -> &'static str {
    match section {
        MinuteSection::Decisions => "決定事項",
        MinuteSection::Contested => "対立点",
        MinuteSection::OpenQuestions => "未解決の問い",
        MinuteSection::Actions => "次のアクション",
    }
}

fn kind_name(kind: MsgKind) -> &'static str {
    match kind {
        MsgKind::Brief => "質問",
        MsgKind::Reply => "回答",
        MsgKind::Summary => "要約",
        MsgKind::Intervention => "介入",
        MsgKind::Argument => "主張",
        MsgKind::Verdict => "判定",
        MsgKind::Conclusion => "結論",
    }
}

/// The outcome so far: the topic, where the discussion stands, the
/// moderator's latest summary, the issue tree with every stance, and the
/// minutes.
pub fn result_markdown(session: &Session) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# {}\n", session.topic);
    let rounds = session.rounds.len();
    let limit = session
        .max_rounds
        .map(|max| format!("（上限 {max}）"))
        .unwrap_or_default();
    let state = if session.closed {
        "終了"
    } else {
        "進行中"
    };
    let _ = writeln!(out, "- ラウンド: {rounds}{limit}・{state}");
    let panel: Vec<&str> = session.panelist_ids().map(|p| p.as_str()).collect();
    let _ = writeln!(
        out,
        "- 進行役: {}（{}）・パネル: {}",
        session.moderator.id,
        session.moderator.kind.command(),
        panel.join(", ")
    );
    if let Some(workdir) = &session.workdir {
        let _ = writeln!(out, "- 作業ディレクトリ: {}", workdir.display());
    }

    if let Some(conclusion) = session
        .messages
        .iter()
        .rev()
        .find(|m| m.kind == MsgKind::Conclusion)
    {
        let _ = writeln!(
            out,
            "\n## 結論\n\n{}（R{}）",
            conclusion.body.trim(),
            conclusion.round
        );
    }

    out.push_str("\n## 最新の要約\n\n");
    match session
        .messages
        .iter()
        .rev()
        .find(|m| m.kind == MsgKind::Summary)
    {
        Some(summary) => {
            let _ = writeln!(out, "{}（R{}）", summary.body.trim(), summary.round);
        }
        None => out.push_str("（まだありません）\n"),
    }

    out.push_str("\n## 論点\n\n");
    let issues = session.issues.walk();
    if issues.is_empty() {
        out.push_str("（なし）\n");
    }
    for (depth, issue) in issues {
        let stances: Vec<String> = issue
            .stances
            .iter()
            .map(|(part, stance)| format!("{part}: {}", position_name(stance.pos)))
            .collect();
        let stances = if stances.is_empty() {
            String::new()
        } else {
            format!("（{}）", stances.join("、"))
        };
        let _ = writeln!(
            out,
            "{}- [{}] {} — {}{stances}",
            "  ".repeat(depth),
            issue.id,
            state_name(issue.state),
            issue.title
        );
    }

    for section in MinuteSection::ALL {
        let _ = writeln!(out, "\n## {}\n", section_name(section));
        let items = session.minutes.section(section);
        if items.is_empty() {
            out.push_str("（なし）\n");
        }
        for item in items {
            let issues: Vec<String> = item.issues.iter().map(|i| format!("[{i}]")).collect();
            let tail = if issues.is_empty() {
                String::new()
            } else {
                format!(" {}", issues.join(" "))
            };
            let _ = writeln!(out, "- {}{tail}", item.text);
        }
    }
    out
}

/// The minutes to save: the result, then every message in order.
pub fn minutes_markdown(session: &Session) -> String {
    let mut out = result_markdown(session);
    out.push_str("\n## 発言記録\n");
    for message in &session.messages {
        let to: Vec<&str> = message.to.iter().map(|p| p.as_str()).collect();
        let to = if to.is_empty() {
            String::new()
        } else {
            format!(" → {}", to.join(", "))
        };
        let author = match &message.author {
            Author::User => "ユーザー".to_owned(),
            author => session.author_name(author),
        };
        let _ = writeln!(
            out,
            "\n### [{}] R{} · {author}{to} · {}\n\n{}",
            message.id,
            message.round,
            kind_name(message.kind),
            message.body.trim()
        );
    }
    out
}

/// What an implementing agent is started with: how to treat the result,
/// then the result itself. `path` is where this text is written.
pub fn implementation_task(session: &Session, path: &Path) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Implementation request from discussion {}\n\n\
         This file ({}) is the outcome of a moderated discussion between coding \
         agents. Implement its decisions and next actions in this repository.\n\n\
         - Follow the decisions. Where an issue is only conditionally agreed, keep \
         to its conditions.\n\
         - Leave open questions and contested points undecided; list them in your \
         final report.\n\
         - Do not push or publish anything.\n",
        session.id,
        path.display()
    );
    out.push('\n');
    out.push_str(&result_markdown(session));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{AgentState, Config, Engine, Input, Setup};
    use crate::model::types::{AgentKind, PartId, SessionId};
    use std::path::PathBuf;

    /// One round: claude and codex reply, the moderator adds an issue, a
    /// stance and two minutes items; the user cut in once.
    fn discussed() -> Session {
        let setup = Setup {
            topic: "Pick a database",
            workdir: Some(PathBuf::from("/repo")),
            max_rounds: Some(5),
            moderator: AgentKind::Claude.into(),
            panel: &[AgentKind::Claude.into(), AgentKind::Codex.into()],
        };
        let config = Config {
            start_timeout: 1_000,
            reply_timeout: 10_000,
            max_retries: 2,
            debate_max_exchanges: 2,
        };
        let (mut e, _) =
            Engine::create_in(SessionId::new("disc-1"), setup, Path::new("/d"), config, 0).unwrap();
        e.handle(Input::Start { now: 0 }).unwrap();
        e.handle(Input::UserIntervention {
            to: vec![],
            body: "Mind the ops cost.".into(),
            at: 1,
        })
        .unwrap();
        for (part, body) in [
            ("claude", "Postgres, for JSONB."),
            ("codex", "SQLite is enough."),
        ] {
            let part = PartId::new(part);
            e.handle(Input::Sent {
                round: 1,
                part: part.clone(),
                at: 2,
            })
            .unwrap();
            e.handle(Input::AgentStateChanged {
                part: part.clone(),
                state: AgentState::Running,
                at: 3,
            })
            .unwrap();
            e.handle(Input::ReplyFileWritten {
                round: 1,
                part,
                content: body.into(),
                at: 4,
            })
            .unwrap();
        }
        let moderator = PartId::new("noa");
        e.handle(Input::Sent {
            round: 1,
            part: moderator.clone(),
            at: 5,
        })
        .unwrap();
        e.handle(Input::ReplyFileWritten {
            round: 1,
            part: moderator,
            content: r#"{"summary":"Split on the engine.",
                "issue_ops":[{"op":"add","id":"A","parent":null,"title":"Which engine"},
                             {"op":"set_state","id":"A","state":"contested"},
                             {"op":"set_stance","id":"A","part":"codex","pos":"against","basis":"m5"}],
                "minutes_ops":[{"op":"add","section":"decisions","text":"Keep one engine.","issues":["A"]},
                               {"op":"add","section":"actions","text":"Benchmark both.","issues":[]}]}"#
                .into(),
            at: 6,
        })
        .unwrap();
        e.session().clone()
    }

    #[test]
    fn the_result_has_the_summary_the_issue_tree_and_the_minutes() {
        let md = result_markdown(&discussed());
        assert!(md.starts_with("# Pick a database\n"));
        assert!(md.contains("- ラウンド: 2（上限 5）・進行中"), "{md}");
        assert!(md.contains("- 作業ディレクトリ: /repo"));
        assert!(md.contains("Split on the engine.（R1）"));
        assert!(
            md.contains("- [A] 対立 — Which engine（codex: 反対）"),
            "{md}"
        );
        assert!(md.contains("## 決定事項\n\n- Keep one engine. [A]"));
        assert!(md.contains("## 次のアクション\n\n- Benchmark both."));
        assert!(md.contains("## 対立点\n\n（なし）"));
        // The result leaves out the message log.
        assert!(!md.contains("Postgres, for JSONB."));
    }

    #[test]
    fn the_minutes_add_every_message_in_order() {
        let session = discussed();
        let md = minutes_markdown(&session);
        assert!(md.starts_with(&result_markdown(&session)));
        let mut from = 0;
        for message in &session.messages {
            let at = md[from..]
                .find(message.body.trim())
                .unwrap_or_else(|| panic!("{} missing or out of order", message.id));
            from += at;
        }
        assert!(
            md.contains("· ユーザー · 介入\n\nMind the ops cost."),
            "{md}"
        );
        assert!(md.contains("· Noa (moderator) · 要約"), "{md}");
    }

    #[test]
    fn the_implementation_task_names_its_file_and_carries_the_result() {
        let session = discussed();
        let path = Path::new("/repo/.noa/discussions/disc-1/implementation.md");
        let task = implementation_task(&session, path);
        assert!(task.contains("/repo/.noa/discussions/disc-1/implementation.md"));
        assert!(task.contains("Do not push"));
        assert!(task.ends_with(&result_markdown(&session)));
    }
}
