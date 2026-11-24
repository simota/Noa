//! A verification driven through the engine for unit tests: a fixed
//! git-like target, a three-verifier panel, and helpers that write agent
//! replies, keeping every batch the engine commits and checking that the
//! log folds into the engine's session after every step.
//!
//! Nothing here touches the disk: what the runner would find at a cited
//! path ([`FileFact`]), store ([`Stored`]) and re-run ([`Rerun`]) is made
//! up, as the I/O modules' bodies are not this unit's.

use std::path::{Path, PathBuf};

use crate::engine::{Config, Effect, Engine, Input, Output, Setup};
use crate::model::reduce::{self, Event};
use crate::model::types::{
    AgentChoice, AgentKind, BriefKind, BuiltinStrategy, CRITERIA_ROUND, EvidenceBody, EvidenceKey,
    FileFact, FileState, FileStatus, Head, Isolation, MAX_MAX_ROUNDS, Media, Millis, Omitted,
    PartId, Phase, Rebuild, Rerun, RerunItem, RerunRequest, RoundNo, SessionId, Stored,
    StrategyChoice, Target, TargetFile, TargetKind, TargetManifest, VerifierChoice,
};

pub fn p(id: &str) -> PartId {
    PartId::new(id)
}

pub fn key(text: &str) -> EvidenceKey {
    EvidenceKey::new(text)
}

pub const HEAD_SHA: &str = "2222222222222222222222222222222222222222";

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
        label: "feat/cwd … main（2 ファイル）".into(),
        repo: Some(PathBuf::from("/repo")),
        base_ref: Some("main".into()),
        base_sha: Some("1111111111111111".into()),
        head_ref: Some("feat/cwd".into()),
        head_sha: Some(HEAD_SHA.into()),
        merge_base: Some("3333333333333333".into()),
        files: vec![
            file("src/tab.rs", 200, vec![(120, 134)]),
            file("src/store.rs", 80, vec![(10, 12)]),
        ],
        omitted: vec![Omitted {
            path: "assets/logo.png".into(),
            reason: "バイナリ（テキストではない）".into(),
        }],
        diff_bytes: 900,
        diff_digest: Some(9),
        captured_at: 1,
    }
}

pub fn target() -> Target {
    Target {
        kind: TargetKind::Diff,
        repo: PathBuf::from("/repo"),
        base: Some("main".into()),
        head: Head::Ref("feat/cwd".into()),
        paths: Vec::new(),
        spec: None,
    }
}

pub fn verifier(kind: AgentKind, strategy: BuiltinStrategy) -> VerifierChoice {
    VerifierChoice {
        agent: kind.into(),
        strategy: StrategyChoice::Builtin(strategy),
    }
}

/// The default panel (L1-9): claude/仕様適合, codex/反証, claude/回帰
/// (`claude-2`).
pub fn panel() -> Vec<VerifierChoice> {
    vec![
        verifier(AgentKind::Claude, BuiltinStrategy::Conformance),
        verifier(AgentKind::Codex, BuiltinStrategy::Falsify),
        verifier(AgentKind::Claude, BuiltinStrategy::Regression),
    ]
}

pub const VERIFIERS: [&str; 3] = ["claude", "codex", "claude-2"];

/// The person's one criterion line.
pub const USER_LINE: &str = "Tabs inherit the focused pane's cwd";

/// The judge's criteria: AC-1 (the person's), AC-2 (request), AC-3
/// (inferred, should).
pub const CRITERIA: &str = r#"{"summary": "Three criteria.", "criteria": [
 {"id": "AC-1", "statement": "Tabs inherit the focused pane's cwd", "how": "cd /tmp; open a tab; pwd", "priority": "must", "source": "user"},
 {"id": "AC-2", "statement": "The store survives a crash", "how": "kill -9 mid-write; reopen", "priority": "must", "source": "request"},
 {"id": "AC-3", "statement": "Startup stays under 200 ms", "how": "hyperfine ./app", "priority": "should", "source": "inferred"}
]}"#;

#[derive(Debug, Clone)]
pub struct Options {
    pub max_rounds: RoundNo,
    pub panel: Vec<VerifierChoice>,
    pub isolation: Isolation,
    pub confirm: bool,
    pub workdir: PathBuf,
    pub user_criteria: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            max_rounds: 2,
            panel: panel(),
            isolation: Isolation::Worktree,
            confirm: true,
            workdir: PathBuf::from("/repo"),
            user_criteria: vec![USER_LINE.to_owned()],
        }
    }
}

