//! Situation briefs, assembled from state alone. Every brief carries every
//! mandatory section, in order, and every message since the recipient's
//! previous brief: what an agent knows is guaranteed here, not left to the
//! moderator.

use std::fmt::Write as _;
use std::path::Path;

use super::types::{
    Author, Brief, MODERATOR_ID, Message, MinuteSection, MsgKind, PartId, Receipt, RoundKind,
    RoundNo, Session, Side,
};

/// The headings of a panelist's brief, in the order they appear.
pub const PANELIST_SECTIONS: [&str; 7] = [
    "## 1. Topic and round",
    "## 2. Issue tree",
    "## 3. Changes since your previous brief",
    "## 4. Messages since your previous brief",
    "## 5. User interventions",
    "## 6. Your question",
    "## 7. How to reply",
];

/// The headings of the moderator's summarize brief, in order.
pub const MODERATOR_SECTIONS: [&str; 7] = [
    "## 1. Topic and round",
    "## 2. Issue tree",
    "## 3. Changes since your previous brief",
    "## 4. Messages since your previous brief",
    "## 5. User interventions",
    "## 6. This round",
    "## 7. Your output",
];

/// The opening question of round 1, when there is no moderator output yet.
pub const OPENING_QUESTION: &str = "This is the opening round. Give your position on the \
     topic: the main issues you see, where you stand on each, and why.";

/// The question for a panelist the moderator left out.
pub const DEFAULT_QUESTION: &str = "Respond to the latest summary and to the open and \
     contested issues: say where you agree or disagree with the other panelists, and why.";

/// What one message looks like inside a brief: a header line with its id,
/// round, author, kind and links, then its full text.
pub fn render_message(session: &Session, message: &Message) -> String {
    let mut line = format!(
        "### [{}] R{} · {} · {}",
        message.id,
        message.round,
        session.author_name(&message.author),
        message.kind.name()
    );
    if !message.issues.is_empty() {
        let issues: Vec<&str> = message.issues.iter().map(|i| i.as_str()).collect();
        let _ = write!(line, " · issues: {}", issues.join(", "));
    }
    if !message.reply_to.is_empty() {
        let to: Vec<&str> = message.reply_to.iter().map(|m| m.as_str()).collect();
        let _ = write!(line, " · ↪ {}", to.join(", "));
    }
    if !message.to.is_empty() {
        let to: Vec<&str> = message.to.iter().map(|p| p.as_str()).collect();
        let _ = write!(line, " · to: {}", to.join(", "));
    }
    if let Some(debate) = &message.debate {
        let _ = write!(line, " · debate {debate}");
    }
    format!("{line}\n{}\n", message.body)
}

fn issue_line(session: &Session, depth: usize, issue: &super::types::Issue) -> String {
    let stances: Vec<String> = session
        .panelist_ids()
        .map(|part| match issue.stances.get(part) {
            Some(stance) => format!("{part}: {} ({})", stance.pos.name(), stance.basis),
            None => format!("{part}: no stance"),
        })
        .collect();
    format!(
        "{}- [{}] {} — {} · {}\n",
        "  ".repeat(depth),
        issue.id,
        issue.title,
        issue.state.name(),
        stances.join(", ")
    )
}

fn section_topic(out: &mut String, heading: &str, session: &Session, round: RoundNo) {
    let kind = match session.round(round).map(|r| &r.kind) {
        Some(RoundKind::Debate(id)) => match session.debate(id) {
            Some(debate) => format!("debate {id} on issue {}", debate.issue),
            None => format!("debate {id}"),
        },
        _ => "open discussion".to_owned(),
    };
    let panel: Vec<&str> = session.panelist_ids().map(|p| p.as_str()).collect();
    let of = session
        .max_rounds
        .map(|max| format!(" of {max}"))
        .unwrap_or_default();
    let _ = write!(
        out,
        "{heading}\nTopic: {}\nRound: {round}{of} ({kind})\nPanelists: {}\nModerator: {}\n\n",
        session.topic,
        panel.join(", "),
        session.moderator.id
    );
}

