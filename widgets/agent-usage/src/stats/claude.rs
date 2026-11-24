//! Claude Code session logs: `<projects>/<encoded-cwd>/<sessionId>.jsonl` and
//! `<projects>/<encoded-cwd>/<sessionId>/subagents/agent-*.jsonl`.
//!
//! `assistant` records carry usage; one API message is written as several
//! lines (one per content block) with the same `message.id` and usage, so
//! usage and the message count once per id, tool calls once per `tool_use.id`.
//! A `user` record whose content is a string or holds a `text` block, without
//! `toolUseResult`, is one turn. Every other record type is skipped. Only the
//! fields below are decoded; no prompt or result text is kept.

use std::collections::BTreeSet;
use std::fmt;

use serde::de::{self, Deserializer, IgnoredAny, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};

use super::index::{Sink, fnv};
use super::types::Tokens;
use crate::time::parse_rfc3339;

/// Per-file state carried across passes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Tail {
    /// The last assistant model seen (interned), for turns and active time.
    pub model: u32,
    pub project: u32,
    pub session: u64,
    /// `fnv(message.id)` already counted.
    pub messages: BTreeSet<u64>,
    /// `fnv(tool_use.id)` already counted.
    pub tool_uses: BTreeSet<u64>,
}

#[derive(Deserialize)]
struct Head {
    #[serde(rename = "type", default)]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct Record {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(rename = "toolUseResult", default)]
    tool_use_result: Option<IgnoredAny>,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    content: Content,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Usage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_creation: Option<CacheCreation>,
    output_tokens_details: Option<OutputDetails>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct CacheCreation {
    ephemeral_1h_input_tokens: Option<u64>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct OutputDetails {
    thinking_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// `message.content`: a string (whose text is not kept) or a list of blocks.
#[derive(Default)]
enum Content {
    #[default]
    None,
    Text,
    Blocks(Vec<Block>),
}

impl<'de> Deserialize<'de> for Content {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Content, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Content;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string or a list of content blocks")
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Content, E> {
                Ok(Content::Text)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Content, E> {
                Ok(Content::None)
            }
            fn visit_none<E: de::Error>(self) -> Result<Content, E> {
                Ok(Content::None)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Content, A::Error> {
                let mut blocks = Vec::new();
                while let Some(b) = seq.next_element::<Block>()? {
                    blocks.push(b);
                }
                Ok(Content::Blocks(blocks))
            }
        }
        d.deserialize_any(V)
    }
}

fn tokens(u: &Usage) -> Tokens {
    let n = |v: Option<u64>| v.unwrap_or(0);
    let output = n(u.output_tokens);
    let cache_write = n(u.cache_creation_input_tokens);
    Tokens {
        input: n(u.input_tokens),
        output,
        cache_read: n(u.cache_read_input_tokens),
        cache_write,
        cache_write_1h: u
            .cache_creation
            .as_ref()
            .map_or(0, |c| n(c.ephemeral_1h_input_tokens))
            .min(cache_write),
        reasoning: u
            .output_tokens_details
            .as_ref()
            .map_or(0, |d| n(d.thinking_tokens))
            .min(output),
    }
}

pub(crate) const SYNTHETIC_MODEL: &str = "<synthetic>";

/// One line; false when it is not a valid record.
pub(crate) fn line(bytes: &[u8], tail: &mut Tail, sink: &mut Sink<'_>) -> bool {
    let Ok(head) = serde_json::from_slice::<Head>(bytes) else {
        return false;
    };
    let assistant = match head.kind.as_deref() {
        Some("assistant") => true,
        Some("user") => false,
        _ => return true,
    };
    let Ok(rec) = serde_json::from_slice::<Record>(bytes) else {
        return false;
    };
    let Some(ts) = rec.timestamp.as_deref().and_then(parse_rfc3339) else {
        return false;
    };
    if let Some(cwd) = rec.cwd.as_deref().filter(|c| !c.is_empty()) {
        tail.project = sink.project(cwd);
    }
    if let Some(id) = rec.session_id.as_deref().filter(|s| !s.is_empty()) {
        tail.session = fnv(id.as_bytes());
    }
    let project = tail.project;
    let Some(message) = rec.message else {
        return !assistant;
    };
    if assistant {
        let model = message.model.as_deref().unwrap_or("");
        if model == SYNTHETIC_MODEL {
            return true;
        }
        let model = sink.intern(model);
        tail.model = model;
        let first = message
            .id
            .as_deref()
            .is_none_or(|id| tail.messages.insert(fnv(id.as_bytes())));
        if first {
            let t = message.usage.as_ref().map(tokens).unwrap_or_default();
            let c = sink.counters(ts, model, project);
            c.messages += 1;
            c.tokens.add(&t);
        }
        if let Content::Blocks(blocks) = &message.content {
            for b in blocks {
                if b.kind.as_deref() != Some("tool_use") {
                    continue;
                }
                let first =
                    b.id.as_deref()
                        .is_none_or(|id| tail.tool_uses.insert(fnv(id.as_bytes())));
                if first {
                    sink.counters(ts, model, project).tool_calls += 1;
                    sink.tool(ts, b.name.as_deref().unwrap_or("?"));
                }
            }
        }
    } else {
        let is_turn = rec.tool_use_result.is_none()
            && match &message.content {
                Content::Text => true,
                Content::Blocks(blocks) => blocks.iter().any(|b| b.kind.as_deref() == Some("text")),
                Content::None => false,
            };
        if is_turn {
            sink.counters(ts, tail.model, project).turns += 1;
        }
    }
    sink.event(ts, tail.session, tail.model, project);
    true
}

#[cfg(test)]
mod tests {
    use crate::model::Agent;
    use crate::stats::testutil::*;

    const MAIN: &str =
        "claude/projects/-synthetic-alpha/0b1c2d3e-0000-4000-8000-000000000001.jsonl";

    #[test]
    fn usage_counts_once_per_message_id() {
        let ix = index_fixture_files(&[MAIN]);
        let c = sum(&ix);
        // msg_a (3 lines, counted once) + msg_b (2 lines) + msg_c.
        assert_eq!(c.messages, 3);
        assert_eq!(c.tokens.input, 100 + 20 + 7);
        assert_eq!(c.tokens.output, 50 + 30 + 9);
        assert_eq!(c.tokens.cache_read, 1000 + 2000);
        assert_eq!(c.tokens.cache_write, 400);
        assert_eq!(c.tokens.cache_write_1h, 300);
        assert_eq!(c.tokens.reasoning, 12);
    }

    #[test]
    fn tool_uses_count_once_per_id() {
        let ix = index_fixture_files(&[MAIN]);
        assert_eq!(sum(&ix).tool_calls, 3);
        assert_eq!(
            tools(&ix),
            vec![("Bash".to_owned(), 2), ("Read".to_owned(), 1)]
        );
    }

    #[test]
    fn turns_skip_tool_results() {
        let ix = index_fixture_files(&[MAIN]);
        // A string prompt and a text-block prompt; the tool results are not turns.
        assert_eq!(sum(&ix).turns, 2);
    }

    #[test]
    fn synthetic_model_is_skipped() {
        let ix = index_fixture_files(&[MAIN]);
        let models = models(&ix);
        assert!(
            !models.iter().any(|m| m == super::SYNTHETIC_MODEL),
            "{models:?}"
        );
        assert_eq!(
            models,
            vec!["claude-opus-5-5", "claude-sonnet-5-5-20261103"]
        );
    }

    #[test]
    fn subagent_file_joins_its_session() {
        let sub = "claude/projects/-synthetic-alpha/0b1c2d3e-0000-4000-8000-000000000001/subagents/agent-a1b2c3.jsonl";
        let ix = index_fixture_files(&[MAIN, sub]);
        let sessions = &ix.sessions;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions.keys().next().unwrap().0, Agent::Claude);
        let c = sum(&ix);
        assert_eq!(c.messages, 4);
        assert_eq!(c.tokens.output, 50 + 30 + 9 + 11);
    }

    #[test]
    fn malformed_and_oversized_lines_are_counted_and_skipped() {
        let t = Tree::new();
        let log = t.claude_file("-synthetic-beta", "s.jsonl");
        let mut bytes = claude_turn_and_reply(0, "msg_x", 10);
        bytes.extend_from_slice(b"{\"type\":\"assistant\", this is not json\n");
        bytes.extend_from_slice(b"{\"type\":\"user\",\"message\":{\"content\":\"x\"}}\n"); // no timestamp
        let mut huge =
            br#"{"type":"user","timestamp":"2026-11-20T00:00:00Z","message":{"content":""#.to_vec();
        huge.resize(crate::stats::index::MAX_LINE + 10, b'a');
        huge.extend_from_slice(b"\"}}\n");
        bytes.extend_from_slice(&huge);
        bytes.extend(claude_turn_and_reply(120, "msg_y", 5));
        bytes.extend_from_slice(b"{\"type\":\"user\",\"timestamp\":\"2026-11-20T00:09:00Z\""); // cut
        write(&log, &bytes);
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let cov = ix.index().coverage();
        assert_eq!((cov.lines_bad, cov.lines_skipped), (2, 1));
        let c = sum(ix.index());
        assert_eq!((c.turns, c.messages, c.tokens.output), (2, 2, 15));
    }
}
