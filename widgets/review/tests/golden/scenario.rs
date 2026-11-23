//! A fixed three-round review driven through the public engine: a blind
//! round with three reviewers and one rejected reply; a consolidation with
//! add, merge and dismiss; a cross-examination round with a dispute, a
//! refine, a withdraw and a new finding; and a final round ending in a
//! verdict with must-fix findings.
//!
//! Every brief the engine issues, every correction it sends, and the report
//! at the end are collected in order.

use std::path::{Path, PathBuf};

use review::engine::{Config, Effect, Engine, Input, Output, Setup};
use review::model::export;
use review::model::types::{
    AgentChoice, AgentKind, AgentSetup, BuiltinLens, FileStatus, Head, LensChoice, Millis, Omitted,
    PartId, ReviewerChoice, SessionId, Target, TargetFile, TargetKind, TargetManifest,
};

pub const DIR: &str = "/golden/reviews/rev-golden";

fn file(path: &str, status: FileStatus, lines: u32, changed: Vec<(u32, u32)>) -> TargetFile {
    TargetFile {
        path: path.into(),
        status,
        lines,
        bytes: u64::from(lines) * 30,
        additions: changed.iter().map(|(a, b)| b - a + 1).sum(),
        deletions: 3,
        changed,
        digest: 7,
    }
}

fn manifest() -> TargetManifest {
    TargetManifest {
        label: "feat/lock … main（4 ファイル）".into(),
        repo: Some(PathBuf::from("/golden/repo")),
        base_ref: Some("main".into()),
        base_sha: Some("aaaaaaaaaaaaaaaa".into()),
        head_ref: Some("feat/lock".into()),
        head_sha: Some("bbbbbbbbbbbbbbbb".into()),
        merge_base: Some("cccccccccccccccc".into()),
        document_name: None,
        files: vec![
            file("src/store.rs", FileStatus::Modified, 120, vec![(40, 52)]),
            file(
                "src/lock.rs",
                FileStatus::Renamed {
                    from: "src/flock.rs".into(),
                },
                60,
                vec![(1, 8)],
            ),
            file("README.md", FileStatus::Added, 10, vec![(1, 10)]),
        ],
        omitted: vec![Omitted {
            path: "logo.png".into(),
            reason: "バイナリ（テキストではない）".into(),
        }],
        diff_bytes: 4_000,
        diff_digest: Some(11),
        captured_at: 1_000,
    }
}

struct Run {
    engine: Engine,
    now: Millis,
    out: String,
}

impl Run {
    fn record(&mut self, output: &Output) {
        for effect in &output.effects {
            if let Effect::SendBrief { round, part, body } = effect {
                let what = if body.starts_with("Your ") {
                    "correction"
                } else {
                    "brief"
                };
                self.out
                    .push_str(&format!("===== R{round} {what} to {part} =====\n{body}"));
                if !body.ends_with('\n') {
                    self.out.push('\n');
                }
            }
        }
    }

    fn feed(&mut self, input: Input) {
        let output = self
            .engine
            .handle(input)
            .expect("the scenario's input is valid");
        self.record(&output);
    }

    fn round(&self) -> u32 {
        self.engine.session().current_round().unwrap().no
    }

    fn sent(&mut self, part: &str) {
        self.now += 100;
        let (round, at) = (self.round(), self.now);
        self.feed(Input::Sent {
            round,
            part: PartId::new(part),
            at,
        });
    }

    fn deliver(&mut self) {
        for part in ["claude", "codex", "agy"] {
            self.sent(part);
        }
    }

    fn reply(&mut self, part: &str, content: &str) {
        self.now += 1_000;
        let (round, at) = (self.round(), self.now);
        self.feed(Input::ReplyFileWritten {
            round,
            part: PartId::new(part),
            content: content.into(),
            at,
        });
    }

    fn lead(&mut self, content: &str) {
        self.sent("lead");
        self.reply("lead", content);
    }
}

