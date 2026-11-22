//! The event log and its fold: a session is `create` of the first batch,
//! then `apply` of every later one. Each batch bumps `rev` once.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::types::{
    Brief, Debate, DebateId, DebateStatus, Issue, IssueId, IssueOp, IssueState, Message, Millis,
    MinuteItem, MinuteSection, ModeratorOutput, MsgKind, PartId, Participant, Phase, Receipt,
    Round, RoundKind, RoundNo, Session, SessionId, Stance, Verdict,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Always the first event of the log, and only there.
    Created {
        id: SessionId,
        topic: String,
        /// Absent in logs written before sessions could name a directory.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workdir: Option<std::path::PathBuf>,
        /// Absent in logs written before discussions had a round limit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_rounds: Option<RoundNo>,
        moderator: Participant,
        panelists: Vec<Participant>,
        at: Millis,
    },
    PaneAssigned {
        part: PartId,
        pane_id: u64,
    },
    RoundStarted {
        no: RoundNo,
        kind: RoundKind,
        at: Millis,
    },
    MessagePosted {
        message: Message,
    },
    /// A brief for a panelist, or the moderator's summarize brief.
    BriefIssued {
        round: RoundNo,
        brief: Brief,
    },
    BriefSent {
        round: RoundNo,
        to: PartId,
        at: Millis,
    },
    BriefStarted {
        round: RoundNo,
        to: PartId,
        at: Millis,
    },
    /// Delivery is tried again: the brief counts as unsent.
    BriefRetried {
        round: RoundNo,
        to: PartId,
        reason: String,
    },
    BriefFailed {
        round: RoundNo,
        to: PartId,
        reason: String,
    },
    /// The user asked to try a failed brief again from the start.
    BriefReset {
        round: RoundNo,
        to: PartId,
    },
    /// A reply file was turned down; the agent is asked to fix it.
    ReplyRejected {
        round: RoundNo,
        to: PartId,
        error: String,
    },
    ReplyAccepted {
        round: RoundNo,
        to: PartId,
        at: Millis,
    },
    PhaseChanged {
        round: RoundNo,
        phase: Phase,
    },
    /// A change a panelist's reply header declared (its stances).
    IssueChanged {
        round: RoundNo,
        op: IssueOp,
    },
    /// The moderator's validated output: its issue and minutes operations
    /// apply here; the summary, debate and close follow as their own events.
    ModeratorAccepted {
        round: RoundNo,
        output: ModeratorOutput,
    },
    DebateProposed {
        issue: IssueId,
        pro: Vec<PartId>,
        con: Vec<PartId>,
        max_exchanges: u8,
        status: DebateStatus,
    },
    DebateApproved {
        id: DebateId,
        pro: Vec<PartId>,
        con: Vec<PartId>,
    },
    DebateDeclined {
        id: DebateId,
    },
    DebateVerdict {
        round: RoundNo,
        id: DebateId,
        verdict: Verdict,
    },
    SessionClosed,
}

/// The log does not fold: an event names something that is not there, or
/// breaks the order the engine writes in. Only a damaged or hand-edited log
/// gets here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReduceError(pub String);

impl fmt::Display for ReduceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReduceError {}

fn err<T>(message: impl Into<String>) -> Result<T, ReduceError> {
    Err(ReduceError(message.into()))
}

/// The session the first batch creates; it must start with `Created`.
pub fn create(batch: &[Event]) -> Result<Session, ReduceError> {
    let Some((
        Event::Created {
            id,
            topic,
            workdir,
            max_rounds,
            moderator,
            panelists,
            at,
        },
        rest,
    )) = batch.split_first()
    else {
        return err("the log does not start with a created event");
    };
    let mut session = Session {
        id: id.clone(),
        topic: topic.clone(),
        workdir: workdir.clone(),
        max_rounds: *max_rounds,
        user_max_rounds: *max_rounds,
        rev: 1,
        created_at: *at,
        moderator: moderator.clone(),
        panelists: panelists.clone(),
        issues: Default::default(),
        minutes: Default::default(),
        messages: Vec::new(),
        rounds: Vec::new(),
        debates: Vec::new(),
        closed: false,
    };
    for event in rest {
        apply_one(&mut session, event)?;
    }
    Ok(session)
}

