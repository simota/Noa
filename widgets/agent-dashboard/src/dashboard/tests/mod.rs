use noa_gpui_core::Key;
use noa_gpui_core::widgets::{Label, Tag};
use noa_gpui_core::{Event, Modifiers, PointerButton};
use noa_gpui_scene::{Border, DisplayList, DrawCommand, Point, Size};

use super::*;

mod actions;
mod attention;
mod bulk_close;
mod closed_workers;
mod details;
mod feed;
mod filtering;
mod hardening;
mod header_fit;
mod health;
mod help_overlay;
mod hints;
mod keys;
mod list;
mod paging;
mod paragraphs;
mod rebuild;
mod saved_view;
mod selection;
mod tabs;
mod triage;
mod view_diff;

fn window() -> Size {
    Size::new(1440.0, 900.0)
}

fn laid_out(grouping: Grouping) -> (Dashboard, TextStore) {
    let mut dashboard = Dashboard::new(grouping);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    (dashboard, text)
}

fn click(dashboard: &mut Dashboard, text: &mut TextStore, pos: Point) {
    let button = PointerButton::Primary;
    dashboard.dispatch(Event::PointerMove { pos }, text);
    dashboard.dispatch(Event::PointerDown { pos, button }, text);
    dashboard.dispatch(Event::PointerUp { pos, button }, text);
    dashboard.prepare(window(), text);
}

/// The notice line at the window's foot.
fn notice_of(dashboard: &Dashboard) -> String {
    let label = dashboard.tree.get::<Label>(dashboard.notice).unwrap();
    label.text().to_owned()
}

fn label_of(dashboard: &Dashboard, pane: u64) -> &str {
    &dashboard.feed.fleet.worker(pane).unwrap().label
}

fn center(tree: &UiTree, id: WidgetId) -> Point {
    let origin = tree.window_origin(id).unwrap();
    let rect = tree.layout(id).unwrap().rect;
    Point::new(origin.x + rect.width / 2.0, origin.y + rect.height / 2.0)
}

fn rect(tree: &UiTree, id: WidgetId) -> [f32; 4] {
    let origin = tree.window_origin(id).unwrap();
    let size = tree.layout(id).unwrap().rect;
    [origin.x, origin.y, size.width, size.height]
}

fn assert_rect(actual: [f32; 4], expected: [f32; 4], what: &str) {
    for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() <= 0.5, "{what}: {actual:?} vs {expected:?}");
    }
}

fn press(dashboard: &mut Dashboard, text: &mut TextStore, digit: char, modifiers: Modifiers) {
    let key = Key::Char(digit);
    dashboard.dispatch(Event::KeyDown { key, modifiers }, text);
    dashboard.prepare(window(), text);
}

/// The display list as plain text, with each text layout given by its size:
/// layout ids depend on the `TextStore` that made them.
fn frame_of(list: &DisplayList, text: &TextStore) -> Vec<String> {
    list.commands()
        .iter()
        .map(|command| match command {
            DrawCommand::Text {
                rect,
                layout,
                color,
            } => format!("Text {rect:?} {color:?} {:?}", text.size(*layout)),
            other => format!("{other:?}"),
        })
        .collect()
}

fn fresh_frame(mut fresh: Dashboard) -> Vec<String> {
    let mut fresh_text = TextStore::new(Default::default());
    fresh.prepare(window(), &mut fresh_text);
    frame_of(fresh.display_list(), &fresh_text)
}

/// The server instance the tests' live snapshots come from.
const INSTANCE: &str = "test-instance";

fn live(fleet: Fleet, synced_secs: u64) -> Snapshot {
    Snapshot {
        fleet,
        link: Link::Live,
        synced_secs,
        can_focus: true,
        can_input: true,
        safe_send: true,
        instance: Some(INSTANCE.to_owned()),
    }
}

fn pulse_id(dashboard: &Dashboard, wanted: impl Fn(Pulse) -> bool) -> WidgetId {
    let (id, _) = dashboard.pulses.iter().find(|(_, p)| wanted(*p)).unwrap();
    *id
}

fn labels_under(tree: &UiTree, id: WidgetId) -> Vec<String> {
    let own = tree.get::<Label>(id).map(|l| l.text().to_owned());
    own.into_iter()
        .chain(
            tree.children(id)
                .iter()
                .flat_map(|child| labels_under(tree, *child)),
        )
        .collect()
}

fn tags_under(tree: &UiTree, id: WidgetId) -> Vec<String> {
    let own = tree.get::<Tag>(id).map(|t| t.text().to_owned());
    own.into_iter()
        .chain(
            tree.children(id)
                .iter()
                .flat_map(|child| tags_under(tree, *child)),
        )
        .collect()
}

fn inside(inner: [f32; 4], outer: [f32; 4]) -> bool {
    inner[0] >= outer[0] - 0.5
        && inner[1] >= outer[1] - 0.5
        && inner[0] + inner[2] <= outer[0] + outer[2] + 0.5
        && inner[1] + inner[3] <= outer[1] + outer[3] + 0.5
}

const LONG_LABEL: &str = "noa-gpui ウィジェットによるUI拡張案とその検証作業のまとめ";

