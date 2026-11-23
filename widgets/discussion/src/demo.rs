//! The canned discussion `--demo` shows. It is not hand-written state: a real
//! [`Engine`] is driven through five rounds with scripted agent output, so the
//! demo is always a session the engine itself can produce.
//!
//! The story: three panelists weigh a storage layer. Issue A is agreed, B
//! went to a debate (claude for, codex against) that ended `conditional`, C
//! is still contested, D is new in the last round, and round 5 is half
//! answered: claude replied, codex started, agy has not started yet.

use std::path::Path;

use discussion::engine::{AgentState, Config, Engine, Input};
use discussion::model::types::{
    AgentKind, Author, Millis, MsgKind, PartId, RoundNo, Session, SessionId,
};
use serde_json::{Value, json};

/// The demo's clock: 2026-11-22 10:00 JST, then a minute per step.
const T0: Millis = 1_763_773_200_000;
const MINUTE: Millis = 60_000;

pub const TOPIC: &str = "社内ツールの永続化層: SQLite のままか、PostgreSQL へ移るか";

struct Script {
    engine: Engine,
    now: Millis,
}

impl Script {
    fn tick(&mut self) -> Millis {
        self.now += MINUTE;
        self.now
    }

    fn feed(&mut self, input: Input) {
        if let Err(e) = self.engine.handle(input) {
            panic!("demo script rejected: {e}");
        }
    }

    fn session(&self) -> &Session {
        self.engine.session()
    }

    fn round(&self) -> RoundNo {
        self.session().current_round().map_or(1, |r| r.no)
    }

    /// The id of `part`'s latest message of `kind`.
    fn last(&self, part: &str, kind: MsgKind) -> String {
        self.session()
            .messages
            .iter()
            .rev()
            .find(|m| m.kind == kind && m.author == Author::Part(PartId::new(part)))
            .map(|m| m.id.to_string())
            .unwrap_or_else(|| panic!("no {kind:?} by {part}"))
    }

    fn deliver(&mut self, part: &str) {
        let round = self.round();
        let at = self.tick();
        self.feed(Input::Sent {
            round,
            part: PartId::new(part),
            at,
        });
    }

    fn start(&mut self, part: &str) {
        let at = self.tick();
        self.feed(Input::AgentStateChanged {
            part: PartId::new(part),
            state: AgentState::Running,
            at,
        });
    }

    fn write(&mut self, part: &str, content: &str) {
        let round = self.round();
        let at = self.tick();
        self.feed(Input::ReplyFileWritten {
            round,
            part: PartId::new(part),
            content: content.to_owned(),
            at,
        });
    }

    /// Delivers the round's brief to every panelist, then takes in `replies`
    /// in order.
    fn panel(&mut self, replies: &[(&str, String)]) {
        for part in ["claude", "codex", "agy"] {
            self.deliver(part);
            self.start(part);
        }
        for (part, text) in replies {
            self.write(part, text);
        }
    }

    fn moderate(&mut self, output: Value) {
        let round = self.round();
        self.deliver("noa");
        self.start("noa");
        self.write("noa", &output.to_string());
        let brief = self
            .session()
            .round(round)
            .and_then(|r| r.moderator_brief.as_ref());
        if let Some(why) = brief.and_then(|b| b.receipt.rejected.as_ref()) {
            panic!("demo moderator output for round {round} rejected:\n{why}");
        }
    }

    fn intervene(&mut self, to: &[&str], body: &str) {
        let at = self.tick();
        self.feed(Input::UserIntervention {
            to: to.iter().map(|p| PartId::new(*p)).collect(),
            body: body.to_owned(),
            at,
        });
    }
}

fn reply(header: &str, body: &str) -> String {
    if header.is_empty() {
        body.to_owned()
    } else {
        format!("---\n{header}\n---\n{body}")
    }
}

/// The demo session, folded from the scripted run.
pub fn session() -> Session {
    run(false)
}

/// The demo held after round 4 (`--demo-held`): the user paused the
/// discussion before the summary, so round 5 waits for the user, who has
/// written an intervention meanwhile.
pub fn held() -> Session {
    run(true)
}

