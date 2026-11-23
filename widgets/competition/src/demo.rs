//! A canned multi-round competition (`--demo`) showing 5 blind rounds followed
//! by a Round 6 peer-reveal showcase with visual design artifacts and scores.

use std::collections::BTreeMap;
use std::path::PathBuf;

use competition::model::types::{
    AgentKind, ArtifactFile, ArtifactKind, Author, Brief, EntryEvaluation, JUDGE_ID, JudgeOutput,
    Message, MessageId, MsgKind, PartId, Participant, Phase, Receipt, Role, Round, RoundKind,
    Session, SessionId, Submission,
};

pub fn session() -> Session {
    let p = |id: &str, kind, role| Participant {
        id: PartId::new(id),
        kind,
        role,
        pane_id: None,
        setup: Default::default(),
    };
    let claude = PartId::new("claude");
    let codex = PartId::new("codex");
    let agy = PartId::new("agy");
    let judge = PartId::new(JUDGE_ID);

    let done_receipt = |t| Receipt {
        sent: Some(t),
        started: Some(t + 100),
        replied: Some(t + 5_000),
        retries: 0,
        failed: None,
        rejected: None,
    };

    let make_brief = |to: &PartId, t| Brief {
        to: to.clone(),
        base_rev: 1,
        body: String::new(),
        reply_path: PathBuf::from(format!("r/{to}.md")),
        receipt: done_receipt(t),
    };

    let mut rounds = Vec::new();
    let mut messages = Vec::new();
    let mut msg_seq = 0usize;
    let mut push_msg = |round, author, kind, body: String, at| {
        msg_seq += 1;
        messages.push(Message {
            id: MessageId::nth(msg_seq),
            round,
            author,
            kind,
            to: Vec::new(),
            body,
            at,
        });
    };

    let scores: [(u8, u8, u8); 6] = [
        (72, 75, 70),
        (78, 79, 76),
        (82, 81, 80),
        (85, 84, 83),
        (88, 86, 85),
        (93, 91, 89),
    ];

    for (idx, (sc, sx, sa)) in scores.into_iter().enumerate() {
        let no = (idx + 1) as u32;
        let kind = RoundKind::for_round(no, 5);
        let t = (no as u64) * 60_000;

        let mut submissions = BTreeMap::new();
        let entries = [
            (
                &claude,
                format!("R{no}: 和紙トーン＋余白重視のランディングページ"),
                format!(
                    "R{no} ではタイポグラフィ階層とヒーローの視線誘導を改善しました。\n\
                     - `index.html`: レスポンシブグリッドと焙煎プロファイルカード\n\
                     - `hero.svg`: 豆の産地マップをベクターで実装"
                ),
                vec![
                    ArtifactFile {
                        rel_path: "index.html".into(),
                        kind: ArtifactKind::Html,
                        bytes: 8420 + no as u64 * 400,
                    },
                    ArtifactFile {
                        rel_path: "hero.svg".into(),
                        kind: ArtifactKind::Svg,
                        bytes: 3120,
                    },
                ],
            ),
            (
                &codex,
                format!("R{no}: インタラクティブ焙煎度スライダー付きデザイン"),
                format!(
                    "R{no} では浅煎り〜深煎りの味覚チャートをリアルタイム切替できる UI を組み込みました。\n\
                     - `index.html`: Vanilla JS のインタラクティブ試飲ガイド\n\
                     - `styles.css`: ダーク＆ゴールドの高級感ある配色"
                ),
                vec![
                    ArtifactFile {
                        rel_path: "index.html".into(),
                        kind: ArtifactKind::Html,
                        bytes: 9650 + no as u64 * 500,
                    },
                    ArtifactFile {
                        rel_path: "styles.css".into(),
                        kind: ArtifactKind::Code,
                        bytes: 4180,
                    },
                ],
            ),
            (
                &agy,
                format!("R{no}: エディトリアル雑誌風レイアウトとストーリー構成"),
                format!(
                    "R{no} では農園ストーリーと定期便プラン比較テーブルを洗練しました。\n\
                     - `index.html`: 2カラム・エディトリアル構成\n\
                     - `preview.png`: ビジュアルモックアップ"
                ),
                vec![
                    ArtifactFile {
                        rel_path: "index.html".into(),
                        kind: ArtifactKind::Html,
                        bytes: 7910 + no as u64 * 350,
                    },
                    ArtifactFile {
                        rel_path: "preview.png".into(),
                        kind: ArtifactKind::Image,
                        bytes: 48_200,
                    },
                ],
            ),
        ];

        for (part, summary, body, artifacts) in entries {
            push_msg(
                no,
                Author::Part(part.clone()),
                MsgKind::Submission,
                body.clone(),
                t + 10,
            );
            submissions.insert(
                part.clone(),
                Submission {
                    part: part.clone(),
                    summary,
                    body,
                    artifacts,
                    at: t + 10,
                },
            );
        }

        let mut evaluations = BTreeMap::new();
        evaluations.insert(
            claude.clone(),
            EntryEvaluation {
                score: sc,
                strengths: "余白設計と視認性の高さ、SVGの完成度".into(),
                feedback: if no < 6 {
                    "購入導線（CTA）のコントラストとモバイル表示の余白をさらに詰めてください。"
                        .into()
                } else {
                    "相互閲覧を経てインタラクションも取り込み、総合力でトップに立ちました。".into()
                },
                evidence: Vec::new(),
                amended: None,
                breakdown: Vec::new(),
            },
        );
        evaluations.insert(
            codex.clone(),
            EntryEvaluation {
                score: sx,
                strengths: "焙煎度スライダーの操作感と情報設計".into(),
                feedback:
                    "ファーストビューの情報密度を少し下げ、ブランドの世界観を強調してください。"
                        .into(),
                evidence: Vec::new(),
                amended: None,
                breakdown: Vec::new(),
            },
        );
        evaluations.insert(
            agy.clone(),
            EntryEvaluation {
                score: sa,
                strengths: "ストーリーテリングと定期便の分かりやすさ".into(),
                feedback: "カード間のリズムとボタン状態のフィードバックを強化してください。".into(),
                evidence: Vec::new(),
                amended: None,
                breakdown: Vec::new(),
            },
        );

        let overview = if kind == RoundKind::Reveal {
            "R5 の相互閲覧を経て全パネルが大きく飛躍。claude が視認性と操作性を両立しリードを広げました。".to_owned()
        } else {
            format!("R{no}（ブラインド）: 各パネルが独自路線でブラッシュアップを進めています。")
        };
        push_msg(
            no,
            Author::Part(judge.clone()),
            MsgKind::Judgment,
            overview.clone(),
            t + 40,
        );

        rounds.push(Round {
            no,
            kind,
            phase: Phase::Closed,
            started_at: t,
            briefs: vec![
                make_brief(&claude, t),
                make_brief(&codex, t),
                make_brief(&agy, t),
            ],
            submissions,
            judge_brief: Some(make_brief(&judge, t + 20)),
            outcome: Some(JudgeOutput {
                overview,
                evaluations,
                ranking: vec![claude.clone(), codex.clone(), agy.clone()],
                winner: None,
                final_verdict: None,
                close: false,
                amendments: Vec::new(),
                checklist: Vec::new(),
            }),
        });
    }

    Session {
        id: SessionId::new("comp-demo"),
        request: "スペシャルティコーヒー焙煎所のブランドランディングページをデザイン・実装する（HTML/CSS/JS で比較可能にすること）".into(),
        workdir: None,
        max_rounds: Some(10),
        reveal_interval: 5,
        rev: 24,
        created_at: 1,
        judge: p(JUDGE_ID, AgentKind::Claude, Role::Judge),
        contestants: vec![
            p("claude", AgentKind::Claude, Role::Contestant),
            p("codex", AgentKind::Codex, Role::Contestant),
            p("agy", AgentKind::Agy, Role::Contestant),
        ],
        messages,
        rounds,
        winner: None,
        closed: false,
    }
}
