//! The discussion window: issue tree | meeting chat | minutes, with the
//! center pane turning into a debate (pro | moderator | con) when one is on,
//! a composer for interventions, and a start view before any session exists.
//!
//! The screen holds no discussion state of its own: it draws the [`Session`]
//! of the latest [`Snapshot`] and sends [`Command`]s. What it keeps is view
//! state: the issue filter, what the center pane shows, the composer text.

mod auto;
mod chat;
mod composer;
mod controls;
mod debate;
mod map;
mod minutes;
mod start;
mod tree;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use noa_gpui_core::style::{
    AlignItems, FlexDirection, LengthPercentage, Overflow, Point as LayoutPoint,
    Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Button, ButtonClicked, Container, ControlSize, Label, ScrollView, SelectionChanged, SplitPane,
    SplitView, StatusBar, Tag, TagStyle, Toast, Tone,
};
use noa_gpui_core::{Message as UiMessage, Role, Style, Theme, UiTree, WidgetId};
use noa_gpui_platform::{FileDialogRequest, FileDialogResult, Screen};
use noa_gpui_scene::{Color, Size};
use noa_gpui_text::TextStore;

use discussion::model::types::{
    AgentKind, Author, DebateId, DebateStatus, IssueId, IssueState, MODERATOR_ID, Message, MsgKind,
    PartId, Phase, Position, Receipt, RoundNo, Session,
};

use discussion::store::Summary;

use crate::installed::Installed;
use crate::live::{Channel, Command, Link, Snapshot};

pub use debate::{DebateTarget, DebateView};

const TREE_WIDTH: f32 = 300.0;
const MINUTES_WIDTH: f32 = 330.0;
/// The chat pane never gets narrower than this.
const CENTER_MIN: f32 = 480.0;
/// The smallest window the widget is laid out for. The side panes keep
/// their width, so a narrower window would cut off the minutes pane.
pub(crate) const MIN_WINDOW: (f64, f64) = (
    (TREE_WIDTH + CENTER_MIN + MINUTES_WIDTH + 2.0 * noa_gpui_core::widgets::DIVIDER) as f64,
    600.0,
);

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
    RetryTimeline,
    Send,
    Start,
    /// Add a row to the start view's panel.
    AddPanelRow,
    /// Remove the start view's panel row at this index.
    RemovePanelRow(usize),
    /// End the discussion: no further round starts.
    Close,
    /// Put the result (summary, issues, minutes) on the clipboard.
    CopyResult,
    /// Save the minutes with every message as a Markdown file.
    ExportMinutes,
    /// Start an agent of this kind in the working directory on the result.
    Implement(AgentKind),
    /// The center pane's tabs.
    ShowChat,
    ShowMap,
    /// Save the whole map as a PNG.
    ExportMap,
    /// Show the stored discussion at this directory, read-only.
    Open(PathBuf),
    /// Pick up the stored discussion at this directory: its agents run again.
    Resume(PathBuf),
    /// Leave the discussion shown and go back to the start view.
    NewDiscussion,
    /// Bring a participant's pane to the front in Noa.
    Focus(PartId),
    /// Hold the discussion after each round (`true`), or stop holding it.
    SetHold(bool),
    /// Start the next round of a held discussion.
    Continue,
    /// Give the discussion this many more rounds.
    Extend(RoundNo),
    /// Close every participant's pane of the ended discussion in Noa.
    ClosePanes,
    /// Show or hide result tools and less frequent session controls.
    ToggleTools,
    /// Reveal why a timeline post cannot proceed.
    ShowTimelineDetails,
}

/// The round counts the "+N ラウンド" buttons offer.
pub(crate) const EXTEND_BY: [RoundNo; 2] = [1, 3];

impl Action {
    /// Whether the action changes the discussion or its agents, which a
    /// stored discussion shown read-only does not offer.
    fn acts_on_session(&self) -> bool {
        matches!(
            self,
            Action::DraftDebate(_)
                | Action::SwapSides
                | Action::ConfirmDebate
                | Action::Retry
                | Action::Send
                | Action::Close
                | Action::Implement(_)
                | Action::SetHold(_)
                | Action::Continue
                | Action::ClosePanes
        )
    }
}

