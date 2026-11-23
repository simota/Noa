//! Right pane: judge overview, winner card, comparison viewer trigger,
//! Markdown export, and adopting the winner into a repository.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Tone};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use competition::model::types::{AgentKind, RoundKind, Session};

use super::{Action, caption, gap, heading, keep_scroll, note, scroll_column, tag};

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    can_act: bool,
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
        if can_act && session.workdir.is_some() {
            let adopt = tree.insert(
                Some(banner),
                Container::wrapping_row(6.0, 6.0).align_items(AlignItems::CENTER),
            );
            caption(tree, adopt, "優勝案を採用:");
            for kind in AgentKind::ALL {
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
        let ranks: Vec<String> = outcome
            .ranking
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let sc = outcome
                    .evaluations
                    .get(p)
                    .map(|e| format!("({})", e.score))
                    .unwrap_or_default();
                format!("#{} {p}{sc}", i + 1)
            })
            .collect();
        caption(tree, card, ranks.join(" · "));
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
