//! Codex CLI rollouts: `<sessions>/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl` (and
//! `archived_sessions/`). Each line is `{timestamp, type, payload}`.
//!
//! `session_meta` opens the session (a thread with `parent_thread_id` is a
//! sub-thread and joins its parent's session, and through it the root's:
//! `index::session_root`); `turn_context` sets the model for the records
//! after it; `token_usage_record` with a usage is one model response
//! (`event_msg`/`token_count` stands in when a file has none, skipping one
//! whose `total_token_usage` equals the previous one in every field);
//! `event_msg`/`task_started` is one turn; `response_item`/`message` from the
//! assistant is one message; function, custom tool, local shell and web
//! search calls are tool calls. `input_tokens` includes the cached input:
//! fresh input = `input_tokens` − `cached_input_tokens`.
//!
//! Codex has no skill event: it uses a skill by reading its `SKILL.md`. A
//! skill use is an `event_msg`/`item_completed` `CommandExecution` whose
//! `parsed_cmd[]` holds `{type: "read", path}` with `path`
//! `<root>/skills/[.system/]<name>/SKILL.md`, absolute or `~/`, under a
//! `.codex` or `.agents` directory (the user's, a plugin's, a repo's skills);
//! once per skill per turn (`turn_id`), as a skill is often read again. The
//! skills list in the instructions names the same paths, so the text is never
//! searched. Only `turn_id` and `parsed_cmd[].{type, path}` are decoded.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::index::{Sink, fnv, skill_name_ok};
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
    /// The direct parent thread's id for a sub-thread, else the thread's own
    /// (0 when `session_meta` names neither).
    pub session: u64,
    /// The thread's own id (`session_meta.id`), 0 when unnamed.
    pub thread: u64,
    /// The session's thread has a parent.
    pub sub_thread: bool,
    /// `info.total_token_usage` of the last `token_count`.
    pub last_total: Option<[u64; 6]>,
    /// `fnv(turn_id)` of the turn `turn_skills` belongs to.
    #[serde(default)]
    pub skill_turn: u64,
    /// `fnv(name)` of each skill already counted in that turn.
    #[serde(default)]
    pub turn_skills: BTreeSet<u64>,
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
    total_tokens: Option<u64>,
}

impl Usage {
    /// Every field, absent ones as 0.
    fn key(&self) -> [u64; 6] {
        let n = |v: Option<u64>| v.unwrap_or(0);
        [
            n(self.input_tokens),
            n(self.cached_input_tokens),
            n(self.cache_write_input_tokens),
            n(self.output_tokens),
            n(self.reasoning_output_tokens),
            n(self.total_tokens),
        ]
    }

    fn is_empty(&self) -> bool {
        *self == Usage::default()
    }