fn run(held: bool) -> Session {
    let config = Config {
        debate_max_exchanges: 2,
        ..Config::default()
    };
    let (engine, _created) = Engine::create(
        SessionId::at(T0),
        TOPIC,
        AgentKind::Claude,
        &[AgentKind::Claude, AgentKind::Codex, AgentKind::Agy],
        Path::new("/demo/discussions/disc-demo"),
        config,
        T0,
    )
    .expect("demo session");
    let mut s = Script { engine, now: T0 };
    for (part, pane_id) in [("noa", 40), ("claude", 41), ("codex", 42), ("agy", 43)] {
        s.feed(Input::PaneSpawned {
            part: PartId::new(part),
            pane_id,
        });
    }
    let at = s.tick();
    s.feed(Input::Start { now: at });

    // ── Round 1: opening views ──────────────────────────────────────────────
    s.panel(&[
        (
            "claude",
            reply(
                "",
                "利用者は 40 人程度で書き込みも少ないので、SQLite (WAL) で十分です。\
                 運用コストがほぼゼロなのが最大の利点です。ただし将来の冗長化は別途検討が必要です。",
            ),
        ),
        (
            "codex",
            reply(
                "",
                "SQLite の継続には賛成ですが、障害時に読み取りを続けられるよう、\
                 初期から同期レプリケーションを入れるべきだと考えます。",
            ),
        ),
        (
            "agy",
            reply(
                "",
                "どちらでも構いませんが、スキーマ移行は前方互換の変更だけに限定するのが安全です。\
                 ロールバック手順を単純にできます。",
            ),
        ),
    ]);
    let (c1, x1, a1) = (
        s.last("claude", MsgKind::Reply),
        s.last("codex", MsgKind::Reply),
        s.last("agy", MsgKind::Reply),
    );
    s.moderate(json!({
        "summary": "全員が SQLite 継続に前向き。論点はレプリケーションの導入時期と、スキーマ移行の制約。",
        "issue_ops": [
            {"op": "add", "id": "A", "title": "SQLite (WAL) を既定のストレージにする"},
            {"op": "add", "id": "B", "title": "同期レプリケーションを初期から入れる"},
            {"op": "add", "id": "B1", "parent": "B", "title": "レプリカ遅延の許容値"},
            {"op": "add", "id": "C", "title": "スキーマ移行は前方互換の変更のみ"},
            {"op": "set_stance", "id": "A", "part": "claude", "pos": "for", "basis": c1},
            {"op": "set_stance", "id": "A", "part": "codex", "pos": "for", "basis": x1},
            {"op": "set_stance", "id": "B", "part": "codex", "pos": "for", "basis": x1},
            {"op": "set_stance", "id": "C", "part": "agy", "pos": "for", "basis": a1}
        ],
        "minutes_ops": [
            {"op": "add", "section": "open_questions", "text": "冗長化はいつ・どの方式で入れるか", "issues": ["B"]}
        ],
        "questions": {
            "claude": "codex の同期レプリケーション案について、運用コストの観点から意見をください。",
            "codex": "同期レプリケーションが必要な具体的な障害シナリオを挙げてください。",
            "agy": "前方互換のみの移行で困るケースはありますか。"
        }
    }));

    // ── Round 2: positions harden ───────────────────────────────────────────
    let c_q = s.last("claude", MsgKind::Reply);
    s.intervene(
        &[],
        "前提として、来期の運用担当は 1 名です。運用負荷も判断材料にしてください。",
    );
    s.panel(&[
        (
            "claude",
            reply(
                &format!("issues: [A, B]\nreply_to: [{x1}]\nstance: {{A: for, B: against}}"),
                "運用担当 1 名なら、同期レプリケーションは過剰です。\
                 夜間スナップショット + 15 分ごとの WAL 退避で RPO 15 分を満たせます。",
            ),
        ),
        (
            "codex",
            reply(
                &format!("issues: [B, C]\nreply_to: [{c_q}]\nstance: {{B: for, C: against}}"),
                "ディスク障害で 15 分分の書き込みを失うのは、承認ワークフローでは受け入れにくいです。\
                 また前方互換のみだと、カラムの型変更が永遠にできません。",
            ),
        ),
        (
            "agy",
            reply(
                "issues: [A, C]\nstance: {A: for, C: for}",
                "A には賛成です。C については、型変更は新カラム追加 + 二重書き込みで代替できます。",
            ),
        ),
    ]);
    s.moderate(json!({
        "summary": "A は全員賛成で合意。B は claude と codex が対立、C も codex が反対。B を討論で決着させたい。",
        "issue_ops": [
            {"op": "set_state", "id": "A", "state": "agreed"},
            {"op": "set_state", "id": "B", "state": "contested"},
            {"op": "set_state", "id": "C", "state": "contested"}
        ],
        "minutes_ops": [
            {"op": "add", "section": "decisions", "text": "既定のストレージは SQLite (WAL)", "issues": ["A"]},
            {"op": "add", "section": "contested", "text": "移行を前方互換のみに縛るか（codex 反対）", "issues": ["C"]}
        ],
        "propose_debate": {"issue": "B", "pro": ["codex"], "con": ["claude"]}
    }));

    // The user approves the debate as proposed.
    let debate = s.session().pending_debate().expect("proposed").id.clone();
    let at = s.tick();
    s.feed(Input::UserApproveDebate {
        id: debate,
        pro: None,
        con: None,
        at,
    });

    // ── Round 3: debate, first exchange ─────────────────────────────────────
    s.panel(&[
        (
            "codex",
            reply(
                "issues: [B]",
                "承認データは監査対象です。RPO 0 を最初から保証しないと、後から入れる際に\
                 アプリ側の書き込み経路をすべて見直すことになります。",
            ),
        ),
        (
            "claude",
            reply(
                "issues: [B]",
                "同期レプリケーションは書き込みレイテンシを 2〜3 倍にし、障害時の切り替え手順も増えます。\
                 運用 1 名でその手順を維持できるとは思えません。",
            ),
        ),
        (
            "agy",
            reply(
                "issues: [B]",
                "観察: 両者とも監査データだけが本当に RPO 0 を要する点は否定していません。",
            ),
        ),
    ]);
    s.moderate(json!({
        "summary": "論点は監査データの RPO。対象を絞れるかを次の応酬で詰める。",
        "questions": {
            "codex": "RPO 0 が必要なのは承認テーブルだけですか。",
            "claude": "監査データだけ別経路で保護する案は運用負荷に収まりますか。"
        }
    }));

    // ── Round 4: debate, second exchange and the verdict ───────────────────
    if held {
        let at = s.tick();
        s.feed(Input::UserSetHold { hold: true, at });
    }
    s.panel(&[
        (
            "codex",
            reply(
                "issues: [B, B1]",
                "はい、承認テーブルだけです。それ以外は数分の遅延を許容できます。",
            ),
        ),
        (
            "claude",
            reply(
                "issues: [B]\nstance: {B: neutral}",
                "承認テーブルだけを Litestream で即時退避するなら、運用負荷は許容範囲です。",
            ),
        ),
        (
            "agy",
            reply(
                "issues: [B1]",
                "B1 は 5 分以内で双方が受け入れられそうです。",
            ),
        ),
    ]);
    s.moderate(json!({
        "summary": "承認テーブルのみ即時退避、それ以外は従来どおりで双方合意。",
        "issue_ops": [
            {"op": "add", "id": "D", "title": "バックアップの復元訓練の頻度"}
        ],
        "minutes_ops": [
            {"op": "add", "section": "actions", "text": "承認テーブル向け Litestream 設定の検証（codex）", "issues": ["B"]},
            {"op": "add", "section": "open_questions", "text": "レプリカ遅延 5 分で監査要件を満たすか", "issues": ["B1"]}
        ],
        "verdict": {
            "outcome": "conditional",
            "rationale": "同期レプリケーションは承認テーブルに限って導入する。全体への適用は見送る。"
        },
        "questions": {
            "claude": "D について、復元訓練をどの頻度で行うべきか提案してください。",
            "codex": "C の型変更の代替手順（新カラム + 二重書き込み）に反対する理由は残りますか。",
            "agy": "D と B1 の関係について意見をください。"
        }
    }));

    // ── Round 5: open again, half answered ─────────────────────────────────
    s.intervene(
        &["codex"],
        "C については、移行ツールの制約も確認してから答えてください。",
    );
    if held {
        return s.engine.session().clone();
    }
    for part in ["claude", "codex", "agy"] {
        s.deliver(part);
    }
    s.start("claude");
    s.start("codex");
    s.write(
        "claude",
        &reply(
            "issues: [D]\nstance: {D: for}",
            "復元訓練は四半期に 1 回、手順書の更新とセットで行うのが妥当です。",
        ),
    );
    s.engine.session().clone()
}

