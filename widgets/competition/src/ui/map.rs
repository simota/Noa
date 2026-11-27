//! Center pane "方向性マップ" tab: each contestant's direction as a mind map,
//! the request on the left, the contestants branching from it, and each
//! contestant's submissions round by round with the judge's score.
//! Positions come from `competition::model::map::layout`; this module draws
//! them.

use std::collections::HashMap;

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Position, Rect as LayoutRect, Size as LayoutSize, auto, length,
};
use noa_gpui_core::widgets::{Container, Label, ScrollView, Tone, Truncation};
use noa_gpui_core::{PaintCx, Role, Style, UiTree, Widget, WidgetId};
use noa_gpui_scene::Point;

use competition::model::map::{Node, NodeKind, layout};
use competition::model::types::{PartId, RoundKind, RoundNo, Session};

use super::{fill, gap, tag};

/// The lines between the nodes, drawn behind them: out of a parent's right
/// edge, across half the gap, down or up to the child's row, and in.
struct Edges {
    lines: Vec<((f32, f32), (f32, f32))>,
    size: (f32, f32),
}

impl Widget for Edges {
    fn style(&self) -> Style {
        Style {
            position: Position::Absolute,
            inset: LayoutRect {
                left: length(0.0),
                top: length(0.0),
                right: auto(),
                bottom: auto(),
            },
            size: LayoutSize {
                width: length(self.size.0),
                height: length(self.size.1),
            },
            ..Default::default()
        }
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        let origin = cx.bounds();
        let color = cx.color(Role::Separator);
        let at = |(x, y): (f32, f32)| Point::new(origin.x + x, origin.y + y);
        for &(from, to) in &self.lines {
            let mid = (from.0 + to.0) / 2.0;
            cx.line(at(from), at((mid, from.1)), 1.5, color);
            cx.line(at((mid, from.1)), at((mid, to.1)), 1.5, color);
            cx.line(at((mid, to.1)), at(to), 1.5, color);
        }
    }
}

fn absolute(node: &Node) -> Style {
    Style {
        position: Position::Absolute,
        inset: LayoutRect {
            left: length(node.x),
            top: length(node.y),
            right: auto(),
            bottom: auto(),
        },
        size: LayoutSize {
            width: length(node.w),
            height: length(node.h),
        },
        ..Default::default()
    }
}

fn card(tree: &mut UiTree, parent: WidgetId, node: &Node, accent: bool) -> WidgetId {
    let outer = tree.insert(Some(parent), Container::new(absolute(node)));
    tree.insert(
        Some(outer),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(4.0),
            padding: LayoutRect::length(crate::appearance::space(8.0)),
            ..fill()
        })
        .background(Role::SurfaceAlt)
        .border(
            if accent { 1.5 } else { 1.0 },
            if accent {
                Role::Accent
            } else {
                Role::Separator
            },
        )
        .radius(8.0)
        .clip(true),
    )
}

/// A score and how it moved since the contestant's previous scored round:
/// "82 pt (+4)".
pub(super) fn score_label(history: &[(RoundNo, u8)], round: RoundNo) -> Option<String> {
    let at = history.iter().position(|(no, _)| *no == round)?;
    let score = history[at].1;
    Some(match at.checked_sub(1).map(|prev| history[prev].1) {
        Some(prev) => format!("{score} pt ({:+})", i32::from(score) - i32::from(prev)),
        None => format!("{score} pt"),
    })
}