/// Applies one batch and bumps `rev`. On an error the session is left part
/// way through the batch and must be dropped.
pub fn apply(session: &mut Session, batch: &[Event]) -> Result<(), ReduceError> {
    session.rev += 1;
    for event in batch {
        apply_one(session, event)?;
    }
    Ok(())
}

/// Folds a whole log.
pub fn replay<I>(batches: I) -> Result<Session, ReduceError>
where
    I: IntoIterator,
    I::Item: AsRef<[Event]>,
{
    let mut batches = batches.into_iter();
    let Some(first) = batches.next() else {
        return err("the log is empty");
    };
    let mut session = create(first.as_ref())?;
    for batch in batches {
        apply(&mut session, batch.as_ref())?;
    }
    Ok(session)
}

fn round_mut(session: &mut Session, no: RoundNo) -> Result<&mut Round, ReduceError> {
    match session.round_mut(no) {
        Some(round) => Ok(round),
        None => err(format!("no round {no}")),
    }
}

fn receipt_mut<'a>(
    session: &'a mut Session,
    round: RoundNo,
    to: &PartId,
) -> Result<&'a mut Receipt, ReduceError> {
    match round_mut(session, round)?.brief_mut(to) {
        Some(brief) => Ok(&mut brief.receipt),
        None => err(format!("no brief to {to} in round {round}")),
    }
}

