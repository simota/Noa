//! The event log and its fold: a session is `create` of the first batch,
//! then `apply` of every later one. Each batch bumps `rev` once.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::aggregate;
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
        /// The judges after the lead; absent from a single-judge log.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        co_judges: Vec<Participant>,
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
    /// In a panel: one judge's accepted scorecard for the round.
    ScorecardAccepted {
        round: RoundNo,
        judge: PartId,
        output: JudgeOutput,
    },
    /// The round's judgment: the judge's output, or in a panel the
    /// combined judgment of every scorecard.
    JudgeAccepted {
        round: RoundNo,
        output: JudgeOutput,
    },
    /// In a panel's deciding round: the lead is asked for the verdict.
    VerdictRequested {
        round: RoundNo,
        brief: Brief,
    },
    /// The lead's verdict: the winner, among the highest means.
    VerdictAccepted {
        round: RoundNo,
        winner: PartId,
        final_verdict: String,
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
            co_judges,
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
        co_judges: co_judges.clone(),
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

/// A panel round's `JudgeAccepted`: only with every judge's scorecard in.
/// Each scorecard's amendments correct that judge's own earlier card, and
/// each corrected round is combined again; then `output` is the outcome.
fn accept_combined(
    session: &mut Session,
    round: RoundNo,
    output: &JudgeOutput,
) -> Result<(), ReduceError> {
    let r = round_mut(session, round)?;
    let cards = r.scorecards.clone();
    let missing = aggregate::missing_scorecards(session, &cards);
    if !missing.is_empty() {
        let missing: Vec<&str> = missing.iter().map(PartId::as_str).collect();
        return err(format!(
            "round {round} judged without the scorecard of {}",
            missing.join(", ")
        ));
    }
    // Per corrected round, per entry: "<judge>: <reason>" in seat order.
    let mut reasons: BTreeMap<RoundNo, BTreeMap<PartId, Vec<String>>> = BTreeMap::new();
    let judges: Vec<PartId> = session.judges().map(|j| j.id.clone()).collect();
    for judge in &judges {
        for amendment in &cards[judge].amendments {
            if amendment.round >= round {
                return err(format!(
                    "round {round} cannot amend round {}",
                    amendment.round
                ));
            }
            let card = round_mut(session, amendment.round)?
                .scorecards
                .get_mut(judge);
            let Some(card) = card else {
                return err(format!(
                    "no R{} scorecard of {judge} to amend",
                    amendment.round
                ));
            };
            let Some(ev) = card.evaluations.get_mut(&amendment.part) else {
                return err(format!(
                    "no R{} evaluation of {} by {judge} to amend",
                    amendment.round, amendment.part
                ));
            };
            let from = ev.score;
            ev.score = amendment.score;
            ev.breakdown.clear();
            ev.amended = Some(AmendNote {
                from,
                by_round: round,
                reason: amendment.reason.clone(),
            });
            // The card is re-ranked by its scores, ties in its own order.
            let evaluations = &card.evaluations;
            card.ranking
                .sort_by_key(|p| std::cmp::Reverse(evaluations.get(p).map_or(0, |e| e.score)));
            reasons
                .entry(amendment.round)
                .or_default()
                .entry(amendment.part.clone())
                .or_default()
                .push(format!("{judge}: {}", amendment.reason));
        }
    }
    for (no, reasons) in &reasons {
        let corrected = session.round(*no).map(|r| &r.scorecards);
        if corrected.is_none_or(|cards| !aggregate::missing_scorecards(session, cards).is_empty()) {
            return err(format!(
                "round {no} has no full set of scorecards to recombine"
            ));
        }
        aggregate::recombine(session, *no, round, reasons);
    }
    if let Some(winner) = &output.winner {
        session.winner = Some(winner.clone());
    }
    round_mut(session, round)?.outcome = Some(output.clone());
    Ok(())
}

fn apply_one(session: &mut Session, event: &Event) -> Result<(), ReduceError> {
    match event {
        Event::Created { .. } => return err("a second created event"),
        Event::PaneAssigned { part, pane_id } => {
            let slot = std::iter::once(&mut session.judge)
                .chain(&mut session.co_judges)
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
                co_judge_briefs: Vec::new(),
                scorecards: BTreeMap::new(),
                verdict_brief: None,
            });
        }
        Event::MessagePosted { message } => {
            session.messages.push(message.clone());
        }
        Event::BriefIssued { round, brief } => {
            let is_lead = brief.to == session.judge.id;
            let is_co_judge = !is_lead && session.is_judge(&brief.to);
            let r = round_mut(session, *round)?;
            if is_lead {
                r.judge_brief = Some(brief.clone());
            } else if is_co_judge {
                r.co_judge_briefs.push(brief.clone());
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
        Event::ScorecardAccepted {
            round,
            judge,
            output,
        } => {
            if !session.is_panel() || !session.is_judge(judge) {
                return err(format!("{judge} is not a judge of a panel"));
            }
            round_mut(session, *round)?
                .scorecards
                .insert(judge.clone(), output.clone());
        }
        Event::JudgeAccepted { round, output } if session.is_panel() => {
            accept_combined(session, *round, output)?;
        }
        Event::VerdictRequested { round, brief } => {
            if brief.to != session.judge.id {
                return err(format!("a verdict asked of {}, not the lead", brief.to));
            }
            let r = round_mut(session, *round)?;
            if !r.outcome.as_ref().is_some_and(|o| o.close) {
                return err(format!(
                    "round {round} has no combined judgment that closes"
                ));
            }
            r.verdict_brief = Some(brief.clone());
        }
        Event::VerdictAccepted {
            round,
            winner,
            final_verdict,
        } => {
            if !session.is_contestant(winner) {
                return err(format!("unknown contestant {winner}"));
            }
            let Some(outcome) = round_mut(session, *round)?.outcome.as_mut() else {
                return err(format!("round {round} has no judgment for a verdict"));
            };
            outcome.winner = Some(winner.clone());
            outcome.final_verdict = Some(final_verdict.clone());
            session.winner = Some(winner.clone());
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
            // Only what the engine would extend: a competition with a
            // limit, running before its final round or ended at its limit,
            // to a strictly higher limit with a round left to run.
            if let Err(why) = session.extendable() {
                return err(format!("rounds extended where none can be: {why}"));
            }
            let base = session
                .max_rounds
                .unwrap_or_default()
                .max(session.rounds.len() as RoundNo);
            if *max_rounds <= base {
                return err(format!(
                    "round limit {max_rounds} is not above {base} after round {}",
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
            judges: &[AgentKind::Agy.into()],
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

    /// The log of a one-round competition (limit 1) the judge's final
    /// verdict ended, won by claude.
    fn ended_at_its_limit() -> Vec<Vec<Event>> {
        let setup = Setup {
            request: "x",
            max_rounds: Some(1),
            reveal_interval: 5,
            workdir: None,
            judges: &[AgentKind::Agy.into()],
            panel: &[AgentKind::Claude.into(), AgentKind::Codex.into()],
        };
        let (mut engine, out) = Engine::create_in(
            SessionId::new("comp-test"),
            setup,
            std::path::Path::new("/tmp/comp-test"),
            Config::default(),
            1,
        )
        .unwrap();
        let mut log = out.batches;
        let mut feed = |input| log.extend(engine.handle(input).unwrap().batches);
        feed(crate::engine::Input::Start { now: 2 });
        for part in ["claude", "codex"] {
            feed(crate::engine::Input::ReplyFileWritten {
                round: 1,
                part: PartId::new(part),
                content: "# Summary: done".into(),
                artifacts: vec![],
                at: 3,
            });
        }
        feed(crate::engine::Input::ReplyFileWritten {
            round: 1,
            part: PartId::new(crate::model::types::JUDGE_ID),
            content: r#"{"overview":"ok","evaluations":{"claude":{"score":80,"feedback":"a"},"codex":{"score":70,"feedback":"b"}},"ranking":["claude","codex"],"winner":"claude","final_verdict":"claude wins","close":true}"#.into(),
            artifacts: vec![],
            at: 4,
        });
        log
    }

    #[test]
    fn an_extension_reopens_a_competition_that_ended_at_its_limit_without_its_winner() {
        let mut session = replay(ended_at_its_limit()).unwrap();
        assert!(session.closed);
        assert_eq!(session.winner, Some(PartId::new("claude")));
        // Not above the limit it has.
        assert!(
            apply(
                &mut session.clone(),
                &[Event::RoundsExtended {
                    max_rounds: 1,
                    at: 5
                }]
            )
            .is_err()
        );
        apply(
            &mut session,
            &[
                Event::HoldSet { hold: true },
                Event::RoundsExtended {
                    max_rounds: 2,
                    at: 5,
                },
            ],
        )
        .unwrap();
        assert!(!session.closed && session.hold);
        assert_eq!(session.winner, None);
        assert_eq!(session.max_rounds, Some(2));
        // The verdict stays with its round, and is not the result now.
        let earlier = session.round(1).unwrap().outcome.as_ref().unwrap();
        assert_eq!(earlier.final_verdict.as_deref(), Some("claude wins"));
        assert_eq!(session.final_verdict(), None);
    }

    #[test]
    fn a_log_extending_what_cannot_be_extended_does_not_fold() {
        let started = || {
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
            session
        };
        let extend = |max_rounds| Event::RoundsExtended { max_rounds, at: 3 };
        // Ended by the user before its limit (5).
        let mut session = started();
        session.max_rounds = Some(5);
        apply(&mut session, &[Event::SessionClosed]).unwrap();
        assert!(apply(&mut session, &[extend(6)]).is_err());
        assert!(session.closed);
        // Running before its limit: only upward.
        let mut session = started();
        session.max_rounds = Some(5);
        assert!(apply(&mut session.clone(), &[extend(2)]).is_err());
        assert!(apply(&mut session.clone(), &[extend(5)]).is_err());
        apply(&mut session, &[extend(6)]).unwrap();
        // The final round under way.
        let mut session = started();
        assert!(apply(&mut session, &[extend(2)]).is_err());
        // No limit.
        let mut session = started();
        session.max_rounds = None;
        assert!(apply(&mut session, &[extend(2)]).is_err());
    }

    use crate::model::aggregate::tests::{
        card, cards, judge_next, panel_created, panel_session, settle,
    };
    use crate::model::types::{Brief, judge_id};

    fn brief_to(to: &PartId, file: &str) -> Brief {
        Brief {
            to: to.clone(),
            base_rev: 1,
            body: format!("brief to {to}"),
            reply_path: PathBuf::from(format!("/c/r1/{file}")),
            receipt: Receipt::default(),
        }
    }

    fn scorecard(judge: usize, output: JudgeOutput) -> Event {
        Event::ScorecardAccepted {
            round: 1,
            judge: judge_id(judge),
            output,
        }
    }

    #[test]
    fn a_panel_round_is_not_judged_before_every_scorecard() {
        let mut session = panel_session(Some(3));
        let all = cards(
            &[("codex", 90), ("claude", 70)],
            &[("codex", 85), ("claude", 75)],
            &[("codex", 80), ("claude", 79)],
        );
        let combined = crate::model::aggregate::combine(&session, 1, &all);
        apply(
            &mut session,
            &[
                scorecard(0, all[&judge_id(0)].clone()),
                scorecard(1, all[&judge_id(1)].clone()),
            ],
        )
        .unwrap();
        let early = apply(
            &mut session.clone(),
            &[Event::JudgeAccepted {
                round: 1,
                output: combined.clone(),
            }],
        );
        assert!(
            early.as_ref().is_err_and(|e| e.0.contains("judge-3")),
            "{early:?}"
        );
        // Only a panel's judges keep scorecards.
        let contestant = Event::ScorecardAccepted {
            round: 1,
            judge: PartId::new("claude"),
            output: card("claude", &[("codex", 1), ("claude", 1)]),
        };
        assert!(apply(&mut session.clone(), std::slice::from_ref(&contestant)).is_err());
        let mut single = create(&created()).unwrap();
        apply(
            &mut single,
            &[Event::RoundStarted {
                no: 1,
                kind: RoundKind::Blind,
                at: 2,
            }],
        )
        .unwrap();
        assert!(apply(&mut single, &[scorecard(0, all[&judge_id(0)].clone())]).is_err());

        apply(
            &mut session,
            &[
                scorecard(2, all[&judge_id(2)].clone()),
                Event::JudgeAccepted {
                    round: 1,
                    output: combined.clone(),
                },
            ],
        )
        .unwrap();
        let round = session.round(1).unwrap();
        assert_eq!(round.outcome.as_ref(), Some(&combined));
        assert_eq!(round.scorecards.len(), 3);
        let codex = session.panel_scores(1, &PartId::new("codex")).unwrap();
        assert_eq!(
            (codex.mean_label(), codex.spread()),
            ("85.0".to_owned(), 10)
        );
        assert_eq!(
            session.score_label(1, &PartId::new("claude")).as_deref(),
            Some("74.7")
        );
    }

    #[test]
    fn co_judge_briefs_are_not_contestant_briefs() {
        let mut session = panel_session(Some(3));
        let lead = judge_id(0);
        let second = judge_id(1);
        let third = judge_id(2);
        apply(
            &mut session,
            &[
                Event::BriefIssued {
                    round: 1,
                    brief: brief_to(&lead, "judge.json"),
                },
                Event::BriefIssued {
                    round: 1,
                    brief: brief_to(&second, "judge-2.json"),
                },
                Event::BriefIssued {
                    round: 1,
                    brief: brief_to(&third, "judge-3.json"),
                },
            ],
        )
        .unwrap();
        let round = session.round(1).unwrap();
        assert!(round.briefs.is_empty());
        assert_eq!(round.judge_brief.as_ref().map(|b| &b.to), Some(&lead));
        let co: Vec<&PartId> = round.co_judge_briefs.iter().map(|b| &b.to).collect();
        assert_eq!(co, [&second, &third]);
        let all: Vec<&PartId> = round.judge_briefs().map(|b| &b.to).collect();
        assert_eq!(all, [&lead, &second, &third]);
        // Receipts and panes find the co-judges.
        apply(
            &mut session,
            &[
                Event::PaneAssigned {
                    part: third.clone(),
                    pane_id: 9,
                },
                Event::BriefSent {
                    round: 1,
                    to: second.clone(),
                    at: 5,
                },
                Event::BriefFailed {
                    round: 1,
                    to: third.clone(),
                    reason: "x".into(),
                },
            ],
        )
        .unwrap();
        assert_eq!(session.co_judges[1].pane_id, Some(9));
        let round = session.round(1).unwrap();
        assert_eq!(round.brief(&second).unwrap().receipt.sent, Some(5));
        assert_eq!(round.retryable(), std::slice::from_ref(&third));
        // The verdict brief takes the lead's place once issued.
        let mut verdict = session.clone();
        let mut all = cards(
            &[("codex", 90), ("claude", 70)],
            &[("codex", 85), ("claude", 75)],
            &[("codex", 80), ("claude", 79)],
        );
        all.get_mut(&lead).unwrap().close = true;
        settle(&mut verdict, 1, &all).unwrap();
        apply(
            &mut verdict,
            &[Event::VerdictRequested {
                round: 1,
                brief: brief_to(&lead, "verdict.json"),
            }],
        )
        .unwrap();
        let round = verdict.round(1).unwrap();
        assert!(
            round
                .brief(&lead)
                .unwrap()
                .reply_path
                .ends_with("verdict.json")
        );
        assert!(
            round
                .judge_brief
                .as_ref()
                .unwrap()
                .reply_path
                .ends_with("judge.json")
        );
    }

    #[test]
    fn an_amendment_corrects_its_own_judges_card_and_recombines_the_round() {
        let mut session = panel_session(Some(3));
        let claude = PartId::new("claude");
        let codex = PartId::new("codex");
        let r1 = cards(
            &[("codex", 80), ("claude", 70)],
            &[("codex", 80), ("claude", 72)],
            &[("codex", 80), ("claude", 74)],
        );
        settle(&mut session, 1, &r1).unwrap();
        let before = session.round(1).unwrap().outcome.clone().unwrap();
        assert_eq!(before.evaluations[&claude].score, 72);
        assert_eq!(before.ranking, [codex.clone(), claude.clone()]);

        judge_next(&mut session, 2);
        let mut r2 = cards(
            &[("codex", 81), ("claude", 82)],
            &[("codex", 81), ("claude", 82)],
            &[("codex", 81), ("claude", 82)],
        );
        r2.get_mut(&judge_id(1)).unwrap().amendments = vec![crate::model::types::Amendment {
            round: 1,
            part: claude.clone(),
            score: 99,
            reason: "misread".into(),
        }];
        settle(&mut session, 2, &r2).unwrap();

        let round = session.round(1).unwrap();
        // judge-2's card only.
        let own = &round.scorecards[&judge_id(1)].evaluations[&claude];
        assert_eq!(own.score, 99);
        assert!(own.breakdown.is_empty());
        assert_eq!(
            own.amended.as_ref().map(|n| (n.from, n.by_round)),
            Some((72, 2))
        );
        for other in [0, 2] {
            assert_eq!(
                round.scorecards[&judge_id(other)],
                r1[&judge_id(other)],
                "judge seat {other}"
            );
        }
        // R1 combined again: 70 + 99 + 74 = 243, a mean of 81 over codex's 80.
        let outcome = round.outcome.as_ref().unwrap();
        let ev = &outcome.evaluations[&claude];
        assert_eq!(ev.score, 81);
        let note = ev.amended.as_ref().unwrap();
        assert_eq!(
            (note.from, note.by_round, note.reason.as_str()),
            (72, 2, "judge-2: misread")
        );
        assert_eq!(outcome.ranking, [claude.clone(), codex.clone()]);
        assert_eq!(outcome.overview, before.overview);
        assert_eq!(outcome.evaluations[&codex], before.evaluations[&codex]);
        assert_eq!(session.score_label(1, &claude).as_deref(), Some("81.0"));
        assert_eq!(session.score_history(&claude), [(1, 81), (2, 82)]);

        // A card that is not there cannot be amended.
        let mut cut = panel_session(Some(3));
        let mut partial = r1.clone();
        partial.remove(&judge_id(1));
        apply(
            &mut cut,
            &partial
                .iter()
                .map(|(judge, output)| Event::ScorecardAccepted {
                    round: 1,
                    judge: judge.clone(),
                    output: output.clone(),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        judge_next(&mut cut, 2);
        assert!(settle(&mut cut, 2, &r2).is_err());
    }

    #[test]
    fn the_verdict_sets_the_winner_and_the_result() {
        let mut session = panel_session(Some(1));
        let lead = judge_id(0);
        let codex = PartId::new("codex");
        let request = |to: &PartId| Event::VerdictRequested {
            round: 1,
            brief: brief_to(to, "verdict.json"),
        };
        // Not before the round is combined.
        assert!(apply(&mut session.clone(), &[request(&lead)]).is_err());
        settle(
            &mut session,
            1,
            &cards(
                &[("codex", 90), ("claude", 70)],
                &[("codex", 85), ("claude", 75)],
                &[("codex", 80), ("claude", 79)],
            ),
        )
        .unwrap();
        // The last round's judgment closes; only the lead gives the verdict.
        assert!(session.round(1).unwrap().outcome.as_ref().unwrap().close);
        assert!(apply(&mut session.clone(), &[request(&judge_id(1))]).is_err());
        apply(&mut session, &[request(&lead)]).unwrap();
        assert_eq!(session.final_verdict(), None);
        let verdict = |winner: &str| Event::VerdictAccepted {
            round: 1,
            winner: PartId::new(winner),
            final_verdict: "codex led every judge.".into(),
        };
        assert!(apply(&mut session.clone(), &[verdict("judge-2")]).is_err());
        apply(
            &mut session,
            &[
                Event::ReplyAccepted {
                    round: 1,
                    to: lead.clone(),
                    at: 7,
                },
                verdict("codex"),
                Event::PhaseChanged {
                    round: 1,
                    phase: Phase::Closed,
                },
                Event::SessionClosed,
            ],
        )
        .unwrap();
        assert_eq!(session.winner, Some(codex.clone()));
        let round = session.round(1).unwrap();
        let outcome = round.outcome.as_ref().unwrap();
        assert_eq!(outcome.winner, Some(codex));
        assert_eq!(session.final_verdict(), Some("codex led every judge."));
        assert_eq!(
            round.verdict_brief.as_ref().unwrap().receipt.replied,
            Some(7)
        );
        assert!(session.closed);
    }

    #[test]
    fn a_single_judge_created_event_has_no_co_judges_key() {
        let batch = created();
        let line = serde_json::to_string(&batch).unwrap();
        assert!(!line.contains("co_judges"), "{line}");
        assert_eq!(serde_json::from_str::<Vec<Event>>(&line).unwrap(), batch);
        let session = create(&batch).unwrap();
        assert!(session.co_judges.is_empty() && !session.is_panel());
        let json = serde_json::to_string(&session).unwrap();
        assert!(!json.contains("co_judges"), "{json}");
        let mut started = session.clone();
        apply(
            &mut started,
            &[Event::RoundStarted {
                no: 1,
                kind: RoundKind::Blind,
                at: 2,
            }],
        )
        .unwrap();
        let round = serde_json::to_string(started.round(1).unwrap()).unwrap();
        for key in ["co_judge_briefs", "scorecards", "verdict_brief"] {
            assert!(!round.contains(key), "{round}");
        }
        // A panel's are written, and read back.
        let panel = vec![panel_created(Some(2))];
        let line = serde_json::to_string(&panel).unwrap();
        assert!(line.contains(r#""co_judges":[{"id":"judge-2""#), "{line}");
        assert_eq!(serde_json::from_str::<Vec<Event>>(&line).unwrap(), panel);
        for (event, name) in [
            (
                scorecard(1, card("judge-2", &[("claude", 1)])),
                "scorecard_accepted",
            ),
            (
                Event::VerdictAccepted {
                    round: 1,
                    winner: PartId::new("claude"),
                    final_verdict: "v".into(),
                },
                "verdict_accepted",
            ),
            (
                Event::VerdictRequested {
                    round: 1,
                    brief: brief_to(&judge_id(0), "verdict.json"),
                },
                "verdict_requested",
            ),
        ] {
            let line = serde_json::to_string(&event).unwrap();
            assert!(
                line.starts_with(&format!(r#"{{"event":"{name}""#)),
                "{line}"
            );
            assert_eq!(serde_json::from_str::<Event>(&line).unwrap(), event);
        }
    }
}
