//! The map tab: the discussion as a mind map, the topic on the left and its
//! issues branching to the right, each with the way its state moved round
//! by round, the agents' stances, and the minutes items that belong to it.
//! Positions come from `discussion::model::map::layout`; this module draws
//! them. A click on an issue opens the chat filtered to it.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Position, Rect as LayoutRect, Size as LayoutSize, auto, length,
};
use noa_gpui_core::widgets::{Container, Label, Pressable, ScrollView, Tone, Truncation};
use std::path::Path;

use noa_gpui_core::{Message, PaintCx, Role, Style, Theme, UiTree, Widget, WidgetId};
use noa_gpui_platform::{Headless, Screen};
use noa_gpui_scene::{Point, Size};
use noa_gpui_text::TextStore;

use discussion::model::map::{MindMap, Node, NodeKind, layout};
use discussion::model::types::{IssueState, Session};

use super::minutes::section_title;
use super::{Action, fill, gap, part_name, position_tag, state_tag, tag};

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

/// How an issue's state moved: "R1 未決 → R2 対立 → R4 条件付き".
pub(crate) fn trail(history: &[(u32, IssueState)]) -> String {
    history
        .iter()
        .map(|(round, state)| format!("R{round} {}", state_tag(*state).0))
        .collect::<Vec<_>>()
        .join(" → ")
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

fn card(tree: &mut UiTree, parent: WidgetId, accent: bool) -> WidgetId {
    tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(4.0),
            padding: LayoutRect::length(8.0),
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

/// Draws the map of `session` into `host`. Returns the widget of each node,
/// in the layout's order.
pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    actions: &mut Vec<(WidgetId, Action)>,
) -> Vec<WidgetId> {
    let map: MindMap = layout(session);
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

    let mut widgets = Vec::with_capacity(map.nodes.len());
    for node in &map.nodes {
        let widget = match &node.kind {
            NodeKind::Topic => {
                let outer = tree.insert(Some(canvas), Container::new(absolute(node)));
                let body = card(tree, outer, true);
                tree.insert(
                    Some(body),
                    Label::new(session.topic.clone())
                        .font_size(12.5)
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
                    .font_size(11.0)
                    .color(Role::Muted),
                );
                outer
            }
            NodeKind::Issue(id) => {
                let Some(issue) = session.issues.get(id) else {
                    continue;
                };
                let outer = tree.insert(
                    Some(canvas),
                    Pressable::new(absolute(node))
                        .label(format!("{} {}", issue.id, issue.title))
                        .radius(8.0),
                );
                actions.push((outer, Action::Filter(id.clone())));
                let body = card(tree, outer, issue.state == IssueState::Contested);
                let head = tree.insert(
                    Some(body),
                    Container::row().align_items(AlignItems::START).gap(6.0),
                );
                let (state, tone) = state_tag(issue.state);
                tag(tree, head, state, tone);
                tree.insert(
                    Some(head),
                    Label::new(format!("{} {}", issue.id, issue.title))
                        .font_size(12.0)
                        .wrap(true)
                        .max_lines(2),
                );
                tree.insert(
                    Some(body),
                    Label::new(trail(&issue.history))
                        .font_size(10.5)
                        .color(Role::Muted)
                        .truncation(Truncation::Tail),
                );
                let stances = tree.insert(Some(body), Container::wrapping_row(4.0, 3.0));
                for part in session.panelist_ids() {
                    let (pos, tone) = position_tag(issue.stances.get(part).map(|s| s.pos));
                    tag(tree, stances, &format!("{} {pos}", part_name(part)), tone);
                }
                outer
            }
            NodeKind::Minute { section, text } => {
                let outer = tree.insert(Some(canvas), Container::new(absolute(node)));
                let body = card(tree, outer, false);
                let row = tree.insert(
                    Some(body),
                    Container::row().align_items(AlignItems::START).gap(6.0),
                );
                tag(tree, row, section_title(*section), Tone::Neutral);
                tree.insert(
                    Some(row),
                    Label::new(text.clone())
                        .font_size(11.0)
                        .wrap(true)
                        .max_lines(2),
                );
                outer
            }
        };
        widgets.push(widget);
    }
    widgets
}

/// The map alone, off screen: what "画像で保存" renders.
struct MapImage {
    tree: UiTree,
}

impl Screen for MapImage {
    fn tree(&self) -> &UiTree {
        &self.tree
    }

    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }

    fn handle(&mut self, _message: Message, _text: &mut TextStore) {}
}

/// The longest side a saved image may have, in pixels: larger maps are
/// saved at a lower scale rather than failing on the GPU's texture limit.
const IMAGE_MAX_SIDE: f32 = 8192.0;
/// The scale a map is saved at when it fits.
const IMAGE_SCALE: f32 = 2.0;

/// Renders the whole map of `session` (not just the part a pane shows) and
/// saves it as a PNG at `path`. Returns the image's size in pixels.
pub(super) fn save_png(session: &Session, path: &Path) -> Result<(u32, u32), String> {
    let map = layout(session);
    let mut tree = UiTree::new();
    tree.set_theme(Theme::dark());
    let root = tree.insert(
        None,
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            size: super::full(),
            ..Default::default()
        })
        .background(Role::Background),
    );
    insert(&mut tree, root, session, &mut Vec::new());
    let size = (map.width.ceil() as u32, map.height.ceil() as u32);
    let scale = IMAGE_SCALE.min(IMAGE_MAX_SIDE / map.width.max(map.height));
    let mut screen = MapImage { tree };
    let mut text = TextStore::new(Default::default());
    screen.prepare(Size::new(size.0 as f32, size.1 as f32), &mut text);
    let image = Headless::new_or_software()
        .render_screen(&mut screen, &mut text, size, scale)
        .map_err(|e| e.to_string())?;
    let pixels = (image.width, image.height);
    image.save_png(path).map_err(|e| e.to_string())?;
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_trail_reads_round_by_round() {
        assert_eq!(
            trail(&[
                (1, IssueState::Open),
                (2, IssueState::Contested),
                (4, IssueState::Conditional)
            ]),
            "R1 未決 → R2 対立 → R4 条件付き"
        );
        assert_eq!(trail(&[]), "");
    }
}