pub struct Rig {
    pub engine: Engine,
    pub log: Vec<Vec<Event>>,
    pub now: Millis,
    /// Every effect the engine asked for, in order.
    pub effects: Vec<Effect>,
}

impl Rig {
    pub fn build(options: Options) -> Rig {
        let (engine, out) = Engine::create_in(
            SessionId::new("ver-1"),
            Setup {
                request: "Verify the cwd change.",
                target: target(),
                manifest: manifest(),
                rebuild: Rebuild {
                    head_sha: HEAD_SHA.into(),
                    worktree_patch: None,
                    untracked: Vec::new(),
                },
                spec: None,
                isolation: options.isolation,
                user_criteria: options.user_criteria,
                confirm: options.confirm,
                workdir: options.workdir.clone(),
                main_worktree: options.workdir,
                max_rounds: options.max_rounds,
                round_cap: MAX_MAX_ROUNDS,
                judge: AgentChoice::from(AgentKind::Claude),
                panel: &options.panel,
            },
            Path::new("/data/verifications/ver-1"),
            Config::default(),
            1,
        )
        .unwrap();
        let mut rig = Rig {
            engine,
            log: out.batches,
            now: 10,
            effects: Vec::new(),
        };
        rig.feed(Input::Start { now: 10 });
        rig
    }

    pub fn new(max_rounds: RoundNo) -> Rig {
        Rig::build(Options {
            max_rounds,
            ..Options::default()
        })
    }

    pub fn in_workdir(max_rounds: RoundNo, workdir: &Path) -> Rig {
        Rig::build(Options {
            max_rounds,
            workdir: workdir.to_owned(),
            ..Options::default()
        })
    }

    pub fn tick(&mut self) -> Millis {
        self.now += 10;
        self.now
    }

    pub fn feed(&mut self, input: Input) -> Output {
        let out = self.engine.handle(input).unwrap();
        self.log.extend(out.batches.iter().cloned());
        self.effects.extend(out.effects.iter().cloned());
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
                self.effects.extend(out.effects.iter().cloned());
                Ok(out)
            }
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn round(&self) -> RoundNo {
        self.engine.session().current_round().map_or(0, |r| r.no)
    }

    pub fn phase(&self) -> Phase {
        self.engine.session().phase()
    }

    /// Marks the judge's open brief sent and writes `content` as its file.
    pub fn judge(&mut self, content: &str) -> Output {
        let round = self.round();
        let at = self.tick();
        self.feed(Input::Sent {
            round,
            part: p("judge"),
            kind: BriefKind::Work,
            at,
        });
        self.write("judge", round, content, Vec::new())
    }

    /// The judge's criteria, sent and written.
    pub fn criteria(&mut self, content: &str) -> Output {
        let at = self.tick();
        self.feed(Input::Sent {
            round: CRITERIA_ROUND,
            part: p("judge"),
            kind: BriefKind::Work,
            at,
        });
        self.write("judge", CRITERIA_ROUND, content, Vec::new())
    }

    pub fn write(
        &mut self,
        part: &str,
        round: RoundNo,
        content: &str,
        files: Vec<FileFact>,
    ) -> Output {
        let at = self.tick();
        self.feed(Input::ReplyFileWritten {
            round,
            part: p(part),
            content: content.into(),
            files,
            at,
        })
    }

    /// Marks every verifier's work brief of the current round sent and
    /// started.
    pub fn deliver(&mut self) {
        let round = self.round();
        let parts: Vec<PartId> = self.engine.session().verifier_ids().cloned().collect();
        for part in parts {
            let at = self.tick();
            self.feed(Input::Sent {
                round,
                part,
                kind: BriefKind::Work,
                at,
            });
        }
    }

    /// `part`'s reply, with a regular file found at every path it cites.
    pub fn reply(&mut self, part: &str, content: &str) -> Output {
        let round = self.round();
        self.write(part, round, content, facts_for(content))
    }

    /// Copies (pretends to) every replied verifier's evidence of the
    /// current round.
    pub fn ingest(&mut self) {
        self.ingest_except(&[], &Default::default());
    }

