//! The keyboard help overlay (AC5). `Dashboard` opens and closes the overlay
//! and routes its keys; U6 fills in the content from `keymap::BINDINGS`.

use noa_gpui_core::widgets::{Button, ControlSize, Label};
use noa_gpui_core::{UiTree, WidgetId};

/// Fills the overlay `root`; returns its 閉じる button.
pub(super) fn build(tree: &mut UiTree, root: WidgetId) -> WidgetId {
    tree.insert(
        Some(root),
        Label::new("キーボード操作").font_size(14.0).weight(700),
    );
    tree.insert(
        Some(root),
        Button::new("閉じる").control_size(ControlSize::Small),
    )
}