/// What the save panel that is up was opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Export {
    Minutes,
    Map,
}

/// What the center pane shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Center {
    Chat,
    /// The discussion as a mind map.
    Map,
    Debate(DebateView),
}

enum Source {
    Demo,
    /// A one-off render of a live snapshot (`--snapshot` without `--demo`).
    Snapshot,
    Live(Channel),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusArea {
    Start,
    Issues,
    Center,
    Minutes,
    Composer,
    Tools,
    StatusLeading,
    StatusTrailing,
}

pub struct Discussion {
    tree: UiTree,
    source: Source,
    link: Link,
    timeline_state: Option<crate::live::timeline::Status>,
    session: Option<Arc<Session>>,
    filter: Option<IssueId>,
    center: Center,
    /// Debates already brought up on their own, so leaving one stays left.
    shown_debates: BTreeSet<DebateId>,
    /// The map tab's nodes, in the layout's order (empty off the map).
    map_nodes: Vec<WidgetId>,
    /// An intervention sent and not yet seen in the log: the composer keeps
    /// its text until then.
    pending_intervention: Option<String>,
    /// Save panels to show (`Screen::take_file_dialogs`).
    file_dialogs: Vec<FileDialogRequest>,
    /// What the last save panel was for.
    export: Option<Export>,
    /// The session (id, rev) and link the panes were last built from.
    built: Option<(Option<(String, u64)>, Link)>,
    /// The stored discussions the start view lists, newest first.
    history: Arc<Vec<Summary>>,
    /// Which agents are installed; `None` while the live check runs. The
    /// demo and one-off renders take every kind as installed.
    installed: Option<Installed>,

    start: start::Parts,
    auto: auto::Auto,
    meeting: WidgetId,
    tree_host: WidgetId,
    center_host: WidgetId,
    minutes_host: WidgetId,
    composer: composer::Parts,
    tools_host: WidgetId,
    tools_open: bool,
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
        Self::build(
            Source::Demo,
            Link::Live,
            Some(Arc::new(crate::demo::session())),
        )
    }

    pub fn live(channel: Channel) -> Self {
        Self::build(Source::Live(channel), Link::Connecting, None)
    }

    pub fn of_snapshot(snapshot: Snapshot) -> Self {
        Self::build(Source::Snapshot, snapshot.link, snapshot.session)
    }

    /// A one-off render of the stored discussion at `dir`, read-only
    /// (`--session` with `--snapshot`).
    pub fn of_stored(dir: PathBuf, session: Session) -> Self {
        Self::build(Source::Snapshot, Link::Stored(dir), Some(Arc::new(session)))
    }

    /// The start view of a one-off render lists these stored discussions.
    pub fn with_history(mut self, history: Arc<Vec<Summary>>) -> Self {
        self.history = history;
        self.rebuild(&mut TextStore::new(Default::default()));
        self
    }

    /// The demo held after a round, waiting for the user (`--demo-held`).
    pub fn demo_held() -> Self {
        Self::build(
            Source::Demo,
            Link::Live,
            Some(Arc::new(crate::demo::held())),
        )
    }

    /// The demo's start view, with canned stored discussions
    /// (`--demo-start`).
    pub fn demo_start() -> Self {
        Self::build(Source::Demo, Link::Live, None).with_history(Arc::new(crate::demo::history()))
    }

    /// Whether the window can leave what it shows or start a runner for it
    /// (resume, add rounds, a new discussion): not in a one-off render of
    /// a snapshot or a stored discussion (`--snapshot`, `--session`), which
    /// has no live feed to do it with.
    fn navigates(&self) -> bool {
        !matches!(self.source, Source::Snapshot)
    }

    /// Whether the session shown is a stored one, read-only: nothing runs
    /// it, so nothing is offered that would change it.
    fn read_only(&self) -> bool {
        matches!(self.link, Link::Stored(_))
    }

