//! Antigravity CLI: `~/.gemini/antigravity-cli/history.jsonl`, one line per
//! prompt `{timestamp (ms epoch), workspace, conversationId?, type?}`. A line
//! without `type == "slash_command"` is one turn. Sessions are the distinct
//! `conversationId`s plus the `conversations/<uuid>.db` files the history
//! never names (those are only stat'ed, see `index::derive_sessions`).
//!
//! Active time links the turns of one `conversationId`. A turn without one
//! belongs to no session and has no neighbour: it adds no active time (turns
//! of different conversations or workspaces are never joined).
//!
//! The `display` field (the prompt text) is not a field of the decoded
//! struct, so it is never read into any structure. Tokens, models and tool
//! calls are not available.

use serde::Deserialize;

use super::index::{Sink, fnv};

#[derive(Deserialize)]
struct HistoryLine {
    timestamp: i64,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(rename = "conversationId", default)]
    conversation_id: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
}

/// One history line; false when it is not a valid record.
pub(crate) fn line(bytes: &[u8], sink: &mut Sink<'_>) -> bool {
    let Ok(rec) = serde_json::from_slice::<HistoryLine>(bytes) else {
        return false;
    };
    let ts = rec.timestamp.div_euclid(1000);
    let project = rec
        .workspace
        .as_deref()
        .filter(|w| !w.is_empty())
        .map_or(0, |w| sink.project(w));
    let session = rec
        .conversation_id
        .as_deref()
        .filter(|c| !c.is_empty())
        .map_or(0, |c| fnv(c.as_bytes()));
    if rec.kind.as_deref() == Some("slash_command") {
        if session != 0 {
            sink.session_hour(session, project, ts);
        }
        return true;
    }
    sink.counters(ts, 0, project).turns += 1;
    if session != 0 {
        sink.event(ts, session, 0, project);
    }
    true
}

#[cfg(test)]
mod tests {
    use crate::model::Agent;
    use crate::stats::index;
    use crate::stats::testutil::*;

    #[test]
    fn slash_commands_are_not_turns() {
        let t = Tree::fixtures();
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let c = sum_agent(ix.index(), Agent::Agy);
        // 7 lines: 2 slash commands, one malformed.
        assert_eq!(c.turns, 4);
        assert_eq!(c.tokens.total(), 0);
        let hist = &ix.index().files[&t.root.join("home/.gemini/antigravity-cli/history.jsonl")];
        assert_eq!(hist.data.lines_bad, 1);
    }

    #[test]
    fn sessions_from_conversation_ids_and_db_files() {
        let t = Tree::fixtures();
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let sessions: Vec<_> = ix
            .index()
            .sessions
            .iter()
            .filter(|((a, _), _)| *a == Agent::Agy)
            .collect();
        // Conversations 1 and 2 from the history (1 also has a .db file),
        // plus conversation 3 known only from its .db file.
        assert_eq!(sessions.len(), 3);
        let only_db = index::agy_db_key(std::path::Path::new(
            "00000000-0000-4000-8000-0000000a9003.db",
        ));
        let (_, session) = sessions.iter().find(|((_, k), _)| *k == only_db).unwrap();
        assert_eq!(session.hours, vec![DB_MTIME - DB_MTIME % 3600]);
    }

    #[test]
    fn prompt_text_is_not_kept() {
        let t = Tree::fixtures();
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let saved = std::fs::read_to_string(crate::paths::index_path(&t.own)).unwrap();
        let history =
            std::fs::read_to_string(t.root.join("home/.gemini/antigravity-cli/history.jsonl"))
                .unwrap();
        assert!(history.contains("SYNTHETIC-PROMPT"));
        assert!(!saved.contains("SYNTHETIC-PROMPT"));
        assert!(!format!("{:?}", ix.index()).contains("SYNTHETIC-PROMPT"));
    }

    #[test]
    fn turns_without_a_conversation_add_no_active_time() {
        let t = Tree::new();
        let history = t.agy_history();
        let turn = |ms: i64, workspace: &str, conversation: Option<&str>| {
            let conversation =
                conversation.map_or(String::new(), |c| format!(r#","conversationId":"{c}""#));
            format!(
                r#"{{"display":"SYNTHETIC-PROMPT","timestamp":{ms},"workspace":"/home/synthetic/work/{workspace}"{conversation}}}"#
            )
        };
        const T0: i64 = 1_795_410_000_000;
        const CONV: &str = "00000000-0000-4000-8000-0000000a9001";
        write(
            &history,
            &jsonl(&[
                // No conversation: 60 s apart in different workspaces, then
                // in the same one. None of them is joined.
                turn(T0, "a", None),
                turn(T0 + 60_000, "b", None),
                turn(T0 + 120_000, "b", None),
                // A conversation's turns are linked.
                turn(T0 + 600_000, "a", Some(CONV)),
                turn(T0 + 690_000, "a", Some(CONV)),
            ]),
        );
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let c = sum_agent(ix.index(), Agent::Agy);
        assert_eq!((c.turns, c.active_secs), (5, 90));
        assert_eq!(ix.index().sessions.len(), 1);
    }
}