    /// As [`Rig::ingest`], but the items keyed in `gone` were gone before
    /// ingest (not stored), and `tails` are the failing logs' tails.
    pub fn ingest_except(
        &mut self,
        gone: &[&str],
        tails: &std::collections::BTreeMap<EvidenceKey, String>,
    ) {
        let round = self.round();
        let parts: Vec<PartId> = self
            .engine
            .session()
            .current_round()
            .map(|r| {
                r.replies
                    .keys()
                    .filter(|p| !r.ingested.contains(*p))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        for part in parts {
            let items = self
                .engine
                .session()
                .reported_items(round, &part)
                .into_iter()
                .filter(|e| !gone.contains(&e.key.as_str()))
                .filter_map(|e| Some((e.key.clone(), stored_for(round, &part, e.file_path()?))))
                .collect();
            let tails = tails
                .iter()
                .filter(|(k, _)| k.split().is_some_and(|(p, _, _)| p == part))
                .map(|(k, t)| (k.clone(), t.clone()))
                .collect();
            self.feed(Input::Ingested {
                round,
                part,
                items,
                tails,
            });
        }
    }

    /// Writes `part`'s re-run script, starts it and reports `outcome` for
    /// each of its items.
    pub fn rerun(&mut self, part: &str, outcome: impl Fn(&EvidenceKey, i32) -> Rerun) {
        let round = self.round();
        let part = p(part);
        let items: Vec<(EvidenceKey, i32)> = self
            .engine
            .session()
            .rerun_items(round, &part)
            .into_iter()
            .map(|e| match &e.body {
                EvidenceBody::Command { exit, .. } => (e.key.clone(), *exit),
                _ => unreachable!("re-run items are commands"),
            })
            .collect();
        let request = RerunRequest {
            nonce: "0".repeat(32),
            items: items
                .iter()
                .enumerate()
                .map(|(i, (key, _))| RerunItem {
                    key: key.clone(),
                    n: i as u32 + 1,
                    cmd_digest: 1,
                })
                .collect(),
            script_digest: 2,
        };
        let reply_dir = crate::store::verifier_reply_dir(self.engine.session(), &part);
        let script = crate::store::rerun_dir(&reply_dir, round, &part).join("run.sh");
        self.feed(Input::RerunWritten {
            round,
            part: part.clone(),
            request,
            script,
        });
        let at = self.tick();
        self.feed(Input::Sent {
            round,
            part: part.clone(),
            kind: BriefKind::Rerun,
            at,
        });
        let at = self.tick();
        self.feed(Input::RerunStarted {
            round,
            part: part.clone(),
            at,
        });
        for (key, exit) in items {
            let at = self.tick();
            let rerun = outcome(&key, exit);
            self.feed(Input::RerunReported {
                round,
                key,
                rerun,
                at,
            });
        }
    }

    /// Re-runs every pending item of the current round with the exit
    /// status its verifier reported.
    pub fn rerun_all_matched(&mut self) {
        for part in VERIFIERS {
            if self
                .engine
                .session()
                .rerun_items(self.round(), &p(part))
                .iter()
                .any(|e| e.rerun.is_none())
            {
                self.rerun(part, |_, exit| Rerun::Matched { exit, log: None });
            }
        }
    }

    /// The current brief text of `part`.
    pub fn brief(&self, part: &str, kind: BriefKind) -> String {
        self.engine
            .session()
            .current_round()
            .and_then(|r| r.brief(&p(part), kind))
            .map(|b| b.body.clone())
            .unwrap_or_default()
    }

    pub fn rejected(&self, part: &str) -> Option<String> {
        let s = self.engine.session();
        if s.current_round().is_none() {
            return s
                .criteria_brief
                .as_ref()
                .and_then(|b| b.receipt.rejected.clone());
        }
        s.current_round()
            .and_then(|r| r.brief(&p(part), BriefKind::Work))
            .and_then(|b| b.receipt.rejected.clone())
    }

    /// Whether `effect` was asked for since the rig was built.
    pub fn asked(&self, effect: &Effect) -> bool {
        self.effects.contains(effect)
    }
}

/// A stored copy as ingest would record it.
pub fn stored_for(round: RoundNo, part: &PartId, path: &str) -> Stored {
    Stored {
        path: crate::store::round_evidence_dir(Path::new(""), round, part).join(path),
        bytes: 100,
        digest: crate::capture::digest_bytes(path.as_bytes()),
    }
}

/// The media an extension names, as the runner's sniffing would agree
/// (`media::by_extension` is another unit's).
pub fn media_of(path: &str) -> Option<Media> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Media::ALL.into_iter().find(|m| {
        m.extension() == ext
            || (ext == "jpeg" && *m == Media::Jpeg)
            || (ext == "m4v" && *m == Media::Mp4)
    })
}

/// A regular 100-byte file at every path `content` cites, its media by
/// extension.
pub fn facts_for(content: &str) -> Vec<FileFact> {
    crate::model::validate::cited_paths(content)
        .into_iter()
        .map(|path| FileFact {
            state: FileState::Regular { bytes: 100 },
            sniffed: media_of(&path),
            by_ext: media_of(&path),
            path,
        })
        .collect()
}

/// A command evidence item `id` with its log at `logs/<id>.log`.
pub fn cmd(id: &str, exit: i32, rerun: bool) -> String {
    format!(
        r#"{{"id": "{id}", "kind": "command", "cmd": "cargo test {id}", "cwd": ".", "exit": {exit}, "log": "logs/{id}.log", "rerun": {rerun}}}"#
    )
}

/// A reply: `results` as `(criterion, result, evidence ids)`, `evidence`
/// as JSON items, `extra` appended fields (`"defects": […]`, with a
/// leading comma).
pub fn reply_json(results: &[(&str, &str, &[&str])], evidence: &[String], extra: &str) -> String {
    let results: Vec<String> = results
        .iter()
        .map(|(c, r, ids)| {
            let ids: Vec<String> = ids.iter().map(|i| format!("\"{i}\"")).collect();
            format!(
                r#"{{"criterion": "{c}", "result": "{r}", "note": "Saw it.", "evidence": [{}]}}"#,
                ids.join(", ")
            )
        })
        .collect();
    format!(
        r#"{{"summary": "Ran it.", "environment": "macOS", "results": [{}], "evidence": [{}]{extra}}}"#,
        results.join(", "),
        evidence.join(", ")
    )
}

/// Every criterion passing on one re-run command `e1` (exit 0).
pub fn all_pass() -> String {
    reply_json(
        &[
            ("AC-1", "pass", &["e1"]),
            ("AC-2", "pass", &["e1"]),
            ("AC-3", "pass", &["e1"]),
        ],
        &[cmd("e1", 0, true)],
        "",
    )
}

/// The judge's criteria accepted; the verification then waits for the
/// person (基準を確認してから検証 on).
pub fn accept_default_criteria(rig: &mut Rig) {
    rig.criteria(CRITERIA);
    assert!(
        rig.engine.session().criteria_accepted,
        "criteria turned down: {:?}",
        rig.rejected("judge")
    );
}

/// 検証を開始 (when held), then every worktree made and a tick: round 1's
/// briefs are out.
pub fn start_round_one(rig: &mut Rig) {
    if rig.phase() == Phase::Confirming {
        let at = rig.tick();
        rig.feed(Input::UserStartVerify { at });
    }
    make_worktrees(rig);
    assert_eq!(rig.round(), 1, "round 1 started");
}

/// Answers a pending `MakeWorktrees` with every worktree made, then ticks.
pub fn make_worktrees(rig: &mut Rig) {
    if rig.engine.session().isolation == Isolation::Worktree {
        let session = rig.engine.session().clone();
        for part in session.verifier_ids() {
            let path = crate::worktree::path_for(&session.main_worktree, &session.id, part);
            rig.feed(Input::WorktreeMade {
                part: part.clone(),
                path,
            });
        }
    }
    let now = rig.tick();
    rig.feed(Input::Tick { now });
}

/// Every criterion verified on a matched re-run of claude's `e1`.
pub const JUDGE_R1: &str = r#"{"summary": "All three hold.",
 "rulings": [
  {"criterion": "AC-1", "ruling": "verified", "rationale": "Re-run matched.", "evidence": ["claude/r1/e1"]},
  {"criterion": "AC-2", "ruling": "verified", "rationale": "Re-run matched.", "evidence": ["codex/r1/e1"]},
  {"criterion": "AC-3", "ruling": "verified", "rationale": "Re-run matched.", "evidence": ["claude-2/r1/e1"]}
 ]}"#;

/// Criteria accepted, round 1 with every verifier passing everything on a
/// matched re-run, and the judge ruling every criterion verified.
pub fn through_round_one(rig: &mut Rig) {
    accept_default_criteria(rig);
    start_round_one(rig);
    rig.deliver();
    for part in VERIFIERS {
        rig.reply(part, &all_pass());
    }
    rig.ingest();
    rig.rerun_all_matched();
    assert_eq!(rig.phase(), Phase::Ruling);
    rig.judge(JUDGE_R1);
}