/// Draws the map of `session` into `host`; with `filter_part`, that
/// contestant's branch alone.
pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    filter_part: Option<&PartId>,
) {
    let ranking = session.current_ranking();
    let order: Vec<PartId> = match filter_part {
        Some(part) => vec![part.clone()],
        None => ranking.clone(),
    };
    let map = layout(session, &order);
    // The scroll view takes its axes from `horizontal`/`vertical`, not from
    // the style's overflow: a map is wider than its pane as often as taller.
    let scroll = tree.insert(
        Some(host),
        ScrollView::new(fill()).horizontal(true).vertical(true),
    );
    tree.set_autosave_name(scroll, format!("{}/map", session.id));
    let canvas = tree.insert(
        Some(scroll),
        Container::new(Style {
            position: Position::Relative,
            flex_shrink: 0.0,
            size: LayoutSize {
                width: length(map.width),
                height: length(map.height),
            },
            ..Default::default()
        }),
    );
    let lines = map
        .edges
        .iter()
        .map(|e| (map.nodes[e.from].out_point(), map.nodes[e.to].in_point()))
        .collect();
    tree.insert(
        Some(canvas),
        Edges {
            lines,
            size: (map.width, map.height),
        },
    );

    let histories: HashMap<&PartId, Vec<(RoundNo, u8)>> = order
        .iter()
        .map(|part| (part, session.score_history(part)))
        .collect();

    for node in &map.nodes {
        match &node.kind {
            NodeKind::Request => {
                let body = card(tree, canvas, node, true);
                tree.insert(
                    Some(body),
                    Label::new(session.request.clone())
                        .font_size(crate::appearance::font(12.5))
                        .weight(600)
                        .wrap(true)
                        .max_lines(3),
                );
                let state = if session.closed {
                    "終了"
                } else {
                    "進行中"
                };
                let limit = session
                    .max_rounds
                    .map(|max| format!(" / {max}"))
                    .unwrap_or_default();
                tree.insert(
                    Some(body),
                    Label::new(format!(
                        "ラウンド {}{limit} · {state}",
                        session.rounds.len()
                    ))
                    .font_size(crate::appearance::font(11.0))
                    .color(Role::Muted),
                );
            }
            NodeKind::Contestant(part) => {
                let winner = session.winner.as_ref() == Some(part);
                let body = card(tree, canvas, node, winner);
                let rank = ranking.iter().position(|p| p == part).map_or(0, |i| i + 1);
                let name = if winner {
                    format!("🏆 #{rank} {part}")
                } else {
                    format!("#{rank} {part}")
                };
                tree.insert(
                    Some(body),
                    Label::new(name)
                        .font_size(crate::appearance::font(13.0))
                        .weight(600),
                );
                let row = tree.insert(
                    Some(body),
                    Container::row()
                        .align_items(AlignItems::CENTER)
                        .gap(crate::appearance::space(6.0)),
                );
                match session.latest_score(part) {
                    Some(score) => tag(tree, row, format!("{score} pt"), Tone::Accent),
                    None => tag(tree, row, "未採点", Tone::Neutral),
                }
                let count = session
                    .rounds
                    .iter()
                    .filter(|r| r.submissions.contains_key(part))
                    .count();
                tree.insert(
                    Some(row),
                    Label::new(format!("提出 {count}件"))
                        .font_size(crate::appearance::font(11.0))
                        .color(Role::Muted),
                );
            }
            NodeKind::Entry { part, round } => {
                let Some(r) = session.round(*round) else {
                    continue;
                };
                let Some(submission) = r.submissions.get(part) else {
                    continue;
                };
                let body = card(tree, canvas, node, false);
                let head = tree.insert(
                    Some(body),
                    Container::row()
                        .align_items(AlignItems::CENTER)
                        .gap(crate::appearance::space(6.0)),
                );
                let (label, tone) = match r.kind {
                    RoundKind::Blind => (format!("R{round}"), Tone::Neutral),
                    RoundKind::Reveal => (format!("R{round} 👁"), Tone::Accent),
                };
                tag(tree, head, label, tone);
                if let Some(history) = histories.get(part)
                    && let Some(score) = score_label(history, *round)
                {
                    tag(tree, head, score, Tone::Success);
                }
                tree.insert(
                    Some(body),
                    Label::new(submission.summary.clone())
                        .font_size(crate::appearance::font(12.0))
                        .wrap(true)
                        .max_lines(2),
                );
                let strengths = r
                    .outcome
                    .as_ref()
                    .and_then(|o| o.evaluations.get(part))
                    .map(|e| e.strengths.trim())
                    .filter(|s| !s.is_empty());
                if let Some(strengths) = strengths {
                    tree.insert(
                        Some(body),
                        Label::new(format!("審判: {strengths}"))
                            .font_size(crate::appearance::font(10.5))
                            .color(Role::Muted)
                            .truncation(Truncation::Tail),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_score_shows_how_it_moved_since_the_last_scored_round() {
        let history = [(1, 70), (3, 78), (4, 75)];
        assert_eq!(score_label(&history, 1).as_deref(), Some("70 pt"));
        assert_eq!(score_label(&history, 3).as_deref(), Some("78 pt (+8)"));
        assert_eq!(score_label(&history, 4).as_deref(), Some("75 pt (-3)"));
        assert_eq!(score_label(&history, 2), None);
    }
}
