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

/// Writes one message's brief representation directly into `out`: a header
/// line with its id, round, author, kind and links, then its full text.
fn write_message(out: &mut String, session: &Session, message: &Message) {
    let _ = write!(
        out,
        "### [{}] R{} · {} · {}",
        message.id,
        message.round,
        session.author_name(&message.author),
        message.kind.name()
    );
    if !message.issues.is_empty() {
        out.push_str(" · issues: ");
        for (i, issue) in message.issues.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(issue.as_str());
        }
    }
    if !message.reply_to.is_empty() {
        out.push_str(" · ↪ ");
        for (i, id) in message.reply_to.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(id.as_str());
        }
    }
    if !message.to.is_empty() {
        out.push_str(" · to: ");
        for (i, part) in message.to.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(part.as_str());
        }
    }
    if let Some(debate) = &message.debate {
        let _ = write!(out, " · debate {debate}");
    }
    out.push('\n');
    out.push_str(&message.body);
    out.push('\n');
}

/// What one message looks like inside a brief: a header line with its id,
/// round, author, kind and links, then its full text.
pub fn render_message(session: &Session, message: &Message) -> String {
    let mut out = String::with_capacity(message.body.len() + 96);
    write_message(&mut out, session, message);
    out
}

fn write_issue_line(
    out: &mut String,
    session: &Session,
    depth: usize,
    issue: &super::types::Issue,
    bullet: bool,
) {
    for _ in 0..depth {
        out.push_str("  ");
    }
    if bullet {
        out.push_str("- ");
    }
    let _ = write!(
        out,
        "[{}] {} — {} · ",
        issue.id,
        issue.title,
        issue.state.name()
    );
    for (i, part) in session.panelist_ids().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        match issue.stances.get(part) {
            Some(stance) => {
                let _ = write!(out, "{part}: {} ({})", stance.pos.name(), stance.basis);
            }
            None => {
                let _ = write!(out, "{part}: no stance");
            }
        }
    }
    out.push('\n');
}

fn write_panelist_list(out: &mut String, session: &Session) {
    for (i, part) in session.panelist_ids().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(part.as_str());
    }
}

fn section_topic(out: &mut String, heading: &str, session: &Session, round: RoundNo) {
    let _ = write!(out, "{heading}\nTopic: {}\nRound: {round}", session.topic);
    if let Some(max) = session.max_rounds {
        let _ = write!(out, " of {max}");
    }
    out.push_str(" (");
    match session.round(round).map(|r| &r.kind) {
        Some(RoundKind::Debate(id)) => match session.debate(id) {
            Some(debate) => {
                let _ = write!(out, "debate {id} on issue {}", debate.issue);
            }
            None => {
                let _ = write!(out, "debate {id}");
            }
        },
        _ => out.push_str("open discussion"),
    }
    out.push_str(")\nPanelists: ");
    write_panelist_list(out, session);
    let _ = write!(out, "\nModerator: {}\n\n", session.moderator.id);
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
        write_issue_line(out, session, depth, issue, true);
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
        out.push_str(what);
        out.push_str(": ");
        write_issue_line(out, session, 0, issue, false);
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
        write_message(out, session, message);
    }
    out.push('\n');
}

fn section_interventions(out: &mut String, heading: &str, messages: &[Message]) {
    out.push_str(heading);
    out.push('\n');
    let mut any = false;
    for message in messages.iter().filter(|m| m.kind == MsgKind::Intervention) {
        any = true;
        let _ = write!(out, "- [{}] to ", message.id);
        if message.to.is_empty() {
            out.push_str("everyone");
        } else {
            for (i, part) in message.to.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(part.as_str());
            }
        }
        out.push_str(": ");
        out.push_str(&message.body);
        out.push('\n');
    }
    if !any {
        out.push_str("(none)\n");
    }
    out.push('\n');
}

fn write_issue_ids(out: &mut String, session: &Session) {
    if session.issues.issues.is_empty() {
        out.push_str("(none yet)");
        return;
    }
    for (i, issue) in session.issues.issues.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(issue.id.as_str());
    }
}

