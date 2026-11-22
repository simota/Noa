//! The discussion window: issue tree | meeting chat | minutes, with the
//! center pane turning into a debate (pro | moderator | con) when one is on,
//! a composer for interventions, and a start view before any session exists.
//!
//! The screen holds no discussion state of its own: it draws the [`Session`]
//! of the latest [`Snapshot`] and sends [`Command`]s. What it keeps is view
//! state: the issue filter, what the center pane shows, the composer text.

mod chat;
mod composer;
mod debate;
mod minutes;
mod start;
mod tree;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use noa_gpui_core::style::{
    AlignItems, FlexDirection, LengthPercentage, Overflow, Point as LayoutPoint,
    Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Badge, Button, ButtonClicked, Container, ControlSize, Label, ScrollView, SplitPane, SplitView,
    StatusBar, Tag, TagStyle, Toast, Tone,
};
use noa_gpui_core::{Message as UiMessage, Role, Style, Theme, UiTree, WidgetId};
use noa_gpui_platform::Screen;
use noa_gpui_scene::{Color, Size};
use noa_gpui_text::TextStore;

use discussion::model::types::{
    AgentKind, Author, DebateId, DebateStatus, IssueId, IssueState, MODERATOR_ID, Message, MsgKind,
    PartId, Phase, Position, Receipt, Session,
};

use crate::live::{Channel, Command, Link, Snapshot};

pub use debate::{DebateTarget, DebateView};

const TREE_WIDTH: f32 = 300.0;
const MINUTES_WIDTH: f32 = 330.0;

/// What a clickable widget does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Filter the chat to an issue and its sub-issues.
    Filter(IssueId),
    ClearFilter,
    /// ⚔ on an issue: draft a debate on it.
    DraftDebate(IssueId),
    /// Show an existing debate in the center pane.
    ShowDebate(DebateId),
    SwapSides,
    /// Start the drafted debate, or approve the proposed one.
    ConfirmDebate,
    /// Drop the draft, or decline the proposed debate.
    CancelDebate,
    BackToChat,
    Retry,
    Send,
    Start,
    /// End the discussion: no further round starts.
    Close,
}

/// What the center pane shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Center {
    Chat,
    Debate(DebateView),
}

enum Source {
    Demo,
    /// A one-off render of a live snapshot (`--snapshot` without `--demo`).
    Snapshot,
    Live(Channel),
}

pub struct Discussion {
    tree: UiTree,
    source: Source,
    link: Link,
    session: Option<Session>,
    filter: Option<IssueId>,
    center: Center,
    /// Debates already brought up on their own, so leaving one stays left.
    shown_debates: BTreeSet<DebateId>,
    /// An intervention sent and not yet seen in the log: the composer keeps
    /// its text until then.
    pending_intervention: Option<String>,
    /// The session (id, rev) and link the panes were last built from.
    built: Option<(Option<(String, u64)>, Link)>,

    start: start::Parts,
    meeting: WidgetId,
    tree_host: WidgetId,
    center_host: WidgetId,
    minutes_host: WidgetId,
    composer: composer::Parts,
    status_leading: WidgetId,
    status_trailing: WidgetId,

    /// Every clickable widget of the current build and what it does.
    actions: Vec<(WidgetId, Action)>,
    /// Every toast shown, for tests to inspect.
    #[cfg(test)]
    toasts: Vec<String>,
}

impl Discussion {
    pub fn demo() -> Self {
        Self::build(Source::Demo, Link::Live, Some(crate::demo::session()))
    }

    pub fn live(channel: Channel) -> Self {
        Self::build(Source::Live(channel), Link::Connecting, None)
    }

    pub fn of_snapshot(snapshot: Snapshot) -> Self {
        Self::build(Source::Snapshot, snapshot.link, snapshot.session)
    }

