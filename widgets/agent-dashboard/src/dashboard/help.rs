//! The keyboard help overlay (AC5). `Dashboard` opens and closes the overlay
//! and routes its keys; this fills in the content from `keymap::BINDINGS`.

use noa_gpui_core::style::{AlignItems, Size as LayoutSize, auto, length};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Kbd, Label};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use super::keymap::{BINDINGS, Mode};
use super::style::{gap, spacer};

/// What one row of the list stands for.
pub(super) const ROW_NOTE: &str = "1 行 = 1 ワーカーペイン（依頼の件数ではありません）";

/// Width of the keycaps column.
const KEYS_WIDTH: f32 = 104.0;

const SECTIONS: [(Mode, &str); 3] = [
    (Mode::Browse, "一覧"),
    (Mode::Filter, "絞り込み中"),
    (Mode::Help, "ヘルプ"),
];

/// One help row: the keys of every binding of `mode` sharing `title`.
struct Row {
    keys: Vec<&'static str>,
    title: &'static str,
}

/// The rows of `mode`: one per distinct title, in `BINDINGS` order, with the
/// keys of the bindings that share the title (j and ↓) side by side.
fn rows(mode: Mode) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for binding in BINDINGS.iter().filter(|b| b.mode == mode) {
        match rows.iter_mut().find(|row| row.title == binding.title) {
            Some(row) => row.keys.extend(binding.keys),
            None => rows.push(Row {
                keys: binding.keys.to_vec(),
                title: binding.title,
            }),
        }
    }
    rows
}

/// Fills the overlay `root`; returns its 閉じる button.
pub(super) fn build(tree: &mut UiTree, root: WidgetId) -> WidgetId {
    tree.insert(
        Some(root),
        Label::new("キーボード操作").font_size(14.0).weight(700),
    );
    for (mode, heading) in SECTIONS {
        tree.insert(
            Some(root),
            Label::new(heading)
                .font_size(11.0)
                .weight(700)
                .color(Role::Muted),
        );
        for row in rows(mode) {
            let line = tree.insert(
                Some(root),
                Container::row().align_items(AlignItems::CENTER).gap(8.0),
            );
            let keys = tree.insert(
                Some(line),
                Container::new(Style {
                    align_items: AlignItems::CENTER,
                    gap: gap(4.0),
                    size: LayoutSize {
                        width: length(KEYS_WIDTH),
                        height: auto(),
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                }),
            );
            for key in row.keys {
                tree.insert(Some(keys), Kbd::new([key]));
            }
            tree.insert(Some(line), Label::new(row.title).font_size(12.0));
        }
    }
    // What a row is: a pane, which can be given more tasks.
    tree.insert(
        Some(root),
        Label::new(ROW_NOTE).font_size(11.0).color(Role::Muted),
    );
    // The footer: the note on the mouse, and 閉じる at the row's end.
    let footer = tree.insert(
        Some(root),
        Container::row().align_items(AlignItems::CENTER).gap(8.0),
    );
    tree.insert(
        Some(footer),
        Label::new("マウス: 詳細・開く・グループはクリックでも操作できます")
            .font_size(11.0)
            .color(Role::Muted),
    );
    spacer(tree, footer);
    tree.insert(
        Some(footer),
        Button::new("閉じる").control_size(ControlSize::Small),
    )
}