fn finish_body(mut body: String) -> String {
    let trimmed = body.trim_end().len();
    body.truncate(trimmed);
    body.push('\n');
    body
}

fn estimate_messages_bytes(messages: &[Message]) -> usize {
    messages.iter().map(|m| m.body.len() + 96).sum()
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
    let cap = estimate_messages_bytes(messages) + session.issues.issues.len() * 128 + 2048;
    let mut body = String::with_capacity(cap);
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
        body: finish_body(body),
        reply_path: reply_path.to_owned(),
        receipt: Receipt::default(),
    }
}

fn write_panelist_tail(
    out: &mut String,
    session: &Session,
    to: &PartId,
    round: RoundNo,
    question: &str,
    reply_path: &Path,
) {
    out.push_str(PANELIST_SECTIONS[5]);
    out.push('\n');
    let debate = session
        .round(round)
        .and_then(|r| r.debate())
        .and_then(|id| session.debate(id));
    if let Some(debate) = debate {
        let _ = write!(out, "Your role in debate {}: ", debate.id);
        match debate.side(to) {
            Some(Side::Pro) => {
                let _ = writeln!(out, "you argue FOR issue {}.", debate.issue);
            }
            Some(Side::Con) => {
                let _ = writeln!(out, "you argue AGAINST issue {}.", debate.issue);
            }
            None => {
                let _ = writeln!(out, "you observe the debate on issue {}.", debate.issue);
            }
        }
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
    out.push_str(question);
    out.push_str("\n\n");

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
     Issue ids: ",
        reply_path.display(),
    );
    write_issue_ids(out, session);
    out.push('\n');
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
        |out, _| write_panelist_tail(out, session, to, round, question, reply_path),
    )
}

