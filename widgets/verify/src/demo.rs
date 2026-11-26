//! Canned verifications for `--demo`, `--demo-start`, `--demo-held` and
//! `--demo-criteria`, and for the UI tests: a branch that makes new tabs
//! inherit the focused pane's directory, verified by three verifiers over
//! two rounds. Each is the log a runner would write, folded through the
//! library's reducer (`model::reduce`), so every state is one the window
//! can be given.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use verify::model::reduce::{self, Event};
use verify::model::types::{
    AgentChoice, AgentKind, AgentSetup, Author, Brief, BriefKind, BuiltinStrategy, Confidence,
    CriteriaEdit, Criterion, CriterionResult, CriterionSource, CrossCheck, CrossOutcome, Decision,
    DefectOp, DefectStatus, Evidence, EvidenceBody, EvidenceKey, FileStatus, Head, Isolation,
    JudgeOutput, Location, Mark, Media, Message, MessageId, Millis, MsgKind, Omitted, Outcome,
    PartId, Participant, Phase, Priority, RawDefect, RawKey, Rebuild, Receipt, Rerun, RerunItem,
    Role, RoundKind, RoundNo, Ruling, RulingKind, Session, SessionId, Severity, SpecFile, Stored,
    Strategy, StrategyChoice, Subject, Target, TargetFile, TargetKind, TargetManifest, Verdict,
    VerifierChoice,
};
use verify::store::{Listed, Summary};

use crate::ui::StartForm as Form;

const T0: Millis = 1_764_000_000_000;
/// Where the canned verifications pretend to be stored.
const DIR: &str = "/demo/verifications";
const REPO: &str = "/Users/demo/repos/noa";
const HEAD: &str = "a71be09d2c5f4e18b3c9d0e7f6a5b4c3d2e1f009";
const BASE: &str = "4f9c2a1e7b3d8c6011223344556677889900aabb";

/// The finished demo's id; the held and criteria demos have their own.
pub const DONE_ID: &str = "ver-demo";
pub const HELD_ID: &str = "ver-demo-held";
pub const CRITERIA_ID: &str = "ver-demo-criteria";

const REQUEST: &str = "新しいタブと分割したペインが、フォーカス中のペインの作業ディレクトリを\
                       引き継ぐようにした変更です。設定で無効にできることも確かめてください。";
const USER_CRITERION: &str = "新しいタブはフォーカス中のペインの作業ディレクトリで開く";

fn file(path: &str, status: FileStatus, lines: u32, changed: Vec<(u32, u32)>) -> TargetFile {
    TargetFile {
        path: path.into(),
        status,
        lines,
        bytes: u64::from(lines) * 34,
        additions: changed.iter().map(|(a, b)| b - a + 1).sum(),
        deletions: 4,
        changed,
        digest: 0,
    }
}

fn manifest() -> TargetManifest {
    TargetManifest {
        label: "feat/cwd-inherit … main（4 ファイル）".into(),
        repo: Some(PathBuf::from(REPO)),
        base_ref: Some("main".into()),
        base_sha: Some(BASE.into()),
        head_ref: Some("feat/cwd-inherit".into()),
        head_sha: Some(HEAD.into()),
        merge_base: Some(BASE.into()),
        files: vec![
            file(
                "crates/noa-app/src/tab.rs",
                FileStatus::Modified,
                212,
                vec![(40, 72)],
            ),
            file(
                "crates/noa-app/src/cwd.rs",
                FileStatus::Added,
                96,
                vec![(1, 96)],
            ),
            file(
                "crates/noa-config/src/keys.rs",
                FileStatus::Modified,
                640,
                vec![(310, 318)],
            ),
            file(
                "docs/specs/cwd-inherit.md",
                FileStatus::Added,
                48,
                vec![(1, 48)],
            ),
        ],
        omitted: vec![Omitted {
            path: "assets/tab.png".into(),
            reason: "バイナリ（テキストではない）".into(),
        }],
        diff_bytes: 21_480,
        diff_digest: Some(1),
        captured_at: T0,
    }
}

fn target() -> Target {
    Target {
        kind: TargetKind::Diff,
        repo: PathBuf::from(REPO),
        base: Some("main".into()),
        head: Head::Ref("feat/cwd-inherit".into()),
        paths: Vec::new(),
        spec: Some("docs/specs/cwd-inherit.md".into()),
    }
}

fn judge_choice() -> AgentChoice {
    AgentChoice {
        kind: AgentKind::Claude,
        setup: AgentSetup {
            model: Some("opus".into()),
            effort: None,
        },
    }
}

fn panel() -> Vec<VerifierChoice> {
    vec![
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
            agent: AgentChoice {
                kind: AgentKind::Codex,
                setup: AgentSetup {
                    model: Some("gpt-6.2-sol".into()),
                    effort: Some("xhigh".into()),
                },
            },
            strategy: StrategyChoice::Builtin(BuiltinStrategy::Falsify),
        },
        VerifierChoice {
            agent: AgentChoice {
                kind: AgentKind::Claude,
                setup: AgentSetup {
                    model: Some("sonnet".into()),
                    effort: None,
                },
            },
            strategy: StrategyChoice::Builtin(BuiltinStrategy::Regression),
        },
    ]
}

fn participant(id: &str, choice: &VerifierChoice) -> Participant {
    let StrategyChoice::Builtin(strategy) = &choice.strategy else {
        unreachable!("the demo panel holds built-in strategies only")
    };
    Participant {
        id: p(id),
        kind: choice.agent.kind,
        role: Role::Verifier,
        pane_id: None,
        setup: choice.agent.setup.clone(),
        strategy: Some(Strategy::Builtin { id: *strategy }),
    }
}

