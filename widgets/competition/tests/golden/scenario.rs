//! A fixed three-round competition driven through the public engine: blind
//! and reveal rounds, interventions (addressed and broadcast), an amendment
//! of a closed round, a final verdict with `</script>` in it, and artifact
//! snapshots on disk, including a `.preview` that appears only after its
//! round closed.
//!
//! Shared by `tests/golden.rs` and the one-off generator that produced the
//! fixtures from `main`'s code, so both see the same inputs.

use std::fs;
use std::path::{Path, PathBuf};

use competition::engine::{AgentState, Config, Engine, Input, Setup};
use competition::model::brief::{contestant_brief, judge_brief};
use competition::model::types::{
    AgentKind, ArtifactFile, ArtifactKind, Millis, PartId, RoundNo, Session, SessionId,
};
use competition::preview;
use competition::store;

pub const DIR: &str = "/golden/competition";

/// The points at which the scenario renders `compare.html`, in order.
pub const STEPS: [&str; 5] = [
    "r1-open",
    "r1-closed",
    "r1-preview-late",
    "r2-amended",
    "r3-final",
];

fn p(id: &str) -> PartId {
    PartId::new(id)
}

/// A file in a contestant's snapshot.
struct File {
    rel: &'static str,
    kind: ArtifactKind,
    body: Vec<u8>,
}

fn files(round: RoundNo, part: &str) -> Vec<File> {
    let tag = format!("r{round}-{part}");
    let mut big = String::new();
    // Over the 32 KiB inline preview limit, with a multi-byte character
    // straddling the cut.
    while big.len() < 32_766 {
        big.push_str(&format!("// {tag} line\n"));
    }
    big.truncate(32_766);
    big.push_str("日本語の行\n");
    match part {
        "agy" => vec![File {
            rel: "notes on design.md",
            kind: ArtifactKind::Markdown,
            body: format!("# Notes {tag}\n\nUnicode: café ✓ <b>raw</b>\r\n").into_bytes(),
        }],
        _ => vec![
            File {
                rel: "index.html",
                kind: ArtifactKind::Html,
                body: format!(
                    "<!doctype html><html><head><link rel=\"stylesheet\" href=\"style.css\">\
                     <title>{tag}</title></head><body><img src=\"logo.svg\">\
                     <script>console.log(\"</scr\" + \"ipt>\")</script><p>{tag}</p></body></html>\n"
                )
                .into_bytes(),
            },
            File {
                rel: "style.css",
                kind: ArtifactKind::Code,
                body: format!("/* {tag} */ body {{ color: #222; }}\n").into_bytes(),
            },
            File {
                rel: "src/app.js",
                kind: ArtifactKind::Code,
                body: big.into_bytes(),
            },
            File {
                rel: "logo.svg",
                kind: ArtifactKind::Svg,
                body: b"<svg xmlns=\"http://www.w3.org/2000/svg\"><circle r=\"4\"/></svg>".to_vec(),
            },
            File {
                rel: "README.md",
                kind: ArtifactKind::Markdown,
                body: format!("# {tag}\n\n- item `<tag>` & \"quotes\"\n").into_bytes(),
            },
        ],
    }
}

fn submission(round: RoundNo, part: &str) -> String {
    format!(
        "# Summary: {part} entry for round {round} <with markup>\n\n\
         ## Beyond feedback\nTightened spacing; added dark mode.\r\n\n\
         Report body for r{round}-{part}: measured 0 axe violations.\n"
    )
}

fn judge_json(round: RoundNo, extra: &str) -> String {
    let s = |base: u8| base + round as u8;
    format!(
        r#"{{
  "overview": "Round {round}: all three improved; claude leads on craft.",
  "evaluations": {{
    "claude": {{ "score": {c}, "strengths": "Clear hierarchy", "feedback": "Fix 320px overflow",
      "breakdown": [ {{ "axis": "impact", "score": 40, "max": 60 }}, {{ "axis": "craft", "score": {cc}, "max": 40 }} ],
      "evidence": [ {{ "claim": "index.html renders at 1440x900", "basis": "measured" }} ] }},
    "codex": {{ "score": {d}, "strengths": "Small CSS", "feedback": "Focus states",
      "breakdown": [ {{ "axis": "impact", "score": 38, "max": 60 }}, {{ "axis": "craft", "score": {dc}, "max": 40 }} ],
      "evidence": [ {{ "claim": "0 axe violations", "basis": "self_reported" }} ] }},
    "agy": {{ "score": {a}, "strengths": "Good notes", "feedback": "Ship a page",
      "breakdown": [ {{ "axis": "impact", "score": 30, "max": 60 }}, {{ "axis": "craft", "score": {ac}, "max": 40 }} ],
      "evidence": [ {{ "claim": "notes only", "basis": "measured" }} ] }}
  }},
  "ranking": ["claude", "codex", "agy"],
  "checklist": ["320x568: no horizontal scroll", "Contrast >= 4.5:1"]{extra}
}}"#,
        c = s(70),
        cc = s(70) - 40,
        d = s(65),
        dc = s(65) - 38,
        a = s(50),
        ac = s(50) - 30,
    )
}

struct Run<'a> {
    engine: Engine,
    now: Millis,
    root: &'a Path,
    on_snapshot: &'a mut dyn FnMut(RoundNo, &PartId),
    render: &'a mut dyn FnMut(&str, &Session, &Path),
}

