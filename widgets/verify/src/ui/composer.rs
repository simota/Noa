//! The composer under the center pane: the person's instructions to
//! everyone, the judge, or one verifier.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, PopUpButton, TextArea};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_text::TextStore;

use verify::model::types::{JUDGE_ID, PartId, Session};

use super::{caption, clear_children, fill, gap, note};

pub(super) struct Parts {
    pub(super) root: WidgetId,
    recipients_host: WidgetId,
    pub(super) recipients: WidgetId,
    pub(super) area: WidgetId,
    pub(super) send: WidgetId,
    /// The row the send button ends.
    row: WidgetId,
    /// Whether the send button is the screen's default (Return) button.
    send_is_default: bool,
    built_for: Option<String>,
}

/// 全員, then each verifier, then 判定役 (L1-7 Bottom).
pub(crate) fn choices(session: &Session) -> Vec<(String, Vec<PartId>)> {
    let mut out = vec![("全員".to_owned(), Vec::new())];
    out.extend(
        session
            .verifiers
            .iter()
            .map(|p| (p.id.to_string(), vec![p.id.clone()])),
    );
    out.push(("判定役".to_owned(), vec![PartId::new(JUDGE_ID)]));
    out
}

pub(super) fn insert(tree: &mut UiTree, parent: WidgetId) -> Parts {
    let root = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            flex_shrink: 0.0,
            gap: gap(6.0),
            padding: LayoutRect {
                left: length(12.0),
                right: length(12.0),
                top: length(8.0),
                bottom: length(10.0),
            },
            size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        })
        .background(Role::SurfaceAlt),
    );
    let row = tree.insert(
        Some(root),
        Container::row().align_items(AlignItems::END).gap(8.0),
    );
    let recipients_host = tree.insert(
        Some(row),
        Container::column().gap(4.0).align_items(AlignItems::START),
    );
    caption(tree, recipients_host, "宛先");
    let recipients = tree.insert(
        Some(recipients_host),
        PopUpButton::new(["全員"]).control_size(ControlSize::Small),
    );
    let field = tree.insert(Some(row), Container::new(fill()));
    let area = tree.insert(
        Some(field),
        TextArea::new("追加指示を送る（例: AC-2 は Linux でも確かめてください）").min_lines(2),
    );
    let send = tree.insert(Some(row), send_button(true));
    note(
        tree,
        root,
        "追加指示は次のブリーフで宛先に届きます（判定役には全員分が届きます）",
    );
    Parts {
        root,
        recipients_host,
        recipients,
        area,
        send,
        row,
        send_is_default: true,
        built_for: None,
    }
}

fn send_button(default: bool) -> Button {
    Button::new("送信")
        .default_button(default)
        .control_size(ControlSize::Small)
}

/// Makes the send button the screen's default (Return) button, or not: a
/// held verification's default is 次のラウンドへ (or 検証を開始), and
/// Return goes to the first default button in the tree.
pub(super) fn set_send_default(
    tree: &mut UiTree,
    parts: &mut Parts,
    default: bool,
    text: &mut TextStore,
) {
    if parts.send_is_default == default {
        return;
    }
    let focused = tree.focused() == Some(parts.send);
    tree.remove(parts.send, text);
    parts.send = tree.insert(Some(parts.row), send_button(default));
    parts.send_is_default = default;
    if focused {
        tree.set_focus(Some(parts.send));
    }
}

pub(super) fn sync(tree: &mut UiTree, parts: &mut Parts, session: &Session, text: &mut TextStore) {
    if parts.built_for.as_deref() == Some(session.id.as_str()) {
        return;
    }
    parts.built_for = Some(session.id.to_string());
    clear_children(tree, parts.recipients_host, text);
    caption(tree, parts.recipients_host, "宛先");
    let labels: Vec<String> = choices(session).into_iter().map(|(l, _)| l).collect();
    parts.recipients = tree.insert(
        Some(parts.recipients_host),
        PopUpButton::new(labels).control_size(ControlSize::Small),
    );
}

pub(super) fn recipient(tree: &UiTree, parts: &Parts, session: &Session) -> (String, Vec<PartId>) {
    let index = tree
        .get::<PopUpButton>(parts.recipients)
        .map_or(0, PopUpButton::selected);
    let mut all = choices(session);
    if index < all.len() {
        all.swap_remove(index)
    } else {
        all.swap_remove(0)
    }
}

pub(super) fn text(tree: &UiTree, parts: &Parts) -> String {
    tree.get::<TextArea>(parts.area)
        .map(|a| a.text().trim().to_owned())
        .unwrap_or_default()
}

/// Empties the draft and has the recipients rebuilt for the next session.
pub(super) fn reset(tree: &mut UiTree, parts: &mut Parts) {
    clear(tree, parts);
    parts.built_for = None;
}

pub(super) fn clear(tree: &mut UiTree, parts: &Parts) {
    tree.update::<TextArea, _>(parts.area, |a, cx| a.set_text(cx, ""));
}
