//! Claude Code session logs: `<projects>/<encoded-cwd>/<sessionId>.jsonl` and
//! `<projects>/<encoded-cwd>/<sessionId>/subagents/agent-*.jsonl`.
//!
//! `assistant` records carry usage; one API message is written as several
//! lines (one per content block) with the same `message.id` and usage, so
//! usage and the message count once per id, tool calls once per `tool_use.id`.
//! A `user` record whose content is a string or holds a `text` block, without
//! `toolUseResult`, is one turn. Every other record type is skipped. Only the
//! fields below are decoded; no prompt or result text is kept.
//!
//! Skill uses: a `tool_use` named `Skill` (the model invoking one, its
//! `input.skill`), once per `tool_use.id`; and a slash command that runs a
//! skill. A slash command is a `user` record whose string content starts
//! with `<command-message>`/`<command-name>`; built-ins (`/clear`, `/model`,
//! `/reload-skills`, …) look the same, so a command counts only when the
//! next `user` record is its skill's: `isMeta`, no `sourceToolUseID`,
//! `parentUuid` the command's `uuid`, and a text block starting with
//! `Base directory for this skill:`. Claude Code writes that record only for
//! a skill. Only the command name and whether a text block has that prefix
//! are read from the content.

use std::collections::BTreeSet;
use std::fmt;

use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};

use super::index::{Sink, fnv, skill_name_ok};
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
    /// The last `user` record's slash command, `(fnv(uuid), name)`, until the
    /// next `user` record says whether it ran a skill.
    #[serde(default)]
    pub command: Option<(u64, String)>,
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
    uuid: Option<String>,
    #[serde(rename = "parentUuid", default)]
    parent_uuid: Option<String>,
    #[serde(rename = "isMeta", default)]
    is_meta: Option<bool>,
    #[serde(rename = "sourceToolUseID", default)]
    source_tool_use_id: Option<IgnoredAny>,
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
    /// Whether `text` starts with the skill record's prefix.
    #[serde(default)]
    text: Picked<SkillBase>,
    /// `input.skill` of a `Skill` call.
    #[serde(default)]
    input: Picked<SkillInput>,
}

/// The text Claude Code writes at the start of a skill's record.
const SKILL_BASE: &str = "Base directory for this skill:";

/// What a `Pick` takes from a JSON value; any value it does not take is
/// skipped without being kept.
struct Picked<T>(Option<T>);

impl<T> Default for Picked<T> {
    fn default() -> Picked<T> {
        Picked(None)
    }
}

trait Pick: Sized {
    fn pick(s: &str) -> Option<Self>;
    /// Whether a map's `key` holds the value to `pick`.
    fn wants(_key: &str) -> bool {
        false
    }
}

struct SkillBase;

impl Pick for SkillBase {
    fn pick(s: &str) -> Option<SkillBase> {
        s.starts_with(SKILL_BASE).then_some(SkillBase)
    }
}

struct SkillInput(String);

impl Pick for SkillInput {
    fn pick(s: &str) -> Option<SkillInput> {
        Some(SkillInput(s.to_owned()))
    }
    fn wants(key: &str) -> bool {
        key == "skill"
    }
}