    fn build(source: Source, link: Link, session: Option<Session>) -> Self {
        let mut tree = UiTree::new();
        tree.set_theme(Theme::dark());
        let root = tree.insert(
            None,
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                size: full(),
                ..Default::default()
            })
            .background(Role::Background),
        );
        let body = tree.insert(
            Some(root),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: length(0.0),
                min_size: LayoutSize {
                    width: length(0.0),
                    height: length(0.0),
                },
                ..Default::default()
            }),
        );

        let start = start::insert(&mut tree, body);

        let meeting = tree.insert(Some(body), Container::new(fill()));
        let split = tree.insert(
            Some(meeting),
            SplitView::horizontal()
                .pane(SplitPane::fixed(TREE_WIDTH).min(220.0))
                .pane(SplitPane::flexible().min(480.0))
                .pane(SplitPane::fixed(MINUTES_WIDTH).min(240.0)),
        );
        let tree_host = tree.insert(Some(split), pane_column(Role::Surface));
        let center_col = tree.insert(Some(split), pane_column(Role::Background));
        let center_host = tree.insert(Some(center_col), Container::new(fill()));
        let composer = composer::insert(&mut tree, center_col);
        let minutes_host = tree.insert(Some(split), pane_column(Role::Surface));

        let status = StatusBar::insert(&mut tree, root);

        let mut screen = Self {
            tree,
            source,
            link,
            session,
            filter: None,
            center: Center::Chat,
            shown_debates: BTreeSet::new(),
            pending_intervention: None,
            built: None,
            start,
            meeting,
            tree_host,
            center_host,
            minutes_host,
            composer,
            status_leading: status.leading,
            status_trailing: status.trailing,
            actions: Vec::new(),
            #[cfg(test)]
            toasts: Vec::new(),
        };
        // The demo opens on the chat: its debate has ended, so it does not
        // come up on its own.
        screen.note_debates();
        let mut text = TextStore::new(Default::default());
        screen.rebuild(&mut text);
        screen
    }

    /// Brings up a debate that has just been proposed or started.
    fn note_debates(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        for debate in &session.debates {
            if !self.shown_debates.insert(debate.id.clone()) {
                continue;
            }
            if matches!(
                debate.status,
                DebateStatus::Proposed | DebateStatus::Approved | DebateStatus::Active
            ) {
                self.center = Center::Debate(DebateView::of(debate.id.clone()));
            }
        }
    }

    fn session_key(&self) -> Option<(String, u64)> {
        self.session.as_ref().map(|s| (s.id.to_string(), s.rev))
    }

    /// Rebuilds every pane from the session and the view state.
    fn rebuild(&mut self, text: &mut TextStore) {
        self.built = Some((self.session_key(), self.link.clone()));
        self.actions.clear();
        // A filter or a debate whose issue is gone falls back.
        if let (Some(session), Some(issue)) = (&self.session, &self.filter)
            && !session.issues.contains(issue)
        {
            self.filter = None;
        }
        if let Center::Debate(view) = &self.center
            && !self.session.as_ref().is_some_and(|s| view.exists_in(s))
        {
            self.center = Center::Chat;
        }

        let started = self.session.is_some();
        self.tree.set_hidden(self.start.root, started);
        self.tree.set_hidden(self.meeting, !started);
        if started {
            self.rebuild_tree(text);
            self.rebuild_center(text);
            self.rebuild_minutes(text);
            self.rebuild_composer(text);
        } else {
            start::rebuild_note(&mut self.tree, &self.start, &self.link, text);
            self.actions.push((self.start.start_button, Action::Start));
        }
        self.rebuild_status(text);
    }

    fn rebuild_tree(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.tree_host, text);
        let Some(session) = &self.session else {
            return;
        };
        tree::insert(
            &mut self.tree,
            self.tree_host,
            session,
            self.filter.as_ref(),
            &mut self.actions,
        );
    }

    fn rebuild_center(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.center_host, text);
        let Some(session) = &self.session else {
            return;
        };
        match &self.center {
            Center::Chat => chat::insert(
                &mut self.tree,
                self.center_host,
                session,
                self.filter.as_ref(),
                &mut self.actions,
            ),
            Center::Debate(view) => debate::insert(
                &mut self.tree,
                self.center_host,
                session,
                view,
                &mut self.actions,
            ),
        }
    }

    fn rebuild_minutes(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.minutes_host, text);
        let Some(session) = &self.session else {
            return;
        };
        minutes::insert(
            &mut self.tree,
            self.minutes_host,
            session,
            &mut self.actions,
        );
    }

    fn rebuild_composer(&mut self, text: &mut TextStore) {
        let Some(session) = &self.session else {
            return;
        };
        composer::sync(&mut self.tree, &mut self.composer, session, text);
        self.actions.push((self.composer.send, Action::Send));
    }

    fn rebuild_status(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.status_leading, text);
        clear_children(&mut self.tree, self.status_trailing, text);
        let (badge, tone) = match (&self.source, &self.link) {
            (Source::Demo, _) => ("DEMO", Tone::Accent),
            (_, Link::Live) => ("LIVE", Tone::Success),
            (_, Link::Connecting) => ("接続中", Tone::Warning),
            (_, Link::Down(_)) => ("オフライン", Tone::Critical),
        };
        self.tree
            .insert(Some(self.status_leading), Badge::text(badge).tone(tone));
        if let Link::Down(why) = &self.link {
            caption(&mut self.tree, self.status_leading, why.clone());
        }
        let Some(session) = &self.session else {
            return;
        };
        if let Some(round) = session.current_round() {
            caption(
                &mut self.tree,
                self.status_leading,
                format!("第 {} ラウンド · {}", round.no, phase_label(round.phase)),
            );
        }
        if session.closed {
            self.tree.insert(
                Some(self.status_leading),
                Badge::text("終了").tone(Tone::Neutral),
            );
        } else {
            let end = self.tree.insert(
                Some(self.status_trailing),
                Button::new("議論を終える").control_size(ControlSize::Small),
            );
            self.actions.push((end, Action::Close));
        }
        caption(
            &mut self.tree,
            self.status_trailing,
            format!(
                "論点 {} · 発言 {} · 討論 {}",
                session.issues.issues.len(),
                session.messages.len(),
                session.debates.len()
            ),
        );
    }

    fn toast(&mut self, message: impl Into<String>) {
        let message = message.into();
        #[cfg(test)]
        self.toasts.push(message.clone());
        Toast::new(message).show(&mut self.tree);
    }

    /// Sends `cmd` on the live feed; the demo and a one-off snapshot only
    /// say what would have been sent.
    fn send(&mut self, cmd: Command, demo_note: impl Into<String>) {
        match &self.source {
            Source::Live(ch) => ch.send(cmd),
            Source::Demo | Source::Snapshot => {
                let note = demo_note.into();
                self.toast(format!("[DEMO] {note}"));
            }
        }
    }

    fn poll_live(&mut self, text: &mut TextStore) {
        let mut latest = None;
        let mut notices = Vec::new();
        if let Source::Live(ch) = &self.source {
            while let Ok(snapshot) = ch.snapshots.try_recv() {
                latest = Some(snapshot);
            }
            while let Ok(notice) = ch.notices.try_recv() {
                notices.push(notice);
            }
        }
        if let Some(snapshot) = latest {
            self.link = snapshot.link;
            self.session = snapshot.session;
            self.note_debates();
            self.settle_intervention();
            if self.built != Some((self.session_key(), self.link.clone())) {
                self.rebuild(text);
            }
        }
        for notice in notices {
            self.toast(notice);
        }
    }

    /// Clears the composer once the sent intervention shows in the log, if
    /// its text was not edited meanwhile.
    fn settle_intervention(&mut self) {
        let (Some(sent), Some(session)) = (&self.pending_intervention, &self.session) else {
            return;
        };
        let logged = session
            .messages
            .iter()
            .any(|m| m.kind == MsgKind::Intervention && m.body == *sent);
        if logged {
            if composer::text(&self.tree, &self.composer) == *sent {
                composer::clear(&mut self.tree, &self.composer);
            }
            self.pending_intervention = None;
        }
    }

    fn run(&mut self, action: Action, text: &mut TextStore) {
        match action {
            Action::Filter(issue) => {
                self.filter = (self.filter.as_ref() != Some(&issue)).then_some(issue);
                self.center = Center::Chat;
                self.rebuild(text);
            }
            Action::ClearFilter => {
                self.filter = None;
                self.rebuild(text);
            }
            Action::DraftDebate(issue) => {
                self.center = Center::Debate(DebateView::draft(issue));
                self.rebuild(text);
            }
            Action::ShowDebate(id) => {
                self.center = Center::Debate(DebateView::of(id));
                self.rebuild(text);
            }
            Action::SwapSides => {
                if let Center::Debate(view) = &mut self.center {
                    view.swapped = !view.swapped;
                }
                self.rebuild(text);
            }
            Action::ConfirmDebate => self.confirm_debate(text),
            Action::CancelDebate => self.cancel_debate(text),
            Action::BackToChat => {
                self.center = Center::Chat;
                self.rebuild(text);
            }
            Action::Retry => self.send(Command::Retry, "失敗したブリーフを再送します"),
            Action::Send => self.send_intervention(),
            Action::Start => self.start_session(),
            Action::Close => self.send(Command::Close, "議論を終えます"),
        }
    }

    fn confirm_debate(&mut self, text: &mut TextStore) {
        let (Center::Debate(view), Some(session)) = (&self.center, &self.session) else {
            return;
        };
        let Some((pro, con)) = view.sides(session) else {
            return;
        };
        if pro.is_empty() || con.is_empty() {
            self.toast("賛成側と反対側に 1 人以上のパネリストが必要です");
            return;
        }
        let view = view.clone();
        let names = format!("賛成 {} / 反対 {}", join(&pro), join(&con));
        match view.target {
            DebateTarget::Draft(issue) => self.send(
                Command::StartDebate {
                    issue: issue.clone(),
                    pro,
                    con,
                },
                format!("{issue} の討論を開始します（{names}）"),
            ),
            DebateTarget::Existing(id) => self.send(
                Command::ApproveDebate {
                    id: id.clone(),
                    pro,
                    con,
                },
                format!("討論 {id} を承認しました（{names}）"),
            ),
        }
        if matches!(self.source, Source::Demo | Source::Snapshot) {
            self.center = Center::Chat;
            self.rebuild(text);
        }
    }

    fn cancel_debate(&mut self, text: &mut TextStore) {
        let Center::Debate(view) = &self.center else {
            return;
        };
        if let DebateTarget::Existing(id) = &view.target {
            let id = id.clone();
            self.send(
                Command::DeclineDebate { id: id.clone() },
                format!("討論 {id} を却下しました"),
            );
        }
        self.center = Center::Chat;
        self.rebuild(text);
    }

    fn send_intervention(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        let body = composer::text(&self.tree, &self.composer);
        if body.is_empty() {
            self.toast("介入の本文が空です");
            return;
        }
        let (label, to) = composer::recipient(&self.tree, &self.composer, session);
        match self.source {
            Source::Live(_) => {
                self.pending_intervention = Some(body.clone());
                self.send(Command::Intervene { to, body }, "");
            }
            Source::Demo | Source::Snapshot => {
                self.toast(format!("[DEMO] {label} へ介入を送りました"));
                composer::clear(&mut self.tree, &self.composer);
            }
        }
    }

    fn start_session(&mut self) {
        let form = start::read(&self.tree, &self.start);
        if form.topic.is_empty() {
            self.toast("議題を入力してください");
            return;
        }
        if form.panel.is_empty() {
            self.toast("パネリストを 1 人以上選んでください");
            return;
        }
        let workdir = match start::workdir(&form.workdir) {
            Ok(workdir) => workdir,
            Err(why) => {
                self.toast(why);
                return;
            }
        };
        let note = format!(
            "司会 {} とパネル {} で開始します",
            form.moderator.command(),
            form.panel
                .iter()
                .map(|k| k.command())
                .collect::<Vec<_>>()
                .join(", ")
        );
        self.send(
            Command::Start {
                topic: form.topic,
                workdir,
                max_rounds: Some(form.max_rounds),
                moderator: form.moderator,
                panel: form.panel,
            },
            note,
        );
    }

    fn handle_message(&mut self, message: &UiMessage, text: &mut TextStore) {
        if message.downcast_ref::<ButtonClicked>().is_none() {
            return;
        }
        let action = self
            .actions
            .iter()
            .find(|(id, _)| *id == message.source)
            .map(|(_, action)| action.clone());
        if let Some(action) = action {
            self.run(action, text);
        }
    }
}

