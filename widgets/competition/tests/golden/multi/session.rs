//! A fixed two-round competition of claude, codex and agy scored by a panel
//! of three judges (claude on opus leads, codex and agy score), folded from
//! a fixed event list through the reducer, so it needs no engine: blind and
//! reveal rounds, a spread over the warning line, an amendment by `judge-2`
//! that moves its own R1 score but not the rounded mean, and the lead's
//! verdict (with `</script>` in it) after the combined final scores.
//!
//! Artifact snapshots are written under the caller's root as a runner
//! would, so `compare.html` links and previews them.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use competition::model::aggregate;
use competition::model::reduce::{self, Event};
use competition::model::types::{
    AgentKind, AgentSetup, Amendment, ArtifactFile, ArtifactKind, Author, AxisScore, Brief,
    EntryEvaluation, JudgeOutput, Message, MessageId, Millis, MsgKind, PartId, Participant, Phase,
    Receipt, Role, RoundKind, RoundNo, Session, SessionId, Submission, judge_id,
};
use competition::store;

pub const DIR: &str = "/golden/multi";

/// The points at which the scenario renders, in order.
pub const STEPS: [&str; 3] = ["r1-closed", "r2-verdict-pending", "r2-final"];

const CONTESTANTS: [&str; 3] = ["claude", "codex", "agy"];

fn p(id: &str) -> PartId {
    PartId::new(id)
}

fn part(id: PartId, kind: AgentKind, role: Role, model: Option<&str>) -> Participant {
    Participant {
        id,
        kind,
        role,
        pane_id: None,
        setup: AgentSetup {
            model: model.map(Into::into),
            effort: None,
        },
    }
}

/// One judge's scorecard: `scores` in its ranking order.
fn card(who: &str, round: RoundNo, scores: [(&str, u8); 3]) -> JudgeOutput {
    let lead = who == "judge";
    JudgeOutput {
        overview: format!("R{round} ({who}): <b>craft</b> & clarity decided it."),
        evaluations: scores
            .iter()
            .map(|(part, score)| {
                (
                    p(part),
                    EntryEvaluation {
                        score: *score,
                        strengths: format!("{who}: {part} keeps the layout calm"),
                        feedback: format!("{who}: {part} should fix 320px overflow"),
                        evidence: Vec::new(),
                        amended: None,
                        breakdown: if lead {
                            vec![
                                AxisScore {
                                    axis: "impact".into(),
                                    score: score / 2,
                                    max: 50,
                                },
                                AxisScore {
                                    axis: "craft".into(),
                                    score: score - score / 2,
                                    max: 50,
                                },
                            ]
                        } else {
                            Vec::new()
                        },
                    },
                )
            })
            .collect(),
        ranking: scores.iter().map(|(part, _)| p(part)).collect(),
        winner: None,
        final_verdict: None,
        close: lead && round == 2,
        amendments: Vec::new(),
        checklist: if lead && round == 1 {
            vec!["320x568: no horizontal scroll".into()]
        } else {
            Vec::new()
        },
    }
}

fn brief(to: PartId, rev: u64, reply_path: PathBuf) -> Brief {
    Brief {
        to,
        base_rev: rev,
        body: "brief".into(),
        reply_path,
        receipt: Receipt::default(),
    }
}

struct Run<'a> {
    session: Session,
    now: Millis,
    messages: usize,
    root: &'a Path,
    on_snapshot: &'a mut dyn FnMut(RoundNo, &PartId),
    render: &'a mut dyn FnMut(&str, &Session, &Path),
}