    fn build(source: Source, link: Link, session: Option<Arc<Session>>) -> Self {
        let mut tree = UiTree::new();
        crate::appearance::install(&mut tree, Theme::dark());
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
        let auto = auto::Auto::new(&mut tree, start.auto_controls);

        let meeting = tree.insert(Some(body), Container::new(fill()));
        let split = tree.insert(
            Some(meeting),
            SplitView::horizontal()
                .pane(SplitPane::fixed(TREE_WIDTH).min(220.0))
                .pane(SplitPane::flexible().min(CENTER_MIN))
                .pane(SplitPane::fixed(MINUTES_WIDTH).min(240.0)),
        );
        let tree_host = tree.insert(Some(split), pane_column(Role::Surface));
        let center_col = tree.insert(Some(split), pane_column(Role::Background));
        let center_host = tree.insert(Some(center_col), Container::new(fill()));
        let composer = composer::insert(&mut tree, center_col);
        let minutes_host = tree.insert(Some(split), pane_column(Role::Surface));

        let tools_host = tree.insert(
            Some(root),
            Container::column()
                .gap(crate::appearance::space(8.0))
                .padding(crate::appearance::space(12.0))
                .background(Role::Surface),
        );
        let status = StatusBar::insert(&mut tree, root);
        let installed = match source {
            Source::Live(_) => None,
            Source::Demo | Source::Snapshot => Some(Installed::all()),
        };

        let mut screen = Self {
            tree,
            source,
            link,
            session,
            filter: None,
            center: Center::Chat,
            shown_debates: BTreeSet::new(),
            map_nodes: Vec::new(),
            pending_intervention: None,
            file_dialogs: Vec::new(),
            export: None,
            built: None,
            timeline_state: None,
            history: Arc::new(Vec::new()),
            installed,
            auto,
            start,
            meeting,
            tree_host,
            center_host,
            minutes_host,
            composer,
            tools_host,
            tools_open: false,
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
            if !session.closed
                && matches!(
                    debate.status,
                    DebateStatus::Proposed | DebateStatus::Approved | DebateStatus::Active
                )
            {
                self.center = Center::Debate(DebateView::of(debate.id.clone()));
            }
        }
    }

    fn session_key(&self) -> Option<(String, u64)> {
        self.session.as_ref().map(|s| (s.id.to_string(), s.rev))
    }

    /// Rebuilds every pane from the session and the view state.
    fn rebuild(&mut self, text: &mut TextStore) {
        let focused = self.focused_action();
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
        self.tree.set_hidden(
            self.composer.root,
            self.read_only() || self.session.as_ref().is_none_or(|session| session.closed),
        );
        if started {
            self.rebuild_tree(text);
            self.rebuild_center(text);
            self.rebuild_minutes(text);
            self.rebuild_composer(text);
        } else {
            self.render_auto(text);
            self.tree
                .set_enabled(self.start.start_button, self.agents_ready());
            start::actions(&self.start, &mut self.actions);
            start::rebuild_history(
                &mut self.tree,
                &self.start,
                &self.history,
                &mut self.actions,
                text,
            );
        }
        self.rebuild_tools(text);
        self.rebuild_status(text);
        if self.read_only() {
            self.strip_controls();
        }
        if self.tree.focused().is_none()
            && let Some((area, action, nth)) = focused
        {
            self.restore_focus(area, &action, nth);
        }
    }

    fn focus_area(&self, id: WidgetId) -> Option<FocusArea> {
        let areas = [
            (self.start.root, FocusArea::Start),
            (self.tree_host, FocusArea::Issues),
            (self.center_host, FocusArea::Center),
            (self.minutes_host, FocusArea::Minutes),
            (self.composer.root, FocusArea::Composer),
            (self.tools_host, FocusArea::Tools),
            (self.status_leading, FocusArea::StatusLeading),
            (self.status_trailing, FocusArea::StatusTrailing),
        ];
        let mut ancestor = Some(id);
        while let Some(id) = ancestor {
            if let Some((_, area)) = areas.iter().find(|(root, _)| *root == id) {
                return Some(*area);
            }
            ancestor = self.tree.parent(id);
        }
        None
    }