fn p(id: &str) -> PartId {
    PartId::new(id)
}

fn key(part: &str, round: RoundNo, id: &str) -> EvidenceKey {
    EvidenceKey::reported(&p(part), round, id)
}

/// A brief as the engine issues one (its text shortened for the demo).
fn brief(to: &str, kind: BriefKind, path: &str) -> Brief {
    Brief {
        to: p(to),
        kind,
        base_rev: 1,
        body: "## 1. Request and round\n（デモ）".into(),
        reply_path: PathBuf::from(path),
        receipt: Receipt::default(),
    }
}

fn criteria() -> Vec<Criterion> {
    let c = |id: &str, statement: &str, how: &str, priority, source| Criterion {
        id: id.into(),
        statement: statement.into(),
        how: how.into(),
        priority,
        source,
        edited: false,
    };
    vec![
        c(
            "AC-1",
            USER_CRITERION,
            "ペインで cd /tmp してから ⌘T で新しいタブを開き、pwd を確かめる",
            Priority::Must,
            CriterionSource::User,
        ),
        c(
            "AC-2",
            "分割したペインも元のペインの作業ディレクトリを引き継ぐ",
            "cd /tmp のあと ⌘D で分割し、新しいペインで pwd を確かめる",
            Priority::Must,
            CriterionSource::Spec { line: 12 },
        ),
        c(
            "AC-3",
            "OSC 7 を送らないシェルではホームディレクトリで開く",
            "OSC 7 を送らない sh でタブを開き、pwd が $HOME であることを確かめる",
            Priority::Should,
            CriterionSource::Request,
        ),
        c(
            "AC-4",
            "保存したセッションの復元で各タブの作業ディレクトリが変わらない",
            "既存の復元テスト（cargo test -p noa-app session_restore）を通す",
            Priority::Must,
            CriterionSource::Inferred,
        ),
        c(
            "AC-5",
            "window-inherit-working-directory = false で継承しない",
            "設定を false にして再起動し、cd /tmp のあと ⌘T で pwd を確かめる",
            Priority::Should,
            CriterionSource::Inferred,
        ),
    ]
}

/// The log being written, batch by batch.
struct Log {
    batches: Vec<Vec<Event>>,
    now: Millis,
}

impl Log {
    fn push(&mut self, batch: Vec<Event>) {
        self.batches.push(batch);
    }

    fn at(&mut self, minutes: u64) -> Millis {
        self.now += minutes * 60_000;
        self.now
    }

    fn session(&self) -> Session {
        reduce::replay(&self.batches).expect("the demo log folds")
    }
}

fn created(id: &str, confirm: bool) -> Log {
    let panel = panel();
    let verifiers = vec![
        participant("claude", &panel[0]),
        participant("codex", &panel[1]),
        participant("claude-2", &panel[2]),
    ];
    let judge = Participant {
        id: p("judge"),
        kind: AgentKind::Claude,
        role: Role::Judge,
        pane_id: None,
        setup: judge_choice().setup,
        strategy: None,
    };
    let first = vec![Event::Created {
        id: SessionId::new(id),
        request: REQUEST.into(),
        target: Box::new(target()),
        manifest: Box::new(manifest()),
        rebuild: Rebuild {
            head_sha: HEAD.into(),
            worktree_patch: None,
            untracked: Vec::new(),
        },
        isolation: Isolation::Worktree,
        spec: Some(SpecFile {
            path: "docs/specs/cwd-inherit.md".into(),
            bytes: 1_632,
            lines: 48,
            digest: 9,
        }),
        user_criteria: vec![USER_CRITERION.into()],
        workdir: PathBuf::from(REPO),
        main_worktree: PathBuf::from(REPO),
        max_rounds: 2,
        judge,
        verifiers,
        confirm,
        at: T0,
        auto: None,
    }];
    let mut log = Log {
        batches: vec![first],
        now: T0,
    };
    let panes: Vec<Event> = ["judge", "claude", "codex", "claude-2"]
        .iter()
        .zip(41..)
        .map(|(part, pane_id)| Event::PaneAssigned {
            part: p(part),
            pane_id,
        })
        .collect();
    log.push(panes);
    // The judge drafts the criteria.
    let sent = log.at(1);
    log.push(vec![
        Event::BriefIssued {
            round: 0,
            brief: brief(
                "judge",
                BriefKind::Work,
                "/Users/demo/repos/noa/.noa/verify/ver-demo/c/criteria.json",
            ),
        },
        Event::BriefSent {
            round: 0,
            to: p("judge"),
            kind: BriefKind::Work,
            at: sent,
        },
    ]);
    let at = log.at(3);
    log.push(vec![
        Event::ReplyAccepted {
            round: 0,
            to: p("judge"),
            at,
        },
        Event::CriteriaAccepted {
            summary:
                "依頼と仕様から 5 件。あなたの 1 行は AC-1 に、仕様 12 行目は AC-2 にしました。"
                    .into(),
            criteria: criteria(),
        },
    ]);
    log
}

/// The person's edits while the criteria wait for confirmation.
fn confirm(log: &mut Log) {
    let at = log.at(1);
    log.push(vec![Event::CriteriaEdited {
        edit: CriteriaEdit::Edit {
            id: "AC-4".into(),
            statement: None,
            how: Some(
                "cargo test -p noa-app session_restore と、実機で復元後の pwd を確かめる".into(),
            ),
            priority: None,
        },
        at,
    }]);
}

