//! Right pane: judge overview, winner card, comparison viewer trigger,
//! Markdown export, and adopting the winner into a repository. With a judge
//! panel each judged round also shows its means, a table of scores by
//! judge with their spread, and each co-judge's overview.

use std::collections::BTreeSet;

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Tone};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use competition::model::aggregate::SPREAD_WARN;
use competition::model::types::{AgentKind, JUDGE_ID, Round, RoundKind, RoundNo, Session};

use super::{Action, caption, gap, heading, keep_scroll, note, scroll_column, spacer, tag};

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    can_act: bool,
    agents: &[AgentKind],
    tools_expanded: bool,
    expanded_rounds: &BTreeSet<RoundNo>,
    actions: &mut Vec<(WidgetId, Action)>,
) -> Option<WidgetId> {
    let header = heading(tree, host, "結果・講評");
    let small = |label: &str| Button::new(label).control_size(ControlSize::Small);
    spacer(tree, header);
    let tools = tree.insert(Some(header), small("操作").selected(tools_expanded));
    tree.set_accessible_label(tools, "結果の出力・コンペの操作を表示");
    actions.push((tools, Action::ToggleTools));

    // Keep the opened controls beside their toggle, independent of the
    // saved scroll position of a long judging history.
    let tools_host = if tools_expanded {
        let group = tree.insert(
            Some(host),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                flex_shrink: 0.0,
                gap: gap(6.0),
                padding: LayoutRect::length(crate::appearance::space(12.0)),
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                ..Default::default()
            }),
        );
        let tools = tree.insert(Some(group), Container::wrapping_row(6.0, 6.0));
        for (label, action) in [
            ("比較ビューアーを開く", Action::OpenViewer),
            ("結果をコピー", Action::CopyResult),
            ("レポート保存…", Action::ExportReport),
            ("新しいコンペ", Action::NewCompetition),
            ("同じ設定でリマッチ", Action::Rematch),
        ] {
            let button = tree.insert(Some(tools), small(label));
            actions.push((button, action));
        }
        if can_act && session.workdir.is_none() {
            note(
                tree,
                group,
                "優勝案の採用には、開始時に参照ディレクトリを指定してください",
            );
        }
        Some(tools)
    } else {
        None
    };

    if let Some(winner) = &session.winner {
        let banner = tree.insert(
            Some(host),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(6.0),
                padding: LayoutRect::length(crate::appearance::space(10.0)),
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                ..Default::default()
            })
            .background(Role::SurfaceAlt)
            .border(1.0, Role::Accent)
            .radius(8.0),
        );
        tag(tree, banner, format!("🏆 優勝: {winner}"), Tone::Success);
        if can_act && session.workdir.is_some() && !agents.is_empty() {
            let adopt = tree.insert(
                Some(banner),
                Container::wrapping_row(6.0, 6.0).align_items(AlignItems::CENTER),
            );
            caption(tree, adopt, "優勝案を採用:");
            for &kind in agents {
                let button = tree.insert(Some(adopt), small(kind.command()));
                actions.push((
                    button,
                    Action::Adopt {
                        winner: winner.clone(),
                        agent: kind,
                    },
                ));
            }
        }
    }

    let list = scroll_column(tree, host, 12.0);
    keep_scroll(tree, list, session, "judge");
    if let Some(verdict) = session.final_verdict() {
        tree.insert(
            Some(list),
            Label::new(verdict.to_owned())
                .font_size(crate::appearance::font(14.0))
                .wrap(true),
        );
    }

    let mut any = false;
    for round in session.rounds.iter().rev() {
        let Some(outcome) = &round.outcome else {
            continue;
        };
        any = true;
        let card = tree.insert(
            Some(list),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(6.0),
                padding: LayoutRect {
                    left: length(10.0),
                    right: length(8.0),
                    top: length(8.0),
                    bottom: length(8.0),
                },
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                ..Default::default()
            })
            .background(Role::SurfaceAlt)
            .radius(6.0),
        );
        let row = tree.insert(
            Some(card),
            Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
        );
        let (kind_label, tone) = match round.kind {
            RoundKind::Blind => ("🔒 ブラインド", Tone::Neutral),
            RoundKind::Reveal => ("👁 相互閲覧", Tone::Accent),
        };
        tag(tree, row, format!("R{} {kind_label}", round.no), tone);
        tree.insert(
            Some(card),
            Label::new(outcome.overview.clone())
                .font_size(crate::appearance::font(14.0))
                .wrap(true),
        );
        let panel = !round.scorecards.is_empty();
        let ranks: Vec<String> = outcome
            .ranking
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let sc = if panel {
                    session
                        .score_label(round.no, p)
                        .map(|mean| format!("({mean})"))
                        .unwrap_or_default()
                } else {
                    outcome
                        .evaluations
                        .get(p)
                        .map(|e| format!("({})", e.score))
                        .unwrap_or_default()
                };
                format!("#{} {p}{sc}", i + 1)
            })
            .collect();
        note(tree, card, ranks.join(" · "));
        if panel {
            let expanded = expanded_rounds.contains(&round.no);
            let details = tree.insert(
                Some(card),
                small(if expanded {
                    "審判別の詳細を閉じる"
                } else {
                    "審判別の評価"
                })
                .selected(expanded),
            );
            actions.push((details, Action::ToggleJudgeDetails(round.no)));
            if expanded {
                panel_details(tree, card, session, round);
            } else {
                for part in &outcome.ranking {
                    if let Some(scores) = session.panel_scores(round.no, part)
                        && scores.spread() >= SPREAD_WARN
                    {
                        tag(
                            tree,
                            card,
                            format!("{part} · 審判間の差 {}", scores.spread()),
                            Tone::Warning,
                        );
                    }
                }
            }
            if awaits_verdict(session, round) {
                note(tree, card, format!("最終講評を待っています（{JUDGE_ID}）"));
            }
        }
        // A verdict that is no longer the result (the competition went on,
        // or a later choice overrode it) stays with its round.
        if let (Some(winner), Some(verdict)) = (&outcome.winner, &outcome.final_verdict)
            && session.final_verdict() != Some(verdict.as_str())
        {
            note(
                tree,
                card,
                format!("この時点の判定: 🏆 {winner} — {}", verdict.trim()),
            );
        }
    }
    if !any {
        caption(tree, list, "（まだ審判の評価はありません）");
    }
    tools_host
}

