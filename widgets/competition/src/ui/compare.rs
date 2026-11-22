//! Center pane "Compare" tab: side-by-side comparison of each contestant's
//! deliverables, scores, private feedback, and submission report for a round.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Tone};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use competition::model::types::{PartId, RoundKind, RoundNo, Session};

use super::{Action, caption, gap, keep_scroll, note, scroll_column, spacer, tag};

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    selected_round: Option<RoundNo>,
    filter_part: Option<&PartId>,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let list = scroll_column(tree, host, 12.0);
    keep_scroll(tree, list, session, "compare");

    let req = tree.insert(
        Some(list),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(4.0),
            padding: LayoutRect::length(10.0),
            size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        })
        .background(Role::Surface)
        .border(1.0, Role::Separator)
        .radius(8.0),
    );
    caption(tree, req, "依頼内容");
    tree.insert(
        Some(req),
        Label::new(session.request.clone())
            .font_size(13.0)
            .wrap(true),
    );

    let round = selected_round
        .and_then(|no| session.round(no))
        .or_else(|| session.current_round());

    let Some(round) = round else {
        note(tree, list, "最初のラウンドを開始しています…");
        return;
    };

    let rhead = tree.insert(
        Some(list),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    let (kind_label, kind_tone) = match round.kind {
        RoundKind::Blind => ("🔒 ブラインドラウンド（他パネル非公開）", Tone::Neutral),
        RoundKind::Reveal => ("👁 相互閲覧ラウンド（前ラウンド成果を公開）", Tone::Accent),
    };
    tree.insert(
        Some(rhead),
        Label::new(format!("Round {}", round.no))
            .font_size(15.0)
            .weight(600),
    );
    tag(tree, rhead, kind_label, kind_tone);

    if let Some(outcome) = &round.outcome {
        let ov = tree.insert(
            Some(list),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(4.0),
                padding: LayoutRect::length(10.0),
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                ..Default::default()
            })
            .background(Role::SurfaceAlt)
            .radius(8.0),
        );
        caption(tree, ov, "審判総評");
        tree.insert(
            Some(ov),
            Label::new(outcome.overview.clone())
                .font_size(12.0)
                .wrap(true),
        );
    }

    let order: Vec<PartId> = match &round.outcome {
        Some(outcome) if !outcome.ranking.is_empty() => outcome.ranking.clone(),
        _ => session.contestant_ids().cloned().collect(),
    };
    let visible = ranked_visible(order, filter_part);

    let columns = tree.insert(
        Some(list),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::STRETCH,
            gap: gap(10.0),
            size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        }),
    );

    for (rank_idx, part) in &visible {
        let is_winner = session.winner.as_ref() == Some(part);
        let col = tree.insert(
            Some(columns),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: length(0.0),
                min_size: LayoutSize {
                    width: length(180.0),
                    height: auto(),
                },
                gap: gap(8.0),
                padding: LayoutRect::length(12.0),
                ..Default::default()
            })
            .background(Role::Surface)
            .border(
                1.0,
                if is_winner {
                    Role::Accent
                } else {
                    Role::Separator
                },
            )
            .radius(8.0),
        );

        let head = tree.insert(
            Some(col),
            Container::row().align_items(AlignItems::CENTER).gap(6.0),
        );
        let title = if is_winner {
            format!("🏆 {part}")
        } else if round.outcome.is_some() {
            format!("#{} {part}", rank_idx + 1)
        } else {
            part.to_string()
        };
        tree.insert(Some(head), Label::new(title).font_size(14.0).weight(600));
        spacer(tree, head);
        if let Some(ev) = round.outcome.as_ref().and_then(|o| o.evaluations.get(part)) {
            tag(tree, head, format!("{}/100", ev.score), Tone::Accent);
        }

        if let Some(sub) = round.submissions.get(part) {
            tree.insert(
                Some(col),
                Label::new(sub.summary.clone())
                    .font_size(12.0)
                    .weight(600)
                    .wrap(true),
            );

            if !sub.artifacts.is_empty() {
                caption(tree, col, format!("成果物（{}件）", sub.artifacts.len()));
                let files = tree.insert(Some(col), Container::wrapping_row(4.0, 4.0));
                for file in &sub.artifacts {
                    let btn = tree.insert(
                        Some(files),
                        Button::new(format!("{} [{}]", file.rel_path, file.kind.label()))
                            .control_size(ControlSize::Small),
                    );
                    actions.push((
                        btn,
                        Action::OpenArtifact {
                            round: round.no,
                            part: part.clone(),
                            rel_path: file.rel_path.clone(),
                        },
                    ));
                }
            } else {
                caption(tree, col, "成果物ファイルなし");
            }

            if let Some(ev) = round.outcome.as_ref().and_then(|o| o.evaluations.get(part)) {
                if !ev.strengths.is_empty() {
                    caption(tree, col, "評価された点");
                    tree.insert(
                        Some(col),
                        Label::new(ev.strengths.clone()).font_size(11.5).wrap(true),
                    );
                }
                caption(tree, col, "審判フィードバック（個別）");
                tree.insert(
                    Some(col),
                    Label::new(ev.feedback.clone())
                        .font_size(11.5)
                        .color(Role::Muted)
                        .wrap(true),
                );
            }

            caption(tree, col, "提出レポート");
            tree.insert(
                Some(col),
                Label::new(sub.body.clone()).font_size(11.5).wrap(true),
            );

            if session.winner.as_ref() != Some(part) {
                let win_btn = tree.insert(
                    Some(col),
                    Button::new("この案を優勝に選ぶ").control_size(ControlSize::Small),
                );
                actions.push((win_btn, Action::SelectWinner(part.clone())));
            }
        } else {
            let status = round
                .brief(part)
                .and_then(|b| b.receipt.failed.as_deref())
                .map(|f| format!("失敗: {f}"))
                .unwrap_or_else(|| "提出待ち…".to_owned());
            note(tree, col, status);
        }
    }
}

/// The contestants shown, in `order`, each with its index in the full order
/// (its rank once judged), kept when `filter_part` narrows them to one.
fn ranked_visible(order: Vec<PartId>, filter_part: Option<&PartId>) -> Vec<(usize, PartId)> {
    order
        .into_iter()
        .enumerate()
        .filter(|(_, p)| filter_part.is_none_or(|f| f == p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filtered_contestant_keeps_its_rank() {
        let order: Vec<PartId> = ["claude", "codex", "agy"].map(PartId::new).into();
        assert_eq!(
            ranked_visible(order.clone(), Some(&PartId::new("agy"))),
            vec![(2, PartId::new("agy"))]
        );
        assert_eq!(ranked_visible(order, None).len(), 3);
    }
}
