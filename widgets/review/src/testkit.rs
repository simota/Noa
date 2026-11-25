//! A review driven through the engine for unit tests: a fixed git-like
//! target, a three-reviewer panel, and helpers that write agent replies.
//! Every batch the engine commits is kept, and the log is checked to fold
//! into the engine's session after every step.

use std::path::{Path, PathBuf};

use crate::engine::{Config, Effect, Engine, Input, Output, Setup};
use crate::model::reduce::{self, Event};
use crate::model::types::{
    AgentChoice, AgentKind, BuiltinLens, FileStatus, Head, LensChoice, Millis, Omitted, PartId,
    ReviewerChoice, RoundNo, SessionId, Target, TargetFile, TargetKind, TargetManifest,
};

pub fn p(id: &str) -> PartId {
    PartId::new(id)
}

pub fn manifest() -> TargetManifest {
    let file = |path: &str, lines, changed: Vec<(u32, u32)>| TargetFile {
        path: path.into(),
        status: FileStatus::Modified,
        lines,
        bytes: u64::from(lines) * 30,
        additions: changed.iter().map(|(a, b)| b - a + 1).sum(),
        deletions: 2,
        changed,
        digest: 1,
    };
    TargetManifest {
        label: "feat/store … main（3 ファイル）".into(),
        repo: Some(PathBuf::from("/repo")),
        base_ref: Some("main".into()),
        base_sha: Some("1111111111111111".into()),
        head_ref: Some("feat/store".into()),
        head_sha: Some("2222222222222222".into()),
        merge_base: Some("3333333333333333".into()),
        document_name: None,
        files: vec![
            file("src/store.rs", 200, vec![(120, 134)]),
            file("src/engine.rs", 80, vec![(10, 12)]),
        ],
        omitted: vec![Omitted {
            path: "assets/logo.png".into(),
            reason: "バイナリ（テキストではない）".into(),
        }],
        diff_bytes: 900,
        diff_digest: Some(9),
        uncommitted: false,
        captured_at: 1,
    }
}

pub fn target() -> Target {
    Target {
        kind: TargetKind::Git,
        path: Some(PathBuf::from("/repo")),
        base: Some("main".into()),
        head: Head::Ref("feat/store".into()),
        paths: Vec::new(),
        text: None,
        workdir: None,
    }
}

pub fn reviewer(kind: AgentKind, lens: BuiltinLens) -> ReviewerChoice {
    ReviewerChoice {
        agent: kind.into(),
        lens: LensChoice::Builtin(lens),
    }
}

pub fn panel() -> Vec<ReviewerChoice> {
    vec![
        reviewer(AgentKind::Claude, BuiltinLens::Correctness),
        reviewer(AgentKind::Codex, BuiltinLens::Security),
        reviewer(AgentKind::Agy, BuiltinLens::Design),
    ]
}

pub struct Rig {
    pub engine: Engine,
    pub log: Vec<Vec<Event>>,
    pub now: Millis,
}

impl Rig {
    pub fn with(max_rounds: RoundNo, panel: &[ReviewerChoice]) -> Rig {
        let (engine, out) = Engine::create_in(
            SessionId::new("rev-1"),
            Setup {
                request: "Check the store rewrite.",
                target: target(),
                manifest: manifest(),
                workdir: None,
                max_rounds,
                round_cap: crate::model::types::MAX_MAX_ROUNDS,
                lead: AgentChoice::from(AgentKind::Claude),
                panel,
            },
            Path::new("/data/reviews/rev-1"),
            Config::default(),
            1,
        )
        .unwrap();
        let mut rig = Rig {
            engine,
            log: out.batches,
            now: 10,
        };
        rig.feed(Input::Start { now: 10 });
        rig
    }

    pub fn new(max_rounds: RoundNo) -> Rig {
        Rig::with(max_rounds, &panel())
    }

    pub fn tick(&mut self) -> Millis {
        self.now += 10;
        self.now
    }

    pub fn feed(&mut self, input: Input) -> Output {
        let out = self.engine.handle(input).unwrap();
        self.log.extend(out.batches.iter().cloned());
        assert_eq!(
            reduce::replay(&self.log).unwrap(),
            *self.engine.session(),
            "the log no longer folds into the session"
        );
        out
    }

