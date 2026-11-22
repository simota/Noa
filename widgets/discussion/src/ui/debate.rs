//! Debate mode: the center pane as pro side | ⚖ moderator | con side for one
//! issue. A drafted (⚔) or proposed debate can have its sides swapped before
//! it starts; a running or ended one shows its turns, which the chat logs
//! too, and the verdict.

use noa_gpui_core::style::{
    AlignItems, FlexDirection, FlexWrap, Rect as LayoutRect, Size as LayoutSize, auto, length,
    percent,
};
use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, Tone, Truncation};
use noa_gpui_core::{Role, Style, UiTree, WidgetId};

use discussion::model::types::{
    Author, Debate, DebateId, DebateStatus, IssueId, Message, MsgKind, PartId, Position, RoundKind,
    Session,
};

use super::chat::message_card;
use super::{
    Action, author_color, caption, debate_in_play, fill, gap, part_name, spacer, state_tag, tag,
    tail_column,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebateTarget {
    /// ⚔ on an issue: not sent yet.
    Draft(IssueId),
    Existing(DebateId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebateView {
    pub target: DebateTarget,
    /// The user swapped the sides; only while the debate has not started.
    pub swapped: bool,
}

impl DebateView {
    pub fn draft(issue: IssueId) -> Self {
        Self {
            target: DebateTarget::Draft(issue),
            swapped: false,
        }
    }

    pub fn of(id: DebateId) -> Self {
        Self {
            target: DebateTarget::Existing(id),
            swapped: false,
        }
    }

    /// Whether the session still has what the view shows. A draft lapses
    /// once another debate is in play, since the engine would refuse it.
    pub fn exists_in(&self, session: &Session) -> bool {
        match &self.target {
            DebateTarget::Draft(issue) => {
                session.issues.contains(issue) && debate_in_play(session).is_none()
            }
            DebateTarget::Existing(id) => session.debate(id).is_some(),
        }
    }

    fn debate<'a>(&self, session: &'a Session) -> Option<&'a Debate> {
        match &self.target {
            DebateTarget::Draft(_) => None,
            DebateTarget::Existing(id) => session.debate(id),
        }
    }

    pub fn issue<'a>(&'a self, session: &'a Session) -> Option<&'a IssueId> {
        match &self.target {
            DebateTarget::Draft(issue) => Some(issue),
            DebateTarget::Existing(id) => session.debate(id).map(|d| &d.issue),
        }
    }

    /// Whether the sides may still change: a draft, or a proposal waiting
    /// for the user.
    pub fn editable(&self, session: &Session) -> bool {
        match &self.target {
            DebateTarget::Draft(_) => true,
            DebateTarget::Existing(_) => self
                .debate(session)
                .is_some_and(|d| d.status == DebateStatus::Proposed),
        }
    }

    /// (pro, con) as shown. A draft's sides come from the latest stances,
    /// as the engine picks them for `UserStartDebate` without sides.
    pub fn sides(&self, session: &Session) -> Option<(Vec<PartId>, Vec<PartId>)> {
        let (pro, con) = match &self.target {
            DebateTarget::Draft(issue) => {
                let found = session.issues.get(issue)?;
                let by = |pos| {
                    session
                        .panelist_ids()
                        .filter(|p| found.stances.get(*p).is_some_and(|s| s.pos == pos))
                        .cloned()
                        .collect::<Vec<_>>()
                };
                (by(Position::For), by(Position::Against))
            }
            DebateTarget::Existing(id) => {
                let debate = session.debate(id)?;
                (debate.pro.clone(), debate.con.clone())
            }
        };
        Some(if self.swapped && self.editable(session) {
            (con, pro)
        } else {
            (pro, con)
        })
    }
}

/// The messages of the three columns: pro side, moderator (summaries, the
/// verdict, observers) and con side. Before a debate starts there are no
/// turns, so each side shows the messages its members took their stance in.
pub(crate) fn columns<'a>(session: &'a Session, view: &DebateView) -> [Vec<&'a Message>; 3] {
    let mut out: [Vec<&Message>; 3] = Default::default();
    let Some((pro, con)) = view.sides(session) else {
        return out;
    };
    let side_of = |part: &PartId| {
        if pro.contains(part) {
            0
        } else if con.contains(part) {
            2
        } else {
            1
        }
    };
    let started = view
        .debate(session)
        .is_some_and(|d| !matches!(d.status, DebateStatus::Proposed | DebateStatus::Declined));
    if !started {
        let Some(issue) = view.issue(session).and_then(|i| session.issues.get(i)) else {
            return out;
        };
        for (part, stance) in &issue.stances {
            let column = side_of(part);
            if column != 1
                && let Some(message) = session.message(&stance.basis)
            {
                out[column].push(message);
            }
        }
        return out;
    }
    let Some(debate) = view.debate(session) else {
        return out;
    };
    for message in session
        .messages
        .iter()
        .filter(|m| m.debate.as_ref() == Some(&debate.id))
    {
        let column = match (&message.author, message.kind) {
            // A question goes with whoever it asks.
            (_, MsgKind::Brief) => message.to.first().map_or(1, side_of),
            (Author::Part(part), _) => side_of(part),
            (Author::User, _) => 1,
        };
        out[column].push(message);
    }
    out
}

fn status_label(status: Option<DebateStatus>) -> (&'static str, Tone) {
    match status {
        None => ("下書き", Tone::Neutral),
        Some(DebateStatus::Proposed) => ("提案中", Tone::Warning),
        Some(DebateStatus::Approved) => ("承認済み", Tone::Accent),
        Some(DebateStatus::Active) => ("進行中", Tone::Accent),
        Some(DebateStatus::Closed) => ("終了", Tone::Success),
        Some(DebateStatus::Declined) => ("却下", Tone::Neutral),
    }
}