fn freeze(log: &mut Log) {
    let at = log.at(1);
    let mut batch = vec![Event::CriteriaFrozen { at }];
    for part in ["claude", "codex", "claude-2"] {
        batch.push(Event::WorktreeMade {
            part: p(part),
            path: PathBuf::from(format!("/Users/demo/repos/noa-wt/verify-ver-demo-{part}")),
        });
    }
    log.push(batch);
}

fn command(cmd: &str, exit: i32, log: &str, rerun: bool) -> EvidenceBody {
    EvidenceBody::Command {
        cmd: cmd.into(),
        cwd: ".".into(),
        exit,
        log: log.into(),
        rerun,
    }
}

fn media(path: &str, caption: &str, media: Media, marks: Vec<Mark>) -> EvidenceBody {
    EvidenceBody::File {
        path: path.into(),
        caption: Some(caption.into()),
        media: Some(media),
        marks,
        shows: Vec::new(),
    }
}

fn item(key: EvidenceKey, body: EvidenceBody) -> Evidence {
    Evidence {
        key,
        body,
        stored: None,
        rerun: None,
        attached: None,
    }
}

fn result(
    round: RoundNo,
    part: &str,
    criterion: &str,
    outcome: Outcome,
    note: &str,
    cites: &[&str],
) -> CriterionResult {
    CriterionResult {
        round,
        part: p(part),
        criterion: criterion.into(),
        outcome,
        note: note.into(),
        evidence: cites.iter().map(|id| key(part, round, id)).collect(),
    }
}

/// One verifier's reply, its evidence ingested at `stored` sizes.
struct Reply {
    part: &'static str,
    summary: &'static str,
    results: Vec<CriterionResult>,
    evidence: Vec<Evidence>,
    defects: Vec<RawDefect>,
    cross_checks: Vec<CrossCheck>,
}

fn stored_of(round: RoundNo, part: &str, item: &Evidence, n: u64) -> Option<Stored> {
    let path = item.file_path()?;
    let bytes = match &item.body {
        EvidenceBody::File { media: Some(m), .. } if m.is_video() => 4_812_330 + n,
        EvidenceBody::File { media: Some(_), .. } => 238_114 + 977 * n,
        _ => 1_204 + 311 * n,
    };
    Some(Stored {
        path: PathBuf::from(format!("evidence/r{round}/{part}/{path}")),
        bytes,
        digest: 0x9e37_79b9_7f4a_7c15 ^ (n << 8) ^ u64::from(round),
    })
}

/// Round `no` from its start through every reply, the ingest and the
/// re-runs (`reruns`: each key's outcome), up to the judge's brief.
fn verify_round(
    log: &mut Log,
    no: RoundNo,
    replies: Vec<Reply>,
    reruns: Vec<(EvidenceKey, Rerun)>,
    assignments: BTreeMap<PartId, Vec<Subject>>,
) {
    let at = log.at(1);
    log.push(vec![Event::RoundStarted {
        no,
        kind: RoundKind::for_round(no),
        assignments,
        at,
    }]);
    let mut issued = Vec::new();
    for reply in &replies {
        issued.push(Event::BriefIssued {
            round: no,
            brief: brief(
                reply.part,
                BriefKind::Work,
                &format!(
                    "/Users/demo/repos/noa-wt/verify-ver-demo-{0}/.noa/verify/ver-demo/r{no}/{0}/reply.json",
                    reply.part
                ),
            ),
        });
    }
    log.push(issued);
    let mut sent = vec![Event::PhaseChanged {
        round: no,
        phase: Phase::Verifying,
    }];
    for reply in &replies {
        let at = log.at(1);
        sent.push(Event::BriefSent {
            round: no,
            to: p(reply.part),
            kind: BriefKind::Work,
            at,
        });
        sent.push(Event::BriefStarted {
            round: no,
            to: p(reply.part),
            kind: BriefKind::Work,
            at: at + 20_000,
        });
    }
    log.push(sent);
    for reply in &replies {
        let at = log.at(9);
        log.push(vec![
            Event::ReplyAccepted {
                round: no,
                to: p(reply.part),
                at,
            },
            Event::ReplyRecorded {
                round: no,
                part: p(reply.part),
                summary: reply.summary.into(),
                environment: "macOS 26.1, rustc 1.92, debug build".into(),
                results: reply.results.clone(),
                evidence: reply.evidence.clone(),
                defects: reply.defects.clone(),
                cross_checks: reply.cross_checks.clone(),
                at,
            },
        ]);
    }
    let mut ingest = vec![Event::PhaseChanged {
        round: no,
        phase: Phase::Ingesting,
    }];
    for reply in &replies {
        let items = reply
            .evidence
            .iter()
            .zip(1..)
            .filter_map(|(e, n)| Some((e.key.clone(), stored_of(no, reply.part, e, n)?)))
            .collect();
        let tails = reply
            .evidence
            .iter()
            .filter(|e| matches!(e.body, EvidenceBody::Command { exit, .. } if exit != 0))
            .map(|e| {
                (
                    e.key.clone(),
                    "running 1 test\ntest cwd_inherit ... FAILED\nleft: \"/Users/demo\" right: \"/tmp\"".to_owned(),
                )
            })
            .collect();
        ingest.push(Event::EvidenceIngested {
            round: no,
            part: p(reply.part),
            items,
            tails,
        });
    }
    log.push(ingest);
    // Each verifier with re-runs gets its script and its re-run brief.
    let mut requested = Vec::new();
    for reply in &replies {
        let items: Vec<RerunItem> = reply
            .evidence
            .iter()
            .filter(|e| matches!(e.body, EvidenceBody::Command { rerun: true, .. }))
            .zip(1..)
            .map(|(e, n)| RerunItem {
                key: e.key.clone(),
                n,
                cmd_digest: 0x51 + u64::from(n),
            })
            .collect();
        if items.is_empty() {
            continue;
        }
        requested.push(Event::RerunRequested {
            round: no,
            part: p(reply.part),
            nonce: "5f0c1e8a9b2d4c6e8f0a1b2c3d4e5f60".into(),
            items,
            script_digest: 0x77,
        });
        requested.push(Event::BriefIssued {
            round: no,
            brief: brief(
                reply.part,
                BriefKind::Rerun,
                &format!(
                    "/Users/demo/repos/noa-wt/verify-ver-demo-{0}/.noa/verify/ver-demo/r{no}/{0}/rerun",
                    reply.part
                ),
            ),
        });
        let at = log.at(1);
        requested.push(Event::BriefSent {
            round: no,
            to: p(reply.part),
            kind: BriefKind::Rerun,
            at,
        });
        requested.push(Event::BriefStarted {
            round: no,
            to: p(reply.part),
            kind: BriefKind::Rerun,
            at: at + 5_000,
        });
    }
    log.push(requested);
    let mut finished: Vec<Event> = reruns
        .into_iter()
        .map(|(key, rerun)| Event::RerunFinished { key, rerun })
        .collect();
    for reply in &replies {
        if reply
            .evidence
            .iter()
            .any(|e| matches!(e.body, EvidenceBody::Command { rerun: true, .. }))
        {
            let at = log.at(2);
            finished.push(Event::ReplyAccepted {
                round: no,
                to: p(reply.part),
                at,
            });
        }
    }
    finished.push(Event::PhaseChanged {
        round: no,
        phase: Phase::Ruling,
    });
    log.push(finished);
    let at = log.at(1);
    log.push(vec![
        Event::BriefIssued {
            round: no,
            brief: brief(
                "judge",
                BriefKind::Work,
                &format!("/Users/demo/repos/noa/.noa/verify/ver-demo/r{no}/judge.json"),
            ),
        },
        Event::BriefSent {
            round: no,
            to: p("judge"),
            kind: BriefKind::Work,
            at,
        },
    ]);
}

