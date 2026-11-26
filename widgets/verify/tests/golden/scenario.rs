//! A fixed two-round verification driven through the public engine:
//! criteria from the judge (one turned down for a missing person's line),
//! confirmed with an edit, a removal and an addition; a blind round with
//! three verifiers (one reply turned down), screenshots and a recording,
//! a log gone before ingest, re-runs that match and one that differs, a
//! person's screenshot, and the judge's rulings with a defect and its
//! assignments; then a cross-check round with a refutation, a reproduced
//! defect, a dispute and a verifier's own result turned to fail, ending in
//! a verdict with a failed criterion.
//!
//! Every brief the engine sends (re-run briefs and corrections included)
//! and the report at the end are collected in order. Nothing touches the
//! disk: what the runner would find, store and re-run is made up here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use verify::engine::{Config, Effect, Engine, Input, Output, Setup};
use verify::model::types::{
    AgentChoice, AgentKind, AgentSetup, BriefKind, BuiltinStrategy, Event, EvidenceBody,
    EvidenceKey, FileFact, FileState, FileStatus, Head, Isolation, MAX_MAX_ROUNDS, Media, Millis,
    Omitted, PartId, Phase, Rebuild, Rerun, RerunItem, RerunRequest, Session, SessionId, SpecFile,
    Stored, StrategyChoice, Target, TargetFile, TargetKind, TargetManifest, VerifierChoice,
};

pub const DIR: &str = "/golden/data/verifications/ver-golden";
pub const REPO: &str = "/golden/repo";
pub const VERIFIERS: [&str; 3] = ["claude", "codex", "agy"];
const HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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
        label: "feat/cwd … main（3 ファイル）".into(),
        repo: Some(PathBuf::from(REPO)),
        base_ref: Some("main".into()),
        base_sha: Some("aaaaaaaaaaaaaaaa".into()),
        head_ref: Some("feat/cwd".into()),
        head_sha: Some(HEAD.into()),
        merge_base: Some("cccccccccccccccc".into()),
        files: vec![
            file("src/tab.rs", FileStatus::Modified, 120, vec![(40, 52)]),
            file(
                "src/cwd.rs",
                FileStatus::Renamed {
                    from: "src/dir.rs".into(),
                },
                60,
                vec![(1, 8)],
            ),
            file("docs/specs/cwd.md", FileStatus::Added, 12, vec![(1, 12)]),
        ],
        omitted: vec![Omitted {
            path: "assets/tab.png".into(),
            reason: "バイナリ（テキストではない）".into(),
        }],
        diff_bytes: 4_000,
        diff_digest: Some(11),
        captured_at: 1_000,
    }
}

/// The media an extension names (the runner sniffs the bytes; here they
/// always agree).
fn media_of(path: &str) -> Option<Media> {
    let ext = path.rsplit_once('.')?.1;
    Media::ALL.into_iter().find(|m| m.extension() == ext)
}

/// Every cited path a regular file, but those in `gone`.
fn facts(content: &str) -> Vec<FileFact> {
    verify::model::validate::cited_paths(content)
        .into_iter()
        .map(|path| FileFact {
            state: FileState::Regular {
                bytes: 1_000 + path.len() as u64,
            },
            sniffed: media_of(&path),
            by_ext: media_of(&path),
            path,
        })
        .collect()
}

fn stored(path: PathBuf) -> Stored {
    let text = path.to_string_lossy().into_owned();
    Stored {
        bytes: 1_000 + text.len() as u64,
        digest: verify::capture::digest_bytes(text.as_bytes()),
        path,
    }
}

pub(super) struct Run {
    pub(super) engine: Engine,
    pub(super) batches: Vec<Vec<Event>>,
    now: Millis,
    out: String,
}

