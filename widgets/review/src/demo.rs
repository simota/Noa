//! Canned reviews for `--demo`, `--demo-start` and `--demo-held`: a git
//! review of a store rewrite by three reviewers over three rounds, driven
//! through the real engine so every state is one the widget can reach.

use std::path::{Path, PathBuf};

use serde_json::json;

use review::engine::{Config, Engine, Input, Setup};
use review::model::types::{
    AgentChoice, AgentKind, AgentSetup, BuiltinLens, FileStatus, Head, LensChoice, Millis, Omitted,
    PartId, ReviewerChoice, Session, SessionId, Target, TargetFile, TargetKind, TargetManifest,
};
use review::store::{Listed, Summary};

use crate::ui::StartForm as Form;

const T0: Millis = 1_764_000_000_000;
const DIR: &str = "/demo/reviews";

fn file(
    path: &str,
    status: FileStatus,
    lines: u32,
    changed: Vec<(u32, u32)>,
    del: u32,
) -> TargetFile {
    TargetFile {
        path: path.into(),
        status,
        lines,
        bytes: u64::from(lines) * 34,
        additions: changed.iter().map(|(a, b)| b - a + 1).sum(),
        deletions: del,
        changed,
        digest: 0,
    }
}

fn manifest() -> TargetManifest {
    TargetManifest {
        label: "feat/store-rewrite … main（6 ファイル）".into(),
        repo: Some(PathBuf::from("/Users/demo/repos/noa")),
        base_ref: Some("main".into()),
        base_sha: Some("4f9c2a1e7b3d8c60".into()),
        head_ref: Some("feat/store-rewrite".into()),
        head_sha: Some("a71be09d2c5f4e18".into()),
        merge_base: Some("4f9c2a1e7b3d8c60".into()),
        document_name: None,
        files: vec![
            file(
                "src/store.rs",
                FileStatus::Modified,
                212,
                vec![(96, 140), (180, 196)],
                31,
            ),
            file(
                "src/engine.rs",
                FileStatus::Modified,
                148,
                vec![(30, 64), (84, 96)],
                12,
            ),
            file(
                "src/live/runner.rs",
                FileStatus::Modified,
                326,
                vec![(200, 232), (290, 310)],
                18,
            ),
            file("src/capture.rs", FileStatus::Added, 120, vec![(1, 120)], 0),
            file(
                "docs/FEATURES.md",
                FileStatus::Modified,
                44,
                vec![(12, 14)],
                1,
            ),
        ],
        omitted: vec![Omitted {
            path: "assets/review.png".into(),
            reason: "バイナリ（テキストではない）".into(),
        }],
        diff_bytes: 48_211,
        diff_digest: Some(1),
        captured_at: T0,
    }
}

fn target() -> Target {
    Target {
        kind: TargetKind::Git,
        path: Some(PathBuf::from("/Users/demo/repos/noa")),
        base: Some("main".into()),
        head: Head::Ref("feat/store-rewrite".into()),
        paths: Vec::new(),
        text: None,
        workdir: None,
    }
}

fn panel() -> Vec<ReviewerChoice> {
    vec![
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
            agent: AgentChoice {
                kind: AgentKind::Codex,
                setup: AgentSetup {
                    model: Some("gpt-6.2-sol".into()),
                    effort: Some("xhigh".into()),
                },
            },
            lens: LensChoice::Builtin(BuiltinLens::Security),
        },
        ReviewerChoice {
            agent: AgentKind::Agy.into(),
            lens: LensChoice::Builtin(BuiltinLens::Design),
        },
    ]
}

struct Driver {
    engine: Engine,
    now: Millis,
    /// Every batch the engine committed, the log a store would hold.
    log: Vec<Vec<review::model::reduce::Event>>,
}

impl Driver {
    fn new(id: &str, hold: bool) -> Driver {
        let (engine, first) = Engine::create_in(
            SessionId::new(id),
            Setup {
                request: "ストアの書き換えです。ログのロックと再開まわりを重点的に見てください。",
                target: target(),
                manifest: manifest(),
                // As a git review's: the repository's top level.
                workdir: Some(PathBuf::from("/Users/demo/repos/noa")),
                max_rounds: 3,
                lead: AgentChoice {
                    kind: AgentKind::Claude,
                    setup: AgentSetup {
                        model: Some("opus".into()),
                        effort: None,
                    },
                },
                panel: &panel(),
            },
            &Path::new(DIR).join(id),
            Config::default(),
            T0,
        )
        .expect("the demo review is valid");
        let mut d = Driver {
            engine,
            now: T0 + 1_000,
            log: first.batches,
        };
        if hold {
            d.feed(Input::UserSetHold {
                hold: true,
                at: d.now,
            });
        }
        let now = d.now;
        d.feed(Input::Start { now });
        d
    }

