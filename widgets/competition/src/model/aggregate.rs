//! A judge panel's scores combined: every judge scores every contestant,
//! and a round's judgment is built from those scorecards. Integer
//! arithmetic only, so the same cards always give the same result.

use std::collections::BTreeMap;

use super::types::{EntryEvaluation, JudgeOutput, PartId, RoundNo, Session};

/// A spread (max − min of the judges' scores) of this many points or more
/// is flagged in the views. A display threshold only.
pub const SPREAD_WARN: u8 = 15;

/// Every judge's score of one entry in one round, in seat order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelScores {
    pub scores: Vec<(PartId, u8)>,
}

impl PanelScores {
    pub fn sum(&self) -> u32 {
        self.scores.iter().map(|(_, s)| u32::from(*s)).sum()
    }

    fn n(&self) -> u32 {
        self.scores.len().max(1) as u32
    }

    /// Ten times the mean, rounded half up: `813` for 81.33….
    pub fn mean_x10(&self) -> u32 {
        let n = self.n();
        (20 * self.sum() + n) / (2 * n)
    }

    /// The mean rounded half up to an integer (80.5 → 81).
    pub fn rounded(&self) -> u8 {
        let n = self.n();
        ((2 * self.sum() + n) / (2 * n)) as u8
    }

    /// The mean to one decimal: `81.3`, `81.0`.
    pub fn mean_label(&self) -> String {
        let x10 = self.mean_x10();
        format!("{}.{}", x10 / 10, x10 % 10)
    }

    /// The highest score less the lowest.
    pub fn spread(&self) -> u8 {
        let scores = self.scores.iter().map(|(_, s)| *s);
        let max = scores.clone().max().unwrap_or(0);
        let min = scores.min().unwrap_or(0);
        max - min
    }
}

/// The judges (seat order) without a scorecard in `cards`.
pub fn missing_scorecards(session: &Session, cards: &BTreeMap<PartId, JudgeOutput>) -> Vec<PartId> {
    session
        .judges()
        .filter(|judge| !cards.contains_key(&judge.id))
        .map(|judge| judge.id.clone())
        .collect()
}

/// The sum of every judge's score of `part` in `cards`.
fn panel_sum(session: &Session, cards: &BTreeMap<PartId, JudgeOutput>, part: &PartId) -> u32 {
    session
        .judges()
        .filter_map(|judge| cards.get(&judge.id)?.evaluations.get(part))
        .map(|ev| u32::from(ev.score))
        .sum()
}

/// The lead's ranking re-sorted by the sum of every judge's score, highest
/// first; ties keep the lead's order. Every judge scores every entry, so
/// comparing sums compares means exactly.
fn ranked_sums(session: &Session, cards: &BTreeMap<PartId, JudgeOutput>) -> Vec<(PartId, u32)> {
    let lead = &cards[&session.judge.id];
    let mut ranked: Vec<(PartId, u32)> = lead
        .ranking
        .iter()
        .map(|p| (p.clone(), panel_sum(session, cards, p)))
        .collect();
    ranked.sort_by_key(|(_, sum)| std::cmp::Reverse(*sum));
    ranked
}

/// The round's combined judgment from every judge's scorecard (seat order,
/// lead first). Pure; the engine logs its result in `JudgeAccepted`.
///
/// Every judge's card must be in `cards` (the engine and the fold check
/// it with [`missing_scorecards`]).
pub fn combine(
    session: &Session,
    round: RoundNo,
    cards: &BTreeMap<PartId, JudgeOutput>,
) -> JudgeOutput {
    let lead = &cards[&session.judge.id];
    let evaluations = lead
        .evaluations
        .iter()
        .map(|(part, ev)| {
            let scores = session
                .judges()
                .filter_map(|judge| {
                    let score = cards.get(&judge.id)?.evaluations.get(part)?.score;
                    Some((judge.id.clone(), score))
                })
                .collect();
            let entry = EntryEvaluation {
                score: PanelScores { scores }.rounded(),
                strengths: ev.strengths.clone(),
                feedback: ev.feedback.clone(),
                evidence: ev.evidence.clone(),
                amended: None,
                // Judges may split on different axes; each card keeps its own.
                breakdown: Vec::new(),
            };
            (part.clone(), entry)
        })
        .collect();
    let is_last = session.max_rounds.is_some_and(|max| round >= max);
    JudgeOutput {
        overview: lead.overview.clone(),
        evaluations,
        ranking: ranked_sums(session, cards)
            .into_iter()
            .map(|(part, _)| part)
            .collect(),
        winner: None,
        final_verdict: None,
        close: lead.close || is_last,
        // Amendments belong to the cards that made them.
        amendments: Vec::new(),
        checklist: lead.checklist.clone(),
    }
}

