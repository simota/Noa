//! The event log and its fold: a session is `create` of the first batch,
//! then `apply` of every later one. Each batch bumps `rev` once.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::types::{
    AmendNote, Brief, JudgeOutput, Message, Millis, PartId, Participant, Phase, Receipt, Round,
    RoundKind, RoundNo, Session, SessionId, Submission,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Always the first event of the log, and only there.
    Created {
        id: SessionId,
        request: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workdir: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_rounds: Option<RoundNo>,
        reveal_interval: RoundNo,
        judge: Participant,
        contestants: Vec<Participant>,
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
    BriefReset {
        round: RoundNo,
        to: PartId,
    },
    ReplyRejected {
        round: RoundNo,
        to: PartId,
        error: String,
        /// [`crate::store::digest`] of the turned-down reply, so a copy
        /// left on disk by a crash is known for what it is. Absent in logs
        /// written before it was recorded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<u64>,
    },
    ReplyAccepted {
        round: RoundNo,
        to: PartId,
        at: Millis,
    },
    SubmissionRecorded {
        round: RoundNo,
        submission: Submission,
    },
    PhaseChanged {
        round: RoundNo,
        phase: Phase,
    },
    JudgeAccepted {
        round: RoundNo,
        output: JudgeOutput,
    },
    WinnerSelected {
        winner: PartId,
    },
    /// The user holds the competition after each judged round (`true`), or
    /// lets rounds follow each other again.
    HoldSet {
        #[serde(default)]
        hold: bool,
    },
    /// The user raised the round limit to `max_rounds`. A competition that
    /// ended at its limit is open again, its winner undecided until the
    /// new final round (the earlier verdict stays in its round).
    RoundsExtended {
        max_rounds: RoundNo,
        #[serde(default)]
        at: Millis,
    },
    SessionClosed,
}

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

pub fn create(batch: &[Event]) -> Result<Session, ReduceError> {
    let Some((
        Event::Created {
            id,
            request,
            workdir,
            max_rounds,
            reveal_interval,
            judge,
            contestants,
            at,
        },
        rest,
    )) = batch.split_first()
    else {
        return err("the log does not start with a created event");
    };
    let mut session = Session {
        id: id.clone(),
        request: request.clone(),
        workdir: workdir.clone(),
        max_rounds: *max_rounds,
        reveal_interval: *reveal_interval,
        rev: 1,
        created_at: *at,
        judge: judge.clone(),
        contestants: contestants.clone(),
        messages: Vec::new(),
        rounds: Vec::new(),
        winner: None,
        closed: false,
        hold: false,
    };
    for event in rest {
        apply_one(&mut session, event)?;
    }
    Ok(session)
}