    fn feed(&mut self, input: Input) {
        let out = self.engine.handle(input).expect("the demo input is valid");
        self.log.extend(out.batches);
    }

    fn deliver(&mut self) {
        let round = self.round();
        for part in ["claude", "codex", "agy"] {
            self.now += 20_000;
            let at = self.now;
            self.feed(Input::Sent {
                round,
                part: PartId::new(part),
                at,
            });
        }
    }

    fn round(&self) -> u32 {
        self.engine.session().current_round().map_or(1, |r| r.no)
    }

    fn reply(&mut self, part: &str, value: serde_json::Value) {
        let round = self.round();
        if part == "lead" {
            self.now += 10_000;
            let at = self.now;
            self.feed(Input::Sent {
                round,
                part: PartId::new(part),
                at,
            });
        }
        self.now += 4 * 60_000;
        let at = self.now;
        let content = value.to_string();
        let out = self.engine.handle(Input::ReplyFileWritten {
            round,
            part: PartId::new(part),
            content,
            at,
        });
        if let Ok(out) = &out {
            self.log.extend(out.batches.iter().cloned());
        }
        let session = self.engine.session();
        let rejected = session
            .current_round()
            .and_then(|r| r.brief(&PartId::new(part)))
            .and_then(|b| b.receipt.rejected.clone());
        assert!(
            out.is_ok() && rejected.is_none(),
            "demo reply of {part} rejected: {rejected:?}"
        );
    }
}

/// One raw finding as a reviewer writes it.
macro_rules! finding {
    ($id:expr, $path:expr, $line:expr, $end:expr, $sev:expr, $cat:expr, $conf:expr,
     $claim:expr, $evidence:expr, $fix:expr) => {
        json!({
            "id": $id,
            "location": {"path": $path, "line": $line, "end_line": $end},
            "severity": $sev, "category": $cat, "confidence": $conf,
            "claim": $claim, "evidence": $evidence, "fix": $fix
        })
    };
}

fn round_one(d: &mut Driver) {
    d.deliver();
    d.reply("claude", json!({
        "summary": "ログの読み込みと拡張の判定を中心に読みました。破損行の扱いに一件、上限拡張の条件に一件。",
        "findings": [
            finding!("1", "src/store.rs", 128, Some(134), "high", "correctness", "high",
                "途中で切れた最終行を残したまま追記するため、次のバッチが同じ行に連結される",
                "store.rs:128 は torn のとき kept < len でだけ切り詰め、kept == len では改行を足さずに戻る。", "torn のときは常に kept まで切り詰める"),
            finding!("2", "src/engine.rs", 88, None::<u32>, "medium", "correctness", "medium",
                "最終ラウンドの取りまとめ中でも +N が通り、総合判定の前に上限が動く",
                "extend() は closed でないとき Ok を返す。phase を見ていない。", "最終ラウンドで Consolidating のときは拒否する")
        ],
        "coverage": {"reviewed": ["src/store.rs", "src/engine.rs"], "skipped": [{"path": "docs/", "reason": "観点外"}]}
    }));
    d.reply("codex", json!({
        "summary": "シェルに渡す引数とログの信頼境界を確認しました。",
        "findings": [
            finding!("1", "src/store.rs", 130, None::<u32>, "high", "correctness", "medium",
                "壊れたログ行が次の起動で別のイベントとして読まれうる",
                "torn 行と次の append が連結されると、serde がエラーにせず別の型として読む余地がある。", "torn 検出時に切り詰めてから開く"),
            finding!("2", "src/live/runner.rs", 212, Some(219), "critical", "security", "high",
                "依頼文がシェルスクリプトに埋め込まれ、引用符で任意コマンドを実行できる",
                "launch_argv が format! で prompt を -c のスクリプトに入れている。\"; touch /tmp/x\" で再現。", "プロンプトは位置引数で渡し、スクリプトは \"$@\" のみにする")
        ],
        "coverage": {"reviewed": ["src/"]}
    }));
    d.reply("agy", json!({
        "summary": "モジュール境界を中心に見ました。",
        "findings": [
            finding!("1", "src/engine.rs", 40, Some(60), "low", "design", "medium",
                "純粋なはずのエンジンが check_resumable でファイルシステムを読む",
                "engine.rs のモジュール文書は I/O をしないと書くが、canonicalize と capture::verify を呼ぶ。", "検証はランナー側に移す"),
            finding!("2", "src/live/runner.rs", 300, None::<u32>, "medium", "performance", "low",
                "返答ファイルを 200ms ごとに読み直している",
                "poll_replies が watch ごとに read_to_string する。", "ファイル監視に置き換える")
        ],
        "coverage": {"reviewed": ["src/engine.rs", "src/live/"]}
    }));
    d.reply("lead", json!({
        "summary": "4件に統合。ログの破損行（F1）とシェル注入（F2）が重い。agy/2 はポーリング対象が有界なので退けた。",
        "ops": [
            {"op": "add", "id": "F1", "from": ["claude/1", "codex/1"], "claim": "破損した最終行を残して追記し、ログの行が連結される",
             "location": {"path": "src/store.rs", "line": 128, "end_line": 134}, "severity": "high", "category": "correctness",
             "confidence": "high", "status": "open", "rationale": "二人が同じ経路を独立に指摘。", "fix": "torn のとき kept まで切り詰める"},
            {"op": "add", "id": "F2", "from": ["codex/2"], "claim": "依頼文がシェルスクリプトに埋め込まれる",
             "location": {"path": "src/live/runner.rs", "line": 212, "end_line": 219}, "severity": "critical", "category": "security",
             "confidence": "high", "status": "open", "rationale": "再現手順つき。"},
            {"op": "add", "id": "F3", "from": ["claude/2"], "claim": "最終ラウンドの取りまとめ中に +N が通る",
             "location": {"path": "src/engine.rs", "line": 88}, "severity": "medium", "category": "correctness",
             "confidence": "medium", "status": "open", "rationale": "phase を見ていないように読める。"},
            {"op": "add", "id": "F4", "from": ["agy/1"], "claim": "エンジンが再開検証でファイルを読む",
             "location": {"path": "src/engine.rs", "line": 40, "end_line": 60}, "severity": "low", "category": "design",
             "confidence": "medium", "status": "open", "rationale": "文書との不一致。"},
            {"op": "dismiss", "raw": "agy/2", "reason": "監視対象はラウンド中の返答ファイルだけで有界。欠陥ではない。"}
        ],
        "questions": {
            "claude": "F2 の再現を確認し、F4 が実害になるか判断してください。",
            "codex": "F3 は権限の観点で影響がありますか。",
            "agy": "F1 と F2 を確認してください。"
        }
    }));
}