impl Screen for Discussion {
    fn tree(&self) -> &UiTree {
        &self.tree
    }

    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }

    fn prepare(&mut self, viewport: Size, text: &mut TextStore) -> bool {
        self.poll_live(text);
        self.tree.prepare(viewport, text).is_some()
    }

    fn handle(&mut self, message: UiMessage, text: &mut TextStore) {
        self.handle_message(&message, text);
    }
}

// ── Labels and colors shared by the panes ───────────────────────────────────

pub(crate) fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::Briefing => "ブリーフ送信中",
        Phase::Awaiting => "返信待ち",
        Phase::Summarizing => "司会が要約中",
        Phase::Closed => "締め",
    }
}

pub(crate) fn state_tag(state: IssueState) -> (&'static str, Tone) {
    match state {
        IssueState::Open => ("未決", Tone::Neutral),
        IssueState::Contested => ("対立", Tone::Warning),
        IssueState::Agreed => ("合意", Tone::Success),
        IssueState::Conditional => ("条件付き", Tone::Accent),
        IssueState::Deferred => ("保留", Tone::Neutral),
    }
}

pub(crate) fn position_tag(pos: Option<Position>) -> (&'static str, Tone) {
    match pos {
        Some(Position::For) => ("賛成", Tone::Success),
        Some(Position::Against) => ("反対", Tone::Critical),
        Some(Position::Neutral) => ("中立", Tone::Neutral),
        Some(Position::Unknown) | None => ("?", Tone::Neutral),
    }
}