fn rerun_log(round: RoundNo, part: &str, n: u32) -> Option<Stored> {
    Some(Stored {
        path: PathBuf::from(format!("evidence/r{round}/{part}.rerun/{n}.log")),
        bytes: 1_877 + u64::from(n),
        digest: 0xabc0 + u64::from(n),
    })
}

fn ruling(round: RoundNo, kind: RulingKind, rationale: &str, cites: &[EvidenceKey]) -> Ruling {
    Ruling {
        round,
        kind,
        rationale: rationale.into(),
        evidence: cites.to_vec(),
    }
}

fn judge(log: &mut Log, no: RoundNo, output: JudgeOutput) {
    let at = log.at(4);
    let summary = output.summary.clone();
    let n = log.session().messages.len() + 1;
    log.push(vec![
        Event::ReplyAccepted {
            round: no,
            to: p("judge"),
            at,
        },
        Event::JudgeAccepted { round: no, output },
        Event::MessagePosted {
            message: Message {
                id: MessageId::nth(n),
                round: no,
                author: Author::Part(p("judge")),
                kind: MsgKind::Summary,
                to: Vec::new(),
                body: summary,
                at,
            },
        },
        Event::PhaseChanged {
            round: no,
            phase: Phase::Closed,
        },
    ]);
}