    /// An issue can be linked repeatedly in several panes. Updates must
    /// keep focus on the same occurrence in the same pane, even when a
    /// message adds another matching link before the minutes in tree order.
    fn focused_action(&self) -> Option<(FocusArea, Action, usize)> {
        let focused = self.tree.focused()?;
        let area = self.focus_area(focused)?;
        let at = self.actions.iter().position(|(id, _)| *id == focused)?;
        let action = self.actions[at].1.clone();
        let nth = self.actions[..at]
            .iter()
            .filter(|(id, candidate)| *candidate == action && self.focus_area(*id) == Some(area))
            .count();
        Some((area, action, nth))
    }

    fn restore_focus(&mut self, area: FocusArea, action: &Action, nth: usize) {
        let mut candidates = self
            .actions
            .iter()
            .filter(|(id, candidate)| candidate == action && self.focus_area(*id) == Some(area));
        let first = candidates.clone().next().map(|(id, _)| *id);
        if let Some(id) = candidates.nth(nth).map(|(id, _)| *id).or(first) {
            self.tree.set_focus(Some(id));
        }
    }

    /// The kinds the agent buttons offer: the installed ones (every kind
    /// while the check runs or when it could not tell).
    fn available_kinds(&self) -> Vec<AgentKind> {
        self.installed
            .as_ref()
            .map_or_else(|| AgentKind::ALL.to_vec(), Installed::available)
    }

    /// Whether a discussion can be started: the installed agents are known
    /// and there is at least one.
    fn agents_ready(&self) -> bool {
        self.installed
            .as_ref()
            .is_some_and(|i| !i.available().is_empty())
    }

    /// The check for installed agents is done: the start view's menus
    /// offer only those. Agents picked meanwhile that are not installed
    /// are left out, and said so.
    fn installed_known(&mut self, installed: Installed, text: &mut TextStore) {
        let available = installed.available();
        let untouched = start::agents_untouched(&self.tree, &self.start);
        let dropped =
            start::set_available(&mut self.tree, &mut self.start, available.clone(), text);
        if !untouched && !dropped.is_empty() && !available.is_empty() {
            self.toast(start::excluded_note(&dropped));
        }
        self.installed = Some(installed);
        self.rebuild(text);
    }