/// The rounds left after `round`; `None` when the discussion has no limit.
fn rounds_left(session: &Session, round: RoundNo) -> Option<RoundNo> {
    session.max_rounds.map(|max| max.saturating_sub(round))
}

/// What the moderator is told about converging, from the rounds left.
fn convergence_note(session: &Session, round: RoundNo) -> String {
    match (session.max_rounds, rounds_left(session, round)) {
        (_, Some(0)) if can_raise_limit(session) => "This is the LAST round under the current \
             limit: \"conclusion\" is required, unless you raise \"round_limit\" as allowed \
             below (then leave \"conclusion\" out). Give your answer \
             to the topic. Where the panel agreed, state it; where it did not, decide it \
             yourself: weigh the arguments, choose the best-supported position, and say why \
             and what would change it. Do not leave the topic open or defer it. Do not \
             propose a debate.\n"
            .to_owned(),
        (_, Some(0)) => "This is the LAST round: \"conclusion\" is required. Give your answer \
             to the topic. Where the panel agreed, state it; where it did not, decide it \
             yourself: weigh the arguments, choose the best-supported position, and say why \
             and what would change it. Do not leave the topic open or defer it. Do not \
             propose a debate.\n"
            .to_owned(),
        (_, Some(1)) => "One round is left after this one, and it is the last. Ask each \
             panelist for their final position and the compromise they can accept on every \
             issue still open or contested. Do not propose a debate. If the issues are \
             already settled, give the \"conclusion\" now.\n"
            .to_owned(),
        (Some(max), Some(left)) => format!(
            "{left} rounds are left after this one; the discussion must reach a conclusion \
             by round {max}. Narrow the questions to the issues still open or contested, mark \
             settled ones agreed, and open no new side issues unless they decide the topic. \
             If the issues are settled sooner, give the \"conclusion\" now.\n"
        ),
        _ => "There is no round limit: give the \"conclusion\" as soon as the issues are \
             settled, and narrow the questions to what is still open or contested until \
             then.\n"
            .to_owned(),
    }
}

/// Whether the moderator may still raise the round limit.
fn can_raise_limit(session: &Session) -> bool {
    matches!(
        (session.max_rounds, session.round_limit_cap()),
        (Some(max), Some(cap)) if max < cap
    )
}

/// When the moderator may move the round limit, and how far; empty for a
/// discussion with no limit.
fn limit_change_note(session: &Session, round: RoundNo) -> String {
    let (Some(max), Some(cap)) = (session.max_rounds, session.round_limit_cap()) else {
        return String::new();
    };
    let raise = if max < cap {
        format!("raise it (at most {cap}) if more rounds would settle it, or ")
    } else {
        String::new()
    };
    format!(
        "Only if the discussion clearly cannot converge within the limit ({max}), you may \
         change it with \"round_limit\": {raise}lower it (not below {round}) if more rounds \
         would not help. Say why in the summary. A \"conclusion\" ends the discussion, so \
         leave it out whenever the new limit is above {round}; the new last round still \
         ends in your conclusion.\n"
    )
}

fn section_tree(out: &mut String, heading: &str, session: &Session) {
    out.push_str(heading);
    out.push('\n');
    let walk = session.issues.walk();
    if walk.is_empty() {
        out.push_str("(no issues yet)\n");
    }
    for (depth, issue) in walk {
        out.push_str(&issue_line(session, depth, issue));
    }
    out.push('\n');
}

fn section_delta(out: &mut String, heading: &str, session: &Session, base: Option<u64>) {
    out.push_str(heading);
    out.push('\n');
    let Some(base) = base else {
        out.push_str("(this is your first brief: everything in it is new to you)\n\n");
        return;
    };
    let mut any = false;
    for issue in &session.issues.issues {
        let what = if issue.added_rev > base {
            "added"
        } else if issue.changed_rev > base {
            "changed"
        } else {
            continue;
        };
        any = true;
        let _ = write!(out, "{what}: ");
        out.push_str(issue_line(session, 0, issue).trim_start_matches("- "));
    }
    for section in MinuteSection::ALL {
        for item in session.minutes.section(section) {
            if item.rev > base {
                any = true;
                let _ = writeln!(out, "minutes, {}: {}", section.name(), item.text);
            }
        }
    }
    if !any {
        out.push_str("(no issue or minutes changes)\n");
    }
    out.push('\n');
}