fn round_one(log: &mut Log) {
    let claude = Reply {
        part: "claude",
        summary: "各基準の how をそのまま実行しました。AC-1 は ⌘T のあと $HOME で開きます。",
        results: vec![
            result(
                1,
                "claude",
                "AC-1",
                Outcome::Fail,
                "cd /tmp のあと ⌘T で開いたタブの pwd が $HOME でした。",
                &["e1", "v1"],
            ),
            result(1, "claude", "AC-2", Outcome::Pass, "", &["e2"]),
            result(1, "claude", "AC-3", Outcome::Pass, "", &["e3"]),
            result(1, "claude", "AC-4", Outcome::Pass, "", &["e4"]),
            result(
                1,
                "claude",
                "AC-5",
                Outcome::NotTested,
                "設定の再読み込みを試す時間がありませんでした。",
                &[],
            ),
        ],
        evidence: vec![
            item(
                key("claude", 1, "e1"),
                command(
                    "cargo test -p noa-app cwd_inherit_new_tab",
                    101,
                    "logs/1.log",
                    true,
                ),
            ),
            item(
                key("claude", 1, "v1"),
                media(
                    "video/new-tab.mov",
                    "cd /tmp、⌘T、新しいタブで pwd",
                    Media::QuickTime,
                    vec![
                        Mark {
                            t_ms: 4_000,
                            note: "⌘T を押す".into(),
                        },
                        Mark {
                            t_ms: 6_500,
                            note: "新しいタブが $HOME を表示".into(),
                        },
                    ],
                ),
            ),
            item(
                key("claude", 1, "e2"),
                command(
                    "cargo test -p noa-app cwd_inherit_split",
                    0,
                    "logs/2.log",
                    true,
                ),
            ),
            item(
                key("claude", 1, "e3"),
                command("env -i sh -c 'noa --new-tab; pwd'", 0, "logs/3.log", false),
            ),
            item(
                key("claude", 1, "e4"),
                command(
                    "cargo test -p noa-app session_restore",
                    0,
                    "logs/4.log",
                    true,
                ),
            ),
        ],
        defects: vec![RawDefect {
            key: RawKey {
                part: p("claude"),
                id: "1".into(),
            },
            round: 1,
            location: Some(Location {
                path: "crates/noa-app/src/tab.rs".into(),
                line: Some(58),
                end_line: Some(64),
            }),
            in_target: true,
            in_diff: Some(true),
            severity: Severity::High,
            category: "conformance".into(),
            confidence: Confidence::High,
            claim: "新しいタブがフォーカスではなく最初のペインの作業ディレクトリを読む".into(),
            repro: "1. ペインを二つに分割 2. 右で cd /tmp 3. ⌘T 4. pwd".into(),
            evidence: vec![key("claude", 1, "e1")],
            fix: Some("focused_pane() の cwd を渡す".into()),
        }],
        cross_checks: Vec::new(),
    };
    let codex = Reply {
        part: "codex",
        summary: "境界を突きました。AC-1 は再現、AC-3 は OSC 7 を送らないシェルが用意できず。",
        results: vec![
            result(
                1,
                "codex",
                "AC-1",
                Outcome::Fail,
                "長いパス（255 文字）で $HOME に戻ります。",
                &["e1"],
            ),
            result(1, "codex", "AC-2", Outcome::Pass, "", &["e2"]),
            result(
                1,
                "codex",
                "AC-3",
                Outcome::Blocked,
                "サンドボックスで OSC 7 を送らないシェルを起動できません。",
                &["o1"],
            ),
            result(1, "codex", "AC-4", Outcome::Pass, "", &["e3"]),
            result(1, "codex", "AC-5", Outcome::Pass, "", &["e4"]),
        ],
        evidence: vec![
            item(
                key("codex", 1, "e1"),
                command(
                    "cargo test -p noa-app cwd_long_path -- --nocapture",
                    101,
                    "logs/1.log",
                    true,
                ),
            ),
            item(
                key("codex", 1, "e2"),
                command(
                    "cargo test -p noa-app split_inherits",
                    0,
                    "logs/2.log",
                    true,
                ),
            ),
            item(
                key("codex", 1, "o1"),
                EvidenceBody::Observation {
                    text: "sh -i を起動するとサンドボックスが /dev/tty を拒否した".into(),
                },
            ),
            item(
                key("codex", 1, "e3"),
                command("cargo bench -p noa-app restore", 0, "logs/3.log", true),
            ),
            item(
                key("codex", 1, "e4"),
                command(
                    "cargo test -p noa-config inherit_off",
                    0,
                    "logs/4.log",
                    true,
                ),
            ),
        ],
        defects: vec![RawDefect {
            key: RawKey {
                part: p("codex"),
                id: "1".into(),
            },
            round: 1,
            location: Some(Location {
                path: "crates/noa-app/src/cwd.rs".into(),
                line: Some(31),
                end_line: None,
            }),
            in_target: true,
            in_diff: Some(true),
            severity: Severity::Medium,
            category: "falsify".into(),
            confidence: Confidence::Medium,
            claim: "タブを閉じた直後に ⌘T すると閉じたペインのディレクトリで開く".into(),
            repro: "1. cd /tmp のペインを閉じる 2. すぐ ⌘T 3. pwd".into(),
            evidence: vec![key("codex", 1, "e1")],
            fix: None,
        }],
        cross_checks: Vec::new(),
    };
    let claude2 = Reply {
        part: "claude-2",
        summary: "リポジトリのチェックと変更箇所のテストを実行しました。",
        results: vec![
            result(1, "claude-2", "AC-1", Outcome::Pass, "", &["e1"]),
            result(1, "claude-2", "AC-2", Outcome::Pass, "", &["s1"]),
            result(
                1,
                "claude-2",
                "AC-3",
                Outcome::NotTested,
                "回帰の観点では対象外としました。",
                &[],
            ),
            result(1, "claude-2", "AC-4", Outcome::Pass, "", &["e2"]),
            result(
                1,
                "claude-2",
                "AC-5",
                Outcome::Blocked,
                "設定の読み込みテストがビルドできません（base でも同じ）。",
                &["e3"],
            ),
        ],
        evidence: vec![
            item(
                key("claude-2", 1, "e1"),
                command("scripts/verify.sh", 0, "logs/1.log", true),
            ),
            item(
                key("claude-2", 1, "s1"),
                media(
                    "shots/split.png",
                    "⌘D で分割したペインの pwd が /tmp",
                    Media::Png,
                    Vec::new(),
                ),
            ),
            item(
                key("claude-2", 1, "e2"),
                command(
                    "cargo test -p noa-app session_restore",
                    0,
                    "logs/2.log",
                    true,
                ),
            ),
            item(
                key("claude-2", 1, "e3"),
                command("cargo test -p noa-config", 101, "logs/3.log", false),
            ),
        ],
        defects: Vec::new(),
        cross_checks: Vec::new(),
    };
    let reruns = vec![
        (
            key("claude", 1, "e1"),
            Rerun::Matched {
                exit: 101,
                log: rerun_log(1, "claude", 1),
            },
        ),
        (
            key("claude", 1, "e2"),
            Rerun::Matched {
                exit: 0,
                log: rerun_log(1, "claude", 2),
            },
        ),
        (
            key("claude", 1, "e4"),
            Rerun::Matched {
                exit: 0,
                log: rerun_log(1, "claude", 3),
            },
        ),
        (
            key("codex", 1, "e1"),
            Rerun::Matched {
                exit: 101,
                log: rerun_log(1, "codex", 1),
            },
        ),
        (
            key("codex", 1, "e2"),
            Rerun::Matched {
                exit: 0,
                log: rerun_log(1, "codex", 2),
            },
        ),
        (
            key("codex", 1, "e3"),
            Rerun::TimedOut {
                log: rerun_log(1, "codex", 3),
            },
        ),
        (
            key("codex", 1, "e4"),
            Rerun::Matched {
                exit: 0,
                log: rerun_log(1, "codex", 4),
            },
        ),
        (
            key("claude-2", 1, "e1"),
            Rerun::Differed {
                exit: Some(101),
                log: rerun_log(1, "claude-2", 1),
            },
        ),
        (
            key("claude-2", 1, "e2"),
            Rerun::NotRun {
                why:
                    "the re-run was stopped before it finished; the agent's tool may have ended it"
                        .into(),
                log: None,
            },
        ),
    ];
    verify_round(
        log,
        1,
        vec![claude, codex, claude2],
        reruns,
        BTreeMap::new(),
    );
    // The person's own screenshot, while the judge rules.
    let at = log.at(1);
    log.push(vec![Event::EvidenceAttached {
        key: EvidenceKey::user(1),
        round: 1,
        body: EvidenceBody::File {
            path: "右ペインで⌘T.png".into(),
            caption: Some("右のペインで cd /tmp のあと ⌘T、新しいタブは $HOME".into()),
            media: Some(Media::Png),
            marks: Vec::new(),
            shows: Vec::new(),
        },
        stored: Stored {
            path: PathBuf::from("evidence/user/1.png"),
            bytes: 312_480,
            digest: 0x1234_5678,
        },
        shows: vec!["AC-1".into()],
        at,
    }]);
    let mut rulings = BTreeMap::new();
    rulings.insert(
        "AC-1".to_owned(),
        ruling(1, RulingKind::Failed, "claude と codex の fail が再実行でも一致（exit 101）。claude-2 の pass は scripts/verify.sh が該当テストを含まない。", &[key("claude", 1, "e1"), key("codex", 1, "e1")]),
    );
    rulings.insert(
        "AC-2".to_owned(),
        ruling(
            1,
            RulingKind::Verified,
            "claude の split テストが再実行で一致し、codex も独立に pass。",
            &[key("claude", 1, "e2"), key("codex", 1, "e2")],
        ),
    );
    rulings.insert(
        "AC-3".to_owned(),
        ruling(
            1,
            RulingKind::Inconclusive,
            "claude の pass は再実行の印がなく単独。codex は実行不能。",
            &[key("claude", 1, "e3")],
        ),
    );
    rulings.insert(
        "AC-4".to_owned(),
        ruling(
            1,
            RulingKind::Verified,
            "claude の復元テストが再実行で一致。",
            &[key("claude", 1, "e4"), key("claude-2", 1, "e2")],
        ),
    );
    rulings.insert(
        "AC-5".to_owned(),
        ruling(
            1,
            RulingKind::Inconclusive,
            "codex の pass のみ。claude-2 は実行不能、claude は未実施。",
            &[key("codex", 1, "e4")],
        ),
    );
    let mut assign = BTreeMap::new();
    assign.insert(
        p("codex"),
        vec![Subject::Result {
            part: p("claude-2"),
            criterion: "AC-1".into(),
        }],
    );
    assign.insert(
        p("claude-2"),
        vec![Subject::Result {
            part: p("codex"),
            criterion: "AC-1".into(),
        }],
    );
    assign.insert(
        p("claude"),
        vec![
            Subject::Defect { id: "D2".into() },
            Subject::Result {
                part: p("codex"),
                criterion: "AC-5".into(),
            },
        ],
    );
    judge(
        log,
        1,
        JudgeOutput {
            summary: "AC-1 は二人の fail が再実行でも一致。AC-2・AC-4 は検証済。AC-3・AC-5 は根拠不足。不具合 2 件を登録。".into(),
            rulings,
            defect_ops: vec![
                DefectOp::Add {
                    id: "D1".into(),
                    from: vec!["claude/1".into()],
                    claim: "新しいタブがフォーカスではなく最初のペインの作業ディレクトリを読む".into(),
                    repro: "1. ペインを二つに分割 2. 右で cd /tmp 3. ⌘T 4. pwd".into(),
                    location: Some(Location { path: "crates/noa-app/src/tab.rs".into(), line: Some(58), end_line: Some(64) }),
                    severity: Severity::High,
                    category: "conformance".into(),
                    confidence: Confidence::High,
                    status: DefectStatus::Open,
                    rationale: "AC-1 の失敗の原因と見られる。".into(),
                    fix: Some("focused_pane() の cwd を渡す".into()),
                },
                DefectOp::Add {
                    id: "D2".into(),
                    from: vec!["codex/1".into()],
                    claim: "タブを閉じた直後に ⌘T すると閉じたペインのディレクトリで開く".into(),
                    repro: "1. cd /tmp のペインを閉じる 2. すぐ ⌘T 3. pwd".into(),
                    location: Some(Location { path: "crates/noa-app/src/cwd.rs".into(), line: Some(31), end_line: None }),
                    severity: Severity::Medium,
                    category: "falsify".into(),
                    confidence: Confidence::Medium,
                    status: DefectStatus::Open,
                    rationale: "単独の報告。追試で確かめる。".into(),
                    fix: None,
                },
            ],
            assign,
            verdict: None,
            close: false,
        },
    );
}