    /// Takes away every control that would change a stored discussion
    /// shown read-only: hidden, and no longer clickable.
    fn strip_controls(&mut self) {
        let tree = &mut self.tree;
        // The send button outlives every build: its composer is hidden
        // instead, so a later live session gets it back as it was.
        let send = self.composer.send;
        self.actions.retain(|(id, action)| {
            if !action.acts_on_session() {
                return true;
            }
            if *id != send {
                tree.set_hidden(*id, true);
            }
            false
        });
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
        self.map_nodes.clear();
        let live = !self.read_only();
        let Some(session) = &self.session else {
            return;
        };
        match &self.center {
            Center::Chat | Center::Map => {
                let tabs = self.tree.insert(
                    Some(self.center_host),
                    Container::new(Style {
                        flex_direction: FlexDirection::Row,
                        flex_shrink: 0.0,
                        gap: gap(6.0),
                        padding: LayoutRect {
                            left: length(12.0),
                            right: length(12.0),
                            top: length(6.0),
                            bottom: length(6.0),
                        },
                        ..Default::default()
                    }),
                );
                let on_map = self.center == Center::Map;
                for (label, action, current) in [
                    ("チャット", Action::ShowChat, !on_map),
                    ("マップ", Action::ShowMap, on_map),
                ] {
                    let button = self.tree.insert(
                        Some(tabs),
                        Button::new(label)
                            .control_size(ControlSize::Small)
                            .selected(current),
                    );
                    self.actions.push((button, action));
                }
                if on_map {
                    spacer(&mut self.tree, tabs);
                    let save = self.tree.insert(
                        Some(tabs),
                        Button::new("画像で保存…").control_size(ControlSize::Small),
                    );
                    self.actions.push((save, Action::ExportMap));
                }
                let body = self
                    .tree
                    .insert(Some(self.center_host), Container::new(fill()));
                if on_map {
                    self.map_nodes = map::insert(&mut self.tree, body, session, &mut self.actions);
                } else {
                    chat::insert(
                        &mut self.tree,
                        body,
                        session,
                        self.filter.as_ref(),
                        live,
                        &mut self.actions,
                    );
                }
            }
            Center::Debate(view) => debate::insert(
                &mut self.tree,
                self.center_host,
                session,
                view,
                live,
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
        composer::set_send_default(
            &mut self.tree,
            &mut self.composer,
            !session.waits_to_continue() && !session.closed,
            text,
        );
        if !session.closed {
            self.actions.push((self.composer.send, Action::Send));
        }
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
        let mut history = None;
        let mut installed = None;
        if let Source::Live(ch) = &self.source {
            while let Ok(snapshot) = ch.snapshots.try_recv() {
                latest = Some(snapshot);
            }
            while let Ok(notice) = ch.notices.try_recv() {
                notices.push(notice);
            }
            while let Ok(listed) = ch.history.try_recv() {
                history = Some(listed);
            }
            if let Ok(found) = ch.installed.try_recv() {
                installed = Some(found);
            }
        }
        if let Some(installed) = installed {
            self.installed_known(installed, text);
        }
        let listed = history.is_some();
        let mut stale = false;
        if let Some(listed) = history {
            self.history = listed;
        }
        if let Some(snapshot) = latest {
            let id = |s: &Option<Arc<Session>>| s.as_ref().map(|s| s.id.clone());
            if id(&snapshot.session) != id(&self.session) {
                self.switched(snapshot.session.is_none());
            }
            self.link = snapshot.link;
            self.session = snapshot.session;
            self.note_debates();
            self.settle_intervention();
            stale |= self.built != Some((self.session_key(), self.link.clone()));
        }
        // A new list is shown on the start view, also when a snapshot in the
        // same poll left the view as it was built (the start view's first
        // one at launch).
        let next_state = match (&self.source, &self.session) {
            (Source::Live(ch), Some(s)) => ch.timeline.as_ref().and_then(|t| t.status(s)),
            _ => None,
        };
        if self.timeline_state != next_state {
            self.timeline_state = next_state;
            stale = true;
        }
        if stale || (listed && self.session.is_none()) {
            self.rebuild(text);
        }
        self.poll_auto(text);
        for notice in notices {
            self.toast(notice);
        }
    }

    /// Another discussion is shown, or none (`to_start`): the view state of
    /// the last one goes with it. Its unsent intervention must not reach
    /// the next one's panel, and its debate ids mean nothing there. Back at
    /// the start view the topic is cleared for the next discussion; the
    /// working directory, round limit and agents stay as they were set.
    fn switched(&mut self, to_start: bool) {
        self.auto.reset();
        self.filter = None;
        self.tools_open = false;
        self.center = Center::Chat;
        self.shown_debates.clear();
        self.pending_intervention = None;
        self.built = None;
        composer::clear(&mut self.tree, &self.composer);
        if to_start {
            start::clear_topic(&mut self.tree, &self.start);
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
            Action::ToggleTools => {
                self.tools_open = !self.tools_open;
                self.rebuild(text);
            }
            Action::ShowTimelineDetails => {
                self.tools_open = true;
                self.rebuild(text);
            }
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
            Action::BackToChat | Action::ShowChat => {
                self.center = Center::Chat;
                self.rebuild(text);
            }
            Action::ShowMap => {
                self.center = Center::Map;
                self.rebuild(text);
            }
            Action::ExportMap => {
                if let Some(session) = &self.session {
                    self.export = Some(Export::Map);
                    self.file_dialogs.push(FileDialogRequest {
                        save: true,
                        title: "マップを画像で保存".into(),
                        file_name: format!("{}-map.png", session.id),
                        filters: vec![("PNG".into(), vec!["png".into()])],
                    });
                }
            }
            Action::RetryTimeline => {
                if let (Source::Live(ch), Some(s)) = (&self.source, &self.session)
                    && let Some(timeline) = &ch.timeline
                {
                    if let Link::Stored(dir) = &self.link {
                        timeline.recover(s, dir);
                    } else {
                        timeline.retry(s);
                    }
                }
            }
            Action::Retry => self.send(Command::Retry, "失敗したブリーフを再送します"),
            Action::Send => self.send_intervention(),
            Action::Start => self.start_session(text),
            Action::AddPanelRow => {
                start::add_row(&mut self.tree, &mut self.start);
                self.rebuild(text);
            }
            Action::RemovePanelRow(index) => {
                start::remove_row(&mut self.tree, &mut self.start, index, text);
                self.rebuild(text);
            }
            Action::Close => self.send(Command::Close, "議論を終えます"),
            Action::CopyResult => {
                if let Some(session) = &self.session {
                    let result = discussion::model::export::result_markdown(session);
                    self.tree.clipboard().write_text(&result);
                    self.toast("結果をコピーしました");
                }
            }
            Action::ExportMinutes => {
                if let Some(session) = &self.session {
                    self.export = Some(Export::Minutes);
                    self.file_dialogs.push(FileDialogRequest {
                        save: true,
                        title: "議事録を書き出す".into(),
                        file_name: format!("{}-minutes.md", session.id),
                        filters: vec![("Markdown".into(), vec!["md".into()])],
                    });
                }
            }
            Action::Implement(agent) => self.send(
                Command::Implement { agent },
                format!("{} に実装を依頼します（新しいタブ）", agent.command()),
            ),
            Action::Open(dir) => self.send(
                Command::Open { dir: dir.clone() },
                format!("{} を表示します", dir.display()),
            ),
            Action::Resume(dir) => self.send(
                Command::Resume { dir: dir.clone() },
                format!("{} を再開します", dir.display()),
            ),
            Action::NewDiscussion => self.send(Command::NewDiscussion, "開始画面に戻ります"),
            Action::SetHold(hold) => self.send(
                Command::SetHold { hold },
                if hold {
                    "各ラウンドのあとで止めます"
                } else {
                    "ラウンドを続けて進めます"
                },
            ),
            Action::Continue => self.send(Command::Continue, "次のラウンドを始めます"),
            Action::Extend(by) => self.send(
                Command::Extend { by },
                format!("ラウンドを {by} 追加します"),
            ),
            Action::Focus(part) => self.send(
                Command::Focus { part: part.clone() },
                format!("{} のペインを表示します", part_name(&part)),
            ),
            Action::ClosePanes => self.send(Command::ClosePanes, "ペインをすべて閉じます"),
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
        if session.closed || self.read_only() {
            return;
        }
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

    fn start_session(&mut self, text: &mut TextStore) {
        if !self.agents_ready() {
            // The start button is disabled meanwhile; its note says why.
            return;
        }
        let form = start::read(&self.tree, &self.start);
        let workdir = match form.checked_workdir() {
            Ok(workdir) => workdir,
            Err(why) => {
                self.toast(why);
                return;
            }
        };
        let note = format!(
            "司会 {} とパネル {} で開始します",
            form.moderator.label(),
            form.panel
                .iter()
                .map(|k| k.label())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let auto = self.auto.record(&self.start, &form);
        self.cancel_planning(text);
        self.send(
            Command::Start {
                auto,
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
        if self.session.is_none() {
            if self.handle_auto(message, text) {
                return;
            }
            if message.downcast_ref::<SelectionChanged>().is_some() {
                if message.source == self.start.moderator {
                    start::moderator_kind_changed(&mut self.tree, &mut self.start, text);
                } else {
                    start::row_kind_changed(&mut self.tree, &mut self.start, message.source, text);
                }
                self.render_auto(text);
            } else if message
                .downcast_ref::<noa_gpui_core::widgets::TextChanged>()
                .is_some()
            {
                self.render_auto(text);
            }
        }
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
    fn set_system_appearance(&mut self, appearance: noa_gpui_core::Appearance) {
        crate::appearance::system(&mut self.tree, appearance);
    }
    fn tree(&self) -> &UiTree {
        &self.tree
    }

    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }

    fn take_file_dialogs(&mut self) -> Vec<FileDialogRequest> {
        std::mem::take(&mut self.file_dialogs)
    }

    fn file_dialog_finished(&mut self, result: FileDialogResult, _text: &mut TextStore) {
        let export = self.export.take();
        let (FileDialogResult::Saved(path), Some(session)) = (result, &self.session) else {
            return;
        };
        let message = match export {
            Some(Export::Minutes) => {
                let minutes = discussion::model::export::minutes_markdown(session);
                match std::fs::write(&path, minutes) {
                    Ok(()) => format!("議事録を書き出しました: {}", path.display()),
                    Err(err) => format!("{} に書き出せません: {err}", path.display()),
                }
            }
            Some(Export::Map) => match map::save_png(session, &path) {
                Ok((w, h)) => format!("マップを画像で保存しました（{w}×{h}）: {}", path.display()),
                Err(err) => format!("{} に保存できません: {err}", path.display()),
            },
            None => return,
        };
        self.toast(message);
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
        MsgKind::Conclusion => "結論",
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
    // Once the discussion has ended, no debate can start or go on.
    if session.closed {
        return None;
    }
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
        width: length(crate::appearance::space(px)),
        height: length(crate::appearance::space(px)),
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

/// Keeps where the user scrolled the scroll view holding `list` across
/// rebuilds: every live update rebuilds the panes, and a new scroll view
/// would otherwise open at its start (or, following its end, at the end).
/// `pane` names the view within `session`.
pub(crate) fn keep_scroll(tree: &mut UiTree, list: WidgetId, session: &Session, pane: &str) {
    let scroll = tree
        .parent(list)
        .expect("a scroll column sits in its scroll view");
    tree.set_autosave_name(scroll, format!("{}/{pane}", session.id));
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
            padding: LayoutRect::length(crate::appearance::space(10.0)),
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
    tree.insert(
        Some(row),
        Label::new(title)
            .font_size(crate::appearance::font(14.0))
            .weight(600),
    );
    row
}

pub(crate) fn caption(tree: &mut UiTree, parent: WidgetId, text: impl Into<String>) -> WidgetId {
    tree.insert(
        Some(parent),
        Label::new(text)
            .font_size(crate::appearance::font(12.0))
            .color(Role::Muted),
    )
}

/// A [`caption`] that wraps: for a sentence that may be longer than its
/// pane is wide.
pub(crate) fn note(tree: &mut UiTree, parent: WidgetId, text: impl Into<String>) -> WidgetId {
    tree.insert(
        Some(parent),
        Label::new(text)
            .font_size(crate::appearance::font(12.0))
            .color(Role::Muted)
            .wrap(true),
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

#[cfg(test)]
mod timeline_header_tests {
    use super::*;
    use crate::live::timeline::{NOT_REGISTERED, Status};
    use noa_ipc::timeline::Seq;
    use noa_ipc::timeline_outbox::PublishState;

    #[test]
    fn ac5_state_visible() {
        let mut screen = Discussion::demo();
        let mut text = TextStore::new(Default::default());
        for (state, expected) in [
            (
                Status::Publish(PublishState::Pending),
                "タイムライン: 投稿待ち",
            ),
            (
                Status::Waiting {
                    why: NOT_REGISTERED.into(),
                },
                "タイムライン: 要確認",
            ),
            (
                Status::Publish(PublishState::Published {
                    post_id: "po-001".into(),
                    seq: Seq(7),
                }),
                "タイムライン: 投稿済み #7",
            ),
            (
                Status::Publish(PublishState::Failed {
                    kind: None,
                    message: "offline".into(),
                }),
                "タイムライン: 投稿失敗（再試行）",
            ),
        ] {
            screen.timeline_state = Some(state);
            screen.rebuild(&mut text);
            let text_visible = screen
                .tree
                .children(screen.status_leading)
                .iter()
                .any(|&w| {
                    screen
                        .tree
                        .get::<Label>(w)
                        .is_some_and(|l| l.text() == expected)
                        || screen
                            .tree
                            .get::<noa_gpui_core::widgets::Badge>(w)
                            .and_then(noa_gpui_core::widgets::Badge::label)
                            .is_some_and(|label| label == expected)
                        || screen.tree.get::<Button>(w).is_some_and(|b| {
                            b.label() == expected
                                && screen.actions.contains(&(w, Action::RetryTimeline))
                        })
                });
            assert!(text_visible, "{expected}");
        }
    }
}
