//! A native, scrollable canvas. Cards and ports are ordinary accessible controls;
//! only the grid and connections use custom paint.

use super::{Action, style};
use noa_gpui_core::style::{
    FlexDirection, Position, Rect as LayoutRect, Size as LayoutSize, length,
};
use noa_gpui_core::widgets::{Container, Label, Pressable};
use noa_gpui_core::{PaintCx, Role, Style, UiTree, Widget, WidgetId};
use noa_gpui_scene::Point;
use workflow::model::{NodeKind, NodeState, Run, WorkflowDefinition};

pub(super) const CARD_W: f32 = 204.0;
pub(super) const CARD_H: f32 = 132.0;

struct Wire {
    points: Vec<(f32, f32)>,
    done: bool,
    auxiliary: bool,
}
struct Background {
    size: (f32, f32),
    zoom: f32,
    wires: Vec<Wire>,
}
impl Widget for Background {
    fn style(&self) -> Style {
        style::absolute(0.0, 0.0, self.size.0, self.size.1)
    }
    fn paint(&mut self, cx: &mut PaintCx) {
        let origin = cx.bounds();
        let muted = cx.color(Role::Separator);
        let step = 24.0 * self.zoom;
        let at =
            |(x, y): (f32, f32)| Point::new(origin.x + x * self.zoom, origin.y + y * self.zoom);
        let mut x = step;
        while x < self.size.0.min(2400.0) {
            let mut y = step;
            while y < self.size.1.min(2400.0) {
                cx.line(
                    Point::new(origin.x + x, origin.y + y),
                    Point::new(origin.x + x + 1.0, origin.y + y),
                    1.0,
                    muted,
                );
                y += step;
            }
            x += step;
        }
        for wire in &self.wires {
            let color = if wire.done {
                cx.color(Role::Accent)
            } else {
                cx.color(Role::Outline)
            };
            for pair in wire.points.windows(2) {
                if wire.auxiliary {
                    let (a, b) = (pair[0], pair[1]);
                    let distance = (b.0 - a.0).abs() + (b.1 - a.1).abs();
                    let mut cursor = 0.0;
                    while cursor < distance {
                        let t = cursor / distance;
                        let u = (cursor + 5.0).min(distance) / distance;
                        cx.line(
                            at((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)),
                            at((a.0 + (b.0 - a.0) * u, a.1 + (b.1 - a.1) * u)),
                            1.2,
                            color,
                        );
                        cursor += 10.0;
                    }
                } else {
                    cx.line(at(pair[0]), at(pair[1]), 1.7, color);
                }
            }
            if let Some(&(x, y)) = wire.points.last() {
                cx.line(at((x - 5.0, y - 4.0)), at((x, y)), 1.5, color);
                cx.line(at((x - 5.0, y + 4.0)), at((x, y)), 1.5, color);
            }
        }
    }
}

pub(super) fn extent(definition: &WorkflowDefinition) -> (f32, f32) {
    definition
        .nodes
        .iter()
        .fold((780.0_f32, 660.0_f32), |(w, h), n| {
            (
                w.max(n.position.x + CARD_W + 48.0),
                h.max(n.position.y + CARD_H + 56.0),
            )
        })
}

pub(super) struct Card {
    pub id: String,
    pub widget: WidgetId,
}
pub(super) struct Canvas {
    pub cards: Vec<Card>,
}