pub(crate) fn kind_label(kind: MsgKind) -> &'static str {
    match kind {
        MsgKind::Brief => "質問",
        MsgKind::Reply => "回答",
        MsgKind::Summary => "要約",
        MsgKind::Intervention => "介入",
        MsgKind::Argument => "主張",
        MsgKind::Verdict => "判定",
    }
}

/// How far one brief got, as its receipt badge shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    Queued,
    Sent,
    Started,
    Replied,
    Failed,
}

impl Delivery {
    pub(crate) fn of(receipt: &Receipt) -> Self {
        if receipt.replied.is_some() {
            Delivery::Replied
        } else if receipt.failed.is_some() {
            Delivery::Failed
        } else if receipt.started.is_some() {
            Delivery::Started
        } else if receipt.sent.is_some() {
            Delivery::Sent
        } else {
            Delivery::Queued
        }
    }

    pub(crate) fn tag(self) -> (&'static str, Tone) {
        match self {
            Delivery::Queued => ("未送信", Tone::Neutral),
            Delivery::Sent => ("送信済", Tone::Neutral),
            Delivery::Started => ("着手", Tone::Accent),
            Delivery::Replied => ("✓ 返信", Tone::Success),
            Delivery::Failed => ("✗ 失敗", Tone::Critical),
        }
    }
}

