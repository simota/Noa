//! Pure split-pane tree and layout math.
//!
//! This module intentionally stays independent of `winit`, `wgpu`, terminals,
//! and ptys so split behavior can be unit-tested without constructing a
//! window or GPU context.

mod close;
mod commands;
mod focus;
mod hit_test;
mod layout;
mod ops;
mod resize;
#[cfg(test)]
mod tests;
mod types;
mod zoom;

pub use close::{CloseOutcome, close_pane};
pub use commands::{ImeOp, focus_switch_plan, resolve_pane_command_target};
pub use focus::{focus_in_direction, focus_in_direction_in_layout};
pub use hit_test::{HitTarget, hit_test};
pub use layout::compute_layout;
pub use ops::{
    can_add_pane_in_direction, contains_pane, equalize, split_pane, split_pane_in_direction,
};
pub use resize::{
    SplitResizeDrag, resize_split, resize_split_to_drag_point, split_resize_drag_target_at_point,
};
pub use types::{
    DIVIDER_HIT_ZONE_PX, DIVIDER_WIDTH_PX, Direction, MAX_PANES_PER_AXIS, MAX_PANES_PER_TAB,
    MIN_PANE_SIZE_PX, PaneId, Point, Rect, SPLIT_RESIZE_STEP_PX, SplitOrientation, SplitTree,
};
pub use zoom::{
    ZoomCloseOutcome, ZoomDecision, close_pane_with_zoom, zoom_decision, zoom_resize_targets,
    zoom_toggle,
};
