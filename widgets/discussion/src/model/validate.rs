//! What agents write is external input: the moderator's JSON and the
//! panelists' reply files are checked here before anything is applied. Every
//! problem is reported precisely, because the text goes back to the agent so
//! it can fix its output.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Deserialize;

use super::types::{
    DebateProposal, DebateStatus, IssueId, IssueOp, IssueState, MessageId, MinuteOp, MinuteSection,
    ModeratorOutput, PartId, Position, RoundKind, RoundNo, Session, Verdict,
};

/// Everything wrong with one piece of agent output, one problem per entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub problems: Vec<String>,
}

impl ValidationError {
    fn one(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, problem) in self.problems.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationError {}

/// Longest issue id the moderator may coin; ids are tags in chat lines.
pub const MAX_ISSUE_ID_LEN: usize = 16;

/// Whether `id` may name a new issue: 1–16 ASCII letters, digits, `-` or `_`.
pub fn is_valid_issue_id(id: &str) -> bool {
    (1..=MAX_ISSUE_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOutput {
    summary: String,
    #[serde(default)]
    issue_ops: Vec<RawIssueOp>,
    #[serde(default)]
    minutes_ops: Vec<RawMinuteOp>,
    #[serde(default)]
    questions: BTreeMap<String, String>,
    #[serde(default)]
    propose_debate: Option<RawProposal>,
    #[serde(default)]
    verdict: Option<RawVerdict>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    close: bool,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum RawIssueOp {
    Add {
        id: String,
        #[serde(default)]
        parent: Option<String>,
        title: String,
    },
    SetState {
        id: String,
        state: String,
    },
    SetStance {
        id: String,
        part: String,
        pos: String,
        basis: String,
    },
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum RawMinuteOp {
    Add {
        section: String,
        text: String,
        #[serde(default)]
        issues: Vec<String>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProposal {
    issue: String,
    pro: Vec<String>,
    con: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVerdict {
    outcome: String,
    rationale: String,
}

/// Drops one Markdown code fence around the whole text, which agents often
/// add even when told not to.
fn strip_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let Some((_, inner)) = rest.split_once('\n') else {
        return trimmed;
    };
    inner.trim_end().strip_suffix("```").unwrap_or(inner).trim()
}

fn list(items: impl IntoIterator<Item = impl fmt::Display>) -> String {
    let items: Vec<String> = items.into_iter().map(|i| i.to_string()).collect();
    if items.is_empty() {
        "(none)".to_owned()
    } else {
        items.join(", ")
    }
}

/// Checks the moderator's JSON for round `round` against the session.
pub fn parse_moderator(
    text: &str,
    session: &Session,
    round: RoundNo,
) -> Result<ModeratorOutput, ValidationError> {
    let raw: RawOutput = serde_json::from_str(strip_fence(text))
        .map_err(|e| ValidationError::one(format!("not valid moderator JSON: {e}")))?;
    let Some(this_round) = session.round(round) else {
        return Err(ValidationError::one(format!("there is no round {round}")));
    };
    let panelists: Vec<&PartId> = session.panelist_ids().collect();
    let known_parts = list(&panelists);
    let mut problems = Vec::new();
    let mut known: Vec<IssueId> = session.issues.issues.iter().map(|i| i.id.clone()).collect();
    let issue_known = |known: &[IssueId], id: &str| known.iter().any(|k| k.as_str() == id);

    let summary = raw.summary.trim().to_owned();
    if summary.is_empty() {
        problems.push("\"summary\" is empty".to_owned());
    }

    let mut issue_ops = Vec::new();
    for (i, op) in raw.issue_ops.into_iter().enumerate() {
        let at = format!("issue_ops[{i}]");
        match op {
            RawIssueOp::Add { id, parent, title } => {
                let before = problems.len();
                if !is_valid_issue_id(&id) {
                    problems.push(format!(
                        "{at} (add): issue id {id:?} must be 1-{MAX_ISSUE_ID_LEN} ASCII letters, digits, '-' or '_'"
                    ));
                } else if issue_known(&known, &id) {
                    problems.push(format!("{at} (add): issue {id:?} already exists"));
                }
                if let Some(parent) = &parent
                    && !issue_known(&known, parent)
                {
                    problems.push(format!(
                        "{at} (add): unknown parent issue {parent:?}; known issues: {}",
                        list(&known)
                    ));
                }
                if title.trim().is_empty() {
                    problems.push(format!("{at} (add): \"title\" is empty"));
                }
                if problems.len() == before {
                    known.push(IssueId::new(&id));
                    issue_ops.push(IssueOp::Add {
                        id: IssueId::new(id),
                        parent: parent.map(IssueId::new),
                        title: title.trim().to_owned(),
                    });
                }
            }
            RawIssueOp::SetState { id, state } => {
                let before = problems.len();
                if !issue_known(&known, &id) {
                    problems.push(format!(
                        "{at} (set_state): unknown issue {id:?}; known issues: {}",
                        list(&known)
                    ));
                }
                let parsed = IssueState::parse(&state);
                if parsed.is_none() {
                    problems.push(format!(
                        "{at} (set_state): unknown state {state:?}; expected open, contested, agreed, conditional or deferred"
                    ));
                }
                if let (Some(state), true) = (parsed, problems.len() == before) {
                    issue_ops.push(IssueOp::SetState {
                        id: IssueId::new(id),
                        state,
                    });
                }
            }
            RawIssueOp::SetStance {
                id,
                part,
                pos,
                basis,
            } => {
                let before = problems.len();
                if !issue_known(&known, &id) {
                    problems.push(format!(
                        "{at} (set_stance): unknown issue {id:?}; known issues: {}",
                        list(&known)
                    ));
                }
                if !panelists.iter().any(|p| p.as_str() == part) {
                    problems.push(format!(
                        "{at} (set_stance): unknown panelist {part:?}; panelists: {known_parts}"
                    ));
                }
                let parsed = Position::parse(&pos);
                if parsed.is_none() {
                    problems.push(format!(
                        "{at} (set_stance): unknown pos {pos:?}; expected for, against, neutral or unknown"
                    ));
                }
                if session.message(&MessageId::new(&basis)).is_none() {
                    problems.push(format!(
                        "{at} (set_stance): unknown basis message {basis:?}; use a message id such as m1"
                    ));
                }
                if let (Some(pos), true) = (parsed, problems.len() == before) {
                    issue_ops.push(IssueOp::SetStance {
                        id: IssueId::new(id),
                        part: PartId::new(part),
                        pos,
                        basis: MessageId::new(basis),
                    });
                }
            }
        }
    }

    let mut minutes_ops = Vec::new();
    for (i, op) in raw.minutes_ops.into_iter().enumerate() {
        let at = format!("minutes_ops[{i}]");
        let RawMinuteOp::Add {
            section,
            text,
            issues,
        } = op;
        let before = problems.len();
        let parsed = MinuteSection::parse(&section);
        if parsed.is_none() {
            problems.push(format!(
                "{at} (add): unknown section {section:?}; expected decisions, contested, open_questions or actions"
            ));
        }
        if text.trim().is_empty() {
            problems.push(format!("{at} (add): \"text\" is empty"));
        }
        for issue in &issues {
            if !issue_known(&known, issue) {
                problems.push(format!(
                    "{at} (add): unknown issue {issue:?}; known issues: {}",
                    list(&known)
                ));
            }
        }
        if let (Some(section), true) = (parsed, problems.len() == before) {
            minutes_ops.push(MinuteOp {
                section,
                text: text.trim().to_owned(),
                issues: issues.into_iter().map(IssueId::new).collect(),
            });
        }
    }

    let mut questions = BTreeMap::new();
    for (part, question) in raw.questions {
        if !panelists.iter().any(|p| p.as_str() == part) {
            problems.push(format!(
                "questions: unknown panelist {part:?}; panelists: {known_parts}"
            ));
        } else if question.trim().is_empty() {
            problems.push(format!("questions: the question for {part:?} is empty"));
        } else {
            questions.insert(PartId::new(part), question.trim().to_owned());
        }
    }

    // The discussion's last round: nothing can run after it, so it must end
    // in the moderator's conclusion (and a debate held in it, in a verdict).
    let last = session.max_rounds.is_some_and(|max| round >= max);
    let debate = this_round.debate().and_then(|id| session.debate(id));
    let propose_debate = raw.propose_debate.and_then(|proposal| {
        let before = problems.len();
        if last {
            problems.push(
                "propose_debate: not allowed in the last round, no round is left to hold it"
                    .to_owned(),
            );
        } else if debate.is_some() {
            problems.push("propose_debate: not allowed during a debate round".to_owned());
        } else if session.debates.iter().any(|d| {
            matches!(d.status, DebateStatus::Proposed | DebateStatus::Approved)
        }) {
            problems.push("propose_debate: a debate is already waiting to run".to_owned());
        }
        if !issue_known(&known, &proposal.issue) {
            problems.push(format!(
                "propose_debate: unknown issue {:?}; known issues: {}",
                proposal.issue,
                list(&known)
            ));
        }
        let mut seen = BTreeSet::new();
        for (side, members) in [("pro", &proposal.pro), ("con", &proposal.con)] {
            if members.is_empty() {
                problems.push(format!("propose_debate: \"{side}\" is empty"));
            }
            for member in members {
                if !panelists.iter().any(|p| p.as_str() == member) {
                    problems.push(format!(
                        "propose_debate: unknown panelist {member:?} in \"{side}\"; panelists: {known_parts}"
                    ));
                } else if !seen.insert(member.clone()) {
                    problems.push(format!(
                        "propose_debate: {member:?} is named more than once"
                    ));
                }
            }
        }
        (problems.len() == before).then(|| DebateProposal {
            issue: IssueId::new(proposal.issue),
            pro: proposal.pro.into_iter().map(PartId::new).collect(),
            con: proposal.con.into_iter().map(PartId::new).collect(),
        })
    });

    let verdict = match (raw.verdict, debate) {
        (Some(_), None) => {
            problems.push("verdict: only allowed in a debate round".to_owned());
            None
        }
        (Some(verdict), Some(_)) => {
            let outcome = IssueState::parse(&verdict.outcome).filter(|s| s.is_verdict());
            if outcome.is_none() {
                problems.push(format!(
                    "verdict: unknown outcome {:?}; expected agreed, conditional or deferred",
                    verdict.outcome
                ));
            }
            if verdict.rationale.trim().is_empty() {
                problems.push("verdict: \"rationale\" is empty".to_owned());
            }
            outcome.map(|outcome| Verdict {
                outcome,
                rationale: verdict.rationale.trim().to_owned(),
            })
        }
        (None, Some(debate)) => {
            let exchanges = session
                .rounds
                .iter()
                .filter(|r| r.no <= round && r.kind == RoundKind::Debate(debate.id.clone()))
                .count();
            if exchanges >= usize::from(debate.max_exchanges) {
                problems.push(format!(
                    "verdict: required, this was the last of {} exchanges of debate {}",
                    debate.max_exchanges, debate.id
                ));
            } else if last {
                problems.push(format!(
                    "verdict: required, debate {} cannot go on after the last round",
                    debate.id
                ));
            }
            None
        }
        (None, None) => None,
    };

    let conclusion = raw
        .conclusion
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty());
    if conclusion.is_none() && (last || raw.close) {
        let why = if last {
            "this is the last round"
        } else {
            "\"close\" is true"
        };
        problems.push(format!(
            "conclusion: required, {why}; give your answer to the topic, deciding it \
             yourself where the panel did not agree"
        ));
    }

    if !problems.is_empty() {
        return Err(ValidationError { problems });
    }
    Ok(ModeratorOutput {
        summary,
        issue_ops,
        minutes_ops,
        questions,
        propose_debate,
        verdict,
        conclusion,
        close: raw.close,
    })
}

/// A panelist's reply file, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// The issues the reply is about, stance issues included.
    pub issues: Vec<IssueId>,
    pub reply_to: Vec<MessageId>,
    pub stances: Vec<(IssueId, Position)>,
    pub body: String,
}

fn strip_brackets(value: &str, open: char, close: char) -> &str {
    let value = value.trim();
    value
        .strip_prefix(open)
        .and_then(|v| v.strip_suffix(close))
        .unwrap_or(value)
}

fn items(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(',')
        .map(|item| item.trim().trim_matches(['"', '\'']).trim())
        .filter(|item| !item.is_empty())
}

/// Parses a reply file: an optional header between `---` lines (`issues`,
/// `reply_to`, `stance`), then the body. A file without a header is all
/// body; a header that is there must be well-formed and name only issues and
/// messages that exist.
pub fn parse_reply(text: &str, session: &Session) -> Result<Reply, ValidationError> {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines();
    let first = text.lines().find(|line| !line.trim().is_empty());
    let mut reply = Reply {
        issues: Vec::new(),
        reply_to: Vec::new(),
        stances: Vec::new(),
        body: String::new(),
    };
    let mut problems = Vec::new();
    let body = if first.map(str::trim) == Some("---") {
        for line in lines.by_ref() {
            if line.trim() == "---" {
                break;
            }
        }
        let mut header = Vec::new();
        let mut closed = false;
        for line in lines.by_ref() {
            if line.trim() == "---" {
                closed = true;
                break;
            }
            header.push(line);
        }
        if !closed {
            return Err(ValidationError::one("the header has no closing '---' line"));
        }
        let mut seen = BTreeSet::new();
        for (n, line) in header.into_iter().enumerate() {
            // `#` starts a comment, as in the example the brief shows. No
            // header value (issue ids, message ids, positions) holds one.
            let line = line.split_once('#').map_or(line, |(before, _)| before);
            if line.trim().is_empty() {
                continue;
            }
            let at = format!("header line {}", n + 2);
            let Some((key, value)) = line.split_once(':') else {
                problems.push(format!("{at}: expected 'key: value', got {line:?}"));
                continue;
            };
            let key = key.trim();
            if !seen.insert(key.to_owned()) {
                problems.push(format!("{at}: {key:?} is given twice"));
                continue;
            }
            match key {
                "issues" => {
                    for id in items(strip_brackets(value, '[', ']')) {
                        if session.issues.contains(&IssueId::new(id)) {
                            reply.issues.push(IssueId::new(id));
                        } else {
                            problems.push(format!(
                                "{at}: unknown issue {id:?}; known issues: {}",
                                list(session.issues.issues.iter().map(|i| &i.id))
                            ));
                        }
                    }
                }
                "reply_to" => {
                    for id in items(strip_brackets(value, '[', ']')) {
                        if session.message(&MessageId::new(id)).is_some() {
                            reply.reply_to.push(MessageId::new(id));
                        } else {
                            problems.push(format!(
                                "{at}: unknown message {id:?}; use message ids such as m1"
                            ));
                        }
                    }
                }
                "stance" => {
                    for entry in items(strip_brackets(value, '{', '}')) {
                        let Some((id, pos)) = entry.split_once(':') else {
                            problems.push(format!(
                                "{at}: expected 'ISSUE: position' in stance, got {entry:?}"
                            ));
                            continue;
                        };
                        let id = id.trim().trim_matches(['"', '\'']);
                        let pos = pos.trim().trim_matches(['"', '\'']);
                        let known = session.issues.contains(&IssueId::new(id));
                        if !known {
                            problems.push(format!(
                                "{at}: unknown issue {id:?} in stance; known issues: {}",
                                list(session.issues.issues.iter().map(|i| &i.id))
                            ));
                        }
                        match Position::parse(pos) {
                            Some(pos) if known => reply.stances.push((IssueId::new(id), pos)),
                            Some(_) => {}
                            None => problems.push(format!(
                                "{at}: unknown position {pos:?} for {id:?}; expected for, against, neutral or unknown"
                            )),
                        }
                    }
                }
                other => problems.push(format!(
                    "{at}: unknown key {other:?}; expected issues, reply_to or stance"
                )),
            }
        }
        lines.collect::<Vec<_>>().join("\n")
    } else {
        text.to_owned()
    };
    reply.body = body.trim().to_owned();
    if reply.body.is_empty() {
        problems.push("the reply body is empty".to_owned());
    }
    if !problems.is_empty() {
        return Err(ValidationError { problems });
    }
    for (id, _) in &reply.stances {
        if !reply.issues.contains(id) {
            reply.issues.push(id.clone());
        }
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::reduce::{self, Event};
    use crate::model::types::{AgentKind, Author, Message, MsgKind, Participant, Role, SessionId};

    fn session() -> Session {
        let part = |id: &str, kind, role| Participant {
            id: PartId::new(id),
            kind,
            role,
            pane_id: None,
        };
        let mut session = reduce::create(&[Event::Created {
            id: SessionId::new("s"),
            topic: "t".into(),
            workdir: None,
            max_rounds: None,
            moderator: part("noa", AgentKind::Claude, Role::Moderator),
            panelists: vec![
                part("claude", AgentKind::Claude, Role::Panelist),
                part("codex", AgentKind::Codex, Role::Panelist),
                part("agy", AgentKind::Agy, Role::Panelist),
            ],
            at: 0,
        }])
        .unwrap();
        reduce::apply(
            &mut session,
            &[
                Event::RoundStarted {
                    no: 1,
                    kind: RoundKind::Open,
                    at: 0,
                },
                Event::MessagePosted {
                    message: Message {
                        id: MessageId::nth(1),
                        round: 1,
                        author: Author::Part(PartId::new("claude")),
                        kind: MsgKind::Reply,
                        issues: vec![],
                        reply_to: vec![],
                        to: vec![],
                        debate: None,
                        body: "hi".into(),
                        at: 0,
                    },
                },
                Event::IssueChanged {
                    round: 1,
                    op: IssueOp::Add {
                        id: IssueId::new("A"),
                        parent: None,
                        title: "a".into(),
                    },
                },
            ],
        )
        .unwrap();
        session
    }

    fn reject(json: &str, session: &Session, round: RoundNo) -> String {
        parse_moderator(json, session, round)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn accepts_the_full_contract() {
        let s = session();
        let json = r#"{ "summary": "sum",
          "issue_ops": [{"op":"add","id":"C","parent":"A","title":"c"},
                        {"op":"set_state","id":"C","state":"contested"},
                        {"op":"set_stance","id":"A","part":"codex","pos":"against","basis":"m1"}],
          "minutes_ops": [{"op":"add","section":"open_questions","text":"q","issues":["C"]}],
          "questions": {"codex": "why?", "agy": "and you?"},
          "propose_debate": {"issue":"C","pro":["claude"],"con":["codex"]},
          "close": false }"#;
        let out = parse_moderator(json, &s, 1).unwrap();
        assert_eq!(out.summary, "sum");
        assert_eq!(out.issue_ops.len(), 3);
        assert_eq!(out.minutes_ops[0].section, MinuteSection::OpenQuestions);
        assert_eq!(out.questions.len(), 2);
        assert_eq!(out.propose_debate.unwrap().issue, IssueId::new("C"));
        assert!(!out.close);
    }

    #[test]
    fn optional_fields_default_and_a_fence_is_tolerated() {
        let s = session();
        let out = parse_moderator("```json\n{\"summary\":\"x\"}\n```\n", &s, 1).unwrap();
        assert!(out.issue_ops.is_empty() && out.questions.is_empty() && !out.close);
        assert_eq!(out.verdict, None);
    }

    #[test]
    fn rejects_malformed_json_and_unknown_fields_and_ops() {
        let s = session();
        assert!(reject("not json", &s, 1).contains("not valid moderator JSON"));
        assert!(reject(r#"{"summary":"x","extra":1}"#, &s, 1).contains("unknown field `extra`"));
        assert!(
            reject(
                r#"{"summary":"x","issue_ops":[{"op":"delete","id":"A"}]}"#,
                &s,
                1
            )
            .contains("unknown variant `delete`")
        );
        assert!(
            reject(
                r#"{"summary":"x","issue_ops":[{"op":"add","id":"Z","title":"z","x":1}]}"#,
                &s,
                1
            )
            .contains("unknown field `x`")
        );
        assert!(reject(r#"{"issue_ops":[]}"#, &s, 1).contains("missing field `summary`"));
    }

    #[test]
    fn rejects_unknown_ids_parts_and_values_with_their_location() {
        let s = session();
        let json = r#"{ "summary": " ",
          "issue_ops": [{"op":"add","id":"A","title":"dup"},
                        {"op":"add","id":"bad id","title":"x"},
                        {"op":"add","id":"D","parent":"Q","title":""},
                        {"op":"set_state","id":"Z","state":"done"},
                        {"op":"set_stance","id":"A","part":"gemini","pos":"maybe","basis":"m99"}],
          "minutes_ops": [{"op":"add","section":"notes","text":"","issues":["Z"]}],
          "questions": {"noa": "self?", "codex": " "} }"#;
        let problems = parse_moderator(json, &s, 1).unwrap_err().problems;
        let expected = [
            "\"summary\" is empty",
            "issue_ops[0] (add): issue \"A\" already exists",
            "issue_ops[1] (add): issue id \"bad id\" must be",
            "issue_ops[2] (add): unknown parent issue \"Q\"; known issues: A",
            "issue_ops[2] (add): \"title\" is empty",
            "issue_ops[3] (set_state): unknown issue \"Z\"",
            "issue_ops[3] (set_state): unknown state \"done\"",
            "issue_ops[4] (set_stance): unknown panelist \"gemini\"; panelists: claude, codex, agy",
            "issue_ops[4] (set_stance): unknown pos \"maybe\"",
            "issue_ops[4] (set_stance): unknown basis message \"m99\"",
            "minutes_ops[0] (add): unknown section \"notes\"",
            "minutes_ops[0] (add): \"text\" is empty",
            "minutes_ops[0] (add): unknown issue \"Z\"",
            "questions: the question for \"codex\" is empty",
            "questions: unknown panelist \"noa\"",
        ];
        for want in expected {
            assert!(
                problems.iter().any(|p| p.starts_with(want)),
                "missing {want:?} in {problems:#?}"
            );
        }
        assert_eq!(problems.len(), expected.len(), "{problems:#?}");
    }

    #[test]
    fn later_ops_see_issues_added_earlier_in_the_same_output() {
        let s = session();
        let json = r#"{"summary":"x","issue_ops":[
            {"op":"add","id":"B","title":"b"},
            {"op":"add","id":"B1","parent":"B","title":"b1"},
            {"op":"set_state","id":"B1","state":"agreed"}]}"#;
        assert_eq!(parse_moderator(json, &s, 1).unwrap().issue_ops.len(), 3);
        // An op that comes before the add does not see it.
        let json = r#"{"summary":"x","issue_ops":[
            {"op":"set_state","id":"B","state":"agreed"},
            {"op":"add","id":"B","title":"b"}]}"#;
        assert!(reject(json, &s, 1).contains("issue_ops[0] (set_state): unknown issue \"B\""));
    }

    #[test]
    fn rejects_bad_debate_proposals() {
        let s = session();
        let json = r#"{"summary":"x","propose_debate":{"issue":"Q","pro":[],"con":["codex","codex","noa"]}}"#;
        let problems = parse_moderator(json, &s, 1).unwrap_err().problems;
        assert!(problems.iter().any(|p| p.contains("unknown issue \"Q\"")));
        assert!(problems.iter().any(|p| p.contains("\"pro\" is empty")));
        assert!(
            problems
                .iter()
                .any(|p| p.contains("\"codex\" is named more than once"))
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("unknown panelist \"noa\" in \"con\""))
        );
    }

    fn debate_session(max_exchanges: u8, exchanges: u32) -> Session {
        let mut s = session();
        reduce::apply(
            &mut s,
            &[Event::DebateProposed {
                issue: IssueId::new("A"),
                pro: vec![PartId::new("claude")],
                con: vec![PartId::new("codex")],
                max_exchanges,
                status: DebateStatus::Approved,
            }],
        )
        .unwrap();
        for _ in 0..exchanges {
            let no = s.rounds.len() as RoundNo + 1;
            reduce::apply(
                &mut s,
                &[Event::RoundStarted {
                    no,
                    kind: RoundKind::Debate(crate::model::types::DebateId::nth(1)),
                    at: 0,
                }],
            )
            .unwrap();
        }
        s
    }

    #[test]
    fn verdicts_belong_to_debate_rounds_and_end_the_last_exchange() {
        let s = session();
        let verdict = r#"{"summary":"x","verdict":{"outcome":"agreed","rationale":"r"}}"#;
        assert!(reject(verdict, &s, 1).contains("verdict: only allowed in a debate round"));

        let s = debate_session(2, 1);
        assert_eq!(
            parse_moderator(r#"{"summary":"x"}"#, &s, 2)
                .unwrap()
                .verdict,
            None
        );
        let out = parse_moderator(verdict, &s, 2).unwrap();
        assert_eq!(out.verdict.unwrap().outcome, IssueState::Agreed);
        let bad = r#"{"summary":"x","verdict":{"outcome":"contested","rationale":" "}}"#;
        let problems = parse_moderator(bad, &s, 2).unwrap_err().problems;
        assert_eq!(problems.len(), 2, "{problems:#?}");
        assert!(
            reject(
                r#"{"summary":"x","propose_debate":{"issue":"A","pro":["agy"],"con":["codex"]}}"#,
                &s,
                2
            )
            .contains("not allowed during a debate round")
        );

        let s = debate_session(2, 2);
        assert!(reject(r#"{"summary":"x"}"#, &s, 3).contains("verdict: required"));
    }

    #[test]
    fn a_second_debate_waits_for_the_first() {
        let mut s = session();
        reduce::apply(
            &mut s,
            &[Event::DebateProposed {
                issue: IssueId::new("A"),
                pro: vec![PartId::new("claude")],
                con: vec![PartId::new("codex")],
                max_exchanges: 2,
                status: DebateStatus::Proposed,
            }],
        )
        .unwrap();
        let json =
            r#"{"summary":"x","propose_debate":{"issue":"A","pro":["agy"],"con":["codex"]}}"#;
        assert!(reject(json, &s, 1).contains("already waiting"));
    }

    #[test]
    fn parses_a_reply_header() {
        let s = session();
        let text =
            "---\nissues: [A]\nreply_to: [m1]\nstance: {A: against}\n---\nI disagree.\n\nBecause.";
        let reply = parse_reply(text, &s).unwrap();
        assert_eq!(reply.issues, vec![IssueId::new("A")]);
        assert_eq!(reply.reply_to, vec![MessageId::nth(1)]);
        assert_eq!(reply.stances, vec![(IssueId::new("A"), Position::Against)]);
        assert_eq!(reply.body, "I disagree.\n\nBecause.");
    }

    #[test]
    fn the_header_example_in_the_brief_parses_as_written() {
        let s = session();
        // The example lines exactly as a panelist's brief shows them, with
        // their trailing `#` comments.
        let brief = crate::model::brief::panelist_brief(
            &s,
            &PartId::new("claude"),
            1,
            "q",
            std::path::Path::new("/r/claude.md"),
        );
        let example: Vec<&str> = brief
            .body
            .lines()
            .filter(|line| {
                ["issues:", "reply_to:", "stance:"]
                    .iter()
                    .any(|k| line.starts_with(k))
            })
            .collect();
        assert_eq!(example.len(), 3, "{}", brief.body);
        assert!(example.iter().all(|line| line.contains('#')));
        let text = format!(
            "---\n{}\n# a note of my own\n---\nI disagree.",
            example.join("\n")
        );
        let reply = parse_reply(&text, &s).unwrap();
        assert_eq!(reply.issues, vec![IssueId::new("A")]);
        assert_eq!(reply.reply_to, vec![MessageId::nth(1)]);
        assert_eq!(reply.stances, vec![(IssueId::new("A"), Position::Against)]);
    }

    #[test]
    fn header_values_may_be_quoted_or_bare_and_stances_imply_issues() {
        let s = session();
        let text = "\n---\nreply_to: \"m1\"\nstance: {\"A\": \"for\"}\n\n---\nok";
        let reply = parse_reply(text, &s).unwrap();
        assert_eq!(reply.issues, vec![IssueId::new("A")]);
        assert_eq!(reply.reply_to, vec![MessageId::nth(1)]);
        assert_eq!(reply.stances, vec![(IssueId::new("A"), Position::For)]);
        let empty = parse_reply("---\nissues: []\n---\nbody", &s).unwrap();
        assert!(empty.issues.is_empty());
    }

    #[test]
    fn a_reply_without_a_header_is_all_body() {
        let s = session();
        let reply = parse_reply("Just my thoughts.\n---\nmore", &s).unwrap();
        assert!(reply.issues.is_empty() && reply.stances.is_empty());
        assert_eq!(reply.body, "Just my thoughts.\n---\nmore");
    }

    #[test]
    fn rejects_bad_reply_headers() {
        let s = session();
        let err = |text: &str| parse_reply(text, &s).unwrap_err().to_string();
        assert!(err("---\nissues: [A]\nbody").contains("no closing '---'"));
        assert!(err("---\nissues: [Z]\n---\nb").contains("unknown issue \"Z\""));
        assert!(err("---\nreply_to: [m7]\n---\nb").contains("unknown message \"m7\""));
        assert!(err("---\nstance: {A: maybe}\n---\nb").contains("unknown position \"maybe\""));
        assert!(err("---\nstance: {A}\n---\nb").contains("expected 'ISSUE: position'"));
        assert!(err("---\nmood: happy\n---\nb").contains("unknown key \"mood\""));
        assert!(err("---\nissues: [A]\nissues: [A]\n---\nb").contains("given twice"));
        assert!(err("---\nnot a pair\n---\nb").contains("expected 'key: value'"));
        assert!(err("---\nissues: [A]\n---\n  \n").contains("body is empty"));
        assert!(err("").contains("body is empty"));
    }

    #[test]
    fn issue_ids_are_short_tags() {
        assert!(is_valid_issue_id("B2"));
        assert!(is_valid_issue_id("cost-model_1"));
        assert!(!is_valid_issue_id(""));
        assert!(!is_valid_issue_id("has space"));
        assert!(!is_valid_issue_id("[B]"));
        assert!(!is_valid_issue_id(&"x".repeat(17)));
    }
}