fn section_messages(out: &mut String, heading: &str, session: &Session, messages: &[Message]) {
    out.push_str(heading);
    out.push('\n');
    if messages.is_empty() {
        out.push_str("(no new messages)\n");
    }
    for (i, message) in messages.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&render_message(session, message));
    }
    out.push('\n');
}

fn section_interventions(out: &mut String, heading: &str, messages: &[Message]) {
    out.push_str(heading);
    out.push('\n');
    let mut any = false;
    for message in messages.iter().filter(|m| m.kind == MsgKind::Intervention) {
        any = true;
        let to = if message.to.is_empty() {
            "everyone".to_owned()
        } else {
            let to: Vec<&str> = message.to.iter().map(|p| p.as_str()).collect();
            to.join(", ")
        };
        let _ = writeln!(out, "- [{}] to {to}: {}", message.id, message.body);
    }
    if !any {
        out.push_str("(none)\n");
    }
    out.push('\n');
}

fn issue_ids(session: &Session) -> String {
    let ids: Vec<&str> = session
        .issues
        .issues
        .iter()
        .map(|i| i.id.as_str())
        .collect();
    if ids.is_empty() {
        "(none yet)".to_owned()
    } else {
        ids.join(", ")
    }
}

fn draft(
    session: &Session,
    to: &PartId,
    round: RoundNo,
    reply_path: &Path,
    sections: &[&str; 7],
    tail: impl FnOnce(&mut String, &[Message]),
) -> Brief {
    let previous = session.previous_brief(to, round);
    let since = previous.and_then(|b| b.through.clone());
    let messages = session.messages_after(since.as_ref());
    let mut body = String::new();
    section_topic(&mut body, sections[0], session, round);
    section_tree(&mut body, sections[1], session);
    section_delta(
        &mut body,
        sections[2],
        session,
        previous.map(|b| b.base_rev),
    );
    section_messages(&mut body, sections[3], session, messages);
    section_interventions(&mut body, sections[4], messages);
    tail(&mut body, messages);
    Brief {
        to: to.clone(),
        base_rev: session.rev,
        since,
        through: session.messages.last().map(|m| m.id.clone()),
        body: body.trim_end().to_owned() + "\n",
        reply_path: reply_path.to_owned(),
        receipt: Receipt::default(),
    }
}

/// The brief for panelist `to` in `round`, asking `question`.
pub fn panelist_brief(
    session: &Session,
    to: &PartId,
    round: RoundNo,
    question: &str,
    reply_path: &Path,
) -> Brief {
    draft(
        session,
        to,
        round,
        reply_path,
        &PANELIST_SECTIONS,
        |out, _| {
            out.push_str(PANELIST_SECTIONS[5]);
            out.push('\n');
            let debate = session
                .round(round)
                .and_then(|r| r.debate())
                .and_then(|id| session.debate(id));
            if let Some(debate) = debate {
                let role = match debate.side(to) {
                    Some(Side::Pro) => format!("you argue FOR issue {}", debate.issue),
                    Some(Side::Con) => format!("you argue AGAINST issue {}", debate.issue),
                    None => format!("you observe the debate on issue {}", debate.issue),
                };
                let _ = writeln!(out, "Your role in debate {}: {role}.", debate.id);
            }
            match (session.max_rounds, rounds_left(session, round)) {
                (_, Some(0)) => out.push_str(
                    "This is the last round: the moderator concludes the discussion after it. \
                     Give your final position on each issue still open or contested, and the \
                     compromise you can accept.\n",
                ),
                (_, Some(1)) => out.push_str(
                    "One round is left after this one, and it is the last. Move toward a \
                     conclusion: say where you can agree and what compromise you could accept.\n",
                ),
                (Some(max), Some(left)) => {
                    let _ = writeln!(
                        out,
                        "{left} rounds are left after this one; the discussion concludes by \
                         round {max}. Help it converge: focus on the issues still open or \
                         contested rather than opening new ones."
                    );
                }
                _ => {}
            }
            let _ = write!(out, "{question}\n\n");

            out.push_str(PANELIST_SECTIONS[6]);
            out.push('\n');
            let _ = write!(
                out,
                "Write your reply as a Markdown file at:\n{}\n\
             Write the whole file in one go when you are done: the file appearing is how \
             your reply is collected. Do not answer in the terminal.\n\n\
             The file starts with this header (every line in it is optional):\n\
             ---\n\
             issues: [A]          # the issues your reply is about\n\
             reply_to: [m1]       # the messages you answer, by their [m…] id\n\
             stance: {{A: against}} # your position per issue: for, against, neutral or unknown\n\
             ---\n\
             Then your reply.\n\n\
             Issue ids: {}\n",
                reply_path.display(),
                issue_ids(session)
            );
        },
    )
}