    pub fn try_feed(&mut self, input: Input) -> Result<Output, String> {
        match self.engine.handle(input) {
            Ok(out) => {
                self.log.extend(out.batches.iter().cloned());
                Ok(out)
            }
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn round(&self) -> RoundNo {
        self.engine.session().current_round().map_or(0, |r| r.no)
    }

    /// Marks every reviewer's brief of the current round sent and started.
    pub fn deliver(&mut self) {
        let round = self.round();
        let parts: Vec<PartId> = self.engine.session().reviewer_ids().cloned().collect();
        for part in parts {
            let at = self.tick();
            self.feed(Input::Sent { round, part, at });
        }
    }

    pub fn reply(&mut self, part: &str, content: &str) -> Output {
        let round = self.round();
        let at = self.tick();
        self.feed(Input::ReplyFileWritten {
            round,
            part: p(part),
            content: content.into(),
            at,
        })
    }

    pub fn lead(&mut self, content: &str) -> Output {
        let round = self.round();
        let at = self.tick();
        self.feed(Input::Sent {
            round,
            part: p("lead"),
            at,
        });
        self.reply("lead", content)
    }

    /// The text last sent to `part`.
    pub fn sent_to(out: &Output, part: &str) -> Option<String> {
        out.effects.iter().rev().find_map(|e| match e {
            Effect::SendBrief { part: to, body, .. } if to.as_str() == part => Some(body.clone()),
            _ => None,
        })
    }

    /// The current brief text of `part`.
    pub fn brief(&self, part: &str) -> String {
        self.engine
            .session()
            .current_round()
            .and_then(|r| r.brief(&p(part)))
            .map(|b| b.body.clone())
            .unwrap_or_default()
    }

    pub fn rejected(&self, part: &str) -> Option<String> {
        self.engine
            .session()
            .current_round()
            .and_then(|r| r.brief(&p(part)))
            .and_then(|b| b.receipt.rejected.clone())
    }
}

/// A reviewer reply with one finding per `(id, path, line, severity,
/// category)`, and stances `(finding, stance, note)`.
pub fn review(
    findings: &[(&str, &str, u32, &str, &str)],
    stances: &[(&str, &str, &str)],
) -> String {
    let findings: Vec<String> = findings
        .iter()
        .map(|(id, path, line, sev, cat)| {
            format!(
                r#"{{"id": "{id}", "location": {{"path": "{path}", "line": {line}}}, "severity": "{sev}", "category": "{cat}", "confidence": "high", "claim": "Claim {id} at {path}:{line}", "evidence": "Evidence for {id}.", "fix": "Fix {id}."}}"#
            )
        })
        .collect();
    let stances: Vec<String> = stances
        .iter()
        .map(|(f, s, note)| format!(r#"{{"finding": "{f}", "stance": "{s}", "note": "{note}"}}"#))
        .collect();
    format!(
        r#"{{"summary": "Reviewed.", "findings": [{}], "stances": [{}], "coverage": {{"reviewed": ["src/"]}}}}"#,
        findings.join(", "),
        stances.join(", ")
    )
}

/// A blind reply without the `stances` field.
pub fn blind(findings: &[(&str, &str, u32, &str, &str)]) -> String {
    review(findings, &[]).replace(r#", "stances": []"#, "")
}

/// Round 1 with one finding each, and the lead's consolidation adding F1
/// (claude/1 + codex/1) and F2 (agy/1).
pub fn through_round_one(rig: &mut Rig) {
    rig.deliver();
    rig.reply(
        "claude",
        &blind(&[("1", "src/store.rs", 128, "high", "correctness")]),
    );
    rig.reply(
        "codex",
        &blind(&[("1", "src/store.rs", 130, "medium", "security")]),
    );
    rig.reply(
        "agy",
        &blind(&[("1", "src/engine.rs", 11, "low", "design")]),
    );
    rig.lead(LEAD_R1);
}

pub const LEAD_R1: &str = r#"{"summary": "Two findings.",
 "ops": [
  {"op": "add", "id": "F1", "from": ["claude/1", "codex/1"], "claim": "Torn line kept", "location": {"path": "src/store.rs", "line": 128}, "severity": "high", "category": "correctness", "confidence": "high", "status": "open", "rationale": "Both saw it."},
  {"op": "add", "id": "F2", "from": ["agy/1"], "claim": "Engine knows the store", "location": null, "severity": "low", "category": "design", "confidence": "medium", "status": "open", "rationale": "Layering."}
 ],
 "questions": {"codex": "Does F1 matter for security?"}}"#;
