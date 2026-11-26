//! What the relation views share (the map, the statistics, the timeline
//! tab and the drawer): a worker's mark and the edge between two nodes.

use noa_gpui_core::widgets::Container;
use noa_gpui_core::{Role, UiTree, WidgetId};
use noa_gpui_scene::Color;

use crate::fleet::{State, Worker};

use super::style::{GRAY, GREEN, fixed_width, need_color};

pub(super) fn mark(worker: &Worker) -> (&'static str, Color) {
    match (worker.need(), worker.state) {
        (Some(need), _) => (need.symbol(), need_color(need)),
        (None, State::Finished) => ("✓", GRAY),
        (None, _) => ("●", GREEN),
    }
}

pub(super) fn edge(tree: &mut UiTree, row: WidgetId, length_px: f32) {
    tree.insert(
        Some(row),
        Container::new(fixed_width(length_px))
            .height(1.0)
            .background(Role::Outline),
    );
}
