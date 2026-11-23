//! Left pane: leaderboard with score history, current round status, a jump
//! to each agent's pane (the judge's too), and round selector (marking Blind
//! vs Peer-Reveal rounds).

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
    can_focus: bool,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let focus_button = |tree: &mut UiTree, row: WidgetId, actions: &mut Vec<_>, part: &PartId| {
        if can_focus {
            let btn = tree.insert(
                Some(row),
                Button::new("ペインを開く").control_size(ControlSize::Small),
            );
            actions.push((btn, Action::FocusPane(part.clone())));
        }
    };
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
        let score_hist = session.score_history(part);
        // Which model and effort this entry runs on: what tells two entries
        // of one CLI apart.
        if let Some(setup) = session.participant(part).and_then(|p| p.setup.label()) {
            caption(tree, card, setup);
        }
        if let Some(&(_, score)) = score_hist.last() {
            tag(tree, top, format!("{score} pt"), Tone::Accent);
        } else {
            tag(tree, top, "未採点", Tone::Neutral);
        }

        if !score_hist.is_empty() {
            let mut line = String::with_capacity(score_hist.len() * 10);
            for (i, (r, s)) in score_hist.iter().enumerate() {
                if i > 0 {
                    line.push_str(" → ");
                }
                let _ = std::fmt::Write::write_fmt(&mut line, format_args!("R{r}:{s}"));
            }
            super::note(tree, card, line);
        }

        let brief = session
            .current_round()
            .and_then(|cur| Some((cur.no, cur.brief(part)?)));
        if brief.is_some() || can_focus {
            let row = tree.insert(
                Some(card),
                Container::row().align_items(AlignItems::CENTER).gap(6.0),
            );
            if let Some((no, brief)) = brief {
                let (status_text, tone) = super::progress::delivery(&brief.receipt);
                tag(tree, row, format!("R{no} {status_text}"), tone);
            }
            spacer(tree, row);
            focus_button(tree, row, actions, part);
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

    judge_card(tree, list, session, &focus_button, actions);

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

/// The judge, below the contestants: its agent, what it is doing this
/// round, and the jump to its pane.
fn judge_card(
    tree: &mut UiTree,
    list: WidgetId,
    session: &Session,
    focus_button: &impl Fn(&mut UiTree, WidgetId, &mut Vec<(WidgetId, Action)>, &PartId),
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let judge = &session.judge;
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
        .background(Role::SurfaceAlt)
        .border(1.0, Role::Separator)
        .radius(8.0),
    );
    let label = match judge.setup.label() {
        Some(setup) => format!("審判 · {} ({setup})", judge.kind.command()),
        None => format!("審判 · {}", judge.kind.command()),
    };
    tree.insert(Some(card), Label::new(label).font_size(12.5).weight(600));
    let row = tree.insert(
        Some(card),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    if let Some(cur) = session.current_round()
        && let Some(brief) = cur.brief(&judge.id)
    {
        let (status_text, tone) = super::progress::delivery(&brief.receipt);
        tag(tree, row, format!("R{} {status_text}", cur.no), tone);
    }
    spacer(tree, row);
    focus_button(tree, row, actions, &judge.id);
}