impl Run {
    fn record(&mut self, output: &Output) {
        for effect in &output.effects {
            if let Effect::SendBrief {
                round,
                part,
                kind,
                body,
            } = effect
            {
                let what = if body.starts_with("Your ") {
                    "correction"
                } else if *kind == BriefKind::Rerun {
                    "re-run brief"
                } else {
                    "brief"
                };
                let round = if *round == 0 {
                    "criteria".to_owned()
                } else {
                    format!("R{round}")
                };
                self.out
                    .push_str(&format!("===== {round} {what} to {part} =====\n{body}"));
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
        self.batches.extend(output.batches);
    }

    fn session(&self) -> &Session {
        self.engine.session()
    }

    fn round(&self) -> u32 {
        self.session().current_round().map_or(0, |r| r.no)
    }

    fn at(&mut self) -> Millis {
        self.now += 100;
        self.now
    }

    fn sent(&mut self, part: &str, kind: BriefKind) {
        let (round, at) = (self.round(), self.at());
        self.feed(Input::Sent {
            round,
            part: PartId::new(part),
            kind,
            at,
        });
    }

    fn deliver(&mut self) {
        for part in VERIFIERS {
            self.sent(part, BriefKind::Work);
        }
    }

    fn write(&mut self, part: &str, content: &str) {
        let round = self.round();
        let at = self.at();
        self.feed(Input::ReplyFileWritten {
            round,
            part: PartId::new(part),
            content: content.into(),
            files: if part == "judge" {
                Vec::new()
            } else {
                facts(content)
            },
            at,
        });
    }

    fn judge(&mut self, content: &str) {
        let no = self.round();
        self.sent("judge", BriefKind::Work);
        self.write("judge", content);
        let round = self.session().round(no).unwrap();
        assert!(
            round.outcome.is_some(),
            "the R{no} ruling was turned down: {:?}",
            round
                .judge_brief
                .as_ref()
                .and_then(|b| b.receipt.rejected.clone())
        );
    }

    /// Stores every replied verifier's files of this round but those in
    /// `gone`, with a tail for each failing command.
    fn ingest(&mut self, gone: &[&str]) {
        let round = self.round();
        for part in VERIFIERS {
            let part = PartId::new(part);
            let mut items = Vec::new();
            let mut tails = BTreeMap::new();
            for item in self.session().reported_items(round, &part) {
                let Some(path) = item.file_path() else {
                    continue;
                };
                if gone.contains(&item.key.as_str()) {
                    continue;
                }
                let at = verify::store::round_evidence_dir(Path::new(""), round, &part).join(path);
                if let EvidenceBody::Command { exit, cmd, .. } = &item.body
                    && *exit != 0
                {
                    tails.insert(
                        item.key.clone(),
                        format!("running {cmd}\ntest cwd_inherit ... FAILED\nexit {exit}\n"),
                    );
                }
                items.push((item.key.clone(), stored(at)));
            }
            if self
                .session()
                .current_round()
                .is_some_and(|r| r.replies.contains_key(&part))
            {
                self.feed(Input::Ingested {
                    round,
                    part,
                    items,
                    tails,
                });
            }
        }
    }

    /// Runs `part`'s re-runs; `differ` names the items whose exit status
    /// comes back as 1.
    fn rerun(&mut self, part: &str, differ: &[&str]) {
        let round = self.round();
        let part = PartId::new(part);
        let items: Vec<(EvidenceKey, i32)> = self
            .session()
            .rerun_items(round, &part)
            .into_iter()
            .map(|e| match &e.body {
                EvidenceBody::Command { exit, .. } => (e.key.clone(), *exit),
                _ => unreachable!(),
            })
            .collect();
        if items.is_empty() {
            return;
        }
        let request = RerunRequest {
            nonce: "0123456789abcdef0123456789abcdef".into(),
            items: items
                .iter()
                .enumerate()
                .map(|(i, (key, _))| RerunItem {
                    key: key.clone(),
                    n: i as u32 + 1,
                    cmd_digest: 40 + i as u64,
                })
                .collect(),
            script_digest: 99,
        };
        let reply_dir = verify::store::verifier_reply_dir(self.session(), &part);
        let script = verify::store::rerun_dir(&reply_dir, round, &part).join("run.sh");
        self.feed(Input::RerunWritten {
            round,
            part: part.clone(),
            request,
            script,
        });
        self.sent(part.as_str(), BriefKind::Rerun);
        let at = self.at();
        self.feed(Input::RerunStarted {
            round,
            part: part.clone(),
            at,
        });
        for (n, (key, exit)) in items.into_iter().enumerate() {
            let log = Some(stored(verify::store::rerun_log_path(
                Path::new(""),
                round,
                &part,
                n as u32 + 1,
            )));
            let rerun = if differ.contains(&key.as_str()) {
                Rerun::Differed { exit: Some(1), log }
            } else {
                Rerun::Matched { exit, log }
            };
            let at = self.at();
            self.feed(Input::RerunReported {
                round,
                key,
                rerun,
                at,
            });
        }
    }
}

const CRITERIA_BAD: &str = r#"{"summary": "First try.", "criteria": [
 {"id": "AC-1", "statement": "Tabs open somewhere sensible", "how": "open a tab", "priority": "must", "source": "inferred"}
]}"#;