fn round_two(d: &mut Driver) {
    d.deliver();
    d.reply("claude", json!({
        "summary": "F2 は再現しました。F4 は文書化された例外なので反論します。",
        "findings": [],
        "stances": [
            {"finding": "F2", "stance": "confirm", "note": "\"; touch x\" で再現。"},
            {"finding": "F4", "stance": "dispute", "note": "check_resumable は再開時の I/O 例外として文書にある。", "evidence": "engine.rs:212 のコメント"}
        ],
        "coverage": {"reviewed": ["src/live/runner.rs"]}
    }));
    d.reply("codex", json!({
        "summary": "F3 は +3 でも同じです。F4 は判断できません。",
        "stances": [
            {"finding": "F3", "stance": "refine", "note": "+1 だけでなく +3 でも同じ経路。", "severity": "medium"},
            {"finding": "F4", "stance": "unsure"}
        ]
    }));
    d.reply("agy", json!({
        "summary": "F1・F2 を確認。F3 は 92 行で phase を見ているので反論。テスト不足を一件追加。",
        "findings": [
            finding!("3", "src/store.rs", 180, Some(196), "low", "tests", "high",
                "破損した最終行の切り詰めにテストがない",
                "store.rs の tests に torn の場合がない。", "torn 行を書いて open し、内容が元に戻ることを確かめる")
        ],
        "stances": [
            {"finding": "F1", "stance": "confirm"},
            {"finding": "F2", "stance": "confirm"},
            {"finding": "F3", "stance": "dispute", "note": "engine.rs:92 で Consolidating を拒否している。"}
        ],
        "coverage": {"reviewed": ["src/store.rs"]}
    }));
    d.reply("lead", json!({
        "summary": "F1・F2 は合意。F3 は対立、F4 は文書どおりなので却下。テスト不足を F5 として追加。",
        "ops": [
            {"op": "update", "id": "F1", "status": "agreed"},
            {"op": "update", "id": "F2", "status": "agreed"},
            {"op": "update", "id": "F3", "status": "disputed", "rationale": "agy は 92 行の検査を挙げ、codex は +3 でも再現と言う。"},
            {"op": "update", "id": "F4", "status": "rejected", "rationale": "再開時の I/O は設計上の例外として文書にある。"},
            {"op": "add", "id": "F5", "from": ["agy/3"], "claim": "破損行の切り詰めにテストがない",
             "location": {"path": "src/store.rs", "line": 180, "end_line": 196}, "severity": "low", "category": "tests",
             "confidence": "high", "status": "open", "rationale": "F1 の修正に回帰テストが要る。"}
        ],
        "questions": {"claude": "F3: 92 行の検査は最終ラウンドの取りまとめ中も効きますか。"}
    }));
}