fn round_two(log: &mut Log) {
    let claude = Reply {
        part: "claude",
        summary: "D2 を再現しました。codex の AC-5 も自分の worktree で確かめ、AC-5 を pass に更新。",
        results: vec![result(
            2,
            "claude",
            "AC-5",
            Outcome::Pass,
            "設定 false で $HOME に開くことを確認。",
            &["e2"],
        )],
        evidence: vec![
            item(
                key("claude", 2, "e1"),
                command(
                    "cargo test -p noa-app close_then_new_tab",
                    1,
                    "logs/1.log",
                    true,
                ),
            ),
            item(
                key("claude", 2, "e2"),
                command(
                    "cargo test -p noa-config inherit_off",
                    0,
                    "logs/2.log",
                    true,
                ),
            ),
        ],
        defects: Vec::new(),
        cross_checks: vec![
            CrossCheck {
                round: 2,
                by: p("claude"),
                subject: Subject::Defect { id: "D2".into() },
                outcome: CrossOutcome::Reproduced,
                note: "閉じた直後の ⌘T で /tmp に開いた。".into(),
                evidence: vec![key("claude", 2, "e1")],
            },
            CrossCheck {
                round: 2,
                by: p("claude"),
                subject: Subject::Result {
                    part: p("codex"),
                    criterion: "AC-5".into(),
                },
                outcome: CrossOutcome::Reproduced,
                note: "".into(),
                evidence: vec![key("claude", 2, "e2")],
            },
        ],
    };
    let codex = Reply {
        part: "codex",
        summary: "claude-2 の AC-1 pass は、該当テストが scripts/verify.sh に含まれないため再現しません。",
        results: Vec::new(),
        evidence: vec![item(
            key("codex", 2, "e1"),
            command(
                "cargo test -p noa-app cwd_inherit_new_tab",
                101,
                "logs/1.log",
                true,
            ),
        )],
        defects: Vec::new(),
        cross_checks: vec![CrossCheck {
            round: 2,
            by: p("codex"),
            subject: Subject::Result {
                part: p("claude-2"),
                criterion: "AC-1".into(),
            },
            outcome: CrossOutcome::NotReproduced,
            note: "同じ worktree 構成で fail（exit 101）。".into(),
            evidence: vec![key("codex", 2, "e1")],
        }],
    };
    let claude2 = Reply {
        part: "claude-2",
        summary: "codex の AC-1 fail を自分の worktree で再現しました。",
        results: Vec::new(),
        evidence: vec![item(
            key("claude-2", 2, "e1"),
            command(
                "cargo test -p noa-app cwd_long_path",
                101,
                "logs/1.log",
                true,
            ),
        )],
        defects: Vec::new(),
        cross_checks: vec![CrossCheck {
            round: 2,
            by: p("claude-2"),
            subject: Subject::Result {
                part: p("codex"),
                criterion: "AC-1".into(),
            },
            outcome: CrossOutcome::Reproduced,
            note: "".into(),
            evidence: vec![key("claude-2", 2, "e1")],
        }],
    };
    let reruns = vec![
        (
            key("claude", 2, "e1"),
            Rerun::Matched {
                exit: 1,
                log: rerun_log(2, "claude", 1),
            },
        ),
        (
            key("claude", 2, "e2"),
            Rerun::Matched {
                exit: 0,
                log: rerun_log(2, "claude", 2),
            },
        ),
        (
            key("codex", 2, "e1"),
            Rerun::Matched {
                exit: 101,
                log: rerun_log(2, "codex", 1),
            },
        ),
        (
            key("claude-2", 2, "e1"),
            Rerun::Differed {
                exit: Some(0),
                log: rerun_log(2, "claude-2", 1),
            },
        ),
    ];
    let mut assignments = BTreeMap::new();
    assignments.insert(
        p("codex"),
        vec![Subject::Result {
            part: p("claude-2"),
            criterion: "AC-1".into(),
        }],
    );
    assignments.insert(
        p("claude-2"),
        vec![Subject::Result {
            part: p("codex"),
            criterion: "AC-1".into(),
        }],
    );
    assignments.insert(
        p("claude"),
        vec![
            Subject::Defect { id: "D2".into() },
            Subject::Result {
                part: p("codex"),
                criterion: "AC-5".into(),
            },
        ],
    );
    verify_round(log, 2, vec![claude, codex, claude2], reruns, assignments);
    let mut rulings = BTreeMap::new();
    rulings.insert(
        "AC-1".to_owned(),
        ruling(
            2,
            RulingKind::Failed,
            "claude-2 が codex の fail を再現し、claude-2 の pass は codex が再現せず。",
            &[key("codex", 1, "e1"), key("claude-2", 2, "e1")],
        ),
    );
    rulings.insert(
        "AC-2".to_owned(),
        ruling(
            2,
            RulingKind::Verified,
            "R1 のまま。",
            &[key("claude", 1, "e2"), key("codex", 1, "e2")],
        ),
    );
    rulings.insert(
        "AC-3".to_owned(),
        ruling(
            2,
            RulingKind::NotApplicable,
            "Noa は OSC 7 なしでも shell integration で cwd を得るため、この基準は当てはまらない。",
            &[],
        ),
    );
    rulings.insert(
        "AC-4".to_owned(),
        ruling(
            2,
            RulingKind::Verified,
            "R1 のまま。",
            &[key("claude", 1, "e4"), key("claude-2", 1, "e2")],
        ),
    );
    rulings.insert(
        "AC-5".to_owned(),
        ruling(2, RulingKind::Inconclusive, "claude の更新は再実行一致だが、claude-2 の実行不能が base でも起きており設定読み込みの検証が揃わない。", &[key("claude", 2, "e2")]),
    );
    judge(
        log,
        2,
        JudgeOutput {
            summary: "AC-1 は不合格で確定。D2 は claude が再現し合意。AC-3 は対象外。".into(),
            rulings,
            defect_ops: vec![DefectOp::Update {
                id: "D2".into(),
                claim: None,
                repro: None,
                location: None,
                severity: None,
                category: None,
                confidence: Some(Confidence::High),
                status: Some(DefectStatus::Agreed),
                rationale: Some("claude が自分の worktree で再現。".into()),
                fix: None,
            }],
            assign: BTreeMap::new(),
            verdict: Some(Verdict {
                decision: Decision::Fail,
                rationale: "必須の AC-1 が不合格。新しいタブは最初のペインのディレクトリを読む（D1）。D2 も同じ PR で直すのが望ましい。".into(),
                blocking: vec!["AC-1".into(), "D1".into()],
            }),
            close: true,
        },
    );
}

