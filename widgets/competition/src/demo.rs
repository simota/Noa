//! A canned multi-round competition (`--demo`) showing 5 blind rounds followed
//! by a Round 6 peer-reveal showcase with visual design artifacts and scores;
//! with `--judges`, the same competition scored by a panel of three judges,
//! waiting for the lead's final verdict.

use std::collections::BTreeMap;
use std::path::PathBuf;

use competition::store::{Listed, Summary};

use competition::model::aggregate;
use competition::model::types::{
    AgentChoice, AgentKind, AgentSetup, ArtifactFile, ArtifactKind, Author, Brief, EntryEvaluation,
    JUDGE_ID, JudgeOutput, Message, MessageId, Millis, MsgKind, PartId, Participant, Phase,
    Receipt, Role, Round, RoundKind, RoundNo, Session, SessionId, Submission, judge_id,
};

/// The start view's demo panel: the same CLI on two models, and codex.
pub fn start_panel() -> Vec<AgentChoice> {
    let on = |kind, model: &str, effort: Option<&str>| AgentChoice {
        kind,
        setup: AgentSetup {
            model: Some(model.into()),
            effort: effort.map(Into::into),
        },
    };
    vec![
        on(AgentKind::Claude, "opus", Some("high")),
        on(AgentKind::Claude, "sonnet", None),
        on(AgentKind::Codex, "gpt-6.2-sol", Some("xhigh")),
    ]
}

/// The demo's past competitions by id, each made from [`session`]: what the
/// start view lists and what opening or rematching a row shows.
pub fn stored(id: &str) -> Option<Session> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as Millis);
    let hour = 3_600_000;
    let mut session = session();
    session.id = SessionId::new(id);
    match id {
        "comp-4" => {
            session.request =
                "音楽プレイヤーのミニマルな UI を設計・実装する（index.html で確認可能にすること）"
                    .into();
            session.rounds.truncate(3);
            session.created_at = now.saturating_sub(2 * hour);
            session.contestants[0].setup = AgentSetup {
                model: Some("opus".into()),
                effort: Some("high".into()),
            };
            session.contestants[1].setup = AgentSetup {
                model: Some("gpt-6.2-sol".into()),
                effort: None,
            };
        }
        "comp-3" => {
            session.request = "社内ダッシュボードの KPI カードを 3 案作る".into();
            session.rounds.truncate(2);
            session.created_at = now.saturating_sub(5 * hour);
        }
        "comp-2" => {
            // Ran all its rounds: it can go on with more.
            session.max_rounds = Some(session.rounds.len() as competition::model::types::RoundNo);
            session.closed = true;
            session.winner = Some(PartId::new("claude"));
            session.created_at = now.saturating_sub(3 * 24 * hour);
        }
        _ => return None,
    }
    Some(session)
}

/// Canned past competitions for the demo's start view, newest first:
/// interrupted, running in another window, finished, and a damaged log.
pub fn past() -> Vec<Listed> {
    let listed = |id: &str, busy: bool| {
        let dir = PathBuf::from("/demo").join(id);
        let session = stored(id).expect("a demo competition");
        Listed {
            summary: Ok(Summary {
                busy,
                ..Summary::of(&dir, &session)
            }),
            dir,
        }
    };
    vec![
        listed("comp-4", false),
        listed("comp-3", true),
        listed("comp-2", false),
        Listed {
            dir: PathBuf::from("/demo/comp-1"),
            summary: Err("events.jsonl line 4 is damaged: EOF while parsing".into()),
        },
    ]
}

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
        rejected_digest: None,
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
            co_judge_briefs: Vec::new(),
            scorecards: BTreeMap::new(),
            verdict_brief: None,
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
        co_judges: Vec::new(),
        contestants: vec![
            p("claude", AgentKind::Claude, Role::Contestant),
            p("codex", AgentKind::Codex, Role::Contestant),
            p("agy", AgentKind::Agy, Role::Contestant),
        ],
        messages,
        rounds,
        winner: None,
        closed: false,
        hold: false,
    }
}

/// [`session`] scored by three judges: claude on opus leads, codex and agy
/// score too. Each round's outcome is the panel's combined judgment; the
/// last round is the final one, scored and waiting for the lead's verdict.
pub fn panel_session() -> Session {
    let mut session = session();
    let judge = |seat: usize, kind, model: Option<&str>| Participant {
        id: judge_id(seat),
        kind,
        role: Role::Judge,
        pane_id: None,
        setup: AgentSetup {
            model: model.map(Into::into),
            effort: None,
        },
    };
    session.judge = judge(0, AgentKind::Claude, Some("opus"));
    session.co_judges = vec![
        judge(1, AgentKind::Codex, Some("gpt-6.2-sol")),
        judge(2, AgentKind::Agy, None),
    ];
    let last = session.rounds.len() as RoundNo;
    session.max_rounds = Some(last);
    // How far each co-judge's score is from the lead's, per entry in the
    // evaluations' order (agy, claude, codex). codex's last round splits
    // the panel widely.
    let offsets = |no: RoundNo| -> [[i16; 3]; 2] {
        if no == last {
            [[-2, 3, 4], [1, -1, -16]]
        } else {
            [[2, -1, 3], [-1, 2, 1]]
        }
    };
    let overviews = [
        "codex の見立て: 操作性と情報設計を重く見ました。",
        "agy の見立て: ストーリーと構成の一貫性を重く見ました。",
    ];
    for round in &mut session.rounds {
        let Some(lead) = round.outcome.clone() else {
            continue;
        };
        let mut cards = BTreeMap::from([(judge_id(0), lead.clone())]);
        for (k, offset) in offsets(round.no).into_iter().enumerate() {
            let mut card = lead.clone();
            card.overview = format!("R{}: {}", round.no, overviews[k]);
            card.checklist = Vec::new();
            for (ev, delta) in card.evaluations.values_mut().zip(offset) {
                ev.score = (i16::from(ev.score) + delta).clamp(0, 100) as u8;
            }
            cards.insert(judge_id(k + 1), card);
        }
        let mut brief = round.judge_brief.clone().expect("a judged round");
        round.co_judge_briefs = (1..=2)
            .map(|seat| {
                brief.to = judge_id(seat);
                brief.reply_path = PathBuf::from(format!("r/{}.json", brief.to));
                brief.clone()
            })
            .collect();
        round.scorecards = cards;
    }
    for no in 1..=last {
        let round = &session.rounds[no as usize - 1];
        let combined = aggregate::combine(&session, no, &round.scorecards);
        session.rounds[no as usize - 1].outcome = Some(combined);
    }
    // The final round: scored, the verdict asked of the lead (not sent
    // yet, so no clock runs in a canned view).
    let final_round = session.rounds.last_mut().expect("rounds");
    final_round.phase = Phase::Judging;
    let mut verdict = final_round.judge_brief.clone().expect("a judged round");
    verdict.reply_path = PathBuf::from("r/verdict.json");
    verdict.receipt = Receipt::default();
    final_round.verdict_brief = Some(verdict);
    session
}