/// Builds panelist briefs for `round` together, sharing the identical topic,
/// issue tree, delta and message sections across panelists that saw the same
/// previous brief baseline.
pub fn panelist_briefs(
    session: &Session,
    round: RoundNo,
    items: &[(&PartId, &str, &Path)],
) -> Vec<Brief> {
    if items.is_empty() {
        return Vec::new();
    }
    if items.len() == 1 {
        let (to, question, reply_path) = items[0];
        return vec![panelist_brief(session, to, round, question, reply_path)];
    }

    let mut prefix = String::with_capacity(session.issues.issues.len() * 128 + 512);
    section_topic(&mut prefix, PANELIST_SECTIONS[0], session, round);
    section_tree(&mut prefix, PANELIST_SECTIONS[1], session);

    let through = session.messages.last().map(|m| m.id.clone());
    // Cache rendered sections 3..=5 per distinct `(base_rev, since)` key; in
    // normal rounds every panelist shares one entry.
    let mut middle_cache: Vec<(Option<u64>, Option<&super::types::MessageId>, String)> =
        Vec::with_capacity(1);
    let mut out = Vec::with_capacity(items.len());

    for &(to, question, reply_path) in items {
        let previous = session.previous_brief(to, round);
        let base_rev = previous.map(|b| b.base_rev);
        let since = previous.and_then(|b| b.through.as_ref());
        let mid_idx = match middle_cache
            .iter()
            .position(|(b, s, _)| *b == base_rev && *s == since)
        {
            Some(idx) => idx,
            None => {
                let messages = session.messages_after(since);
                let mut mid = String::with_capacity(estimate_messages_bytes(messages) + 512);
                section_delta(&mut mid, PANELIST_SECTIONS[2], session, base_rev);
                section_messages(&mut mid, PANELIST_SECTIONS[3], session, messages);
                section_interventions(&mut mid, PANELIST_SECTIONS[4], messages);
                middle_cache.push((base_rev, since, mid));
                middle_cache.len() - 1
            }
        };
        let mid = &middle_cache[mid_idx].2;
        let mut body = String::with_capacity(prefix.len() + mid.len() + question.len() + 1024);
        body.push_str(&prefix);
        body.push_str(mid);
        write_panelist_tail(&mut body, session, to, round, question, reply_path);
        out.push(Brief {
            to: to.clone(),
            base_rev: session.rev,
            since: since.cloned(),
            through: through.clone(),
            body: finish_body(body),
            reply_path: reply_path.to_owned(),
            receipt: Receipt::default(),
        });
    }
    out
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
                        && matches!(&m.author, Author::Part(id) if id == part)
                        && matches!(m.kind, MsgKind::Reply | MsgKind::Argument)
                });
                let receipt = this.and_then(|r| r.brief(part)).map(|b| &b.receipt);
                let _ = write!(out, "- {part}: ");
                match (reply, receipt.and_then(|r| r.failed.as_deref())) {
                    (Some(reply), _) => {
                        let _ = writeln!(out, "replied in {} (full text in section 4)", reply.id);
                    }
                    (None, Some(why)) => {
                        let _ = writeln!(out, "no reply: {why}");
                    }
                    (None, None) => {
                        out.push_str("no reply\n");
                    }
                }
            }
            let debate = this
                .and_then(|r| r.debate())
                .and_then(|id| session.debate(id));
            if let Some(debate) = debate {
                let exchanges = session
                    .rounds
                    .iter()
                    .filter(|r| {
                        r.no <= round
                            && matches!(&r.kind, RoundKind::Debate(id) if id == &debate.id)
                    })
                    .count();
                let _ = write!(out, "Debate {} on issue {}: pro ", debate.id, debate.issue);
                for (i, p) in debate.pro.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(p.as_str());
                }
                out.push_str(" · con ");
                for (i, p) in debate.con.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(p.as_str());
                }
                let _ = writeln!(out, " · exchange {exchanges} of {}.", debate.max_exchanges);
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
             - issue ids: 1-16 letters, digits, '-' or '_'; existing: ",
                reply_path.display(),
                optional,
                conclusion,
                required,
            );
            write_issue_ids(out, session);
            out.push_str(
                "\n\
             - states: open, contested, agreed, conditional, deferred\n\
             - stance pos: for, against, neutral, unknown; \"basis\" is a message id from section 4\n\
             - minutes sections: decisions, contested, open_questions, actions\n\
             - panelists (for questions, stances and debate sides): ",
            );
            write_panelist_list(out, session);
            let _ = write!(
                out,
                "\n\
             - a panelist with no question gets a default one\n\
             - {rule}\n\
             - conclusion: your answer to the topic, deciding it yourself where the panel \
             did not agree; it ends the discussion. Required in the last round, unless \
             \"round_limit\" raises the limit past it\n\
             - round_limit: a new round limit, only as section 6 allows; it replaces the \
             current one for the rest of the discussion\n\
             - close: true ends the discussion and needs a \"conclusion\"\n",
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

    #[test]
    fn brief_formatting_preserves_exact_sections_metadata_and_deltas() {
        let mut s = session();
        start(&mut s, 1, RoundKind::Open);
        reduce::apply(
            &mut s,
            &[
                Event::IssueChanged {
                    round: 1,
                    op: IssueOp::Add {
                        id: IssueId::new("A"),
                        parent: None,
                        title: "Storage".into(),
                    },
                },
                Event::IssueChanged {
                    round: 1,
                    op: IssueOp::Add {
                        id: IssueId::new("A1"),
                        parent: Some(IssueId::new("A")),
                        title: "Durability".into(),
                    },
                },
            ],
        )
        .unwrap();
        let claude_r1 = panelist_brief(&s, &part("claude"), 1, "Q1", Path::new("/r1/claude.md"));
        issue(&claude_r1, &mut s, 1);

        let reply = Message {
            id: s.next_message_id(0),
            round: 1,
            author: Author::Part(part("claude")),
            kind: MsgKind::Reply,
            issues: vec![IssueId::new("A"), IssueId::new("A1")],
            reply_to: vec![],
            to: vec![part("codex")],
            debate: None,
            body: "Line one\nLine two".into(),
            at: 1,
        };
        let intervention = Message {
            id: s.next_message_id(1),
            round: 1,
            author: Author::User,
            kind: MsgKind::Intervention,
            issues: vec![],
            reply_to: vec![MessageId::nth(1)],
            to: vec![part("claude"), part("codex")],
            debate: None,
            body: "Check latency".into(),
            at: 2,
        };
        reduce::apply(
            &mut s,
            &[
                Event::MessagePosted { message: reply },
                Event::MessagePosted {
                    message: intervention,
                },
                Event::IssueChanged {
                    round: 1,
                    op: IssueOp::SetStance {
                        id: IssueId::new("A"),
                        part: part("claude"),
                        pos: Position::For,
                        basis: MessageId::nth(1),
                    },
                },
            ],
        )
        .unwrap();

        let mod_r1 = moderator_brief(&s, 1, Path::new("/r1/mod.json"));
        assert!(mod_r1.body.ends_with('\n'));
        assert!(!mod_r1.body.ends_with("\n\n"));
        assert!(mod_r1.body.contains(
            "### [m1] R1 · claude · reply · issues: A, A1 · to: codex\nLine one\nLine two\n"
        ));
        assert!(mod_r1.body.contains(
            "### [m2] R1 · user · intervention · ↪ m1 · to: claude, codex\nCheck latency\n"
        ));
        assert!(
            mod_r1
                .body
                .contains("- [m2] to claude, codex: Check latency\n")
        );
        assert!(
            mod_r1
                .body
                .contains("- claude: replied in m1 (full text in section 4)\n")
        );
        assert!(mod_r1.body.contains("- codex: no reply\n"));
        issue(&mod_r1, &mut s, 1);

        start(&mut s, 2, RoundKind::Open);
        let claude_r2 = panelist_brief(&s, &part("claude"), 2, "Q2", Path::new("/r2/claude.md"));
        let codex_r2 = panelist_brief(&s, &part("codex"), 2, "Q2x", Path::new("/r2/codex.md"));
        assert!(
            claude_r2
                .body
                .contains("changed: [A] Storage — open · claude: for (m1), codex: no stance\n")
        );
        assert!(
            codex_r2
                .body
                .contains("(this is your first brief: everything in it is new to you)")
        );
        assert!(claude_r2.body.ends_with('\n'));
        assert!(!claude_r2.body.ends_with("\n\n"));

        // Batch `panelist_briefs` must match per-panelist `panelist_brief`
        // byte-for-byte both when panelists have divergent baselines (r2 here:
        // claude had an r1 brief, codex did not) and when they share one.
        let p_claude = part("claude");
        let p_codex = part("codex");
        let batch_divergent = panelist_briefs(
            &s,
            2,
            &[
                (&p_claude, "Q2", Path::new("/r2/claude.md")),
                (&p_codex, "Q2x", Path::new("/r2/codex.md")),
            ],
        );
        assert_eq!(batch_divergent, vec![claude_r2.clone(), codex_r2.clone()]);

        issue(&claude_r2, &mut s, 2);
        issue(&codex_r2, &mut s, 2);
        post(
            &mut s,
            2,
            Author::Part(part("codex")),
            MsgKind::Reply,
            "Round 2 body",
        );
        start(&mut s, 3, RoundKind::Open);
        let expected_shared = vec![
            panelist_brief(&s, &p_claude, 3, "Q3a", Path::new("/r3/claude.md")),
            panelist_brief(&s, &p_codex, 3, "Q3b", Path::new("/r3/codex.md")),
        ];
        let batch_shared = panelist_briefs(
            &s,
            3,
            &[
                (&p_claude, "Q3a", Path::new("/r3/claude.md")),
                (&p_codex, "Q3b", Path::new("/r3/codex.md")),
            ],
        );
        assert_eq!(batch_shared, expected_shared);
    }
}
