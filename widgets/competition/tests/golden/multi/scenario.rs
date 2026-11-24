//! A fixed three-round competition judged by a panel of three, driven
//! through the public engine: three contestants, a lead (claude) and two
//! co-judges of one kind (codex), a co-judge's reply turned down and
//! corrected, interventions to one judge and to everyone, a co-judge's
//! amendment of its own earlier score, a peer-reveal round, and the
//! lead's verdict (turned down once, for naming an entry without the
//! highest mean).
//!
//! Every text a judge writes is marked `[by <judge>]`, so a brief that
//! quotes another judge is easy to find.

use std::path::Path;

use competition::engine::{AgentState, Config, Effect, Engine, Input, Setup};
use competition::model::types::{
    AgentChoice, AgentKind, AgentSetup, ArtifactFile, ArtifactKind, Millis, PartId, RoundNo,
    Session, SessionId,
};

pub const DIR: &str = "/golden/multi-competition";

fn p(id: &str) -> PartId {
    PartId::new(id)
}

const CONTESTANTS: [&str; 3] = ["claude", "codex", "agy"];
const JUDGES: [&str; 3] = ["judge", "judge-2", "judge-3"];

fn submission(round: RoundNo, part: &str) -> String {
    format!(
        "# Summary: {part} entry for round {round}\n\n\
         ## Beyond feedback\nAdded a compact layout.\n\n\
         Report body for r{round}-{part}.\n"
    )
}

fn artifacts(part: &str) -> Vec<ArtifactFile> {
    match part {
        "agy" => vec![ArtifactFile {
            rel_path: "notes.md".into(),
            kind: ArtifactKind::Markdown,
            bytes: 300,
        }],
        _ => vec![
            ArtifactFile {
                rel_path: "index.html".into(),
                kind: ArtifactKind::Html,
                bytes: 2048,
            },
            ArtifactFile {
                rel_path: "style.css".into(),
                kind: ArtifactKind::Code,
                bytes: 512,
            },
        ],
    }
}

/// `judge`'s scorecard: scores for claude, codex, agy (ranked by score),
/// then `extra` JSON members.
fn card(judge: &str, round: RoundNo, scores: [u8; 3], extra: &str) -> String {
    let mut ranked: Vec<(&str, u8)> = CONTESTANTS.into_iter().zip(scores).collect();
    ranked.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
    let ranking: Vec<String> = ranked.iter().map(|(p, _)| format!("\"{p}\"")).collect();
    let evaluations: Vec<String> = CONTESTANTS
        .into_iter()
        .zip(scores)
        .map(|(part, score)| {
            format!(
                r#"    "{part}": {{ "score": {score}, "strengths": "[by {judge}] R{round} strengths of {part}", "feedback": "[by {judge}] R{round} feedback to {part}",
      "evidence": [ {{ "claim": "[by {judge}] R{round} claim on {part}", "basis": "measured" }} ] }}"#
            )
        })
        .collect();
    format!(
        "{{\n  \"overview\": \"[by {judge}] R{round} overview\",\n  \"evaluations\": {{\n{}\n  }},\n  \"ranking\": [{}]{extra}\n}}",
        evaluations.join(",\n"),
        ranking.join(", ")
    )
}

struct Run {
    engine: Engine,
    now: Millis,
    /// Correction notes sent back, in order.
    notes: Vec<(RoundNo, PartId, String)>,
}

impl Run {
    fn feed(&mut self, input: Input) {
        let out = self.engine.handle(input).expect("engine accepts input");
        for effect in out.effects {
            if let Effect::SendBrief { round, part, body } = effect
                && body.starts_with("Your previous output")
            {
                self.notes.push((round, part, body));
            }
        }
    }

    fn tick(&mut self) -> Millis {
        self.now += 100;
        self.now
    }

    fn no(&self) -> RoundNo {
        self.engine.session().rounds.last().expect("a round").no
    }

    fn deliver(&mut self, part: &PartId) {
        let round = self.no();
        let at = self.tick();
        self.feed(Input::Sent {
            round,
            part: part.clone(),
            at,
        });
        let at = self.tick();
        self.feed(Input::AgentStateChanged {
            part: part.clone(),
            state: AgentState::Running,
            at,
        });
    }

    fn write(&mut self, part: &str, content: String) {
        let round = self.no();
        let at = self.tick();
        let artifacts = if CONTESTANTS.contains(&part) {
            artifacts(part)
        } else {
            Vec::new()
        };
        self.feed(Input::ReplyFileWritten {
            round,
            part: p(part),
            content,
            artifacts,
            at,
        });
    }

    fn compete(&mut self) {
        let round = self.no();
        for part in CONTESTANTS {
            self.deliver(&p(part));
        }
        for part in CONTESTANTS {
            self.write(part, submission(round, part));
        }
    }