fn close(log: &mut Log) {
    let mut batch: Vec<Event> = ["claude", "codex", "claude-2"]
        .iter()
        .map(|part| Event::WorktreeRemoved { part: p(part) })
        .collect();
    batch.push(Event::SessionClosed);
    log.push(batch);
}

fn done_log() -> Log {
    let mut log = created(DONE_ID, true);
    confirm(&mut log);
    freeze(&mut log);
    round_one(&mut log);
    round_two(&mut log);
    close(&mut log);
    log
}

fn held_log_of() -> Log {
    let mut log = created(HELD_ID, true);
    log.push(vec![Event::HoldSet { hold: true }]);
    confirm(&mut log);
    freeze(&mut log);
    round_one(&mut log);
    log
}

fn criteria_log_of() -> Log {
    let mut log = created(CRITERIA_ID, true);
    confirm(&mut log);
    log
}

/// The finished demo verification.
pub fn session() -> Session {
    done_log().session()
}

/// The finished demo's event log, for tests that need it on disk.
#[cfg(test)]
pub fn log() -> Vec<Vec<Event>> {
    done_log().batches
}

/// The held demo's event log (unfinished).
#[cfg(test)]
pub fn held_log() -> Vec<Vec<Event>> {
    held_log_of().batches
}

/// The demo held after its first ruled round.
pub fn held() -> Session {
    held_log_of().session()
}