const R1_CLAUDE: &str = r#"{"summary": "Read the store and the lock.",
 "findings": [
  {"id": "1", "location": {"path": "src/store.rs", "line": 44, "end_line": 48}, "severity": "high", "category": "correctness", "confidence": "high",
   "claim": "A torn last line is kept and the next batch joins it", "evidence": "store.rs:44 returns before set_len when kept == len.", "fix": "Always truncate to kept."},
  {"id": "2", "location": {"path": "src/lock.rs", "line": 3}, "severity": "info", "category": "design", "confidence": "low",
   "claim": "The lock module could be private", "evidence": "Only store.rs uses it."}
 ],
 "coverage": {"reviewed": ["src/"], "skipped": [{"path": "README.md", "reason": "docs"}]}}"#;

const R1_CODEX: &str = r#"```json
{"summary": "Checked trust boundaries.",
 "findings": [
  {"id": "a", "location": {"path": "src/store.rs", "line": 46}, "severity": "medium", "category": "security", "confidence": "medium",
   "claim": "A <torn> line can be replayed as another event", "evidence": "serde accepts the joined line \"as is\".", "fix": "Truncate on open."}
 ]}
```"#;

const R1_AGY: &str = r#"{"summary": "Looked at the API.",
 "findings": [
  {"id": "x1", "location": null, "severity": "medium", "category": "custom", "confidence": "high",
   "claim": "Renaming flock.rs to lock.rs breaks the public path", "evidence": "lib.rs re-exported flock."}
 ],
 "coverage": {"reviewed": ["src/lock.rs"]}}"#;

const R1_LEAD: &str = r#"{"summary": "Two findings; the design nit is dismissed.",
 "ops": [
  {"op": "add", "id": "F1", "from": ["claude/1"], "claim": "Torn last line joins the next batch",
   "location": {"path": "src/store.rs", "line": 44, "end_line": 48}, "severity": "high", "category": "correctness",
   "confidence": "high", "status": "open", "rationale": "Traced by claude.", "fix": "Truncate to kept."},
  {"op": "merge", "id": "F1", "from": ["codex/a"]},
  {"op": "add", "id": "F2", "from": ["agy/x1"], "claim": "The rename breaks a public path", "severity": "medium",
   "category": "custom", "confidence": "medium", "status": "open", "rationale": "Needs a check of lib.rs."},
  {"op": "dismiss", "raw": "claude/2", "reason": "A style preference, not a defect."}
 ],
 "questions": {"codex": "Is F2 a compatibility break for users?"}}"#;

const R2_CLAUDE: &str = r#"{"summary": "F2 is not public.",
 "stances": [{"finding": "F2", "stance": "dispute", "note": "flock was never exported.", "evidence": "lib.rs has no pub use."}]}"#;

const R2_CODEX: &str = r#"{"summary": "Withdrawing my part of F1; F2 needs a note.",
 "stances": [
  {"finding": "F1", "stance": "withdraw"},
  {"finding": "F2", "stance": "refine", "note": "Breaks only the docs link.", "severity": "low"}
 ]}"#;

const R2_AGY: &str = r#"{"summary": "F1 holds; one more.",
 "findings": [
  {"id": "x2", "location": {"path": "src/store.rs", "line": 50}, "severity": "low", "category": "tests", "confidence": "high",
   "claim": "No test covers the torn line", "evidence": "tests have no torn case."}
 ],
 "stances": [{"finding": "F1", "stance": "confirm"}]}"#;

const R2_LEAD: &str = r#"{"summary": "F1 agreed; F2 disputed; F3 added.",
 "ops": [
  {"op": "update", "id": "F1", "status": "agreed"},
  {"op": "update", "id": "F2", "status": "disputed", "rationale": "claude and agy disagree on the export."},
  {"op": "add", "id": "F3", "from": ["agy/x2"], "claim": "No test covers the torn line",
   "location": {"path": "src/store.rs", "line": 50}, "severity": "low", "category": "tests",
   "confidence": "high", "status": "open", "rationale": "Needed for F1's fix."}
 ],
 "questions": {"claude": "Does F3 need more than one test?"}}"#;

const R3_CLAUDE: &str = r#"{"summary": "One test is enough.",
 "stances": [{"finding": "F3", "stance": "confirm"}]}"#;

const R3_CODEX: &str = r#"{"summary": "Agree.",
 "stances": [{"finding": "F3", "stance": "confirm"}]}"#;

const R3_AGY: &str = r#"{"summary": "Nothing more."}"#;

const R3_LEAD: &str = r#"{"summary": "Settled.",
 "ops": [{"op": "update", "id": "F3", "status": "agreed"}],
 "verdict": {"decision": "request_changes", "rationale": "F1 loses data; F3 guards the fix.", "must_fix": ["F1", "F3"]}}"#;

/// Runs the scenario and returns every brief and correction sent, then
/// (separately) the report at the end.
pub fn run() -> (String, String) {
    let panel = vec![
        ReviewerChoice {
            agent: AgentChoice {
                kind: AgentKind::Claude,
                setup: AgentSetup {
                    model: Some("opus".into()),
                    effort: Some("high".into()),
                },
            },
            lens: LensChoice::Builtin(BuiltinLens::Correctness),
        },
        ReviewerChoice {
            agent: AgentKind::Codex.into(),
            lens: LensChoice::Builtin(BuiltinLens::Security),
        },
        ReviewerChoice {
            agent: AgentKind::Agy.into(),
            lens: LensChoice::Custom("API compatibility & migration".into()),
        },
    ];
    let (engine, _) = Engine::create_in(
        SessionId::new("rev-golden"),
        Setup {
            request: "Review the lock rewrite; it must stay \"crash-safe\" & <fast>.",
            target: Target {
                kind: TargetKind::Git,
                path: Some(PathBuf::from("/golden/repo")),
                base: Some("main".into()),
                head: Head::Ref("feat/lock".into()),
                paths: Vec::new(),
                text: None,
                workdir: None,
            },
            manifest: manifest(),
            workdir: None,
            max_rounds: 3,
            lead: AgentKind::Claude.into(),
            panel: &panel,
        },
        Path::new(DIR),
        Config::default(),
        1_000,
    )
    .expect("the scenario's setup is valid");
    let mut run = Run {
        engine,
        now: 1_000,
        out: String::new(),
    };
    run.feed(Input::Start { now: 1_000 });

    // Round 1: blind, one reply turned down and corrected.
    run.deliver();
    run.reply("claude", r#"{"summary": "", "findings": [{"id": "1"}]}"#);
    run.sent("claude");
    run.reply("claude", R1_CLAUDE);
    run.feed(Input::UserIntervention {
        to: vec![PartId::new("codex")],
        body: "Look at the lock's error paths too.".into(),
        at: 5_000,
    });
    run.reply("codex", R1_CODEX);
    run.reply("agy", R1_AGY);
    run.lead(R1_LEAD);

    // Round 2: cross-examination.
    run.deliver();
    run.reply("claude", R2_CLAUDE);
    run.reply("codex", R2_CODEX);
    run.reply("agy", R2_AGY);
    run.lead(R2_LEAD);

    // Round 3: the last, ending in the verdict.
    run.deliver();
    run.reply("claude", R3_CLAUDE);
    run.reply("codex", R3_CODEX);
    run.reply("agy", R3_AGY);
    run.lead(R3_LEAD);

    let session = run.engine.session();
    assert!(session.closed, "the scenario ends in a verdict");
    // AC-12: every raw finding is placed or dismissed.
    for raw in &session.raw {
        assert!(
            session.placement(&raw.key).is_some(),
            "{} unplaced",
            raw.key
        );
    }
    (run.out, export::report_markdown(session))
}