impl Run<'_> {
    fn feed(&mut self, input: Input) {
        self.engine.handle(input).expect("engine accepts input");
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

    /// Writes the snapshot, then hands the submission to the engine, as the
    /// runner does.
    fn submit(&mut self, part: &str, preview_now: bool) {
        let round = self.no();
        let id = p(part);
        let dir = store::artifact_snapshot_dir(self.root, round, &id);
        let mut artifacts = Vec::new();
        for f in files(round, part) {
            let path = dir.join(f.rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &f.body).unwrap();
            artifacts.push(ArtifactFile {
                rel_path: f.rel.to_owned(),
                kind: f.kind,
                bytes: f.body.len() as u64,
            });
        }
        (self.on_snapshot)(round, &id);
        if preview_now {
            preview::write(&dir).expect("preview");
        }
        let at = self.tick();
        self.feed(Input::ReplyFileWritten {
            round,
            part: id,
            content: submission(round, part),
            artifacts,
            at,
        });
    }

    fn round(&mut self, previews: [bool; 3], judge: &str, open_step: Option<&str>) {
        for part in ["claude", "codex", "agy"] {
            self.deliver(&p(part));
        }
        for (part, preview_now) in ["claude", "codex", "agy"].into_iter().zip(previews) {
            self.submit(part, preview_now);
        }
        if let Some(step) = open_step {
            self.show(step);
        }
        let round = self.no();
        let judge_id = p("judge");
        self.deliver(&judge_id);
        let at = self.tick();
        self.feed(Input::ReplyFileWritten {
            round,
            part: judge_id,
            content: judge.to_owned(),
            artifacts: Vec::new(),
            at,
        });
        let r = &self.engine.session().rounds[round as usize - 1];
        assert!(r.outcome.is_some(), "judge JSON accepted for R{round}");
    }

    fn show(&mut self, step: &str) {
        (self.render)(step, self.engine.session(), self.root);
    }
}

/// Runs the scenario with snapshots under `root`, calling `on_snapshot`
/// after each snapshot is written and `render` at each of [`STEPS`].
/// Returns every brief text in a fixed order, as one string.
pub fn run(
    root: &Path,
    on_snapshot: &mut dyn FnMut(RoundNo, &PartId),
    render: &mut dyn FnMut(&str, &Session, &Path),
) -> String {
    let setup = Setup {
        request: "Build a responsive <audio> mixer page & explain\r\nthe trade-offs (\"mobile\" first)",
        max_rounds: Some(3),
        reveal_interval: 2,
        workdir: None,
        judges: &[AgentKind::Claude.into()],
        panel: &[
            AgentKind::Claude.into(),
            AgentKind::Codex.into(),
            AgentKind::Agy.into(),
        ],
    };
    let (engine, _) = Engine::create_in(
        SessionId::new("golden-comp"),
        setup,
        Path::new(DIR),
        Config::default(),
        1_000,
    )
    .expect("engine");
    let mut run = Run {
        engine,
        now: 1_000,
        root,
        on_snapshot,
        render,
    };
    run.feed(Input::Start { now: 1_100 });

    // R1: claude's `.preview` is written only after the round closed.
    run.round([false, false, false], &judge_json(1, ""), Some("r1-open"));
    run.show("r1-closed");
    preview::write(&store::artifact_snapshot_dir(root, 1, &p("claude"))).expect("preview");
    run.show("r1-preview-late");

    let at = run.tick();
    run.feed(Input::UserIntervention {
        to: vec![p("codex")],
        body: "Use system\r\nfonts only.".to_owned(),
        at,
    });
    // R2 amends R1; codex's preview is there from the start.
    run.round(
        [false, true, false],
        &judge_json(
            2,
            r#",
  "amendments": [ { "round": 1, "contestant": "claude", "score": 78, "reason": "R1 was judged without its <style>sheet" } ]"#,
        ),
        None,
    );
    run.show("r2-amended");

    let at = run.tick();
    run.feed(Input::UserIntervention {
        to: Vec::new(),
        body: "Last round: polish only.".to_owned(),
        at,
    });
    // R3 is the final, reveal round.
    run.round(
        [true, true, false],
        &judge_json(
            3,
            r#",
  "winner": "claude",
  "final_verdict": "claude wins </script><!-- on craft & clarity",
  "close": true"#,
        ),
        None,
    );
    run.show("r3-final");

    let session = run.engine.session();
    let mut out = String::new();
    for r in &session.rounds {
        for b in &r.briefs {
            out.push_str(&format!("===== R{} brief to {} =====\n", r.no, b.to));
            out.push_str(&b.body);
        }
        if let Some(b) = &r.judge_brief {
            out.push_str(&format!("===== R{} judge brief to {} =====\n", r.no, b.to));
            out.push_str(&b.body);
        }
    }
    // Direct calls on the final session, outside the engine's own flow.
    let reply_dir = PathBuf::from(DIR);
    for r in &session.rounds {
        for part in session.contestant_ids() {
            let b = contestant_brief(
                session,
                part,
                r.no,
                &reply_dir.join("entries").join(part.as_str()),
                &reply_dir,
                &reply_dir.join(format!("direct-r{}-{part}.md", r.no)),
            );
            out.push_str(&format!(
                "===== direct contestant_brief R{} {part} =====\n",
                r.no
            ));
            out.push_str(&b.body);
        }
        let b = judge_brief(
            session,
            r.no,
            &reply_dir,
            &reply_dir.join(format!("direct-r{}-judge.json", r.no)),
        );
        out.push_str(&format!("===== direct judge_brief R{} =====\n", r.no));
        out.push_str(&b.body);
    }
    // Contestant briefs quote the running executable (here the test binary,
    // whose path differs per build) as the `--check` command.
    let exe = std::env::current_exe().expect("current exe");
    out.replace(&format!("\"{}\"", exe.display()), "\"<current_exe>\"")
}