pub(super) struct Options<'a> {
    pub definition: &'a WorkflowDefinition,
    pub run: Option<&'a Run>,
    pub selected: Option<&'a str>,
    pub source: Option<&'a str>,
    pub zoom: f32,
    pub editable: bool,
}

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    options: Options<'_>,
    actions: &mut Vec<(WidgetId, Action)>,
) -> Canvas {
    let Options {
        definition,
        run,
        selected,
        source,
        zoom,
        editable,
    } = options;
    let (w, h) = extent(definition);
    let size = (w * zoom, h * zoom);
    let canvas = tree.insert(
        Some(host),
        Container::new(Style {
            position: Position::Relative,
            flex_shrink: 0.0,
            size: LayoutSize {
                width: length(size.0),
                height: length(size.1),
            },
            ..Default::default()
        }),
    );
    let mut wires = Vec::new();
    for edge in &definition.edges {
        let (Some(from), Some(to)) = (definition.node(&edge.from), definition.node(&edge.to))
        else {
            continue;
        };
        let a = (from.position.x + CARD_W, from.position.y + CARD_H / 2.0);
        let b = (to.position.x, to.position.y + CARD_H / 2.0);
        let auxiliary = from.kind == NodeKind::Implement && to.kind == NodeKind::Approval;
        let points = if b.0 > a.0 && (a.1 - b.1).abs() < 1.0 {
            vec![a, b]
        } else if auxiliary {
            let x = w - 24.0;
            let y = to.position.y + CARD_H + 26.0;
            vec![a, (x, a.1), (x, y), (b.0 - 20.0, y), (b.0 - 20.0, b.1), b]
        } else if b.1 > a.1 {
            let y = (from.position.y + CARD_H + to.position.y) / 2.0;
            vec![
                a,
                (a.0 + 18.0, a.1),
                (a.0 + 18.0, y),
                (b.0 - 18.0, y),
                (b.0 - 18.0, b.1),
                b,
            ]
        } else {
            let y = from.position.y - 22.0;
            vec![
                a,
                (a.0 + 18.0, a.1),
                (a.0 + 18.0, y),
                (b.0 - 18.0, y),
                (b.0 - 18.0, b.1),
                b,
            ]
        };
        let done = run
            .and_then(|r| r.nodes.get(&edge.from))
            .is_some_and(|n| n.state == NodeState::Succeeded);
        wires.push(Wire {
            points,
            done,
            auxiliary,
        });
    }
    tree.insert(Some(canvas), Background { size, zoom, wires });
    let mut cards = Vec::new();
    for (index, node) in definition.nodes.iter().enumerate() {
        let x = node.position.x * zoom;
        let y = node.position.y * zoom;
        let is_selected = selected == Some(node.id.as_str());
        let state = run
            .and_then(|r| r.nodes.get(&node.id))
            .map_or(NodeState::Pending, |n| n.state);
        let outer = tree.insert(
            Some(canvas),
            Pressable::new(style::absolute(x, y, CARD_W * zoom, CARD_H * zoom))
                .label(format!(
                    "{}、{}、{}",
                    node.title,
                    node.kind.label(),
                    state.label()
                ))
                .selected(is_selected)
                .radius(10.0),
        );
        actions.push((outer, Action::Select(node.id.clone())));
        let body = tree.insert(
            Some(outer),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                padding: LayoutRect::length(13.0 * zoom),
                gap: LayoutSize {
                    width: length(6.0 * zoom),
                    height: length(6.0 * zoom),
                },
                ..style::root()
            })
            .background(Role::Surface)
            .border(
                if is_selected { 2.0 } else { 1.0 },
                if is_selected {
                    Role::Accent
                } else {
                    Role::Separator
                },
            )
            .radius(10.0),
        );
        tree.insert(
            Some(body),
            Label::new(format!("{:02}   {}", index + 1, node.kind.label()))
                .font_size(11.0 * zoom)
                .weight(600)
                .color(Role::Muted),
        );
        tree.insert(
            Some(body),
            Label::new(&node.title)
                .font_size(16.0 * zoom)
                .weight(600)
                .max_lines(1),
        );
        let detail = if run.is_some() {
            format!("{}  {}", style::state_symbol(state), state.label())
        } else if node.kind == NodeKind::Approval {
            "◇  変更と根拠を確認".into()
        } else if node.kind == NodeKind::PullRequest {
            "↗  承認後に公開".into()
        } else {
            node.kind.description().into()
        };
        tree.insert(
            Some(body),
            Label::new(detail)
                .font_size(11.0 * zoom)
                .color(if run.is_some() {
                    style::state_color(state).into()
                } else {
                    noa_gpui_core::Paint::Role(Role::Muted)
                })
                .wrap(true)
                .max_lines(2),
        );
        tree.insert(
            Some(body),
            Label::new(format!("{}  →", node.kind.output_kind().label()))
                .font_size(10.0 * zoom)
                .color(Role::Muted),
        );
        if editable {
            if node.kind != NodeKind::Trigger {
                let input = tree.insert(
                    Some(canvas),
                    Pressable::new(style::absolute(
                        x - 11.0 * zoom,
                        y + (CARD_H / 2.0 - 11.0) * zoom,
                        22.0 * zoom,
                        22.0 * zoom,
                    ))
                    .label(format!("{}へ接続", node.title))
                    .tooltip("出力ポートを選び、この入力ポートを選択")
                    .radius(11.0),
                );
                tree.insert(
                    Some(input),
                    Label::new("○").font_size(18.0 * zoom).color(Role::Accent),
                );
                actions.push((input, Action::Connect(node.id.clone())));
            }
            if node.kind != NodeKind::PullRequest {
                let output = tree.insert(
                    Some(canvas),
                    Pressable::new(style::absolute(
                        x + (CARD_W - 11.0) * zoom,
                        y + (CARD_H / 2.0 - 11.0) * zoom,
                        22.0 * zoom,
                        22.0 * zoom,
                    ))
                    .label(format!("{}から接続", node.title))
                    .selected(source == Some(node.id.as_str()))
                    .tooltip("ここから接続先の入力へドラッグ、または順に選択")
                    .radius(11.0),
                );
                tree.insert(
                    Some(output),
                    Label::new("●").font_size(15.0 * zoom).color(Role::Accent),
                );
                actions.push((output, Action::Source(node.id.clone())));
            }
        }
        cards.push(Card {
            id: node.id.clone(),
            widget: outer,
        });
    }
    Canvas { cards }
}