impl<'de, T: Pick> Deserialize<'de> for Picked<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Picked<T>, D::Error> {
        struct V<T>(std::marker::PhantomData<T>);
        impl<'de, T: Pick> Visitor<'de> for V<T> {
            type Value = Picked<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Picked<T>, E> {
                Ok(Picked(T::pick(s)))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Picked<T>, E> {
                Ok(Picked(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Picked<T>, E> {
                Ok(Picked(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Picked<T>, E> {
                Ok(Picked(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Picked<T>, E> {
                Ok(Picked(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Picked<T>, E> {
                Ok(Picked(None))
            }
            fn visit_none<E: de::Error>(self) -> Result<Picked<T>, E> {
                Ok(Picked(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Picked<T>, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Picked(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Picked<T>, A::Error> {
                let mut picked = None;
                while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                    if picked.is_none() && T::wants(&key) {
                        picked = map.next_value::<Picked<T>>()?.0;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(Picked(picked))
            }
        }
        d.deserialize_any(V(std::marker::PhantomData))
    }
}

/// `message.content`: a string (of which only a slash command's name is
/// kept) or a list of blocks.
#[derive(Default)]
enum Content {
    #[default]
    None,
    Text(Option<String>),
    Blocks(Vec<Block>),
}

/// The command a slash-command record names: its content starts with
/// `<command-message>` or `<command-name>` and holds
/// `<command-name>/NAME</command-name>`.
fn command_name(s: &str) -> Option<String> {
    const OPEN: &str = "<command-name>";
    let s = s.trim_start();
    if !s.starts_with("<command-") {
        return None;
    }
    let rest = &s[s.find(OPEN)? + OPEN.len()..];
    let name = rest[..rest.find("</command-name>")?].trim();
    let name = name.strip_prefix('/').unwrap_or(name);
    skill_name_ok(name).then(|| name.to_owned())
}

impl<'de> Deserialize<'de> for Content {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Content, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Content;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string or a list of content blocks")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Content, E> {
                Ok(Content::Text(command_name(s)))
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
                    if b.name.as_deref() == Some("Skill")
                        && let Some(SkillInput(skill)) = &b.input.0
                    {
                        let skill = skill.strip_prefix('/').unwrap_or(skill);
                        if skill_name_ok(skill) {
                            sink.skill(ts, skill, tail.session, model, project);
                        }
                    }
                }
            }
        }
    } else {
        let command = tail.command.take();
        let is_turn = rec.tool_use_result.is_none()
            && match &message.content {
                Content::Text(_) => true,
                Content::Blocks(blocks) => blocks.iter().any(|b| b.kind.as_deref() == Some("text")),
                Content::None => false,
            };
        if is_turn {
            sink.counters(ts, tail.model, project).turns += 1;
        }
        match message.content {
            Content::Text(Some(name)) => {
                tail.command = rec.uuid.map(|u| (fnv(u.as_bytes()), name));
            }
            Content::Blocks(blocks)
                if rec.is_meta == Some(true)
                    && rec.source_tool_use_id.is_none()
                    && blocks.iter().any(|b| b.text.0.is_some()) =>
            {
                let parent = rec.parent_uuid.as_deref().map(|p| fnv(p.as_bytes()));
                if let Some((_, name)) = command.filter(|(u, _)| Some(*u) == parent) {
                    sink.skill(ts, &name, tail.session, tail.model, project);
                }
            }
            _ => {}
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

    /// A synthetic Claude record: `kind`, a uuid, a parent and extra fields
    /// (JSON members, comma-led), at minute `m`.
    fn rec(kind: &str, uuid: &str, parent: &str, m: u32, extra: &str) -> String {
        format!(
            r#"{{"type":"{kind}","uuid":"{uuid}","parentUuid":"{parent}","timestamp":"2026-11-20T01:{m:02}:00Z","sessionId":"sess-skill","cwd":"/home/synthetic/work/alpha"{extra}}}"#
        )
    }

    fn command(uuid: &str, m: u32, name: &str) -> String {
        rec(
            "user",
            uuid,
            "p",
            m,
            &format!(
                r#","message":{{"role":"user","content":"<command-message>{name}</command-message>\n<command-name>/{name}</command-name>"}}"#
            ),
        )
    }

    /// The meta record Claude Code writes after a skill's command (or, with
    /// `source`, after a `Skill` call).
    fn skill_base(uuid: &str, parent: &str, m: u32, source: bool) -> String {
        let source = if source {
            r#","sourceToolUseID":"tu_1""#
        } else {
            ""
        };
        rec(
            "user",
            uuid,
            parent,
            m,
            &format!(
                r#","isMeta":true{source},"message":{{"role":"user","content":[{{"type":"text","text":"Base directory for this skill: /home/synthetic/.claude/skills/x\n\nsynthetic body"}}]}}"#
            ),
        )
    }

    fn skill_call(uuid: &str, m: u32, id: &str, input: &str) -> String {
        rec(
            "assistant",
            uuid,
            "p",
            m,
            &format!(
                r#","message":{{"id":"msg_{uuid}","model":"claude-opus-5-5","content":[{{"type":"tool_use","id":"{id}","name":"Skill","input":{input}}}],"usage":{{"input_tokens":1,"output_tokens":1}}}}"#
            ),
        )
    }

    #[test]
    fn skill_uses_are_skill_calls_and_skill_commands_not_built_ins() {
        let t = Tree::new();
        let log = t.claude_file("-synthetic-alpha", "skills.jsonl");
        let lines = [
            // A model-invoked skill, written twice (one line per block):
            // once; its meta record is not a second use.
            skill_call("a1", 0, "tu_1", r#"{"skill":"coding-review","args":"x"}"#),
            skill_call("a1", 0, "tu_1", r#"{"skill":"coding-review","args":"x"}"#),
            skill_base("m1", "a1", 0, true),
            // A skill's slash command followed by its record.
            command("c1", 1, "coding-plan"),
            skill_base("m2", "c1", 1, false),
            // Built-ins: nothing follows, or a plain prompt does.
            command("c2", 2, "clear"),
            command("c3", 3, "model"),
            rec(
                "user",
                "u3",
                "c3",
                3,
                r#","message":{"role":"user","content":"synthetic prompt"}"#,
            ),
            // A skill record whose parent is not the command before it.
            command("c4", 4, "reload-skills"),
            skill_base("m4", "elsewhere", 4, false),
            // Pasted text naming a command and the prefix is no command.
            rec(
                "user",
                "u5",
                "p",
                5,
                r#","message":{"role":"user","content":"\n\nsee <command-name>/coding-ship</command-name> Base directory for this skill: x"}"#,
            ),
            skill_base("m5", "u5", 5, false),
            // A Skill call naming no skill string, and another tool whose
            // input has an object `skill`: valid lines, no skill.
            skill_call("a6", 6, "tu_6", r#"{"skill":7}"#),
            rec(
                "assistant",
                "a7",
                "p",
                7,
                r#","message":{"id":"msg_a7","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"tu_7","name":"Other","input":{"skill":{"n":1},"list":[1,"two"]}}],"usage":{}}"#,
            ),
            // A skill command whose record arrives in the next pass.
            command("c8", 8, "coding-ship"),
        ];
        let mut bytes = jsonl(&lines.map(|l| l.to_owned()));
        bytes.extend_from_slice(b"{\"type\":\"user\", not json\n");
        write(&log, &bytes);
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let claude = |n: &str, c| (Agent::Claude, n.to_owned(), c);
        assert_eq!(
            skills(ix.index()),
            vec![claude("coding-plan", 1), claude("coding-review", 1)]
        );
        assert_eq!(ix.index().coverage().lines_bad, 1);
        // The pending command survives the pass (the bad line is no user
        // record).
        append(&log, &jsonl(&[skill_base("m8", "c8", 9, false)]));
        ix.pass(&mut |_| {});
        assert_eq!(
            skills(ix.index()),
            vec![
                claude("coding-plan", 1),
                claude("coding-review", 1),
                claude("coding-ship", 1)
            ]
        );
        assert_eq!(sum(ix.index()).skills, 3);
        // Built-in names are never interned: no content is kept.
        let strings = &ix.index().strings;
        assert!(
            !strings
                .iter()
                .any(|s| s == "clear" || s == "model" || s.contains("synthetic"))
        );
        let k = &ix.index().files[&log].data.skills;
        assert!(
            k.iter()
                .all(|k| k.session == crate::stats::index::fnv(b"sess-skill"))
        );
    }
}