/// The demo's criteria, held for the person to confirm.
pub fn criteria_held() -> Session {
    criteria_log_of().session()
}

/// The demo's start view form.
pub fn start_form() -> Form {
    Form {
        kind: TargetKind::Diff,
        repo: "~/repos/noa".into(),
        base: "main".into(),
        head: "feat/cwd-inherit".into(),
        spec: "docs/specs/cwd-inherit.md".into(),
        request: REQUEST.into(),
        criteria: USER_CRITERION.into(),
        judge: judge_choice(),
        panel: panel(),
        ..Form::default()
    }
}

/// Canned past verifications for the start view.
pub fn past() -> Vec<Listed> {
    let row = |id: &str, session: &Session| Listed {
        dir: Path::new(DIR).join(id),
        summary: Ok(Summary::of(&Path::new(DIR).join(id), session)),
        stamp: None,
    };
    vec![
        row(HELD_ID, &held()),
        row(DONE_ID, &session()),
        Listed {
            dir: Path::new(DIR).join("ver-broken"),
            summary: Err("events.jsonl line 3 is damaged: expected value".into()),
            stamp: None,
        },
    ]
}

/// A canned stored verification by its id.
pub fn stored(id: &str) -> Option<Session> {
    match id {
        DONE_ID => Some(session()),
        HELD_ID => Some(held()),
        CRITERIA_ID => Some(criteria_held()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_runs_two_rounds_to_a_failing_verdict() {
        let s = session();
        assert!(s.closed);
        assert_eq!(s.rounds.len(), 2);
        assert_eq!(s.verdict().unwrap().decision, Decision::Fail);
        // Every ruling and every result outcome is shown somewhere.
        let kinds: Vec<RulingKind> = s
            .criteria
            .iter()
            .map(|c| s.ruling(&c.id).unwrap().kind)
            .collect();
        for kind in RulingKind::ALL {
            assert!(kinds.contains(&kind), "{kind:?}");
        }
        for outcome in Outcome::ALL {
            assert!(
                s.results.iter().any(|r| r.outcome == outcome),
                "{outcome:?}"
            );
        }
        assert!(
            s.evidence
                .iter()
                .any(|e| e.media().is_some_and(Media::is_video))
        );
        assert!(s.evidence.iter().any(|e| e.key.is_user()));
        assert_eq!(s.ruling_counts(), [2, 1, 1, 1, 0]);
        let h = held();
        assert!(h.waits_to_continue());
        assert_eq!(h.defects.len(), 2);
        let c = criteria_held();
        assert_eq!(c.phase(), Phase::Confirming);
        assert!(c.criterion("AC-4").unwrap().edited);
    }
}