pub(super) fn insert(
    tree: &mut UiTree,
    host: WidgetId,
    session: &Session,
    view: &DebateView,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    let debate = view.debate(session);
    let editable = view.editable(session);
    let issue_id = view.issue(session).cloned();
    let issue = issue_id.as_ref().and_then(|i| session.issues.get(i));
    let (pro, con) = view.sides(session).unwrap_or_default();

    // ── Header: what is debated, and the controls ──────────────────────────
    let head = tree.insert(
        Some(host),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Wrap,
            align_items: AlignItems::CENTER,
            flex_shrink: 0.0,
            gap: gap(6.0),
            padding: LayoutRect {
                left: length(12.0),
                right: length(12.0),
                top: length(8.0),
                bottom: length(8.0),
            },
            size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        })
        .background(Role::SurfaceAlt),
    );
    tree.insert(
        Some(head),
        Label::new(match debate {
            Some(d) => format!("⚔ 討論 {}", d.id),
            None => "⚔ 討論の準備".to_owned(),
        })
        .font_size(13.0)
        .weight(600),
    );
    if let Some(issue) = issue {
        tag(tree, head, issue.id.as_str(), Tone::Accent);
        tree.insert(
            Some(head),
            Label::new(issue.title.clone())
                .font_size(12.5)
                .truncation(Truncation::Tail),
        );
    }
    let (status, tone) = status_label(debate.map(|d| d.status));
    tag(tree, head, status, tone);
    if let Some(d) = debate {
        let exchanges = session
            .rounds
            .iter()
            .filter(|r| r.kind == RoundKind::Debate(d.id.clone()))
            .count();
        caption(tree, head, format!("応酬 {exchanges}/{}", d.max_exchanges));
    }
    spacer(tree, head);
    if editable {
        let swap = tree.insert(
            Some(head),
            Button::new("⇄ 左右を入れ替え").control_size(ControlSize::Small),
        );
        actions.push((swap, Action::SwapSides));
        let (confirm, cancel) = if debate.is_some() {
            ("承認して開始", "却下")
        } else {
            ("討論を開始", "取り消し")
        };
        let cancel = tree.insert(
            Some(head),
            Button::new(cancel).control_size(ControlSize::Small),
        );
        actions.push((cancel, Action::CancelDebate));
        let confirm = tree.insert(
            Some(head),
            Button::new(confirm)
                .default_button(true)
                .control_size(ControlSize::Small),
        );
        actions.push((confirm, Action::ConfirmDebate));
    } else {
        let back = tree.insert(
            Some(head),
            Button::new("チャットへ戻る").control_size(ControlSize::Small),
        );
        actions.push((back, Action::BackToChat));
    }

    if let Some(verdict) = debate.and_then(|d| d.verdict.as_ref()) {
        let row = tree.insert(
            Some(host),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::START,
                flex_shrink: 0.0,
                gap: gap(8.0),
                padding: LayoutRect::length(10.0),
                ..Default::default()
            })
            .background(Role::Surface),
        );
        caption(tree, row, "判定");
        let (outcome, tone) = state_tag(verdict.outcome);
        tag(tree, row, outcome, tone);
        let text = tree.insert(Some(row), Container::new(fill()));
        tree.insert(
            Some(text),
            Label::new(verdict.rationale.clone())
                .font_size(12.5)
                .wrap(true),
        );
    }

    // ── pro | ⚖ moderator | con ────────────────────────────────────────────
    let body = tree.insert(
        Some(host),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            gap: gap(1.0),
            ..fill()
        })
        .background(Role::Separator),
    );
    let [pro_msgs, mid_msgs, con_msgs] = columns(session, view);
    let mut moderator = vec![PartId::new(discussion::model::types::MODERATOR_ID)];
    moderator.extend(
        session
            .panelist_ids()
            .filter(|p| !pro.contains(p) && !con.contains(p))
            .cloned(),
    );
    let empty_side = if editable {
        "このラウンドではまだ発言がありません。立場を取った発言がここに出ます"
    } else {
        "まだ発言がありません"
    };
    for (title, parts, messages, empty) in [
        ("賛成側", &pro, pro_msgs, empty_side),
        (
            "⚖ 司会・傍聴",
            &moderator,
            mid_msgs,
            "司会の要約と判定がここに出ます",
        ),
        ("反対側", &con, con_msgs, empty_side),
    ] {
        let column = tree.insert(
            Some(body),
            Container::new(fill()).background(Role::Background),
        );
        let top = tree.insert(
            Some(column),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                align_items: AlignItems::CENTER,
                flex_shrink: 0.0,
                gap: gap(6.0),
                padding: LayoutRect {
                    left: length(10.0),
                    right: length(10.0),
                    top: length(8.0),
                    bottom: length(4.0),
                },
                ..Default::default()
            }),
        );
        tree.insert(Some(top), Label::new(title).font_size(12.0).weight(600));
        if parts.is_empty() {
            caption(tree, top, "（不在）");
        }
        for part in parts.iter() {
            tree.insert(
                Some(top),
                Label::new(part_name(part))
                    .font_size(12.0)
                    .color(author_color(session, &Author::Part(part.clone()))),
            );
        }
        let list = tail_column(tree, column, 8.0);
        if messages.is_empty() {
            caption(tree, list, empty);
        }
        for message in messages {
            message_card(tree, list, session, message, actions);
        }
    }
}
