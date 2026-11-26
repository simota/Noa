//! The composer under the center pane: user instructions to everyone, the
//! judge (with a panel: every judge, or one of them), or a specific
//! contestant.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, PopUpButton, TextArea};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};
use noa_gpui_text::TextStore;

use competition::model::types::{Author, JUDGE_ID, PartId, Phase, Round, Session};

use super::{caption, clear_children, fill, gap};

pub(super) struct Parts {
    pub(super) root: WidgetId,
    recipients_host: WidgetId,
    pub(super) recipients: WidgetId,
    pub(super) area: WidgetId,
    pub(super) send: WidgetId,
    /// Where an intervention to the chosen recipients will reach.
    pub(super) hint: WidgetId,
    built_for: Option<String>,
}

const NEXT_ROUND_HINT: &str = "追加指示は次のラウンドのブリーフで宛先パネルに届きます（他パネルへの個別指示はブラインド維持）";
/// A panel's deciding round before its verdict brief: that brief carries
/// every intervention of the competition, whatever its addressees, so
/// nothing stays blind to the lead.
const VERDICT_HINT: &str = "追加指示はこのラウンドでこのあと出る最終講評のブリーフでリード審判に届きます。延長した場合は次のラウンドのブリーフで宛先パネルに届きます";
const VERDICT_TOO_HINT: &str = "追加指示は宛先にリード審判がいなくても、このラウンドでこのあと出る最終講評のブリーフでリード審判に届きます。延長した場合は次のラウンドのブリーフで宛先パネルに届きます";
/// A round the lead's scorecard closed before the last: the competition
/// ends with the verdict and cannot be extended, so there is no next round.
const EARLY_CLOSE_HINT: &str =
    "追加指示はこのラウンドでこのあと出る最終講評のブリーフでリード審判に届きます";

/// Where an intervention to `to` (empty: everyone) reaches, as the hint
/// under the composer says it. In a panel's deciding round being scored,
/// every intervention also goes into the lead's verdict brief issued later
/// in the same round (L1-6.1), whoever it is addressed to; otherwise only
/// into the next round's briefs. A round the lead closed early has no next
/// round (that competition cannot be extended), so there it reaches the
/// verdict brief only.
pub(crate) fn hint(session: &Session, to: &[PartId]) -> &'static str {
    let deciding = session
        .current_round()
        .filter(|round| session.is_panel() && !session.closed && verdict_ahead(session, round));
    let Some(round) = deciding else {
        return NEXT_ROUND_HINT;
    };
    if session.max_rounds.is_none_or(|max| round.no < max) {
        EARLY_CLOSE_HINT
    } else if to.is_empty() || to.contains(&session.judge.id) {
        VERDICT_HINT
    } else {
        VERDICT_TOO_HINT
    }
}

/// Whether `round` is being scored, ends the competition (the last round,
/// or one the lead's scorecard closes), and its verdict brief is not
/// issued yet.
fn verdict_ahead(session: &Session, round: &Round) -> bool {
    let deciding = session.max_rounds.is_some_and(|max| round.no >= max)
        || round
            .scorecards
            .get(&session.judge.id)
            .is_some_and(|card| card.close);
    round.phase == Phase::Judging && round.verdict_brief.is_none() && deciding
}

pub(crate) fn choices(session: &Session) -> Vec<(String, Vec<PartId>)> {
    let mut out = vec![("全員".to_owned(), Vec::new())];
    if session.is_panel() {
        // Every judge at once, then each judge on its own.
        out.push((
            "審判全員".to_owned(),
            session.judges().map(|j| j.id.clone()).collect(),
        ));
        out.extend(session.judges().map(|j| {
            let name = session.author_name(&Author::Part(j.id.clone()));
            (name, vec![j.id.clone()])
        }));
    } else {
        out.push(("審判 (Judge)".to_owned(), vec![PartId::new(JUDGE_ID)]));
    }
    out.extend(
        session
            .contestants
            .iter()
            .map(|p| (p.id.to_string(), vec![p.id.clone()])),
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
        TextArea::new("追加指示を送る（例: モバイル表示の可読性を重視してください）").min_lines(2),
    );
    let send = tree.insert(
        Some(row),
        Button::new("送信")
            .default_button(true)
            .control_size(ControlSize::Small),
    );
    let hint = tree.insert(
        Some(root),
        Label::new(NEXT_ROUND_HINT)
            .font_size(11.5)
            .color(Role::Muted)
            .wrap(true),
    );
    Parts {
        root,
        recipients_host,
        recipients,
        area,
        send,
        hint,
        built_for: None,
    }
}

pub(super) fn sync(tree: &mut UiTree, parts: &mut Parts, session: &Session, text: &mut TextStore) {
    // While held, Return belongs to the explicit next-round action.
    tree.update::<Button, _>(parts.send, |button, cx| {
        let existing = std::mem::replace(button, Button::new("送信"));
        *button = existing.default_button(!session.closed && !session.waits_to_continue());
        cx.request_paint();
    });
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

/// Says under the composer where an intervention to the chosen recipients
/// reaches now.
pub(super) fn sync_hint(tree: &mut UiTree, parts: &Parts, session: &Session) {
    let (_, to) = recipient(tree, parts, session);
    let text = hint(session, &to);
    tree.update::<Label, _>(parts.hint, |l, cx| l.set_text(cx, text));
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