impl Run<'_> {
    fn apply(&mut self, batch: &[Event]) {
        reduce::apply(&mut self.session, batch).expect("the reducer takes the batch");
    }

    fn tick(&mut self) -> Millis {
        self.now += 100;
        self.now
    }

    fn message(&mut self, round: RoundNo, author: PartId, kind: MsgKind, body: &str) -> Event {
        self.messages += 1;
        Event::MessagePosted {
            message: Message {
                id: MessageId::nth(self.messages),
                round,
                author: Author::Part(author),
                kind,
                to: Vec::new(),
                body: body.into(),
                at: self.now,
            },
        }
    }

    /// Every contestant is briefed, writes its snapshot and its report.
    fn compete(&mut self, round: RoundNo) {
        let dir = Path::new(DIR);
        let rev = self.session.rev;
        let issued: Vec<Event> = CONTESTANTS
            .iter()
            .map(|c| Event::BriefIssued {
                round,
                brief: brief(p(c), rev, store::reply_path(dir, round, &p(c))),
            })
            .collect();
        self.apply(&issued);
        for c in CONTESTANTS {
            let at = self.tick();
            self.apply(&[Event::BriefSent {
                round,
                to: p(c),
                at,
            }]);
        }
        for c in CONTESTANTS {
            let id = p(c);
            let snap = store::artifact_snapshot_dir(self.root, round, &id);
            let files = [
                (
                    "index.html",
                    ArtifactKind::Html,
                    format!("<!doctype html><title>r{round}-{c}</title><p>{c} R{round}</p>\n"),
                ),
                (
                    "notes.md",
                    ArtifactKind::Markdown,
                    format!("# {c} R{round}\n\n- `<tag>` & \"quotes\"\n"),
                ),
            ];
            let mut artifacts = Vec::new();
            for (rel, kind, body) in files {
                fs::create_dir_all(&snap).unwrap();
                fs::write(snap.join(rel), &body).unwrap();
                artifacts.push(ArtifactFile {
                    rel_path: rel.into(),
                    kind,
                    bytes: body.len() as u64,
                });
            }
            (self.on_snapshot)(round, &id);
            let at = self.tick();
            self.apply(&[
                Event::ReplyAccepted {
                    round,
                    to: id.clone(),
                    at,
                },
                Event::SubmissionRecorded {
                    round,
                    submission: Submission {
                        part: id.clone(),
                        summary: format!("{c} entry for round {round} <with markup>"),
                        body: format!("Report for r{round}-{c}: 0 axe violations.\n"),
                        artifacts,
                        at,
                    },
                },
            ]);
        }
    }

    /// Judging starts with a brief per judge; each scorecard is accepted
    /// in seat order, the last with the combined judgment.
    fn judge(&mut self, round: RoundNo, cards: [JudgeOutput; 3], deciding: bool) {
        let dir = Path::new(DIR);
        let rev = self.session.rev;
        let mut batch = vec![Event::PhaseChanged {
            round,
            phase: Phase::Judging,
        }];
        for seat in 0..3 {
            let id = judge_id(seat);
            let path = store::judge_reply_path(dir, round, &id);
            batch.push(Event::BriefIssued {
                round,
                brief: brief(id, rev, path),
            });
        }
        self.apply(&batch);
        for seat in 0..3 {
            let at = self.tick();
            self.apply(&[Event::BriefSent {
                round,
                to: judge_id(seat),
                at,
            }]);
        }
        let all: BTreeMap<PartId, JudgeOutput> = cards
            .into_iter()
            .enumerate()
            .map(|(seat, card)| (judge_id(seat), card))
            .collect();
        for seat in 0..3 {
            let id = judge_id(seat);
            let at = self.tick();
            let mut batch = vec![
                Event::ReplyAccepted {
                    round,
                    to: id.clone(),
                    at,
                },
                Event::ScorecardAccepted {
                    round,
                    judge: id.clone(),
                    output: all[&id].clone(),
                },
            ];
            if seat == 2 {
                let mut with_cards = self.session.clone();
                reduce::apply(&mut with_cards, &batch).unwrap();
                let cards = &with_cards.round(round).unwrap().scorecards;
                let combined = aggregate::combine(&with_cards, round, cards);
                let overview = combined.overview.clone();
                batch.push(Event::JudgeAccepted {
                    round,
                    output: combined,
                });
                batch.push(self.message(round, judge_id(0), MsgKind::Judgment, &overview));
                if deciding {
                    let rev = self.session.rev;
                    batch.push(Event::VerdictRequested {
                        round,
                        brief: brief(judge_id(0), rev, store::verdict_reply_path(dir, round)),
                    });
                } else {
                    batch.push(Event::PhaseChanged {
                        round,
                        phase: Phase::Closed,
                    });
                }
            }
            self.apply(&batch);
        }
    }

    fn show(&mut self, step: &str) {
        (self.render)(step, &self.session, self.root);
    }
}