pub fn apply(session: &mut Session, batch: &[Event]) -> Result<(), ReduceError> {
    session.rev += 1;
    for event in batch {
        apply_one(session, event)?;
    }
    Ok(())
}

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
            let slot = std::iter::once(&mut session.judge)
                .chain(&mut session.contestants)
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
            session.rounds.push(Round {
                no: *no,
                kind: *kind,
                phase: Phase::Briefing,
                started_at: *at,
                briefs: Vec::new(),
                submissions: BTreeMap::new(),
                judge_brief: None,
                outcome: None,
            });
        }
        Event::MessagePosted { message } => {
            session.messages.push(message.clone());
        }
        Event::BriefIssued { round, brief } => {
            let is_judge = brief.to == session.judge.id;
            let r = round_mut(session, *round)?;
            if is_judge {
                r.judge_brief = Some(brief.clone());
            } else {
                r.briefs.push(brief.clone());
            }
        }
        Event::BriefSent { round, to, at } => {
            let receipt = receipt_mut(session, *round, to)?;
            receipt.sent = Some(*at);
            receipt.failed = None;
        }
        Event::BriefStarted { round, to, at } => {
            let receipt = receipt_mut(session, *round, to)?;
            if receipt.started.is_none() {
                receipt.started = Some(*at);
            }
        }
        Event::BriefRetried { round, to, .. } => {
            let receipt = receipt_mut(session, *round, to)?;
            receipt.retries = receipt.retries.saturating_add(1);
            receipt.sent = None;
            receipt.started = None;
            receipt.failed = None;
            // A retry sends the whole brief: the agent may be a new one that
            // never saw the output a correction would refer to.
            receipt.rejected = None;
            receipt.rejected_digest = None;
        }
        Event::BriefFailed { round, to, reason } => {
            let receipt = receipt_mut(session, *round, to)?;
            receipt.failed = Some(reason.clone());
        }
        Event::BriefReset { round, to } => {
            let receipt = receipt_mut(session, *round, to)?;
            receipt.sent = None;
            receipt.started = None;
            receipt.retries = 0;
            receipt.failed = None;
            receipt.rejected = None;
            receipt.rejected_digest = None;
        }
        Event::ReplyRejected {
            round,
            to,
            error,
            digest,
        } => {
            let receipt = receipt_mut(session, *round, to)?;
            receipt.retries = receipt.retries.saturating_add(1);
            receipt.sent = None;
            receipt.started = None;
            receipt.rejected = Some(error.clone());
            receipt.rejected_digest = *digest;
        }
        Event::ReplyAccepted { round, to, at } => {
            let receipt = receipt_mut(session, *round, to)?;
            receipt.replied = Some(*at);
            // A reply proves the brief arrived, even when the `Sent` was lost
            // with the link.
            receipt.sent.get_or_insert(*at);
            receipt.rejected = None;
            receipt.rejected_digest = None;
            receipt.failed = None;
        }
        Event::SubmissionRecorded { round, submission } => {
            let r = round_mut(session, *round)?;
            r.submissions
                .insert(submission.part.clone(), submission.clone());
        }
        Event::PhaseChanged { round, phase } => {
            round_mut(session, *round)?.phase = *phase;
        }
        Event::JudgeAccepted { round, output } => {
            for amendment in &output.amendments {
                if amendment.round >= *round {
                    return err(format!(
                        "round {round} cannot amend round {}",
                        amendment.round
                    ));
                }
                let ev = round_mut(session, amendment.round)?
                    .outcome
                    .as_mut()
                    .and_then(|o| o.evaluations.get_mut(&amendment.part));
                let Some(ev) = ev else {
                    return err(format!(
                        "no R{} evaluation of {} to amend",
                        amendment.round, amendment.part
                    ));
                };
                let from = ev.score;
                ev.score = amendment.score;
                // The split no longer adds up to the corrected score.
                ev.breakdown.clear();
                ev.amended = Some(AmendNote {
                    from,
                    by_round: *round,
                    reason: amendment.reason.clone(),
                });
            }
            // A corrected round is re-ranked by its scores (ties keep the
            // judge's order), so its standings never show a negative gap.
            let mut amended: Vec<RoundNo> = output.amendments.iter().map(|a| a.round).collect();
            amended.dedup();
            for no in amended {
                if let Some(outcome) = round_mut(session, no)?.outcome.as_mut() {
                    let evaluations = &outcome.evaluations;
                    outcome.ranking.sort_by(|a, b| {
                        let score = |p| {
                            evaluations
                                .get(p)
                                .map_or(0, |e: &super::types::EntryEvaluation| e.score)
                        };
                        score(b).cmp(&score(a))
                    });
                }
            }
            if let Some(winner) = &output.winner {
                session.winner = Some(winner.clone());
            }
            round_mut(session, *round)?.outcome = Some(output.clone());
        }
        Event::WinnerSelected { winner } => {
            if !session.is_contestant(winner) {
                return err(format!("unknown contestant {winner}"));
            }
            session.winner = Some(winner.clone());
        }
        Event::HoldSet { hold } => session.hold = *hold,
        Event::RoundsExtended { max_rounds, .. } => {
            if (*max_rounds as usize) <= session.rounds.len() {
                return err(format!(
                    "round limit {max_rounds} leaves no round after round {}",
                    session.rounds.len()
                ));
            }
            session.max_rounds = Some(*max_rounds);
            if session.closed {
                session.closed = false;
                session.winner = None;
            }
        }
        Event::SessionClosed => {
            session.closed = true;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Config, Engine, Setup};
    use crate::model::types::{AgentKind, SessionId};

    fn created() -> Vec<Event> {
        let setup = Setup {
            request: "x",
            max_rounds: Some(1),
            reveal_interval: 5,
            workdir: None,
            judge: AgentKind::Agy.into(),
            panel: &[AgentKind::Claude.into(), AgentKind::Codex.into()],
        };
        let (_, out) = Engine::create_in(
            SessionId::new("comp-test"),
            setup,
            std::path::Path::new("/tmp/comp-test"),
            Config::default(),
            1,
        )
        .unwrap();
        out.batches[0].clone()
    }

    #[test]
    fn round_control_events_read_back_and_default_their_fields() {
        for event in [
            Event::HoldSet { hold: true },
            Event::RoundsExtended {
                max_rounds: 3,
                at: 9,
            },
        ] {
            let line = serde_json::to_string(&event).unwrap();
            assert_eq!(serde_json::from_str::<Event>(&line).unwrap(), event);
        }
        let bare: Event = serde_json::from_str(r#"{"event":"hold_set"}"#).unwrap();
        assert_eq!(bare, Event::HoldSet { hold: false });
        let bare: Event =
            serde_json::from_str(r#"{"event":"rounds_extended","max_rounds":2}"#).unwrap();
        assert_eq!(
            bare,
            Event::RoundsExtended {
                max_rounds: 2,
                at: 0
            }
        );
        // A log from before them folds with no hold.
        assert!(!create(&created()).unwrap().hold);
    }

    #[test]
    fn an_extension_reopens_a_finished_competition_without_its_winner() {
        let mut session = create(&created()).unwrap();
        apply(
            &mut session,
            &[Event::RoundStarted {
                no: 1,
                kind: RoundKind::Blind,
                at: 2,
            }],
        )
        .unwrap();
        // No round left after round 1 with a limit of 1.
        assert!(
            apply(
                &mut session,
                &[Event::RoundsExtended {
                    max_rounds: 1,
                    at: 3
                }]
            )
            .is_err()
        );
        apply(
            &mut session,
            &[
                Event::WinnerSelected {
                    winner: PartId::new("codex"),
                },
                Event::SessionClosed,
                Event::HoldSet { hold: true },
                Event::RoundsExtended {
                    max_rounds: 2,
                    at: 4,
                },
            ],
        )
        .unwrap();
        assert!(!session.closed && session.hold);
        assert_eq!(session.winner, None);
        assert_eq!(session.max_rounds, Some(2));
    }
}