fn round_three(d: &mut Driver) {
    d.deliver();
    d.reply("claude", json!({
        "summary": "92 行の検査は Briefing でしか効かないので F3 は残ります。F5 は支持。",
        "stances": [
            {"finding": "F3", "stance": "refine", "note": "92 行は Briefing のみ。Consolidating では素通り。", "evidence": "engine.rs:92 の match 腕"},
            {"finding": "F5", "stance": "confirm"}
        ]
    }));
    d.reply(
        "codex",
        json!({
            "summary": "F5 を支持します。",
            "stances": [{"finding": "F5", "stance": "confirm"}]
        }),
    );
    d.reply("agy", json!({
        "summary": "claude の指摘で F3 に納得しました。",
        "stances": [{"finding": "F3", "stance": "confirm", "note": "92 行は Briefing だけでした。"}]
    }));
    d.reply("lead", json!({
        "summary": "F3 も合意に。F1 と F2 は必ず直すこと。",
        "ops": [
            {"op": "update", "id": "F3", "status": "agreed", "confidence": "high"},
            {"op": "update", "id": "F5", "status": "agreed"}
        ],
        "verdict": {
            "decision": "request_changes",
            "rationale": "ログの行が連結される不具合（F1）とシェル注入（F2）はマージ前に直す必要がある。F3・F5 も同じ PR で直すのが望ましい。",
            "must_fix": ["F1", "F2"]
        }
    }));
}

/// The finished demo review.
pub fn session() -> Session {
    let mut d = Driver::new("rev-demo", false);
    round_one(&mut d);
    round_two(&mut d);
    round_three(&mut d);
    d.engine.session().clone()
}

/// The finished demo review's event log, for tests that need it on disk.
#[cfg(test)]
pub fn log() -> Vec<Vec<review::model::reduce::Event>> {
    let mut d = Driver::new("rev-demo", false);
    round_one(&mut d);
    round_two(&mut d);
    round_three(&mut d);
    d.log
}

/// The demo held after its first consolidation.
pub fn held() -> Session {
    let mut d = Driver::new("rev-demo-held", true);
    round_one(&mut d);
    d.engine.session().clone()
}

/// The demo's start view form.
pub fn start_form() -> Form {
    Form {
        kind: TargetKind::Git,
        repo: "~/repos/noa".into(),
        base: "main".into(),
        head: "feat/store-rewrite".into(),
        filter: "src/".into(),
        request: "ストアの書き換えです。ログのロックと再開まわりを重点的に見てください。".into(),
        lead: AgentChoice {
            kind: AgentKind::Claude,
            setup: AgentSetup {
                model: Some("opus".into()),
                effort: None,
            },
        },
        panel: {
            let mut panel = panel();
            panel[2].lens = LensChoice::Custom("API 互換性と移行手順".into());
            panel
        },
        ..Form::default()
    }
}

/// Canned past reviews for the start view.
pub fn past() -> Vec<Listed> {
    let done = session();
    let held = held();
    let row = |id: &str, session: &Session, busy: bool| Listed {
        dir: Path::new(DIR).join(id),
        summary: Ok(Summary {
            busy,
            ..Summary::of(&Path::new(DIR).join(id), session)
        }),
    };
    vec![
        row("rev-demo-held", &held, false),
        row("rev-demo", &done, false),
        Listed {
            dir: Path::new(DIR).join("rev-broken"),
            summary: Err("events.jsonl line 3 is damaged: expected value".into()),
        },
    ]
}

/// A canned stored review by its id.
pub fn stored(id: &str) -> Option<Session> {
    match id {
        "rev-demo" => Some(session()),
        "rev-demo-held" => Some(held()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use review::model::types::{Decision, FindingStatus};

    #[test]
    fn the_demo_runs_three_rounds_to_a_verdict() {
        let s = session();
        assert!(s.closed);
        assert_eq!(s.rounds.len(), 3);
        assert_eq!(s.verdict().unwrap().decision, Decision::RequestChanges);
        assert_eq!(s.finding("F4").unwrap().status, FindingStatus::Rejected);
        assert_eq!(s.dismissed.len(), 1);
        let h = held();
        assert!(h.waits_to_continue());
        assert_eq!(h.findings.len(), 4);
    }
}
