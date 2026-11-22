//! Left pane: leaderboard with score history, current round status, and
//! round selector (marking Blind vs Peer-Reveal rounds).

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Tone};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use competition::model::types::{PartId, RoundKind, RoundNo, Session};

use super::{Action, caption, gap, heading, keep_scroll, scroll_column, spacer, tag};

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    selected_round: Option<RoundNo>,
    filter_part: Option<&PartId>,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    heading(tree, host, "リーダーボード");
    let list = scroll_column(tree, host, 10.0);
    keep_scroll(tree, list, session, "leaderboard");

    let ranking = session.current_ranking();
    for (idx, part) in ranking.iter().enumerate() {
        let is_winner = session.winner.as_ref() == Some(part);
        let is_selected = filter_part == Some(part);
        let card = tree.insert(
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
            .background(if is_selected {
                Role::Surface
            } else {
                Role::SurfaceAlt
            })
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

        let top = tree.insert(
            Some(card),
            Container::row().align_items(AlignItems::CENTER).gap(6.0),
        );
        let rank_label = if is_winner {
            format!("🏆 #{} {part}", idx + 1)
        } else {
            format!("#{} {part}", idx + 1)
        };
        tree.insert(
            Some(top),
            Label::new(rank_label).font_size(13.0).weight(600),
        );
        spacer(tree, top);
        if let Some(score) = session.latest_score(part) {
            tag(tree, top, format!("{score} pt"), Tone::Accent);
        } else {
            tag(tree, top, "未採点", Tone::Neutral);
        }

        let history: Vec<String> = session
            .score_history(part)
            .into_iter()
            .map(|(r, s)| format!("R{r}:{s}"))
            .collect();
        if !history.is_empty() {
            super::note(tree, card, history.join(" → "));
        }

        if let Some(cur) = session.current_round()
            && let Some(brief) = cur.brief(part)
        {
            let (status_text, tone) = if brief.receipt.failed.is_some() {
                ("失敗", Tone::Critical)
            } else if brief.receipt.replied.is_some() {
                ("提出済", Tone::Success)
            } else if brief.receipt.started.is_some() {
                ("制作中", Tone::Accent)
            } else if brief.receipt.sent.is_some() {
                ("送信済", Tone::Warning)
            } else {
                ("待機中", Tone::Neutral)
            };
            let row = tree.insert(
                Some(card),
                Container::row().align_items(AlignItems::CENTER).gap(6.0),
            );
            tag(tree, row, format!("R{} {status_text}", cur.no), tone);
            spacer(tree, row);
            let btn = tree.insert(
                Some(row),
                Button::new(if is_selected { "解除" } else { "絞込" })
                    .control_size(ControlSize::Small),
            );
            let action = if is_selected {
                Action::ClearPartFilter
            } else {
                Action::FilterPart(part.clone())
            };
            actions.push((btn, action));
        }
    }

    let rhead = tree.insert(
        Some(list),
        Container::new(Style {
            padding: LayoutRect {
                top: length(8.0),
                ..LayoutRect::length(0.0)
            },
            ..Default::default()
        }),
    );
    caption(
        tree,
        rhead,
        if session.reveal_interval == 0 {
            "ラウンド履歴（完全ブラインド）".to_owned()
        } else {
            format!("ラウンド履歴（{}R毎に相互閲覧）", session.reveal_interval)
        },
    );

    let active_no = selected_round.or_else(|| session.current_round().map(|r| r.no));
    for round in session.rounds.iter().rev() {
        let is_active = active_no == Some(round.no);
        let badge = match round.kind {
            RoundKind::Blind => "🔒 ブラインド",
            RoundKind::Reveal => "👁 相互閲覧",
        };
        let label = format!("R{} · {badge}", round.no);
        let btn = tree.insert(
            Some(list),
            Button::new(label)
                .control_size(ControlSize::Small)
                .default_button(is_active),
        );
        actions.push((btn, Action::SelectRound(round.no)));
    }
}