pub(crate) fn part_name(part: &PartId) -> String {
    if part.as_str() == MODERATOR_ID {
        "Noa".to_owned()
    } else {
        part.to_string()
    }
}

pub(crate) fn author_name(author: &Author) -> String {
    match author {
        Author::User => "あなた".to_owned(),
        Author::Part(part) => part_name(part),
    }
}

pub(crate) fn author_color(session: &Session, author: &Author) -> Color {
    match author {
        Author::User => Color::hex(0xffcc00),
        Author::Part(part) if part.as_str() == MODERATOR_ID => Color::hex(0x64d2ff),
        Author::Part(part) => match session.participant(part).map(|p| p.kind) {
            Some(AgentKind::Claude) => Color::hex(0xe8936b),
            Some(AgentKind::Codex) => Color::hex(0x5fd4a8),
            Some(AgentKind::Agy) => Color::hex(0x8ab4f8),
            None => Color::hex(0x8e8e93),
        },
    }
}

/// Every issue at or below `root`.
pub(crate) fn subtree(session: &Session, root: &IssueId) -> BTreeSet<IssueId> {
    let mut out = BTreeSet::from([root.clone()]);
    // Children always come after their parent in the tree.
    for issue in &session.issues.issues {
        if let Some(parent) = &issue.parent
            && out.contains(parent)
        {
            out.insert(issue.id.clone());
        }
    }
    out
}

/// Whether `message` is about an issue in `issues`: tagged with one, or part
/// of a debate on one.
pub(crate) fn is_about(session: &Session, message: &Message, issues: &BTreeSet<IssueId>) -> bool {
    message.issues.iter().any(|i| issues.contains(i))
        || message
            .debate
            .as_ref()
            .and_then(|d| session.debate(d))
            .is_some_and(|d| issues.contains(&d.issue))
}

