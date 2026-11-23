//! A fixed discussion driven through the public engine: open rounds, a
//! moderator-proposed debate run to a verdict, interventions (addressed and
//! broadcast), front matter with issues, stances and replies, and CRLF text.
//!
//! Shared by `tests/golden.rs` and the one-off generator that produced the
//! fixtures from `main`'s code, so both see the same inputs.

use std::path::{Path, PathBuf};

use discussion::engine::{AgentState, Config, Engine, Input, Setup};
use discussion::model::brief::{default_question, moderator_brief, panelist_brief};
use discussion::model::types::{AgentKind, Millis, PartId, RoundNo, SessionId};

pub const DIR: &str = "/golden/discussion";

fn p(id: &str) -> PartId {
    PartId::new(id)
}

struct Run {
    engine: Engine,
    now: Millis,
}

impl Run {
    fn feed(&mut self, input: Input) {
        self.engine.handle(input).expect("engine accepts input");
    }

    fn tick(&mut self) -> Millis {
        self.now += 100;
        self.now
    }

    fn no(&self) -> RoundNo {
        self.engine.session().current_round().expect("a round").no
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

    fn write(&mut self, part: &PartId, content: &str) {
        let round = self.no();
        let at = self.tick();
        self.feed(Input::ReplyFileWritten {
            round,
            part: part.clone(),
            content: content.to_owned(),
            at,
        });
    }

    fn panel(&mut self, replies: &[(&str, &str)]) {
        for (part, _) in replies {
            self.deliver(&p(part));
        }
        for (part, body) in replies {
            self.write(&p(part), body);
        }
    }

    fn moderate(&mut self, json: &str, expect_round: RoundNo) {
        let moderator = p("noa");
        self.deliver(&moderator);
        self.write(&moderator, json);
        assert_eq!(self.no(), expect_round, "moderator JSON accepted: {json}");
    }
}

/// Every brief text the scenario produces, in a fixed order, as one string.
pub fn briefs() -> String {
    let setup = Setup {
        topic: "Pick a datastore for a multi-tenant <SaaS> & its reporting\r\n(\"hot\" path first)",
        workdir: None,
        max_rounds: Some(6),
        moderator: AgentKind::Codex.into(),
        panel: &[
            AgentKind::Claude.into(),
            AgentKind::Codex.into(),
            AgentKind::Agy.into(),
        ],
    };
    let config = Config {
        debate_max_exchanges: 2,
        ..Config::default()
    };
    let (engine, _) = Engine::create_in(
        SessionId::new("golden-disc"),
        setup,
        Path::new(DIR),
        config,
        1_000,
    )
    .expect("engine");
    let mut run = Run { engine, now: 1_000 };
    run.feed(Input::Start { now: 1_100 });

    // R1: opening round, plain replies (one with CRLF line ends).
    run.panel(&[
        (
            "claude",
            "Postgres with row-level security.\nPartition by tenant.",
        ),
        (
            "codex",
            "SQLite per tenant.\r\nCheap, isolated,\r\nbut reporting is harder.\r\n",
        ),
        ("agy", "A managed document store; schema churn is high."),
    ]);
    run.moderate(
        r#"{"summary":"Two camps: shared Postgres vs per-tenant SQLite.\nReporting is the crux.",
            "issue_ops":[
              {"op":"add","id":"A","parent":null,"title":"Storage model"},
              {"op":"add","id":"B","parent":"A","title":"Reporting across tenants"},
              {"op":"add","id":"C","parent":"A","title":"Schema churn <migrations>"},
              {"op":"set_state","id":"B","state":"contested"},
              {"op":"set_stance","id":"A","part":"claude","pos":"for","basis":"m1"},
              {"op":"set_stance","id":"A","part":"codex","pos":"against","basis":"m2"}
            ],
            "minutes_ops":[
              {"op":"add","section":"contested","text":"Cross-tenant reporting cost","issues":["B"]},
              {"op":"add","section":"open_questions","text":"How often does the schema change?","issues":["C"]}
            ],
            "questions":{"claude":"Quantify RLS overhead.","agy":"How bad is schema churn really?"},
            "propose_debate":{"issue":"B","pro":["claude"],"con":["codex"]},
            "close":false}"#,
        1,
    );
    let id = run
        .engine
        .session()
        .pending_debate()
        .expect("debate proposed")
        .id
        .clone();
    let at = run.tick();
    run.feed(Input::UserIntervention {
        to: vec![p("claude")],
        body: "Keep the\r\nbudget under $500/month.".to_owned(),
        at,
    });
    let at = run.tick();
    run.feed(Input::UserApproveDebate {
        id,
        pro: None,
        con: None,
        at,
    });