/// The entries with the highest mean in round `round`, in combined order:
/// the ones the lead's verdict may name. Empty unless every judge's
/// scorecard of the round is in.
pub fn top_set(session: &Session, round: RoundNo) -> Vec<PartId> {
    let Some(r) = session.round(round) else {
        return Vec::new();
    };
    if r.scorecards.is_empty() || !missing_scorecards(session, &r.scorecards).is_empty() {
        return Vec::new();
    }
    let ranked = ranked_sums(session, &r.scorecards);
    let Some(best) = ranked.first().map(|(_, sum)| *sum) else {
        return Vec::new();
    };
    ranked
        .into_iter()
        .take_while(|(_, sum)| *sum == best)
        .map(|(part, _)| part)
        .collect()
}

/// Recomputes `round`'s outcome from its scorecards after an amendment
/// made in round `by_round`, noting on each amended entry `AmendNote {
/// from: <old rounded mean>, by_round, reason: "<judge>: <reason>[; …]" }`.
/// `reasons` holds, per amended entry, `"<judge>: <reason>"` in seat order.
/// The rest of the outcome (texts, notes of entries not amended now, a
/// verdict) stays. The round's cards must be complete.
pub fn recombine(
    session: &mut Session,
    round: RoundNo,
    by_round: RoundNo,
    reasons: &BTreeMap<PartId, Vec<String>>,
) {
    let Some(r) = session.round(round) else {
        return;
    };
    let mut combined = combine(session, round, &r.scorecards);
    let Some(old) = r.outcome.as_ref() else {
        return;
    };
    for (part, ev) in &mut combined.evaluations {
        let before = old.evaluations.get(part);
        ev.amended = match reasons.get(part) {
            Some(why) => Some(super::types::AmendNote {
                from: before.map_or(ev.score, |b| b.score),
                by_round,
                reason: why.join("; "),
            }),
            None => before.and_then(|b| b.amended.clone()),
        };
    }
    combined.winner = old.winner.clone();
    combined.final_verdict = old.final_verdict.clone();
    combined.close = old.close;
    if let Some(r) = session.round_mut(round) {
        r.outcome = Some(combined);
    }
    winner_first(session, round);
}

