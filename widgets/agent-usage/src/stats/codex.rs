//! Codex CLI rollouts: `<sessions>/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl` (and
//! `archived_sessions/`). Each line is `{timestamp, type, payload}`.
//!
//! `session_meta` opens the session (a thread with `parent_thread_id` is a
//! sub-thread and joins its parent's session); `turn_context` sets the model
//! for the records after it; `token_usage_record` is one model response
//! (`event_msg`/`token_count` stands in when a file has none, skipping a
//! repeat of the previous total); `event_msg`/`task_started` is one turn;
//! `response_item`/`message` from the assistant is one message; function,
//! custom tool, local shell and web search calls are tool calls.
//! `input_tokens` includes the cached input.

use serde::{Deserialize, Serialize};

use super::index::{Sink, fnv};
use super::types::Tokens;
use crate::time::parse_rfc3339;

/// Per-file state carried across passes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Tail {
    pub model: u32,
    pub project: u32,
    /// Whether the project came from `session_meta` (then `turn_context.cwd`
    /// does not override it).
    pub project_from_meta: bool,
    pub session: u64,
    /// The session's thread has a parent.
    pub sub_thread: bool,
    /// `info.total_token_usage` of the last `token_count`.
    pub last_total: Option<[u64; 5]>,
}

#[derive(Deserialize)]
struct Head {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    payload: Option<PayloadHead>,
}

#[derive(Deserialize)]
struct PayloadHead {
    #[serde(rename = "type", default)]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct Rec<P> {
    payload: P,
}

#[derive(Deserialize)]
struct SessionMeta {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    parent_thread_id: Option<String>,
}

#[derive(Deserialize)]
struct TurnContext {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(default)]
struct Usage {
    input_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    cache_write_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_output_tokens: Option<u64>,
}

impl Usage {
    fn key(&self) -> [u64; 5] {
        let n = |v: Option<u64>| v.unwrap_or(0);
        [
            n(self.input_tokens),
            n(self.cached_input_tokens),
            n(self.cache_write_input_tokens),
            n(self.output_tokens),
            n(self.reasoning_output_tokens),
        ]
    }

    /// Fresh input is what `input_tokens` holds beyond the cached reads and
    /// the cache writes (`total_tokens == input_tokens + output_tokens`, so
    /// both are part of it).
    fn tokens(&self) -> Tokens {
        let [input, cached, write, output, reasoning] = self.key();
        Tokens {
            input: input.saturating_sub(cached).saturating_sub(write),
            output,
            cache_read: cached,
            cache_write: write,
            cache_write_1h: 0,
            reasoning: reasoning.min(output),
        }
    }
}

