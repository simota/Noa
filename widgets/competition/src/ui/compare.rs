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
    can_act: bool,
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
        // In the header, not after the report: a long report or artifact
        // list must not push the choice out of reach.
        if can_act && round.submissions.contains_key(part) && !is_winner {
            let win_btn = tree.insert(
                Some(col),
                Button::new("この案を優勝に選ぶ").control_size(ControlSize::Small),
            );
            actions.push((win_btn, Action::SelectWinner(part.clone())));
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
                for (title, text, muted) in evaluation_sections(ev) {
                    caption(tree, col, title);
                    let label = Label::new(text).font_size(11.5).wrap(true);
                    tree.insert(
                        Some(col),
                        if muted {
                            label.color(Role::Muted)
                        } else {
                            label
                        },
                    );
                }
            }

            caption(tree, col, "提出レポート");
            tree.insert(
                Some(col),
                Label::new(sub.body.clone()).font_size(11.5).wrap(true),
            );
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

/// The judge's evaluation of one entry as (caption, text, muted) sections.
fn evaluation_sections(
    ev: &competition::model::types::EntryEvaluation,
) -> Vec<(&'static str, String, bool)> {
    let mut sections = Vec::new();
    if !ev.strengths.is_empty() {
        sections.push(("評価された点", ev.strengths.clone(), false));
    }
    sections.push(("審判フィードバック（個別）", ev.feedback.clone(), true));
    if !ev.evidence.is_empty() {
        let lines: Vec<String> = ev
            .evidence
            .iter()
            .map(|e| format!("{}: {}", e.basis.as_str(), e.claim))
            .collect();
        sections.push(("採点の根拠", lines.join("\n"), true));
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_evaluation_shows_the_evidence_with_its_basis() {
        use competition::model::types::{EntryEvaluation, Evidence, EvidenceBasis};
        let ev = EntryEvaluation {
            score: 80,
            strengths: "s".into(),
            feedback: "f".into(),
            evidence: vec![
                Evidence {
                    claim: "index.html is 19 KB".into(),
                    basis: EvidenceBasis::Measured,
                },
                Evidence {
                    claim: "0 axe violations".into(),
                    basis: EvidenceBasis::SelfReported,
                },
            ],
            amended: None,
            breakdown: vec![],
        };
        let sections = evaluation_sections(&ev);
        let evidence = sections
            .iter()
            .find(|(title, _, _)| *title == "採点の根拠")
            .map(|(_, text, _)| text.as_str());
        assert_eq!(
            evidence,
            Some("measured: index.html is 19 KB\nself_reported: 0 axe violations")
        );
    }

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