const NAME_WIDTH: f32 = 58.0;
const SCORE_WIDTH: f32 = 40.0;

/// One cell of the scores table: `text` in a box `width` wide.
fn cell(tree: &mut UiTree, row: WidgetId, text: impl Into<String>, width: f32, strong: bool) {
    let host = tree.insert(
        Some(row),
        Container::new(Style {
            flex_shrink: 0.0,
            size: LayoutSize {
                width: length(width),
                height: auto(),
            },
            ..Default::default()
        }),
    );
    let label = Label::new(text.into()).font_size(crate::appearance::font(12.0));
    let label = if strong {
        label.weight(600)
    } else {
        label.color(Role::Muted)
    };
    tree.insert(Some(host), label);
}

/// A panel round's scores by judge (one row per entry in the combined
/// order, a column per judge and the spread, a warning from
/// [`SPREAD_WARN`] up), each co-judge's overview, and whether the lead's
/// verdict is still awaited.
fn panel_details(tree: &mut UiTree, card: WidgetId, session: &Session, round: &Round) {
    let Some(outcome) = &round.outcome else {
        return;
    };
    caption(tree, card, "審判別スコア");
    let table = tree.insert(
        Some(card),
        Container::column().gap(crate::appearance::space(2.0)),
    );
    let head = tree.insert(
        Some(table),
        Container::row().align_items(AlignItems::CENTER),
    );
    cell(tree, head, "", NAME_WIDTH, false);
    for judge in session.judges() {
        cell(tree, head, judge.id.as_str(), SCORE_WIDTH, false);
    }
    cell(tree, head, "差", SCORE_WIDTH, false);
    for part in &outcome.ranking {
        let Some(panel) = session.panel_scores(round.no, part) else {
            continue;
        };
        let row = tree.insert(
            Some(table),
            Container::row().align_items(AlignItems::CENTER),
        );
        cell(tree, row, part.as_str(), NAME_WIDTH, true);
        for (_, score) in &panel.scores {
            cell(tree, row, score.to_string(), SCORE_WIDTH, false);
        }
        let spread = panel.spread();
        if spread >= SPREAD_WARN {
            tag(tree, row, format!("⚠ 差 {spread}"), Tone::Warning);
        } else {
            cell(tree, row, spread.to_string(), SCORE_WIDTH, false);
        }
    }
    for (seat, judge) in session.co_judges.iter().enumerate() {
        let Some(card_out) = round.scorecards.get(&judge.id) else {
            continue;
        };
        caption(
            tree,
            card,
            format!("審判 {} ({}) の総評", seat + 2, judge.kind.command()),
        );
        tree.insert(
            Some(card),
            Label::new(card_out.overview.clone())
                .font_size(crate::appearance::font(14.0))
                .wrap(true),
        );
    }
}

/// Whether `round` still waits for the lead's verdict: asked for, not in,
/// and the competition still running. 終了 and a winner chosen by hand
/// both close the session (`UserSelectWinner` commits `SessionClosed` with
/// the winner), and the verdict is then never written. A round that waits
/// is the session's last, since a closed round cannot be extended past.
pub(super) fn awaits_verdict(session: &Session, round: &Round) -> bool {
    !session.closed && round.verdict_pending()
}