/// Moves `round`'s verdict winner to the front of the entries whose sum
/// equals its own, so a tie the lead broke ranks the winner above the
/// entries it tied with. A no-op without a winner or when no entry ranked
/// before it shares its sum (an amendment that broke the tie leaves plain
/// sum order).
pub fn winner_first(session: &mut Session, round: RoundNo) {
    let Some(r) = session.round(round) else {
        return;
    };
    let Some(outcome) = r.outcome.as_ref() else {
        return;
    };
    let Some(winner) = outcome.winner.as_ref() else {
        return;
    };
    let Some(at) = outcome.ranking.iter().position(|p| p == winner) else {
        return;
    };
    let sum = panel_sum(session, &r.scorecards, winner);
    let Some(front) = outcome.ranking[..at]
        .iter()
        .position(|p| panel_sum(session, &r.scorecards, p) == sum)
    else {
        return;
    };
    if let Some(outcome) = session.round_mut(round).and_then(|r| r.outcome.as_mut()) {
        let winner = outcome.ranking.remove(at);
        outcome.ranking.insert(front, winner);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::reduce::{self, Event};
    use crate::model::types::{
        AgentKind, Participant, Phase, Role, RoundKind, SessionId, judge_id,
    };

    fn part(id: &str, kind: AgentKind, role: Role) -> Participant {
        Participant {
            id: PartId::new(id),
            kind,
            role,
            pane_id: None,
            setup: Default::default(),
        }
    }

    /// The `Created` event of a panel of three judges (agy leads, claude
    /// and codex score) over contestants claude and codex.
    pub(crate) fn panel_created(max_rounds: Option<RoundNo>) -> Event {
        Event::Created {
            id: SessionId::new("comp-panel"),
            request: "Build a widget".into(),
            workdir: None,
            max_rounds,
            reveal_interval: 5,
            judge: part("judge", AgentKind::Agy, Role::Judge),
            co_judges: vec![
                part(judge_id(1).as_str(), AgentKind::Claude, Role::Judge),
                part(judge_id(2).as_str(), AgentKind::Codex, Role::Judge),
            ],
            contestants: vec![
                part("claude", AgentKind::Claude, Role::Contestant),
                part("codex", AgentKind::Codex, Role::Contestant),
            ],
            at: 1,
        }
    }

    /// A panel session (see [`panel_created`]) with round 1 in `Judging`.
    pub(crate) fn panel_session(max_rounds: Option<RoundNo>) -> Session {
        let mut session = reduce::create(&[panel_created(max_rounds)]).unwrap();
        reduce::apply(
            &mut session,
            &[
                Event::RoundStarted {
                    no: 1,
                    kind: RoundKind::Blind,
                    at: 2,
                },
                Event::PhaseChanged {
                    round: 1,
                    phase: Phase::Judging,
                },
            ],
        )
        .unwrap();
        session
    }

    /// A scorecard scoring `scores` (contestant, score) and ranking them in
    /// that order, with texts naming `who`.
    pub(crate) fn card(who: &str, scores: &[(&str, u8)]) -> JudgeOutput {
        JudgeOutput {
            overview: format!("{who} overview"),
            evaluations: scores
                .iter()
                .map(|(p, score)| {
                    (
                        PartId::new(*p),
                        EntryEvaluation {
                            score: *score,
                            strengths: format!("{who} strengths of {p}"),
                            feedback: format!("{who} feedback to {p}"),
                            evidence: Vec::new(),
                            amended: None,
                            breakdown: vec![crate::model::types::AxisScore {
                                axis: "all".into(),
                                score: *score,
                                max: 100,
                            }],
                        },
                    )
                })
                .collect(),
            ranking: scores.iter().map(|(p, _)| PartId::new(*p)).collect(),
            winner: None,
            final_verdict: None,
            close: false,
            amendments: Vec::new(),
            checklist: Vec::new(),
        }
    }

    pub(crate) fn cards(
        lead: &[(&str, u8)],
        second: &[(&str, u8)],
        third: &[(&str, u8)],
    ) -> BTreeMap<PartId, JudgeOutput> {
        BTreeMap::from([
            (judge_id(0), card("judge", lead)),
            (judge_id(1), card("judge-2", second)),
            (judge_id(2), card("judge-3", third)),
        ])
    }

    /// Logs every card of `round` and its combined judgment, as the engine
    /// commits the last scorecard.
    pub(crate) fn settle(
        session: &mut Session,
        round: RoundNo,
        all: &BTreeMap<PartId, JudgeOutput>,
    ) -> Result<(), reduce::ReduceError> {
        let mut batch: Vec<Event> = all
            .iter()
            .map(|(judge, output)| Event::ScorecardAccepted {
                round,
                judge: judge.clone(),
                output: output.clone(),
            })
            .collect();
        let mut with_cards = session.clone();
        reduce::apply(&mut with_cards, &batch)?;
        let cards = &with_cards.round(round).unwrap().scorecards;
        batch.push(Event::JudgeAccepted {
            round,
            output: combine(&with_cards, round, cards),
        });
        reduce::apply(session, &batch)
    }

    /// Closes the current round and starts round `no` in `Judging`.
    pub(crate) fn judge_next(session: &mut Session, no: RoundNo) {
        reduce::apply(
            session,
            &[
                Event::PhaseChanged {
                    round: no - 1,
                    phase: Phase::Closed,
                },
                Event::RoundStarted {
                    no,
                    kind: RoundKind::Blind,
                    at: u64::from(no),
                },
                Event::PhaseChanged {
                    round: no,
                    phase: Phase::Judging,
                },
            ],
        )
        .unwrap();
    }

    #[test]
    fn mean_rounds_half_up_and_spread_is_max_minus_min() {
        let panel = |scores: &[u8]| PanelScores {
            scores: scores
                .iter()
                .enumerate()
                .map(|(seat, s)| (judge_id(seat), *s))
                .collect(),
        };
        let three = panel(&[80, 83, 81]);
        assert_eq!(three.sum(), 244);
        assert_eq!(three.mean_x10(), 813);
        assert_eq!(three.mean_label(), "81.3");
        assert_eq!(three.rounded(), 81);
        assert_eq!(three.spread(), 3);
        // 80.5 rounds up, and the label keeps its decimal.
        let half = panel(&[80, 81]);
        assert_eq!((half.rounded(), half.mean_label()), (81, "80.5".to_owned()));
        let whole = panel(&[81, 81, 81]);
        assert_eq!(
            (whole.rounded(), whole.mean_label()),
            (81, "81.0".to_owned())
        );
        // 80.666… is 80.7 and 81; 80.333… is 80.3 and 80.
        assert_eq!(panel(&[80, 81, 81]).mean_label(), "80.7");
        assert_eq!(panel(&[80, 81, 81]).rounded(), 81);
        assert_eq!(panel(&[80, 80, 81]).mean_label(), "80.3");
        assert_eq!(panel(&[80, 80, 81]).rounded(), 80);
        assert_eq!(panel(&[0, 100, 50]).spread(), 100);
        assert_eq!(panel(&[100, 100, 100]).mean_label(), "100.0");
        assert!(panel(&[60, 75]).spread() >= SPREAD_WARN);
    }

    #[test]
    fn ranking_is_by_exact_mean_with_ties_in_the_leads_order() {
        let session = panel_session(Some(3));
        // Rounded, both are 80; exactly, codex (80.33) is ahead of claude
        // (80.0), though the lead ranked claude first.
        let close = cards(
            &[("claude", 80), ("codex", 80)],
            &[("claude", 80), ("codex", 81)],
            &[("codex", 80), ("claude", 80)],
        );
        let out = combine(&session, 1, &close);
        assert_eq!(out.evaluations[&PartId::new("claude")].score, 80);
        assert_eq!(out.evaluations[&PartId::new("codex")].score, 80);
        assert_eq!(out.ranking, [PartId::new("codex"), PartId::new("claude")]);
        // A true tie keeps the lead's order, whatever the others ranked.
        let tie = cards(
            &[("claude", 81), ("codex", 80)],
            &[("codex", 82), ("claude", 80)],
            &[("codex", 80), ("claude", 81)],
        );
        let out = combine(&session, 1, &tie);
        assert_eq!(out.ranking, [PartId::new("claude"), PartId::new("codex")]);
        let mut session = session;
        session.rounds[0].scorecards = tie;
        assert_eq!(
            top_set(&session, 1),
            [PartId::new("claude"), PartId::new("codex")]
        );
        session.rounds[0].scorecards = close;
        assert_eq!(top_set(&session, 1), [PartId::new("codex")]);
    }

    #[test]
    fn combined_texts_are_the_leads() {
        let session = panel_session(Some(3));
        let mut all = cards(
            &[("codex", 90), ("claude", 70)],
            &[("codex", 85), ("claude", 75)],
            &[("claude", 80), ("codex", 79)],
        );
        let lead = all.get_mut(&judge_id(0)).unwrap();
        lead.checklist = vec!["no horizontal scroll".into()];
        lead.close = true;
        all.get_mut(&judge_id(1)).unwrap().checklist = vec!["ignored".into()];
        let out = combine(&session, 1, &all);
        assert_eq!(out.overview, "judge overview");
        assert_eq!(out.checklist, ["no horizontal scroll"]);
        let codex = &out.evaluations[&PartId::new("codex")];
        assert_eq!(codex.score, 85);
        assert_eq!(codex.strengths, "judge strengths of codex");
        assert_eq!(codex.feedback, "judge feedback to codex");
        assert!(codex.breakdown.is_empty());
        assert!(out.close, "the lead's close");
        assert_eq!((out.winner, out.final_verdict), (None, None));
        assert!(out.amendments.is_empty());
        // The last round closes whatever the lead said.
        let mut last = all.clone();
        last.get_mut(&judge_id(0)).unwrap().close = false;
        assert!(!combine(&session, 1, &last).close);
        assert!(combine(&session, 3, &last).close);
    }
}
