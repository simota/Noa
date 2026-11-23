//! The composer under the chat: the user speaks into the meeting, to
//! everyone, to the moderator or to one panelist. The engine logs it and the
//! next briefs carry it.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, PopUpButton, TextArea};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_text::TextStore;

use discussion::model::types::{MODERATOR_ID, PartId, Session};

use super::{caption, clear_children, fill, gap, note, part_name};

pub(super) struct Parts {
    /// The whole composer, hidden while a stored discussion is shown.
    pub(super) root: WidgetId,
    recipients_host: WidgetId,
    pub(super) recipients: WidgetId,
    pub(super) area: WidgetId,
    pub(super) send: WidgetId,
    /// The session the recipient menu lists the panel of.
    built_for: Option<String>,
}

/// The menu's choices: label and addressees (empty: everyone).
pub(crate) fn choices(session: &Session) -> Vec<(String, Vec<PartId>)> {
    let mut out = vec![
        ("全員".to_owned(), Vec::new()),
        ("Noa（司会）".to_owned(), vec![PartId::new(MODERATOR_ID)]),
    ];
    out.extend(
        session
            .panelists
            .iter()
            .map(|p| (part_name(&p.id), vec![p.id.clone()])),
    );
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
        TextArea::new("議論に介入する（例: コスト面の制約も考慮してください）").min_lines(2),
    );
    let send = tree.insert(
        Some(row),
        Button::new("送信")
            .default_button(true)
            .control_size(ControlSize::Small),
    );
    note(
        tree,
        root,
        "介入はログに残り、次のラウンドのブリーフで全員に届きます。宛先は誰への発言かを示します",
    );
    Parts {
        root,
        recipients_host,
        recipients,
        area,
        send,
        built_for: None,
    }
}

/// Lists the session's panel in the recipient menu (once per session).
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

/// The chosen recipient: label and addressees.
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

pub(super) fn clear(tree: &mut UiTree, parts: &Parts) {
    tree.update::<TextArea, _>(parts.area, |a, cx| a.set_text(cx, ""));
}