/// The debate the engine would refuse a new one alongside, if any.
pub(crate) fn debate_in_play(session: &Session) -> Option<&discussion::model::types::Debate> {
    session.debates.iter().find(|d| {
        matches!(
            d.status,
            DebateStatus::Proposed | DebateStatus::Approved | DebateStatus::Active
        )
    })
}

pub(crate) fn join(parts: &[PartId]) -> String {
    if parts.is_empty() {
        return "—".to_owned();
    }
    parts.iter().map(part_name).collect::<Vec<_>>().join(", ")
}

// ── Layout helpers ──────────────────────────────────────────────────────────

pub(crate) fn full() -> LayoutSize<noa_gpui_core::style::Dimension> {
    LayoutSize {
        width: percent(1.0),
        height: percent(1.0),
    }
}

pub(crate) fn gap(px: f32) -> LayoutSize<LengthPercentage> {
    LayoutSize {
        width: length(px),
        height: length(px),
    }
}

/// A column that takes the space left in its parent, and may shrink to
/// nothing so long content scrolls instead of pushing siblings out.
pub(crate) fn fill() -> Style {
    Style {
        flex_direction: FlexDirection::Column,
        flex_grow: 1.0,
        flex_shrink: 1.0,
        flex_basis: length(0.0),
        min_size: LayoutSize {
            width: length(0.0),
            height: length(0.0),
        },
        size: LayoutSize {
            width: percent(1.0),
            height: auto(),
        },
        ..Default::default()
    }
}

fn pane_column(background: Role) -> Container {
    Container::new(Style {
        flex_direction: FlexDirection::Column,
        size: full(),
        ..Default::default()
    })
    .background(background)
}

/// A vertical scroller taking the space left in `parent`, with a padded
/// column inside; returns the column.
pub(crate) fn scroll_column(tree: &mut UiTree, parent: WidgetId, spacing: f32) -> WidgetId {
    scroller(tree, parent, spacing, false)
}

/// [`scroll_column`] that opens at its end and stays there as content
/// grows, as a chat log does.
pub(crate) fn tail_column(tree: &mut UiTree, parent: WidgetId, spacing: f32) -> WidgetId {
    scroller(tree, parent, spacing, true)
}

fn scroller(tree: &mut UiTree, parent: WidgetId, spacing: f32, tail: bool) -> WidgetId {
    let scroll = tree.insert(
        Some(parent),
        ScrollView::new(Style {
            overflow: LayoutPoint {
                x: Overflow::Hidden,
                y: Overflow::Scroll,
            },
            ..fill()
        })
        .follows_end(tail),
    );
    tree.insert(
        Some(scroll),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(spacing),
            padding: LayoutRect::length(10.0),
            size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        }),
    )
}

/// A pane's heading row: a bold title, then whatever the caller adds.
pub(crate) fn heading(tree: &mut UiTree, parent: WidgetId, title: &str) -> WidgetId {
    let row = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::CENTER,
            flex_shrink: 0.0,
            gap: gap(8.0),
            padding: LayoutRect {
                left: length(12.0),
                right: length(12.0),
                top: length(10.0),
                bottom: length(8.0),
            },
            ..Default::default()
        }),
    );
    tree.insert(Some(row), Label::new(title).font_size(13.0).weight(600));
    row
}

pub(crate) fn caption(tree: &mut UiTree, parent: WidgetId, text: impl Into<String>) -> WidgetId {
    tree.insert(
        Some(parent),
        Label::new(text).font_size(11.0).color(Role::Muted),
    )
}

pub(crate) fn tag(tree: &mut UiTree, parent: WidgetId, text: &str, tone: Tone) -> WidgetId {
    tree.insert(Some(parent), Tag::new(text).tone(tone))
}

pub(crate) fn outlined(tree: &mut UiTree, parent: WidgetId, text: &str) -> WidgetId {
    tree.insert(Some(parent), Tag::new(text).style(TagStyle::Outlined))
}

pub(crate) fn spacer(tree: &mut UiTree, parent: WidgetId) {
    tree.insert(Some(parent), Container::row().grow(1.0));
}

pub(crate) fn clear_children(tree: &mut UiTree, parent: WidgetId, text: &mut TextStore) {
    let children: Vec<WidgetId> = tree.children(parent).to_vec();
    for child in children {
        tree.remove(child, text);
    }
}