fn apply_one(session: &mut Session, event: &Event) -> Result<(), ReduceError> {
    match event {
        Event::Created { .. } => return err("a second created event"),
        Event::PaneAssigned { part, pane_id } => {
            let slot = std::iter::once(&mut session.moderator)
                .chain(&mut session.panelists)
                .find(|p| &p.id == part);
            match slot {
                Some(participant) => participant.pane_id = Some(*pane_id),
                None => return err(format!("no participant {part}")),
            }
        }
        Event::RoundStarted { no, kind, at } => {
            let expected = session.rounds.len() as RoundNo + 1;
            if *no != expected {
                return err(format!("round {no} started, expected round {expected}"));
            }
            if let RoundKind::Debate(id) = kind {
                match session.debate_mut(id) {
                    Some(debate) => debate.status = DebateStatus::Active,
                    None => return err(format!("no debate {id}")),
                }
            }
            session.rounds.push(Round {
                no: *no,
                kind: kind.clone(),
                phase: Phase::Briefing,
                started_at: *at,
                briefs: Vec::new(),
                moderator_brief: None,
                outcome: None,
            });
        }
        Event::MessagePosted { message } => {
            let expected = session.next_message_id(0);
            if message.id != expected {
                return err(format!(
                    "message {} posted, expected {expected}",
                    message.id
                ));
            }
            if let (MsgKind::Argument, Some(id)) = (message.kind, &message.debate) {
                match session.debate_mut(id) {
                    Some(debate) => debate.turns.push(message.id.clone()),
                    None => return err(format!("no debate {id}")),
                }
            }
            session.messages.push(message.clone());
        }
        Event::BriefIssued { round, brief } => {
            let to_moderator = brief.to == session.moderator.id;
            if !to_moderator && !session.is_panelist(&brief.to) {
                return err(format!("brief to unknown participant {}", brief.to));
            }
            let round = round_mut(session, *round)?;
            if round.brief(&brief.to).is_some() {
                return err(format!(
                    "a second brief to {} in round {}",
                    brief.to, round.no
                ));
            }
            if to_moderator {
                round.moderator_brief = Some(brief.clone());
            } else {
                round.briefs.push(brief.clone());
            }
        }
        Event::BriefSent { round, to, at } => {
            receipt_mut(session, *round, to)?.sent = Some(*at);
        }
        Event::BriefStarted { round, to, at } => {
            let receipt = receipt_mut(session, *round, to)?;
            receipt.started.get_or_insert(*at);
        }
        Event::BriefRetried { round, to, .. } => {
            let receipt = receipt_mut(session, *round, to)?;
            receipt.retries = receipt.retries.saturating_add(1);
            receipt.sent = None;
            receipt.started = None;
            // A resend may reach a new agent (its pane was reopened): it
            // gets the whole brief, not a correction to a reply it never
            // wrote.
            receipt.rejected = None;
        }
        Event::BriefFailed { round, to, reason } => {
            receipt_mut(session, *round, to)?.failed = Some(reason.clone());
        }
        Event::BriefReset { round, to } => {
            let receipt = receipt_mut(session, *round, to)?;
            *receipt = Receipt {
                replied: receipt.replied,
                ..Receipt::default()
            };
        }
        Event::ReplyRejected { round, to, error } => {
            let receipt = receipt_mut(session, *round, to)?;
            receipt.retries = receipt.retries.saturating_add(1);
            receipt.rejected = Some(error.clone());
            // The correction is a new send: until it goes out and the agent
            // starts on it, the brief is neither sent nor started.
            receipt.sent = None;
            receipt.started = None;
        }
        Event::ReplyAccepted { round, to, at } => {
            let receipt = receipt_mut(session, *round, to)?;
            // A reply proves the brief arrived, even when the send was never
            // confirmed (the link dropped right after the Enter).
            receipt.sent.get_or_insert(*at);
            receipt.started.get_or_insert(*at);
            receipt.replied = Some(*at);
            receipt.rejected = None;
        }
        Event::PhaseChanged { round, phase } => {
            round_mut(session, *round)?.phase = *phase;
        }
        Event::IssueChanged { round, op } => apply_issue_op(session, *round, op)?,
        Event::ModeratorAccepted { round, output } => {
            for op in &output.issue_ops {
                apply_issue_op(session, *round, op)?;
            }
            let rev = session.rev;
            for op in &output.minutes_ops {
                session.minutes.section_mut(op.section).push(MinuteItem {
                    text: op.text.clone(),
                    issues: op.issues.clone(),
                    since: *round,
                    rev,
                });
            }
            if let Some(limit) = output.round_limit {
                session.max_rounds = Some(limit);
            }
            round_mut(session, *round)?.outcome = Some(output.clone());
        }
        Event::DebateProposed {
            issue,
            pro,
            con,
            max_exchanges,
            status,
        } => {
            if !session.issues.contains(issue) {
                return err(format!("debate on unknown issue {issue}"));
            }
            let id = DebateId::nth(session.debates.len() + 1);
            session.debates.push(Debate {
                id,
                issue: issue.clone(),
                pro: pro.clone(),
                con: con.clone(),
                max_exchanges: *max_exchanges,
                status: *status,
                turns: Vec::new(),
                verdict: None,
            });
        }
        Event::DebateApproved { id, pro, con } => {
            let Some(debate) = session.debate_mut(id) else {
                return err(format!("no debate {id}"));
            };
            debate.pro = pro.clone();
            debate.con = con.clone();
            debate.status = DebateStatus::Approved;
        }
        Event::DebateDeclined { id } => match session.debate_mut(id) {
            Some(debate) => debate.status = DebateStatus::Declined,
            None => return err(format!("no debate {id}")),
        },
        Event::DebateVerdict { round, id, verdict } => {
            let rev = session.rev;
            let Some(debate) = session.debate_mut(id) else {
                return err(format!("no debate {id}"));
            };
            debate.verdict = Some(verdict.clone());
            debate.status = DebateStatus::Closed;
            let issue_id = debate.issue.clone();
            let Some(issue) = session.issues.get_mut(&issue_id) else {
                return err(format!("debate {id} on unknown issue {issue_id}"));
            };
            issue.record(*round, verdict.outcome);
            issue.changed_rev = rev;
            let section = match verdict.outcome {
                IssueState::Deferred => MinuteSection::OpenQuestions,
                _ => MinuteSection::Decisions,
            };
            let text = format!(
                "Debate on {issue_id} ({}): {} — {}",
                issue.title,
                verdict.outcome.name(),
                verdict.rationale
            );
            session.minutes.section_mut(section).push(MinuteItem {
                text,
                issues: vec![issue_id],
                since: *round,
                rev,
            });
        }
        Event::SessionClosed => session.closed = true,
    }
    Ok(())
}

