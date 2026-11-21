//! The details drawer for any worker (AC8). `Dashboard` opens it on
//! `View::detail`, reopens it after a rebuild, moves it with the selection,
//! closes it when the worker goes, and asks the feed for its screen tail;
//! U4 fills in the content.

use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Truncation};
use noa_gpui_core::{UiTree, WidgetId};

use crate::fleet::Worker;

use super::style::{caption, set_label};
use super::{Feed, Pulse, Registry};

/// Fills the drawer overlay `root` for `worker`; returns its 開く and 閉じる
/// buttons. Time labels register `Pulse::Drawer(pane)` in `reg.pulses`.
pub(super) fn build(
    tree: &mut UiTree,
    root: WidgetId,
    worker: &Worker,
    _feed: &Feed,
    reg: &mut Registry,
) -> (WidgetId, WidgetId) {
    tree.insert(
        Some(root),
        Label::new(worker.label.as_str())
            .font_size(16.0)
            .weight(700)
            .truncation(Truncation::Tail),
    );
    let since = caption(tree, root, worker.since.as_str());
    reg.pulses.push((since, Pulse::Drawer(worker.pane)));
    let buttons = tree.insert(Some(root), Container::row().gap(8.0));
    let open = tree.insert(
        Some(buttons),
        Button::new("開く").control_size(ControlSize::Small),
    );
    let close = tree.insert(
        Some(buttons),
        Button::new("閉じる").control_size(ControlSize::Small),
    );
    (open, close)
}

/// Brings the drawer's time label `id` up to date (`Pulse::Drawer`).
pub(super) fn refresh(tree: &mut UiTree, id: WidgetId, worker: &Worker) {
    set_label(tree, id, worker.since.clone());
}