/// The moderator's summarize brief for `round`.
pub fn moderator_brief(session: &Session, round: RoundNo, reply_path: &Path) -> Brief {
    let moderator = session.moderator.id.clone();
    draft(
        session,
        &moderator,
        round,
        reply_path,
        &MODERATOR_SECTIONS,
        |out, _| {
            out.push_str(MODERATOR_SECTIONS[5]);
            out.push('\n');
            let this = session.round(round);
            for part in session.panelist_ids() {
                let reply = session.messages.iter().rev().find(|m| {
                    m.round == round
                        && m.author == Author::Part(part.clone())
                        && matches!(m.kind, MsgKind::Reply | MsgKind::Argument)
                });
                let receipt = this.and_then(|r| r.brief(part)).map(|b| &b.receipt);
                let status = match (reply, receipt.and_then(|r| r.failed.as_deref())) {
                    (Some(reply), _) => format!("replied in {} (full text in section 4)", reply.id),
                    (None, Some(why)) => format!("no reply: {why}"),
                    (None, None) => "no reply".to_owned(),
                };
                let _ = writeln!(out, "- {part}: {status}");
            }
            let debate = this
                .and_then(|r| r.debate())
                .and_then(|id| session.debate(id));
            if let Some(debate) = debate {
                let exchanges = session
                    .rounds
                    .iter()
                    .filter(|r| r.no <= round && r.kind == RoundKind::Debate(debate.id.clone()))
                    .count();
                let names = |side: &[PartId]| {
                    side.iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let _ = writeln!(
                    out,
                    "Debate {} on issue {}: pro {} · con {} · exchange {exchanges} of {}.",
                    debate.id,
                    debate.issue,
                    names(&debate.pro),
                    names(&debate.con),
                    debate.max_exchanges
                );
                if exchanges >= usize::from(debate.max_exchanges)
                    || rounds_left(session, round) == Some(0)
                {
                    out.push_str("This is the last exchange: \"verdict\" is required.\n");
                } else {
                    out.push_str("Give a \"verdict\" once the debate is settled; otherwise the next exchange runs.\n");
                }
            }
            out.push_str(&convergence_note(session, round));
            out.push_str(&limit_change_note(session, round));
            out.push('\n');

            out.push_str(MODERATOR_SECTIONS[6]);
            out.push('\n');
            let panel: Vec<&str> = session.panelist_ids().map(|p| p.as_str()).collect();
            // A debate round may end in a verdict; any other may propose one.
            let last = rounds_left(session, round) == Some(0);
            let (optional, rule) = if debate.is_some() {
                (
                    "  \"verdict\": {\"outcome\":\"agreed\",\"rationale\":\"…\"},\n",
                    "verdict: outcome agreed, conditional or deferred; it ends the debate",
                )
            } else if last {
                ("", "propose_debate: not allowed in the last round")
            } else {
                (
                    "  \"propose_debate\": {\"issue\":\"B\",\"pro\":[\"claude\"],\"con\":[\"codex\"]},\n",
                    "propose_debate: a debate on one issue; it runs once the user approves it",
                )
            };
            // Shown in the example only where it is required, so that it is
            // not filled in (ending the discussion) by imitation.
            let (conclusion, required) = if last && can_raise_limit(session) {
                (
                    "  \"conclusion\": \"…\",\n",
                    "Only \"summary\" and \"conclusion\" are required (leave \"conclusion\" \
                     out if you raise \"round_limit\").",
                )
            } else if last {
                (
                    "  \"conclusion\": \"…\",\n",
                    "Only \"summary\" and \"conclusion\" are required.",
                )
            } else {
                ("", "Only \"summary\" is required.")
            };
            let _ = write!(
                out,
                "Summarize the round, update the issue tree and the minutes, and ask each \
             panelist the next question. Write ONLY this JSON to the file:\n{}\n\
             Write the whole file in one go when you are done. Do not answer in the terminal.\n\n\
             {{ \"summary\": \"…\",\n\
             \x20 \"issue_ops\": [{{\"op\":\"add\",\"id\":\"C\",\"parent\":null,\"title\":\"…\"}},\n\
             \x20               {{\"op\":\"set_state\",\"id\":\"B\",\"state\":\"contested\"}},\n\
             \x20               {{\"op\":\"set_stance\",\"id\":\"B\",\"part\":\"codex\",\"pos\":\"against\",\"basis\":\"m12\"}}],\n\
             \x20 \"minutes_ops\": [{{\"op\":\"add\",\"section\":\"decisions\",\"text\":\"…\",\"issues\":[\"A\"]}}],\n\
             \x20 \"questions\": {{\"codex\": \"…\"}},\n\
             {}{}\
             \x20 \"close\": false }}\n\n\
             {} Rules:\n\
             - issue ids: 1-16 letters, digits, '-' or '_'; existing: {}\n\
             - states: open, contested, agreed, conditional, deferred\n\
             - stance pos: for, against, neutral, unknown; \"basis\" is a message id from section 4\n\
             - minutes sections: decisions, contested, open_questions, actions\n\
             - panelists (for questions, stances and debate sides): {}\n\
             - a panelist with no question gets a default one\n\
             - {}\n\
             - conclusion: your answer to the topic, deciding it yourself where the panel \
             did not agree; it ends the discussion. Required in the last round, unless \
             \"round_limit\" raises the limit past it\n\
             - round_limit: a new round limit, only as section 6 allows; it replaces the \
             current one for the rest of the discussion\n\
             - close: true ends the discussion and needs a \"conclusion\"\n",
                reply_path.display(),
                optional,
                conclusion,
                required,
                issue_ids(session),
                panel.join(", "),
                rule,
            );
        },
    )
}

/// The question for a debate side the moderator gave none.
pub const DEBATE_SIDE_QUESTION: &str = "Make your strongest argument for your side and answer \
     the other side's latest points.";

/// The question for a panelist outside the debate the moderator gave none.
pub const DEBATE_OBSERVER_QUESTION: &str = "Add anything the debate is missing; if you have \
     nothing to add, reply briefly that you have nothing to add.";

/// The question panelist `part` gets in a round of `kind` when the moderator
/// gave none.
pub fn default_question(session: &Session, kind: &RoundKind, part: &PartId) -> &'static str {
    match kind {
        RoundKind::Debate(id) => match session.debate(id).and_then(|d| d.side(part)) {
            Some(_) => DEBATE_SIDE_QUESTION,
            None => DEBATE_OBSERVER_QUESTION,
        },
        RoundKind::Open if session.messages.iter().all(|m| m.kind != MsgKind::Summary) => {
            OPENING_QUESTION
        }
        RoundKind::Open => DEFAULT_QUESTION,
    }
}