#[derive(Deserialize)]
struct UsageRecord {
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct TokenCount {
    #[serde(default)]
    info: Option<TokenInfo>,
}

#[derive(Deserialize)]
struct TokenInfo {
    #[serde(default)]
    last_token_usage: Option<Usage>,
    #[serde(default)]
    total_token_usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Item {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

const TOOL_CALLS: [&str; 4] = [
    "function_call",
    "custom_tool_call",
    "local_shell_call",
    "web_search_call",
];

fn payload<'a, P: Deserialize<'a>>(bytes: &'a [u8]) -> Option<P> {
    serde_json::from_slice::<Rec<P>>(bytes)
        .ok()
        .map(|r| r.payload)
}

/// One line; false when it is not a valid record.
pub(crate) fn line(bytes: &[u8], tail: &mut Tail, sink: &mut Sink<'_>) -> bool {
    let Ok(head) = serde_json::from_slice::<Head>(bytes) else {
        return false;
    };
    let sub = head.payload.as_ref().and_then(|p| p.kind.as_deref());
    let kind = head.kind.as_deref().unwrap_or("");
    let relevant = matches!(
        (kind, sub),
        ("session_meta" | "turn_context" | "token_usage_record", _)
            | ("event_msg", Some("token_count" | "task_started"))
            | ("response_item", _)
    );
    if !relevant {
        return true;
    }
    let Some(ts) = head.timestamp.as_deref().and_then(parse_rfc3339) else {
        return false;
    };
    match (kind, sub) {
        ("session_meta", _) => {
            let Some(meta) = payload::<SessionMeta>(bytes) else {
                return false;
            };
            // The first `session_meta` names the thread; later ones (resumed
            // or forked history) do not move it.
            if tail.session == 0 {
                let parent = meta.parent_thread_id.as_deref().filter(|p| !p.is_empty());
                let own = meta.id.as_deref().unwrap_or("");
                tail.session = fnv(parent.unwrap_or(own).as_bytes());
                tail.sub_thread = parent.is_some();
                if let Some(cwd) = meta.cwd.as_deref().filter(|c| !c.is_empty()) {
                    tail.project = sink.project(cwd);
                    tail.project_from_meta = true;
                }
            }
        }
        ("turn_context", _) => {
            let Some(ctx) = payload::<TurnContext>(bytes) else {
                return false;
            };
            if let Some(model) = ctx.model.as_deref() {
                tail.model = sink.intern(model);
            }
            if let Some(cwd) = ctx.cwd.as_deref().filter(|c| !c.is_empty())
                && !tail.project_from_meta
            {
                tail.project = sink.project(cwd);
            }
        }
        ("token_usage_record", _) => {
            let Some(rec) = payload::<UsageRecord>(bytes) else {
                return false;
            };
            sink.set_has_records();
            let t = rec.usage.unwrap_or_default().tokens();
            sink.counters(ts, tail.model, tail.project).tokens.add(&t);
        }
        ("event_msg", Some("token_count")) => {
            let Some(tc) = payload::<TokenCount>(bytes) else {
                return false;
            };
            if let Some(info) = tc.info {
                let total = info.total_token_usage.map(|u| u.key());
                let repeat = total.is_some() && total == tail.last_total;
                if total.is_some() {
                    tail.last_total = total;
                }
                if let (false, Some(last)) = (repeat, info.last_token_usage) {
                    let t = last.tokens();
                    sink.fallback_tokens(ts, tail.model, tail.project).add(&t);
                }
            }
        }
        ("event_msg", Some("task_started")) => {
            sink.counters(ts, tail.model, tail.project).turns += 1;
        }
        ("response_item", Some(item_kind)) => {
            let is_message = item_kind == "message";
            let is_call = TOOL_CALLS.contains(&item_kind);
            if is_message || is_call {
                let Some(item) = payload::<Item>(bytes) else {
                    return false;
                };
                if is_message && item.role.as_deref() == Some("assistant") {
                    sink.counters(ts, tail.model, tail.project).messages += 1;
                } else if is_call {
                    sink.counters(ts, tail.model, tail.project).tool_calls += 1;
                    let name = item.name.as_deref().filter(|n| !n.is_empty());
                    sink.tool(ts, name.unwrap_or(item_kind));
                }
            }
        }
        _ => {}
    }
    sink.event(ts, tail.session, tail.model, tail.project);
    true
}

#[cfg(test)]
mod tests {
    use crate::stats::index::Tail;
    use crate::stats::testutil::*;

    const MAIN: &str = "codex/sessions/2026/11/21/rollout-2026-11-21T09-00-00-00000000-0000-4000-8000-00000000c0de.jsonl";
    const NO_RECORDS: &str = "codex/sessions/2026/11/22/rollout-2026-11-22T10-00-00-00000000-0000-4000-8000-00000000beef.jsonl";
    const SUB: &str = "codex/sessions/2026/11/21/rollout-2026-11-21T09-05-00-00000000-0000-4000-8000-00000000cafe.jsonl";

    #[test]
    fn fresh_input_excludes_cached() {
        let ix = index_fixture_files(&[MAIN]);
        let c = sum(&ix);
        // Records: input 1000 (cached 600), 500 (cached 0), 300 (cached 200,
        // write 50), 10.
        assert_eq!(c.tokens.input, 400 + 500 + 50 + 10);
        assert_eq!(c.tokens.cache_read, 800);
        assert_eq!(c.tokens.cache_write, 50);
        assert_eq!(c.tokens.output, 100 + 40 + 60 + 5);
        assert_eq!(c.tokens.reasoning, 30 + 10);
    }

    #[test]
    fn usage_records_preferred_over_token_count() {
        let ix = index_fixture_files(&[MAIN]);
        let e = ix.files.values().next().unwrap();
        assert!(e.data.has_records);
        assert!(!e.data.fallback.is_empty(), "token_count was read too");
        // The total is the records' only (token_count says 99999 output).
        assert_eq!(sum(&ix).tokens.output, 205);
    }

    #[test]
    fn token_count_used_when_no_records_and_repeats_skipped() {
        let ix = index_fixture_files(&[NO_RECORDS]);
        let e = ix.files.values().next().unwrap();
        assert!(!e.data.has_records);
        let c = sum(&ix);
        // Three token_count events, the second repeating the first's total;
        // one has no info at all.
        assert_eq!(c.tokens.input, (800 - 300) + (400 - 100));
        assert_eq!(c.tokens.cache_read, 400);
        assert_eq!(c.tokens.output, 70 + 25);
    }

    #[test]
    fn model_follows_turn_context() {
        let ix = index_fixture_files(&[MAIN]);
        let by_model = tokens_by_model(&ix);
        assert_eq!(
            by_model,
            vec![
                ("codex-auto-review".to_owned(), 10 + 5),
                ("gpt-6.1-sol".to_owned(), 300 + 60),
                ("gpt-6.2-sol".to_owned(), 1000 + 100 + 500 + 40),
            ]
        );
    }

    #[test]
    fn tool_call_kinds_counted() {
        let ix = index_fixture_files(&[MAIN]);
        let c = sum(&ix);
        assert_eq!(c.tool_calls, 4);
        assert_eq!(c.messages, 2, "assistant messages only");
        assert_eq!(c.turns, 2);
        assert_eq!(
            tools(&ix),
            vec![
                ("apply_patch".to_owned(), 1),
                ("exec_command".to_owned(), 1),
                ("local_shell_call".to_owned(), 1),
                ("web_search_call".to_owned(), 1),
            ]
        );
    }

    #[test]
    fn sub_thread_is_flagged() {
        let ix = index_fixture_files(&[MAIN, SUB]);
        let tails: Vec<_> = ix
            .files
            .values()
            .map(|e| match &e.tail {
                Tail::Codex(t) => (t.sub_thread, t.session),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(tails.len(), 2);
        let main = tails.iter().find(|t| !t.0).unwrap();
        let sub = tails.iter().find(|t| t.0).unwrap();
        // The sub-thread joins its parent's session.
        assert_eq!(main.1, sub.1);
        assert_eq!(crate::stats::index::merged_sessions(&ix).len(), 1);
    }
}