    // R2, R3: the debate's two exchanges.
    run.panel(&[
        (
            "claude",
            "---\nissues: [B]\nreply_to: [m2]\nstance: {B: for}\n---\nA read replica handles reporting.",
        ),
        (
            "codex",
            "---\nissues: [B]\nreply_to: [m1]\nstance: {B: against}\n---\nFan-out queries over SQLite files are fine.",
        ),
        ("agy", "Observing: both ignore backups."),
    ]);
    run.moderate(r#"{"summary":"First exchange: replica vs fan-out."}"#, 3);
    run.panel(&[
        (
            "claude",
            "---\nissues: [B]\n---\nReplica lag is seconds, acceptable.",
        ),
        (
            "codex",
            "---\nissues: [B]\n---\nFan-out cost grows with tenants.",
        ),
        ("agy", "Backups favour Postgres."),
    ]);
    run.moderate(
        r#"{"summary":"The debate settles on a replica.","verdict":{"outcome":"conditional","rationale":"if replica lag stays under 5s"}}"#,
        4,
    );

    // R4: open again after a broadcast intervention.
    let at = run.tick();
    run.feed(Input::UserIntervention {
        to: Vec::new(),
        body: "Please converge; one more round.".to_owned(),
        at,
    });
    run.panel(&[
        (
            "claude",
            "---\nissues: [A, C]\nreply_to: [m3]\nstance: {A: for, C: neutral}\n---\nPostgres + migrations tooling. ✅ 結論",
        ),
        ("codex", "---\nissues: [A]\nstance: {A: neutral}\n---\nI can live with Postgres."),
        ("agy", "---\nissues: [C]\n---\nSchema churn is manageable."),
    ]);
    run.moderate(
        r#"{"summary":"Converging on Postgres.",
            "issue_ops":[{"op":"set_state","id":"A","state":"agreed"}],
            "minutes_ops":[
              {"op":"add","section":"decisions","text":"Use Postgres with RLS","issues":["A"]},
              {"op":"add","section":"actions","text":"Benchmark replica lag","issues":["B"]}
            ],
            "questions":{"codex":"Any remaining blockers?"},
            "close":false}"#,
        5,
    );

    let session = run.engine.session();
    let mut out = String::new();
    for r in &session.rounds {
        for b in &r.briefs {
            out.push_str(&format!("===== R{} brief to {} =====\n", r.no, b.to));
            out.push_str(&b.body);
        }
        if let Some(b) = &r.moderator_brief {
            out.push_str(&format!(
                "===== R{} moderator brief to {} =====\n",
                r.no, b.to
            ));
            out.push_str(&b.body);
        }
    }
    // Direct calls on the final session, outside the engine's own flow.
    let last = session.current_round().expect("a round");
    let reply_dir = PathBuf::from(DIR);
    for part in session.panelist_ids() {
        let q = default_question(session, &last.kind, part);
        let path = reply_dir.join(format!("direct-{part}.md"));
        let b = panelist_brief(session, part, last.no, q, &path);
        out.push_str(&format!("===== direct panelist_brief {part} =====\n"));
        out.push_str(&b.body);
        let b = panelist_brief(session, part, last.no, "Custom\r\nquestion?", &path);
        out.push_str(&format!(
            "===== direct panelist_brief {part} (custom question) =====\n"
        ));
        out.push_str(&b.body);
    }
    for r in &session.rounds {
        let b = moderator_brief(session, r.no, &reply_dir.join("direct-noa.json"));
        out.push_str(&format!("===== direct moderator_brief R{} =====\n", r.no));
        out.push_str(&b.body);
    }
    out
}