const CRITERIA: &str = r#"```json
{"summary": "The change makes new tabs start in the focused pane's directory; four criteria cover it, one from the person, one from the spec.",
 "criteria": [
  {"id": "AC-1", "statement": "New tabs open in the focused pane's cwd", "how": "cd /tmp in a pane, open a tab with Cmd-T, run pwd", "priority": "must", "source": "user"},
  {"id": "AC-2", "statement": "A cwd that no longer exists falls back to $HOME", "how": "cd into a temp dir, remove it, open a tab", "priority": "must", "source": "spec:4"},
  {"id": "AC-3", "statement": "Opening a tab stays under 50 ms", "how": "time 20 tab opens", "priority": "should", "source": "inferred"},
  {"id": "AC-4", "statement": "The change is documented", "how": "read docs/FEATURES.md", "priority": "must", "source": "request"}
 ]}
```"#;

const R1_CLAUDE_BAD: &str = r#"{"summary": "Draft.", "results": [
  {"criterion": "AC-1", "result": "pass", "evidence": ["e1"]}],
 "evidence": [{"id": "e1", "kind": "command", "cmd": "cargo test", "cwd": ".", "exit": 0, "log": "../e1.log", "rerun": true}]}"#;

const R1_CLAUDE: &str = r#"{"summary": "Ran the cwd tests and checked a new tab by hand.",
 "environment": "macOS 26.1, rustc 1.92, debug build",
 "results": [
  {"criterion": "AC-1", "result": "pass", "note": "pwd shows /tmp.", "evidence": ["e1", "e2"]},
  {"criterion": "AC-2", "result": "pass", "evidence": ["e1"]},
  {"criterion": "AC-3", "result": "pass", "note": "31 ms median.", "evidence": ["e3"]},
  {"criterion": "AC-5", "result": "not_tested", "note": "Out of my strategy's reach this round."}
 ],
 "evidence": [
  {"id": "e1", "kind": "command", "cmd": "cargo test -p app cwd_", "cwd": ".", "exit": 0, "log": "logs/1.log", "rerun": true},
  {"id": "e2", "kind": "file", "path": "shots/new tab.png", "caption": "pwd in the new tab shows /tmp", "shows": ["AC-1"]},
  {"id": "e3", "kind": "command", "cmd": "for i in $(seq 20); do\n  ./bench open-tab\ndone", "cwd": "bench", "exit": 0, "log": "logs/3.log", "rerun": false}
 ]}"#;

const R1_CODEX: &str = r#"{"summary": "Tried odd directories: a removed cwd breaks the new tab.",
 "environment": "macOS 26.1 (sandboxed)",
 "results": [
  {"criterion": "AC-1", "result": "fail", "note": "With a symlinked cwd the tab opens in $HOME.", "evidence": ["e1", "e2"]},
  {"criterion": "AC-2", "result": "pass", "evidence": ["e3"]},
  {"criterion": "AC-3", "result": "blocked", "note": "No display in the sandbox."},
  {"criterion": "AC-5", "result": "pass", "evidence": ["e3"]}
 ],
 "evidence": [
  {"id": "e1", "kind": "command", "cmd": "cargo test -p app cwd_symlink -- --exact", "cwd": ".", "exit": 101, "log": "logs/1.log", "rerun": true},
  {"id": "e2", "kind": "file", "path": "video/symlink.mov", "caption": "cd into a symlink, Cmd-T, pwd",
   "marks": [{"t": "00:04", "note": "Cmd-T pressed"}, {"t": "00:06.5", "note": "the new tab shows $HOME"}]},
  {"id": "e3", "kind": "command", "cmd": "cargo test -p app cwd_gone", "cwd": ".", "exit": 0, "log": "logs/3.log", "rerun": true}
 ],
 "defects": [
  {"id": "1", "claim": "A symlinked cwd is resolved to its target's parent", "severity": "high", "category": "falsify", "confidence": "medium",
   "location": {"path": "src/cwd.rs", "line": 4}, "repro": "1. ln -s /tmp /tmp/l 2. cd /tmp/l 3. Cmd-T 4. pwd", "evidence": ["e1"]}
 ]}"#;

const R1_AGY: &str = r#"{"summary": "Long paths and a Japanese locale.",
 "results": [
  {"criterion": "AC-1", "result": "pass", "evidence": ["e1"]},
  {"criterion": "AC-2", "result": "pass", "evidence": ["e1"]},
  {"criterion": "AC-3", "result": "pass", "evidence": ["e2"]},
  {"criterion": "AC-5", "result": "blocked", "note": "Quitting kills my pane.", "evidence": ["o1"]}
 ],
 "evidence": [
  {"id": "e1", "kind": "command", "cmd": "LANG=ja_JP.UTF-8 cargo test -p app cwd_", "cwd": ".", "exit": 0, "log": "logs/1.log", "rerun": true},
  {"id": "e2", "kind": "command", "cmd": "./bench open-tab --long-path", "cwd": "bench", "exit": 0, "log": "logs/2.log", "rerun": false},
  {"id": "o1", "kind": "observation", "text": "The pane closed when I pressed Cmd-Q."}
 ]}"#;

const R1_JUDGE: &str = r#"{"summary": "AC-1 fails for a symlinked cwd; AC-2 holds; AC-3 rests on one stored benchmark; AC-5 is untested.",
 "rulings": [
  {"criterion": "AC-1", "ruling": "failed", "rationale": "codex's symlink test fails and the recording shows $HOME.", "evidence": ["codex/r1/e1", "codex/r1/e2"]},
  {"criterion": "AC-2", "ruling": "verified", "rationale": "claude's test re-ran with the same status.", "evidence": ["claude/r1/e1"]},
  {"criterion": "AC-3", "ruling": "inconclusive", "rationale": "Only claude's benchmark is stored; agy's log was gone before ingest and counts for nothing.", "evidence": ["claude/r1/e3"]},
  {"criterion": "AC-5", "ruling": "inconclusive", "rationale": "Only codex passed it, on a test that does not quit."}
 ],
 "defect_ops": [
  {"op": "add", "id": "D1", "from": ["codex/1"], "claim": "A symlinked cwd is resolved to its target's parent",
   "repro": "1. ln -s /tmp /tmp/l 2. cd /tmp/l 3. Cmd-T 4. pwd", "location": {"path": "src/cwd.rs", "line": 4},
   "severity": "high", "category": "falsify", "confidence": "medium", "status": "open", "rationale": "Seen by codex only."}
 ],
 "assign": {"claude": ["codex/AC-1", "D1"], "agy": ["codex/AC-5"]}}"#;

const R2_CLAUDE: &str = r#"{"summary": "codex's symlink test passes here; the defect is real for a nested link.",
 "cross_checks": [
  {"subject": "codex/AC-1", "outcome": "not_reproduced", "note": "cwd_symlink passes in my worktree.", "evidence": ["e1"]},
  {"subject": "D1", "outcome": "reproduced", "note": "A link inside a link resolves wrong.", "evidence": ["e2"]}
 ],
 "evidence": [
  {"id": "e1", "kind": "command", "cmd": "cargo test -p app cwd_symlink -- --exact", "cwd": ".", "exit": 0, "log": "logs/1.log", "rerun": true},
  {"id": "e2", "kind": "command", "cmd": "./scripts/nested-link.sh", "cwd": ".", "exit": 1, "log": "logs/2.log", "rerun": false}
 ]}"#;

const R2_CODEX: &str = r#"{"summary": "Nothing assigned to me."}"#;

const R2_AGY: &str = r#"{"summary": "codex's AC-5 test never quits the app; a cold start is slow.",
 "results": [
  {"criterion": "AC-3", "result": "fail", "note": "A cold start takes 72 ms.", "evidence": ["e1"]}
 ],
 "evidence": [
  {"id": "e1", "kind": "command", "cmd": "./bench open-tab --cold", "cwd": "bench", "exit": 1, "log": "logs/1.log", "rerun": false}
 ],
 "cross_checks": [
  {"subject": "codex/AC-5", "outcome": "disputed", "note": "cwd_gone does not exercise quitting at all."}
 ]}"#;

const R2_JUDGE: &str = r#"{"summary": "codex's AC-1 failure did not reproduce; the nested-link defect did; a cold start breaks AC-3. AC-5 does not apply to this change.",
 "rulings": [
  {"criterion": "AC-1", "ruling": "verified", "rationale": "claude's re-run matched and the person's screenshot shows /tmp; codex's failure did not reproduce.", "evidence": ["claude/r1/e1", "user/1"]},
  {"criterion": "AC-2", "ruling": "same"},
  {"criterion": "AC-3", "ruling": "failed", "rationale": "agy's cold-start benchmark takes 72 ms, over the limit.", "evidence": ["agy/r2/e1"]},
  {"criterion": "AC-5", "ruling": "not_applicable", "rationale": "Session restore on quit is not part of this change."}
 ],
 "defect_ops": [
  {"op": "update", "id": "D1", "status": "agreed", "rationale": "claude reproduced it with a nested link."}
 ],
 "verdict": {"decision": "pass_with_issues", "rationale": "Every must criterion holds; the slow cold start (should) and the nested-link defect remain.", "blocking": ["AC-3", "D1"]}}"#;

/// Runs the scenario and returns every brief and correction sent, then
/// (separately) the session at the end.
pub fn run() -> (String, Session) {
    let run = play();
    let session = run.engine.session().clone();
    assert!(session.closed, "the scenario ends in a verdict");
    (run.out, session)
}

/// The scenario given one more round (+1) after its verdict: the briefs
/// round 3 sends once the removed worktrees are made again.
pub fn extended() -> String {
    let mut run = play();
    run.out.clear();
    let at = run.at();
    run.feed(Input::UserExtend { by: 1, at });
    for part in VERIFIERS {
        let path = verify::worktree::path_for(
            Path::new(REPO),
            &SessionId::new("ver-golden"),
            &PartId::new(part),
        );
        run.feed(Input::WorktreeMade {
            part: PartId::new(part),
            path,
        });
    }
    let now = run.at();
    run.feed(Input::Tick { now });
    assert_eq!(run.round(), 3, "round 3 started");
    run.out
}

pub(super) fn play() -> Run {
    let panel = vec![
        VerifierChoice {
            agent: AgentChoice {
                kind: AgentKind::Claude,
                setup: AgentSetup {
                    model: Some("opus".into()),
                    effort: Some("high".into()),
                },
            },
            strategy: StrategyChoice::Builtin(BuiltinStrategy::Conformance),
        },
        VerifierChoice {
            agent: AgentKind::Codex.into(),
            strategy: StrategyChoice::Builtin(BuiltinStrategy::Falsify),
        },
        VerifierChoice {
            agent: AgentKind::Agy.into(),
            strategy: StrategyChoice::Custom("Odd locales & very long paths".into()),
        },
    ];
    let (engine, created) = Engine::create_in(
        SessionId::new("ver-golden"),
        Setup {
            request: "Verify that new tabs inherit the cwd; it must stay \"fast\" & <safe>.",
            target: Target {
                kind: TargetKind::Diff,
                repo: PathBuf::from(REPO),
                base: Some("main".into()),
                head: Head::Ref("feat/cwd".into()),
                paths: Vec::new(),
                spec: Some("docs/specs/cwd.md".into()),
            },
            manifest: manifest(),
            rebuild: Rebuild {
                head_sha: HEAD.into(),
                worktree_patch: None,
                untracked: Vec::new(),
            },
            spec: Some(SpecFile {
                path: "docs/specs/cwd.md".into(),
                bytes: 600,
                lines: 12,
                digest: 5,
            }),
            isolation: Isolation::Worktree,
            user_criteria: vec!["New tabs open in the focused pane's cwd".into()],
            confirm: true,
            workdir: PathBuf::from(REPO),
            main_worktree: PathBuf::from(REPO),
            max_rounds: 2,
            round_cap: MAX_MAX_ROUNDS,
            judge: AgentKind::Claude.into(),
            panel: &panel,
        },
        Path::new(DIR),
        Config::default(),
        1_000,
    )
    .expect("the scenario's setup is valid");
    let mut run = Run {
        engine,
        batches: created.batches,
        now: 1_000,
        out: String::new(),
    };
    run.feed(Input::Start { now: 1_000 });

    // Criteria: one draft turned down, then held for the person.
    run.sent("judge", BriefKind::Work);
    run.write("judge", CRITERIA_BAD);
    run.sent("judge", BriefKind::Work);
    run.write("judge", CRITERIA);
    assert_eq!(run.session().phase(), Phase::Confirming);
    for edit in [
        verify::model::types::CriteriaEdit::Edit {
            id: "AC-3".into(),
            statement: None,
            how: Some("hyperfine 'app --open-tab' (20 runs)".into()),
            priority: None,
        },
        verify::model::types::CriteriaEdit::Remove { id: "AC-4".into() },
        verify::model::types::CriteriaEdit::Add {
            statement: "Quitting keeps every tab's cwd for the next launch".into(),
            how: "Cmd-Q, relaunch, check each tab".into(),
            priority: verify::model::types::Priority::Should,
        },
    ] {
        let at = run.at();
        run.feed(Input::CriteriaEdit { edit, at });
    }
    let at = run.at();
    run.feed(Input::UserStartVerify { edits: Vec::new(), at });
    for part in VERIFIERS {
        let path = verify::worktree::path_for(
            Path::new(REPO),
            &SessionId::new("ver-golden"),
            &PartId::new(part),
        );
        run.feed(Input::WorktreeMade {
            part: PartId::new(part),
            path,
        });
    }
    let now = run.at();
    run.feed(Input::Tick { now });
    assert_eq!(run.round(), 1, "round 1 started");

    // Round 1: blind; claude's first reply is turned down.
    run.deliver();
    run.feed(Input::UserIntervention {
        to: vec![PartId::new("codex")],
        body: "Try a cwd behind a symlink too.".into(),
        at: 5_000,
    });
    run.write("claude", R1_CLAUDE_BAD);
    run.sent("claude", BriefKind::Work);
    run.write("claude", R1_CLAUDE);
    run.write("codex", R1_CODEX);
    run.write("agy", R1_AGY);
    assert_eq!(run.session().phase(), Phase::Ingesting);
    let at = run.at();
    run.feed(Input::AttachEvidence {
        key: EvidenceKey::user(1),
        body: EvidenceBody::File {
            path: "Screenshot 2026-11-24.png".into(),
            caption: Some("pwd shows /tmp in the new tab".into()),
            media: Some(Media::Png),
            marks: Vec::new(),
            shows: Vec::new(),
        },
        stored: stored(PathBuf::from("evidence/user/1.png")),
        shows: vec!["AC-1".into()],
        at,
    });
    run.ingest(&["agy/r1/e2"]);
    run.rerun("claude", &[]);
    run.rerun("codex", &["codex/r1/e3"]);
    run.rerun("agy", &[]);
    assert_eq!(run.session().phase(), Phase::Ruling);
    run.judge(R1_JUDGE);

    // Round 2: cross-checks, ending in the verdict.
    assert_eq!(run.round(), 2);
    run.deliver();
    run.write("claude", R2_CLAUDE);
    run.write("codex", R2_CODEX);
    run.write("agy", R2_AGY);
    run.ingest(&[]);
    run.rerun("claude", &[]);
    run.judge(R2_JUDGE);
    for part in VERIFIERS {
        run.feed(Input::WorktreeRemoved {
            part: PartId::new(part),
        });
    }
    run
}
