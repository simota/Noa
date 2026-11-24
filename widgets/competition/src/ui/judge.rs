//! Right pane: judge overview, winner card, comparison viewer trigger,
//! Markdown export, and adopting the winner into a repository. With a judge
//! panel each judged round also shows its means, a table of scores by
//! judge with their spread, and each co-judge's overview.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Tone};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use competition::model::aggregate::SPREAD_WARN;
use competition::model::types::{AgentKind, JUDGE_ID, Round, RoundKind, Session};

use super::{Action, caption, gap, heading, keep_scroll, note, scroll_column, tag};

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    can_act: bool,
    agents: &[AgentKind],
    actions: &mut Vec<(WidgetId, Action)>,
) {
    heading(tree, host, "審判・アクション");
    let small = |label: &str| Button::new(label).control_size(ControlSize::Small);

    let tools = tree.insert(Some(host), Container::wrapping_row(6.0, 6.0));
    let viewer = tree.insert(
        Some(tools),
        Button::new("比較ビューアーを開く")
            .control_size(ControlSize::Small)
            .default_button(true),
    );
    actions.push((viewer, Action::OpenViewer));
    let copy = tree.insert(Some(tools), small("結果をコピー"));
    actions.push((copy, Action::CopyResult));
    let export = tree.insert(Some(tools), small("レポート保存…"));
    actions.push((export, Action::ExportReport));

    if let Some(winner) = &session.winner {
        let banner = tree.insert(
            Some(host),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(6.0),
                padding: LayoutRect::length(10.0),
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
        if let Some(verdict) = session.final_verdict() {
            tree.insert(
                Some(banner),
                Label::new(verdict.to_owned()).font_size(12.0).wrap(true),
            );
        }
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
    } else if can_act && session.workdir.is_none() {
        note(
            tree,
            host,
            "優勝案のリポジトリ採用は、参照ディレクトリを指定したコンペで使えます",
        );
    }

    let list = scroll_column(tree, host, 12.0);
    keep_scroll(tree, list, session, "judge");

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
            Container::row().align_items(AlignItems::CENTER).gap(6.0),
        );
        let (kind_label, tone) = match round.kind {
            RoundKind::Blind => ("🔒 ブラインド", Tone::Neutral),
            RoundKind::Reveal => ("👁 相互閲覧", Tone::Accent),
        };
        tag(tree, row, format!("R{} {kind_label}", round.no), tone);
        tree.insert(
            Some(card),
            Label::new(outcome.overview.clone())
                .font_size(12.0)
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
        caption(tree, card, ranks.join(" · "));
        if panel {
            panel_details(tree, card, session, round);
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
}

const NAME_WIDTH: f32 = 72.0;
const SCORE_WIDTH: f32 = 50.0;

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
    let label = Label::new(text.into()).font_size(11.5);
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
    let table = tree.insert(Some(card), Container::column().gap(2.0));
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
                .font_size(12.0)
                .wrap(true),
        );
    }
    if round.verdict_pending() {
        note(tree, card, format!("最終講評を待っています（{JUDGE_ID}）"));
    }
}