    fn intervene(&mut self, to: &[&str], body: &str) {
        let at = self.tick();
        self.feed(Input::UserIntervention {
            to: to.iter().map(|t| p(t)).collect(),
            body: body.to_owned(),
            at,
        });
    }
}

/// Runs the scenario; returns every brief and correction note in a fixed
/// order as one string, and the final session.
pub fn run() -> (String, Session) {
    let codex = |model: &str| AgentChoice {
        kind: AgentKind::Codex,
        setup: AgentSetup {
            model: Some(model.into()),
            effort: None,
        },
    };
    let setup = Setup {
        request: "Build a responsive audio mixer page",
        max_rounds: Some(3),
        reveal_interval: 2,
        workdir: None,
        judges: &[
            AgentKind::Claude.into(),
            codex("gpt-6.2-sol"),
            codex("gpt-6-luna"),
        ],
        panel: &[
            AgentKind::Claude.into(),
            AgentKind::Codex.into(),
            AgentKind::Agy.into(),
        ],
    };
    let (engine, _) = Engine::create_in(
        SessionId::new("golden-panel"),
        setup,
        Path::new(DIR),
        Config::default(),
        1_000,
    )
    .expect("engine");
    let mut run = Run {
        engine,
        now: 1_000,
        notes: Vec::new(),
    };
    run.feed(Input::Start { now: 1_100 });

    // R1. judge-2 first publishes a checklist (only the lead may) and is
    // sent back; an intervention to judge-2 arrives while judges judge.
    run.compete();
    for judge in JUDGES {
        run.deliver(&p(judge));
    }
    run.intervene(&["judge-2"], "Check keyboard control too.");
    run.write(
        "judge",
        card(
            "judge",
            1,
            [78, 74, 60],
            r#",
  "checklist": ["320x568: no horizontal scroll", "Contrast >= 4.5:1"]"#,
        ),
    );
    run.write(
        "judge-2",
        card("judge-2", 1, [80, 70, 62], r#", "checklist": ["mine"]"#),
    );
    run.deliver(&p("judge-2"));
    run.write("judge-2", card("judge-2", 1, [80, 70, 62], ""));
    run.write("judge-3", card("judge-3", 1, [75, 77, 58], ""));

    // R2. judge-3 corrects its own R1 score of agy.
    run.intervene(&[], "Mobile first.");
    run.compete();
    for judge in JUDGES {
        run.deliver(&p(judge));
    }
    run.write("judge-2", card("judge-2", 2, [83, 79, 66], ""));
    run.write(
        "judge-3",
        card(
            "judge-3",
            2,
            [81, 84, 65],
            r#",
  "amendments": [ { "round": 1, "contestant": "agy", "score": 64, "reason": "[by judge-3] R1 notes were read from the wrong folder" } ]"#,
        ),
    );
    run.write("judge", card("judge", 2, [85, 80, 63], ""));

    // R3, the final and a peer-reveal round: the lead scores, then writes
    // the verdict once every judge is in.
    run.compete();
    for judge in JUDGES {
        run.deliver(&p(judge));
    }
    run.write(
        "judge",
        card("judge", 3, [88, 86, 70], r#", "close": true"#),
    );
    run.write("judge-3", card("judge-3", 3, [84, 87, 69], ""));
    run.intervene(&["judge"], "Name the trade-offs in the verdict.");
    run.write("judge-2", card("judge-2", 3, [87, 85, 71], ""));
    run.deliver(&p("judge"));
    run.write(
        "judge",
        r#"{"winner": "agy", "final_verdict": "[by judge] agy"}"#.to_owned(),
    );
    run.deliver(&p("judge"));
    run.write(
        "judge",
        r#"{"winner": "claude", "final_verdict": "[by judge] claude leads on two of three cards; judge-3 preferred codex by 3."}"#
            .to_owned(),
    );

    let session = run.engine.session().clone();
    assert!(session.closed, "the verdict closes the competition");
    let mut out = String::new();
    for r in &session.rounds {
        for b in &r.briefs {
            out.push_str(&format!("===== R{} brief to {} =====\n", r.no, b.to));
            out.push_str(&b.body);
        }
        for b in r.judge_brief.iter().chain(&r.co_judge_briefs) {
            out.push_str(&format!("===== R{} judge brief to {} =====\n", r.no, b.to));
            out.push_str(&b.body);
        }
        if let Some(b) = &r.verdict_brief {
            out.push_str(&format!(
                "===== R{} verdict brief to {} =====\n",
                r.no, b.to
            ));
            out.push_str(&b.body);
        }
    }
    for (round, part, note) in &run.notes {
        out.push_str(&format!("===== R{round} correction to {part} =====\n"));
        out.push_str(note);
    }
    // Contestant briefs quote the running executable as the `--check`
    // command; its path differs per build.
    let exe = std::env::current_exe().expect("current exe");
    let out = out.replace(&format!("\"{}\"", exe.display()), "\"<current_exe>\"");
    (out, session)
}