/// Runs the scenario with snapshots under `root`, calling `on_snapshot`
/// after each snapshot is written and `render` at each of [`STEPS`].
/// Returns the finished session.
pub fn run(
    root: &Path,
    on_snapshot: &mut dyn FnMut(RoundNo, &PartId),
    render: &mut dyn FnMut(&str, &Session, &Path),
) -> Session {
    let created = Event::Created {
        id: SessionId::new("golden-multi"),
        request: "Build a responsive <audio> mixer page & explain the trade-offs".into(),
        workdir: None,
        max_rounds: Some(2),
        reveal_interval: 2,
        judge: part(judge_id(0), AgentKind::Claude, Role::Judge, Some("opus")),
        co_judges: vec![
            part(judge_id(1), AgentKind::Codex, Role::Judge, None),
            part(judge_id(2), AgentKind::Agy, Role::Judge, None),
        ],
        contestants: vec![
            part(p("claude"), AgentKind::Claude, Role::Contestant, None),
            part(p("codex"), AgentKind::Codex, Role::Contestant, None),
            part(p("agy"), AgentKind::Agy, Role::Contestant, None),
        ],
        at: 1_000,
    };
    let mut run = Run {
        session: reduce::create(&[created]).expect("a panel session"),
        now: 1_000,
        messages: 0,
        root,
        on_snapshot,
        render,
    };

    // R1: agy's scores split the panel by 18 points.
    let at = run.tick();
    run.apply(&[Event::RoundStarted {
        no: 1,
        kind: RoundKind::Blind,
        at,
    }]);
    run.compete(1);
    run.judge(
        1,
        [
            card("judge", 1, [("codex", 85), ("claude", 80), ("agy", 60)]),
            card("judge-2", 1, [("claude", 83), ("codex", 80), ("agy", 78)]),
            card("judge-3", 1, [("codex", 88), ("claude", 81), ("agy", 62)]),
        ],
        false,
    );
    run.show("r1-closed");

    // R2, the final reveal round: judge-2 lowers its own R1 score of
    // claude (83 → 82), which leaves R1's rounded mean at 81.
    let at = run.tick();
    run.apply(&[Event::RoundStarted {
        no: 2,
        kind: RoundKind::Reveal,
        at,
    }]);
    run.compete(2);
    let mut second = card("judge-2", 2, [("codex", 90), ("claude", 84), ("agy", 72)]);
    second.amendments = vec![Amendment {
        round: 1,
        part: p("claude"),
        score: 82,
        reason: "R1 was scored without its <style>sheet".into(),
    }];
    run.judge(
        2,
        [
            card("judge", 2, [("claude", 88), ("codex", 86), ("agy", 70)]),
            second,
            card("judge-3", 2, [("claude", 90), ("codex", 85), ("agy", 69)]),
        ],
        true,
    );
    run.show("r2-verdict-pending");

    let verdict = "claude wins </script><!-- on the highest mean (87.3); judge-2 preferred codex";
    let at = run.tick();
    run.apply(&[Event::BriefSent {
        round: 2,
        to: judge_id(0),
        at,
    }]);
    let at = run.tick();
    let posted = run.message(
        2,
        judge_id(0),
        MsgKind::FinalVerdict,
        &format!("優勝: claude\n\n{verdict}"),
    );
    run.apply(&[
        Event::ReplyAccepted {
            round: 2,
            to: judge_id(0),
            at,
        },
        Event::VerdictAccepted {
            round: 2,
            winner: p("claude"),
            final_verdict: verdict.into(),
        },
        posted,
        Event::PhaseChanged {
            round: 2,
            phase: Phase::Closed,
        },
        Event::SessionClosed,
    ]);
    run.show("r2-final");
    run.session
}