/// What an agent is sent when its output file was turned down.
pub fn correction_note(to: &PartId, error: &str, reply_path: &Path) -> String {
    let what = if to.as_str() == MODERATOR_ID {
        "moderator JSON"
    } else {
        "reply file"
    };
    format!(
        "Your {what} could not be used:\n{error}\n\nFix it and write the whole file again at:\n{}\n",
        reply_path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::reduce::{self, Event};
    use crate::model::types::{
        AgentKind, DebateId, DebateStatus, IssueId, IssueOp, IssueState, MessageId, MinuteOp,
        ModeratorOutput, Participant, Position, Role, SessionId,
    };
    use std::path::PathBuf;

    fn part(id: &str) -> PartId {
        PartId::new(id)
    }

    fn session() -> Session {
        let p = |id: &str, kind, role| Participant {
            id: part(id),
            kind,
            role,
            pane_id: None,
            setup: Default::default(),
        };
        reduce::create(&[Event::Created {
            id: SessionId::new("s"),
            topic: "Pick a database".into(),
            workdir: None,
            max_rounds: None,
            moderator: p("noa", AgentKind::Claude, Role::Moderator),
            panelists: vec![
                p("claude", AgentKind::Claude, Role::Panelist),
                p("codex", AgentKind::Codex, Role::Panelist),
            ],
            at: 0,
        }])
        .unwrap()
    }

    fn post(session: &mut Session, round: RoundNo, author: Author, kind: MsgKind, body: &str) {
        let message = Message {
            id: session.next_message_id(0),
            round,
            author,
            kind,
            issues: vec![],
            reply_to: vec![],
            to: vec![],
            debate: None,
            body: body.into(),
            at: 0,
        };
        reduce::apply(session, &[Event::MessagePosted { message }]).unwrap();
    }

    fn start(session: &mut Session, no: RoundNo, kind: RoundKind) {
        reduce::apply(session, &[Event::RoundStarted { no, kind, at: 0 }]).unwrap();
    }

    /// Issues `brief` and records it as sent: the next brief to its
    /// recipient starts where a sent one ended.
    fn issue(brief: &Brief, session: &mut Session, round: RoundNo) {
        reduce::apply(
            session,
            &[
                Event::BriefIssued {
                    round,
                    brief: brief.clone(),
                },
                Event::BriefSent {
                    round,
                    to: brief.to.clone(),
                    at: 0,
                },
            ],
        )
        .unwrap();
    }

    fn assert_sections(body: &str, sections: &[&str; 7]) {
        let mut from = 0;
        for heading in sections {
            let at = body[from..]
                .find(heading)
                .unwrap_or_else(|| panic!("{heading:?} missing or out of order in:\n{body}"));
            from += at + heading.len();
        }
    }

    fn path() -> PathBuf {
        PathBuf::from("/d/r1/claude.md")
    }

    #[test]
    fn an_empty_session_still_has_every_section() {
        let mut s = session();
        start(&mut s, 1, RoundKind::Open);
        let brief = panelist_brief(&s, &part("claude"), 1, OPENING_QUESTION, &path());
        assert_sections(&brief.body, &PANELIST_SECTIONS);
        assert!(brief.body.contains("Topic: Pick a database"));
        assert!(brief.body.contains("Round: 1 (open discussion)"));
        assert!(brief.body.contains("(no issues yet)"));
        assert!(brief.body.contains("this is your first brief"));
        assert!(brief.body.contains("(no new messages)"));
        assert!(brief.body.contains(OPENING_QUESTION));
        assert!(brief.body.contains("/d/r1/claude.md"));
        assert_eq!((brief.since.clone(), brief.through.clone()), (None, None));

        let moderator = moderator_brief(&s, 1, Path::new("/d/r1/moderator.json"));
        assert_sections(&moderator.body, &MODERATOR_SECTIONS);
        assert!(moderator.body.contains("/d/r1/moderator.json"));
        assert!(moderator.body.contains("- claude: no reply"));
        assert!(moderator.body.contains("\"propose_debate\": {"));
        assert!(!moderator.body.contains("\"verdict\": {"));
    }

    #[test]
    fn a_full_session_has_every_section_in_order() {
        let mut s = session();
        start(&mut s, 1, RoundKind::Open);
        post(
            &mut s,
            1,
            Author::User,
            MsgKind::Intervention,
            "Consider cost.",
        );
        reduce::apply(
            &mut s,
            &[
                Event::IssueChanged {
                    round: 1,
                    op: IssueOp::Add {
                        id: IssueId::new("A"),
                        parent: None,
                        title: "Postgres".into(),
                    },
                },
                Event::IssueChanged {
                    round: 1,
                    op: IssueOp::Add {
                        id: IssueId::new("A1"),
                        parent: Some(IssueId::new("A")),
                        title: "Hosting".into(),
                    },
                },
                Event::IssueChanged {
                    round: 1,
                    op: IssueOp::SetStance {
                        id: IssueId::new("A"),
                        part: part("codex"),
                        pos: Position::Against,
                        basis: MessageId::nth(1),
                    },
                },
            ],
        )
        .unwrap();
        let brief = panelist_brief(&s, &part("claude"), 1, "Why?", &path());
        assert_sections(&brief.body, &PANELIST_SECTIONS);
        assert!(brief.body.contains(
            "- [A] Postgres — open · claude: no stance, codex: against (m1)\n  - [A1] Hosting"
        ));
        assert!(brief.body.contains("- [m1] to everyone: Consider cost."));
        assert!(brief.body.contains("Issue ids: A, A1"));
    }

    #[test]
    fn the_delta_names_what_changed_since_the_previous_brief() {
        let mut s = session();
        start(&mut s, 1, RoundKind::Open);
        reduce::apply(
            &mut s,
            &[Event::IssueChanged {
                round: 1,
                op: IssueOp::Add {
                    id: IssueId::new("A"),
                    parent: None,
                    title: "old".into(),
                },
            }],
        )
        .unwrap();
        let first = panelist_brief(&s, &part("claude"), 1, "q", &path());
        issue(&first, &mut s, 1);
        start(&mut s, 2, RoundKind::Open);
        let unchanged = panelist_brief(&s, &part("claude"), 2, "q", &path());
        assert!(unchanged.body.contains("(no issue or minutes changes)"));

        post(&mut s, 1, Author::User, MsgKind::Intervention, "x");
        reduce::apply(
            &mut s,
            &[Event::ModeratorAccepted {
                round: 1,
                output: ModeratorOutput {
                    summary: "s".into(),
                    issue_ops: vec![
                        IssueOp::Add {
                            id: IssueId::new("B"),
                            parent: None,
                            title: "new".into(),
                        },
                        IssueOp::SetState {
                            id: IssueId::new("A"),
                            state: IssueState::Contested,
                        },
                    ],
                    minutes_ops: vec![MinuteOp {
                        section: MinuteSection::Actions,
                        text: "benchmark".into(),
                        issues: vec![],
                    }],
                    questions: Default::default(),
                    propose_debate: None,
                    verdict: None,
                    conclusion: None,
                    round_limit: None,
                    close: false,
                },
            }],
        )
        .unwrap();
        let second = panelist_brief(&s, &part("claude"), 2, "q", &path());
        assert!(second.body.contains("added: [B] new — open"));
        assert!(second.body.contains("changed: [A] old — contested"));
        assert!(second.body.contains("minutes, actions: benchmark"));
    }

    #[test]
    fn messages_are_carried_in_full_without_gaps_across_rounds() {
        let mut s = session();
        let claude = part("claude");
        let mut carried: Vec<String> = Vec::new();
        let mut previous_through = None;
        for round in 1..=4 {
            start(&mut s, round, RoundKind::Open);
            for n in 0..round {
                post(
                    &mut s,
                    round,
                    Author::Part(part("codex")),
                    MsgKind::Reply,
                    &format!("round {round} message {n}\nsecond line {n}"),
                );
            }
            let brief = panelist_brief(&s, &claude, round, "q", &path());
            assert_eq!(brief.since, previous_through, "round {round}");
            assert_sections(&brief.body, &PANELIST_SECTIONS);
            for message in s.messages_after(brief.since.as_ref()) {
                assert!(brief.body.contains(&render_message(&s, message)));
                carried.push(message.id.to_string());
            }
            previous_through = brief.through.clone();
            issue(&brief, &mut s, round);
            // Messages after the brief, still in this round, reach the next one.
            post(&mut s, round, Author::User, MsgKind::Intervention, "late");
        }
        let delivered: Vec<String> = s.messages[..carried.len()]
            .iter()
            .map(|m| m.id.to_string())
            .collect();
        assert_eq!(carried, delivered, "every message, once, in order");
        assert_eq!(
            s.messages.len(),
            carried.len() + 1,
            "only the last is still due"
        );
    }

    #[test]
    fn debate_briefs_state_the_recipient_role() {
        let mut s = session();
        reduce::apply(
            &mut s,
            &[
                Event::IssueChanged {
                    round: 1,
                    op: IssueOp::Add {
                        id: IssueId::new("B"),
                        parent: None,
                        title: "b".into(),
                    },
                },
                Event::DebateProposed {
                    issue: IssueId::new("B"),
                    pro: vec![part("claude")],
                    con: vec![part("codex")],
                    max_exchanges: 1,
                    status: DebateStatus::Approved,
                },
            ],
        )
        .unwrap();
        start(&mut s, 1, RoundKind::Debate(DebateId::nth(1)));
        let pro = panelist_brief(&s, &part("claude"), 1, "q", &path());
        let con = panelist_brief(&s, &part("codex"), 1, "q", &path());
        assert!(pro.body.contains("you argue FOR issue B"));
        assert!(con.body.contains("you argue AGAINST issue B"));
        assert!(pro.body.contains("Round: 1 (debate d1 on issue B)"));
        let moderator = moderator_brief(&s, 1, Path::new("/m.json"));
        assert!(moderator.body.contains("exchange 1 of 1"));
        assert!(moderator.body.contains("\"verdict\": {"));
        assert!(!moderator.body.contains("\"propose_debate\": {"));
        assert!(moderator.body.contains("\"verdict\" is required"));
    }

    #[test]
    fn the_moderator_brief_carries_every_reply_of_the_round_in_full() {
        let mut s = session();
        start(&mut s, 1, RoundKind::Open);
        let first = moderator_brief(&s, 1, Path::new("/m1.json"));
        issue(&first, &mut s, 1);
        start(&mut s, 2, RoundKind::Open);
        post(
            &mut s,
            2,
            Author::Part(part("claude")),
            MsgKind::Reply,
            "claude's whole\nargument",
        );
        post(
            &mut s,
            2,
            Author::Part(part("codex")),
            MsgKind::Reply,
            "codex's answer",
        );
        let brief = moderator_brief(&s, 2, Path::new("/m2.json"));
        assert_sections(&brief.body, &MODERATOR_SECTIONS);
        for message in &s.messages {
            assert!(brief.body.contains(&render_message(&s, message)));
        }
        assert!(brief.body.contains("- claude: replied in m1"));
        assert!(brief.body.contains("- codex: replied in m2"));
    }

    #[test]
    fn notes_name_the_problem_and_the_path() {
        let error = "- issue_ops[0] (add): \"title\" is empty";
        let note = correction_note(&part("noa"), error, Path::new("/m.json"));
        assert!(
            note.contains("moderator JSON") && note.contains("title") && note.contains("/m.json")
        );
        let note = correction_note(&part("codex"), error, Path::new("/c.md"));
        assert!(note.contains("reply file") && note.contains("/c.md"));
    }

    #[test]
    fn the_default_question_opens_round_one_only() {
        let mut s = session();
        let claude = part("claude");
        assert_eq!(
            default_question(&s, &RoundKind::Open, &claude),
            OPENING_QUESTION
        );
        post(&mut s, 1, Author::Part(part("noa")), MsgKind::Summary, "s");
        assert_eq!(
            default_question(&s, &RoundKind::Open, &claude),
            DEFAULT_QUESTION
        );
    }
}