/// Stored discussions for the demo's start view (`--demo-start`): one
/// that ended, one still going, one whose log does not fold.
pub fn history() -> Vec<discussion::store::Summary> {
    use discussion::store::{Outline, Summary};
    let session = session();
    let base = std::path::PathBuf::from("/demo/discussions");
    let ended = Outline {
        closed: true,
        ..Outline::of(&session)
    };
    let going = Outline {
        topic: "CI のキャッシュ戦略をどうするか".into(),
        rounds: 2,
        max_rounds: Some(5),
        closed: false,
        created_at: session.created_at + 86_400_000,
    };
    vec![
        Summary {
            id: SessionId::new("disc-demo-3"),
            dir: base.join("disc-demo-3"),
            state: Ok(going),
        },
        Summary {
            id: SessionId::new("disc-demo-2"),
            dir: base.join("disc-demo-2"),
            state: Err("events.jsonl line 4 is damaged: expected value".into()),
        },
        Summary {
            id: SessionId::new("disc-demo-1"),
            dir: base.join("disc-demo-1"),
            state: Ok(ended),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use discussion::model::types::{DebateStatus, IssueId, IssueState, Phase};

    #[test]
    fn the_demo_tells_its_story() {
        let session = session();
        let state = |id: &str| session.issues.get(&IssueId::new(id)).unwrap().state;
        assert_eq!(state("A"), IssueState::Agreed);
        assert_eq!(state("B"), IssueState::Conditional);
        assert_eq!(state("C"), IssueState::Contested);
        assert_eq!(state("D"), IssueState::Open);
        let debate = &session.debates[0];
        assert_eq!(debate.status, DebateStatus::Closed);
        assert!(debate.verdict.is_some());
        let round = session.current_round().unwrap();
        assert_eq!(round.no, 5);
        assert_eq!(round.phase, Phase::Awaiting);
        let receipt = |p: &str| &round.brief(&PartId::new(p)).unwrap().receipt;
        assert!(receipt("claude").replied.is_some());
        assert!(receipt("codex").started.is_some() && receipt("codex").replied.is_none());
        assert!(receipt("agy").sent.is_some() && receipt("agy").started.is_none());
    }
}