    /// Fresh input is `input_tokens` without the cached reads it includes.
    fn tokens(&self) -> Tokens {
        let [input, cached, write, output, reasoning, _] = self.key();
        Tokens {
            input: input.saturating_sub(cached),
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

#[derive(Deserialize)]
struct ItemCompleted {
    #[serde(default)]
    turn_id: Option<String>,
    #[serde(default)]
    item: Option<ExecItem>,
}

#[derive(Deserialize)]
struct ExecItem {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    parsed_cmd: Option<Vec<ParsedCmd>>,
}

#[derive(Deserialize)]
struct ParsedCmd {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

/// The skill whose `SKILL.md` is at `path`, when that is one of Codex's
/// skill directories (see the module comment).
fn skill_of(path: &str) -> Option<&str> {
    if !(path.starts_with('/') || path.starts_with("~/")) {
        return None;
    }
    let mut parts = path.rsplit('/');
    if parts.next()? != "SKILL.md" {
        return None;
    }
    let name = parts.next()?;
    let mut dir = parts.next()?;
    if dir == ".system" {
        dir = parts.next()?;
    }
    let under_home = dir == "skills" && parts.any(|p| p == ".codex" || p == ".agents");
    (under_home && skill_name_ok(name)).then_some(name)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// An `item_completed` line: the skills a command read. It is no event of
/// its own (active time comes from the records around it).
fn skill_reads(bytes: &[u8], ts: Option<&str>, tail: &mut Tail, sink: &mut Sink<'_>) -> bool {
    if !contains(bytes, b"SKILL.md") {
        return true;
    }
    let Some(ts) = ts.and_then(parse_rfc3339) else {
        return false;
    };
    let Some(done) = payload::<ItemCompleted>(bytes) else {
        return false;
    };
    let Some(item) = done
        .item
        .filter(|i| i.kind.as_deref() == Some("CommandExecution"))
    else {
        return true;
    };
    let turn = done.turn_id.as_deref().map_or(0, |t| fnv(t.as_bytes()));
    if turn != tail.skill_turn {
        tail.skill_turn = turn;
        tail.turn_skills.clear();
    }
    for cmd in item.parsed_cmd.unwrap_or_default() {
        if cmd.kind.as_deref() != Some("read") {
            continue;
        }
        if let Some(name) = cmd.path.as_deref().and_then(skill_of)
            && tail.turn_skills.insert(fnv(name.as_bytes()))
        {
            sink.skill(ts, name, tail.session, tail.model, tail.project);
        }
    }
    true
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
    if (kind, sub) == ("event_msg", Some("item_completed")) {
        return skill_reads(bytes, head.timestamp.as_deref(), tail, sink);
    }
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
                let id = |s: Option<&str>| s.filter(|s| !s.is_empty()).map(|s| fnv(s.as_bytes()));
                let parent = id(meta.parent_thread_id.as_deref());
                tail.thread = id(meta.id.as_deref()).unwrap_or(0);
                tail.session = parent.unwrap_or(tail.thread);
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
            // A record without a usage is no response: it leaves the
            // `token_count` fallback in place.
            if let Some(usage) = rec.usage.filter(|u| !u.is_empty()) {
                sink.set_has_records();
                let t = usage.tokens();
                sink.counters(ts, tail.model, tail.project).tokens.add(&t);
            }
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
            // A turn without `turn_id`s ends its predecessor's skills.
            tail.skill_turn = 0;
            tail.turn_skills.clear();
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
        // write 50), 10. Fresh input is input − cached; the cache write is
        // not taken from it.
        assert_eq!(c.tokens.input, 400 + 500 + 100 + 10);
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
                // resp_3: fresh 100 + cache read 200 + cache write 50 + output 60.
                ("gpt-6.1-sol".to_owned(), 100 + 200 + 50 + 60),
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
        assert_eq!(ix.sessions.len(), 1);
    }

    fn meta(id: &str, parent: Option<&str>, minute: u32) -> String {
        let parent = parent.map_or(String::new(), |p| format!(r#","parent_thread_id":"{p}""#));
        format!(
            r#"{{"timestamp":"2026-11-21T00:{minute:02}:00Z","type":"session_meta","payload":{{"id":"{id}","cwd":"/home/synthetic/work/alpha"{parent}}}}}"#
        )
    }

    fn token_count(second: u32, input: u64, total: u64) -> String {
        format!(
            r#"{{"timestamp":"2026-11-21T00:00:{second:02}Z","type":"event_msg","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":{input}}},"total_token_usage":{{"total_tokens":{total}}}}}}}}}"#
        )
    }

    #[test]
    fn token_count_repeat_compares_every_total_field_and_empty_records_keep_it() {
        const ID: &str = "00000000-0000-4000-8000-0000000000a1";
        let t = Tree::new();
        let log = t.codex_file(&format!("rollout-2026-11-21T09-00-00-{ID}.jsonl"));
        // Totals that differ only in `total_tokens` are not repeats.
        write(
            &log,
            &jsonl(&[
                meta(ID, None, 0),
                token_count(1, 10, 10),
                token_count(2, 10, 20),
            ]),
        );
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        assert_eq!(sum(ix.index()).tokens.total(), 20);
        // A record without a usage, or with an empty one, is no response.
        append(
            &log,
            &jsonl(&[
                r#"{"timestamp":"2026-11-21T00:00:03Z","type":"token_usage_record","payload":{}}"#
                    .to_owned(),
                r#"{"timestamp":"2026-11-21T00:00:04Z","type":"token_usage_record","payload":{"usage":{}}}"#
                    .to_owned(),
            ]),
        );
        ix.pass(&mut |_| {});
        assert!(!ix.index().files[&log].data.has_records);
        assert_eq!(sum(ix.index()).tokens.total(), 20);
        // A total equal in every field is still a repeat.
        append(&log, &jsonl(&[token_count(5, 10, 20)]));
        ix.pass(&mut |_| {});
        assert_eq!(sum(ix.index()).tokens.total(), 20);
    }

    #[test]
    fn sub_thread_of_a_sub_thread_counts_toward_the_root() {
        const A: &str = "00000000-0000-4000-8000-00000000000a";
        const B: &str = "00000000-0000-4000-8000-00000000000b";
        const C: &str = "00000000-0000-4000-8000-00000000000c";
        const D: &str = "00000000-0000-4000-8000-00000000000d";
        let t = Tree::new();
        let threads = [
            (A, None, 0),
            (B, Some(A), 2),
            (C, Some(B), 4),
            (D, Some(C), 6),
        ];
        for (id, parent, minute) in threads {
            let log = t.codex_file(&format!("rollout-2026-11-21T09-{minute:02}-00-{id}.jsonl"));
            let turn = format!(
                r#"{{"timestamp":"2026-11-21T00:{minute:02}:30Z","type":"event_msg","payload":{{"type":"task_started"}}}}"#
            );
            write(&log, &jsonl(&[meta(id, parent, minute), turn]));
        }
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let sessions = &ix.index().sessions;
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            *sessions.keys().next().unwrap(),
            (
                crate::model::Agent::Codex,
                crate::stats::index::fnv(A.as_bytes())
            )
        );
        // One session's events: 0:00, 0:30, 2:00, 2:30, … 6:00, 6:30.
        assert_eq!(sum(ix.index()).active_secs, 3 * (30 + 90) + 30);
    }

    fn item(second: u32, turn: Option<&str>, cmds: &[(&str, &str)]) -> String {
        let turn = turn.map_or(String::new(), |t| format!(r#""turn_id":"{t}","#));
        let cmds: Vec<String> = cmds
            .iter()
            .map(|(kind, path)| format!(r#"{{"type":"{kind}","cmd":"synthetic","path":"{path}"}}"#))
            .collect();
        format!(
            r#"{{"timestamp":"2026-11-21T00:01:{second:02}Z","type":"event_msg","payload":{{"type":"item_completed",{turn}"thread_id":"x","item":{{"type":"CommandExecution","id":"i{second}","command":"synthetic","aggregated_output":"synthetic output","parsed_cmd":[{}]}}}}}}"#,
            cmds.join(",")
        )
    }

    fn task_started(second: u32, turn: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-11-21T00:01:{second:02}Z","type":"event_msg","payload":{{"type":"task_started","turn_id":"{turn}"}}}}"#
        )
    }

    #[test]
    fn skill_reads_count_once_per_turn_from_codex_skill_dirs_only() {
        const ID: &str = "00000000-0000-4000-8000-0000000005c1";
        let t = Tree::new();
        let log = t.codex_file(&format!("rollout-2026-11-21T09-00-00-{ID}.jsonl"));
        let review = "/home/synthetic/.codex/skills/coding-review/SKILL.md";
        let mut lines = vec![
            meta(ID, None, 0),
            // The instructions list skills by path: not a use.
            r#"{"timestamp":"2026-11-21T00:00:30Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"skills/listed/SKILL.md ~/.codex/skills/listed/SKILL.md"}]}}"#.to_owned(),
            task_started(1, "t1"),
            item(2, Some("t1"), &[("read", review)]),
            // Read again in the same turn: once.
            item(3, Some("t1"), &[("read", review), ("read", "~/.codex/skills/.system/imagegen/SKILL.md")]),
            // Not a skill directory of Codex, not a read, relative, not SKILL.md.
            item(4, Some("t1"), &[
                ("read", "/home/synthetic/work/repo/skills/foo/SKILL.md"),
                ("search", "/home/synthetic/.codex/skills/bar/SKILL.md"),
                ("read", "skills/baz/SKILL.md"),
                ("read", "/home/synthetic/.codex/skills/qux/README.md"),
            ]),
            // A repo's `.agents` skills and a plugin's count.
            item(5, Some("t1"), &[
                ("read", "/home/synthetic/work/repo/.agents/skills/repo-skill/SKILL.md"),
                ("read", "~/.codex/plugins/cache/vendor/sites/1.0.0/skills/sites/SKILL.md"),
            ]),
            // A new turn counts it again.
            task_started(6, "t2"),
            item(7, Some("t2"), &[("read", review)]),
            // Other items, and one naming SKILL.md that is no command.
            r#"{"timestamp":"2026-11-21T00:01:08Z","type":"event_msg","payload":{"type":"item_completed","turn_id":"t2","item":{"type":"AgentMessage","text":"see SKILL.md"}}}"#.to_owned(),
        ];
        lines.push(r#"{"timestamp":"2026-11-21T00:01:09Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","parsed_cmd":"SKILL.md"}}}"#.to_owned());
        let mut bytes = jsonl(&lines);
        // Cut mid-line: read next pass.
        let last = item(10, None, &[("read", review)]);
        bytes.extend_from_slice(&last.as_bytes()[..40]);
        write(&log, &bytes);
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let codex = |n: &str, c| (crate::model::Agent::Codex, n.to_owned(), c);
        assert_eq!(
            skills(ix.index()),
            vec![
                codex("coding-review", 2),
                codex("imagegen", 1),
                codex("repo-skill", 1),
                codex("sites", 1),
            ]
        );
        // `parsed_cmd` not a list: a bad line.
        assert_eq!(ix.index().coverage().lines_bad, 1);
        let e = &ix.index().files[&log];
        let at = |s: &str| crate::time::parse_rfc3339(s).unwrap();
        // Item lines are no events: active time comes from the records.
        let events: Vec<_> = e
            .data
            .activity
            .iter()
            .flat_map(|a| &a.events)
            .map(|e| e.0)
            .collect();
        assert!(!events.contains(&at("2026-11-21T00:01:02Z")), "{events:?}");
        // The rest of the cut line, without a turn id: after `t2`, a new turn.
        let mut rest = last.as_bytes()[40..].to_vec();
        rest.push(b'\n');
        append(&log, &rest);
        ix.pass(&mut |_| {});
        assert_eq!(skills(ix.index())[0], codex("coding-review", 3));
        assert_eq!(sum(ix.index()).skills, 6);
    }
}