/// A key pressed with no modifier, then the next frame's layout.
fn key(dashboard: &mut Dashboard, text: &mut TextStore, key: Key) {
    let modifiers = Modifiers::default();
    dashboard.dispatch(Event::KeyDown { key, modifiers }, text);
    dashboard.prepare(window(), text);
}

/// A character key as the platform sends it: `KeyDown`, then its `Text`.
fn stroke(dashboard: &mut Dashboard, text: &mut TextStore, c: char) {
    let modifiers = Modifiers::default();
    dashboard.dispatch(
        Event::KeyDown {
            key: Key::Char(c),
            modifiers,
        },
        text,
    );
    dashboard.dispatch(Event::Text(c.to_string()), text);
    dashboard.prepare(window(), text);
}

/// Committed text, as the platform sends it after the key that typed it.
fn typed(dashboard: &mut Dashboard, text: &mut TextStore, typed: &str) {
    dashboard.dispatch(Event::Text(typed.to_owned()), text);
    dashboard.prepare(window(), text);
}

/// The fill and border of the last quad drawn at `rect` (window
/// coordinates, ±0.5).
fn quad_at(dashboard: &Dashboard, rect: [f32; 4]) -> Option<(Color, Border)> {
    dashboard
        .display_list()
        .commands()
        .iter()
        .rev()
        .find_map(|command| match command {
            DrawCommand::Quad {
                rect: r,
                color,
                border,
                ..
            } if [r.x, r.y, r.width, r.height]
                .iter()
                .zip(rect)
                .all(|(a, b)| (a - b).abs() <= 0.5) =>
            {
                Some((*color, *border))
            }
            _ => None,
        })
}

/// The panes of the list's rows, in order (all of them, shown or paged
/// away).
fn rows_of(dashboard: &Dashboard) -> Vec<u64> {
    dashboard
        .registry
        .list
        .as_ref()
        .map(|parts| parts.panes())
        .unwrap_or_default()
}

/// The rows the list shows now (the pager hides the others).
fn visible_rows(dashboard: &Dashboard) -> Vec<WidgetId> {
    dashboard
        .registry
        .list
        .as_ref()
        .map(|parts| {
            parts
                .items
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| !dashboard.tree.is_hidden(*id))
                .collect()
        })
        .unwrap_or_default()
}

/// The row of `pane`.
fn row_of(dashboard: &Dashboard, pane: u64) -> WidgetId {
    let (id, _) = dashboard
        .registry
        .rows
        .iter()
        .find(|(_, p)| *p == pane)
        .unwrap_or_else(|| panic!("no row for pane {pane}"));
    *id
}

/// The button of `pane`'s row.
fn row_button(dashboard: &Dashboard, pane: u64) -> WidgetId {
    let (id, _, _) = dashboard
        .registry
        .row_actions
        .iter()
        .find(|(_, p, _)| *p == pane)
        .unwrap_or_else(|| panic!("no button for pane {pane}"));
    *id
}

/// A point on `pane`'s row away from its button: near its left edge,
/// clear of the drawer.
fn row_body(dashboard: &Dashboard, pane: u64) -> Point {
    let [x, y, _, height] = rect(&dashboard.tree, row_of(dashboard, pane));
    Point::new(x + 24.0, y + height / 2.0)
}

/// Every text under `id`, depth first: labels, tags and buttons.
fn texts_under(tree: &UiTree, id: WidgetId) -> Vec<String> {
    let own = tree
        .get::<Label>(id)
        .map(|l| l.text().to_owned())
        .or_else(|| tree.get::<Tag>(id).map(|t| t.text().to_owned()))
        .or_else(|| {
            let button = tree.get::<noa_gpui_core::widgets::Button>(id);
            button.map(|b| b.label().to_owned())
        });
    own.into_iter()
        .chain(
            tree.children(id)
                .iter()
                .flat_map(|child| texts_under(tree, *child)),
        )
        .collect()
}

/// The segment titles of the list's scope switch, as assistive technology
/// reads them.
fn scope_segments(dashboard: &Dashboard) -> Vec<String> {
    let control = dashboard.registry.scope_control.expect("the scope switch");
    let update = dashboard.tree.accessibility_update(1.0);
    (0..3)
        .map(|item| {
            let id = noa_gpui_core::access_item_id(control, item);
            let (_, node) = update
                .nodes
                .iter()
                .find(|(node, _)| *node == id)
                .expect("a segment");
            node.label().unwrap_or_default().to_owned()
        })
        .collect()
}

/// The N of a segment titled 「{scope} N」.
fn segment_count(dashboard: &Dashboard, scope: Scope) -> usize {
    let at = Scope::ALL.iter().position(|s| *s == scope).unwrap();
    let title = &scope_segments(dashboard)[at];
    let (name, n) = title.rsplit_once(' ').expect("「{scope} N」");
    assert_eq!(name, scope.title());
    n.parse().unwrap()
}

/// The dashboard switched to `scope` by its key.
fn in_scope(dashboard: &mut Dashboard, text: &mut TextStore, scope: Scope) {
    let digit = match scope {
        Scope::Current => '1',
        Scope::Attention => '2',
        Scope::Ended => '3',
    };
    key(dashboard, text, Key::Char(digit));
    assert_eq!(dashboard.view.scope, scope);
}