fn apply_issue_op(session: &mut Session, round: RoundNo, op: &IssueOp) -> Result<(), ReduceError> {
    let rev = session.rev;
    match op {
        IssueOp::Add { id, parent, title } => {
            if session.issues.contains(id) {
                return err(format!("issue {id} added twice"));
            }
            if let Some(parent) = parent
                && !session.issues.contains(parent)
            {
                return err(format!("issue {id} under unknown parent {parent}"));
            }
            session.issues.issues.push(Issue {
                id: id.clone(),
                parent: parent.clone(),
                title: title.clone(),
                state: IssueState::Open,
                stances: Default::default(),
                opened_in: round,
                added_rev: rev,
                changed_rev: rev,
                history: vec![(round, IssueState::Open)],
            });
        }
        IssueOp::SetState { id, state } => {
            let Some(issue) = session.issues.get_mut(id) else {
                return err(format!("no issue {id}"));
            };
            issue.record(round, *state);
            issue.changed_rev = rev;
        }
        IssueOp::SetStance {
            id,
            part,
            pos,
            basis,
        } => {
            let Some(issue) = session.issues.get_mut(id) else {
                return err(format!("no issue {id}"));
            };
            issue.stances.insert(
                part.clone(),
                Stance {
                    pos: *pos,
                    basis: basis.clone(),
                },
            );
            issue.changed_rev = rev;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::{
        AgentKind, Author, IssueId, IssueOp, IssueState, MessageId, Position, Role, RoundKind,
    };

    #[test]
    fn a_log_written_before_workdirs_reads_as_none() {
        let line = r#"{"event":"created","id":"disc-1","topic":"t","moderator":{"id":"noa","kind":"claude","role":"moderator","pane_id":null},"panelists":[],"at":1}"#;
        let event: Event = serde_json::from_str(line).unwrap();
        let session = create(&[event]).unwrap();
        assert_eq!(session.workdir, None);
        let back = serde_json::to_string(&Event::Created {
            id: SessionId::new("disc-1"),
            topic: "t".into(),
            workdir: None,
            max_rounds: None,
            moderator: session.moderator.clone(),
            panelists: Vec::new(),
            at: 1,
        })
        .unwrap();
        assert!(!back.contains("workdir"), "{back}");
    }

    fn part(id: &str, kind: AgentKind, role: Role) -> Participant {
        Participant {
            id: PartId::new(id),
            kind,
            role,
            pane_id: None,
        }
    }

    fn created() -> Vec<Event> {
        vec![Event::Created {
            id: SessionId::new("s"),
            topic: "t".into(),
            workdir: None,
            max_rounds: None,
            moderator: part("noa", AgentKind::Claude, Role::Moderator),
            panelists: vec![part("claude", AgentKind::Claude, Role::Panelist)],
            at: 5,
        }]
    }

    fn message(seq: usize) -> Message {
        Message {
            id: MessageId::nth(seq),
            round: 1,
            author: Author::User,
            kind: MsgKind::Intervention,
            issues: vec![],
            reply_to: vec![],
            to: vec![],
            debate: None,
            body: format!("body {seq}"),
            at: 0,
        }
    }

    #[test]
    fn an_issue_keeps_the_rounds_its_state_changed_in() {
        let mut session = create(&created()).unwrap();
        let round = |no| Event::RoundStarted {
            no,
            kind: RoundKind::Open,
            at: 0,
        };
        let set = |round, state| Event::IssueChanged {
            round,
            op: IssueOp::SetState {
                id: IssueId::new("A"),
                state,
            },
        };
        apply(
            &mut session,
            &[
                round(1),
                Event::IssueChanged {
                    round: 1,
                    op: IssueOp::Add {
                        id: IssueId::new("A"),
                        parent: None,
                        title: "engine".into(),
                    },
                },
                round(2),
                set(2, IssueState::Contested),
                round(3),
                // The same state again is not a change.
                set(3, IssueState::Contested),
                round(4),
                set(4, IssueState::Agreed),
            ],
        )
        .unwrap();
        assert_eq!(
            session.issues.get(&IssueId::new("A")).unwrap().history,
            vec![
                (1, IssueState::Open),
                (2, IssueState::Contested),
                (4, IssueState::Agreed)
            ]
        );
    }

    #[test]
    fn rev_bumps_once_per_batch() {
        let mut session = create(&created()).unwrap();
        assert_eq!(session.rev, 1);
        apply(
            &mut session,
            &[
                Event::MessagePosted {
                    message: message(1),
                },
                Event::MessagePosted {
                    message: message(2),
                },
            ],
        )
        .unwrap();
        assert_eq!(session.rev, 2);
        assert_eq!(session.messages.len(), 2);
    }

    #[test]
    fn the_log_must_start_with_created() {
        assert!(create(&[Event::SessionClosed]).is_err());
        let mut session = create(&created()).unwrap();
        assert!(apply(&mut session, &created()).is_err());
    }

    #[test]
    fn messages_must_arrive_in_id_order() {
        let mut session = create(&created()).unwrap();
        let error = apply(
            &mut session,
            &[Event::MessagePosted {
                message: message(2),
            }],
        )
        .unwrap_err();
        assert!(error.0.contains("expected m1"), "{error}");
    }

    #[test]
    fn issue_ops_stamp_the_rev_they_happen_in() {
        let mut session = create(&created()).unwrap();
        apply(
            &mut session,
            &[Event::MessagePosted {
                message: message(1),
            }],
        )
        .unwrap();
        apply(
            &mut session,
            &[Event::IssueChanged {
                round: 1,
                op: IssueOp::Add {
                    id: IssueId::new("A"),
                    parent: None,
                    title: "a".into(),
                },
            }],
        )
        .unwrap();
        apply(
            &mut session,
            &[Event::IssueChanged {
                round: 1,
                op: IssueOp::SetStance {
                    id: IssueId::new("A"),
                    part: PartId::new("claude"),
                    pos: Position::For,
                    basis: MessageId::nth(1),
                },
            }],
        )
        .unwrap();
        let issue = session.issues.get(&IssueId::new("A")).unwrap();
        assert_eq!((issue.added_rev, issue.changed_rev), (3, 4));
        assert_eq!(issue.stances[&PartId::new("claude")].pos, Position::For);
        assert_eq!(issue.state, IssueState::Open);
    }

    #[test]
    fn replay_equals_stepwise_apply() {
        let batches = vec![
            created(),
            vec![Event::RoundStarted {
                no: 1,
                kind: RoundKind::Open,
                at: 9,
            }],
            vec![Event::MessagePosted {
                message: message(1),
            }],
        ];
        let replayed = replay(&batches).unwrap();
        let mut stepped = create(&batches[0]).unwrap();
        apply(&mut stepped, &batches[1]).unwrap();
        apply(&mut stepped, &batches[2]).unwrap();
        assert_eq!(replayed, stepped);
        assert!(replay(Vec::<Vec<Event>>::new()).is_err());
    }

    #[test]
    fn rounds_must_be_consecutive() {
        let mut session = create(&created()).unwrap();
        let error = apply(
            &mut session,
            &[Event::RoundStarted {
                no: 2,
                kind: RoundKind::Open,
                at: 0,
            }],
        )
        .unwrap_err();
        assert!(error.0.contains("expected round 1"), "{error}");
    }
}
