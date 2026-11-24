//! Left pane: leaderboard with score history, current round status, a jump
//! to each agent's pane (the judges' too), and round selector (marking Blind
//! vs Peer-Reveal rounds). With a judge panel the scores are means, and the
//! latest round's score by each judge and their spread show under them.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Tone};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use competition::model::aggregate::{PanelScores, SPREAD_WARN};
use competition::model::types::{
    Brief, PartId, Participant, Round, RoundKind, RoundNo, Session,
};

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
        if session.is_panel() {
            panel_scores(tree, top, card, session, part, &score_hist);
        } else if let Some(&(_, score)) = score_hist.last() {
            tag(tree, top, format!("{score} pt"), Tone::Accent);
        } else {
            tag(tree, top, "未採点", Tone::Neutral);
        }

        if !session.is_panel() && !score_hist.is_empty() {
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

    if session.is_panel() {
        for (seat, judge) in session.judges().enumerate() {
            let name = match seat {
                0 => "リード審判".to_owned(),
                n => format!("審判 {}", n + 1),
            };
            judge_card(tree, list, session, judge, &name, &focus_button, actions);
        }
    } else {
        judge_card(
            tree,
            list,
            session,
            &session.judge,
            "審判",
            &focus_button,
            actions,
        );
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

/// A panel entry's mean scores: the latest mean as its tag, the means by
/// round, and the latest round's score by each judge with their spread
/// (a warning from [`SPREAD_WARN`] up).
fn panel_scores(
    tree: &mut UiTree,
    top: WidgetId,
    card: WidgetId,
    session: &Session,
    part: &PartId,
    score_hist: &[(RoundNo, u8)],
) {
    let label = |no: RoundNo| {
        session
            .score_label(no, part)
            .unwrap_or_else(|| "—".to_owned())
    };
    let Some(&(last, _)) = score_hist.last() else {
        tag(tree, top, "未採点", Tone::Neutral);
        return;
    };
    tag(tree, top, format!("{} pt", label(last)), Tone::Accent);
    let hist: Vec<String> = score_hist
        .iter()
        .map(|(no, _)| format!("R{no}:{}", label(*no)))
        .collect();
    super::note(tree, card, hist.join(" → "));
    if let Some(panel) = session.panel_scores(last, part) {
        if panel.spread() >= SPREAD_WARN {
            // The spread as its own tag: a whole line would not fit.
            let row = tree.insert(
                Some(card),
                Container::wrapping_row(6.0, 4.0).align_items(AlignItems::CENTER),
            );
            super::note(tree, row, judge_scores(&panel));
            tag(tree, row, format!("⚠ 差 {}", panel.spread()), Tone::Warning);
        } else {
            super::note(tree, card, judge_line(&panel));
        }
    }
}

/// The brief a judge card shows the status of: the judge's brief of
/// `round`, the lead's verdict brief while it is awaited. A verdict the
/// competition ended without (終了, or a winner chosen by hand) is not
/// awaited, so the lead's card then shows its scorecard brief.
fn judge_status_brief<'a>(
    session: &Session,
    round: &'a Round,
    judge: &Participant,
) -> Option<&'a Brief> {
    if judge.id == session.judge.id
        && round.verdict_pending()
        && !super::judge::awaits_verdict(session, round)
    {
        round.judge_brief.as_ref()
    } else {
        round.brief(&judge.id)
    }
}

/// `judge 80 · judge-2 83 · judge-3 81`.
fn judge_scores(panel: &PanelScores) -> String {
    let parts: Vec<String> = panel
        .scores
        .iter()
        .map(|(judge, score)| format!("{judge} {score}"))
        .collect();
    parts.join(" · ")
}

/// `judge 80 · judge-2 83 · judge-3 81 · 差 3`.
fn judge_line(panel: &PanelScores) -> String {
    format!("{} · 差 {}", judge_scores(panel), panel.spread())
}

/// A judge, below the contestants: its seat (`name`), its agent, what it
/// is doing this round (the lead's verdict once asked), and the jump to
/// its pane.
fn judge_card(
    tree: &mut UiTree,
    list: WidgetId,
    session: &Session,
    judge: &Participant,
    name: &str,
    focus_button: &impl Fn(&mut UiTree, WidgetId, &mut Vec<(WidgetId, Action)>, &PartId),
    actions: &mut Vec<(WidgetId, Action)>,
) {
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
        Some(setup) => format!("{name} · {} ({setup})", judge.kind.command()),
        None => format!("{name} · {}", judge.kind.command()),
    };
    tree.insert(Some(card), Label::new(label).font_size(12.5).weight(600));
    let row = tree.insert(
        Some(card),
        Container::row().align_items(AlignItems::CENTER).gap(6.0),
    );
    if let Some(cur) = session.current_round()
        && let Some(brief) = judge_status_brief(session, cur, judge)
    {
        let (status_text, tone) = super::progress::delivery(&brief.receipt);
        tag(tree, row, format!("R{} {status_text}", cur.no), tone);
    }
    spacer(tree, row);
    focus_button(tree, row, actions, &judge.id);
}
