//! The review window: reviewer cards | findings board, matrix or report |
//! finding thread, with a progress strip and a header above the center pane,
//! an intervention composer under it, and a start view before any session
//! exists.
//!
//! The screen holds no review state of its own: it draws the [`Session`] of
//! the latest [`Snapshot`] and sends [`Command`]s. What it keeps is view
//! state: the tab, the filters, the selected finding, the composer text.

mod auto;
mod board;
mod cards;
mod composer;
mod fix;
mod matrix;
mod progress;
mod report;
mod start;
mod thread;

#[cfg(test)]
mod tests {
    use crate::live::TestSnapshot as Snapshot;
    include!("tests.rs");
}

#[cfg(test)]
mod links_tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;

    use review::sibling::{self, Helper, Launcher};
    use review::task_links::LinkEvent;

    #[derive(Default)]
    struct RecordingLauncher(Vec<(Helper, Vec<OsString>)>);

    impl Launcher for RecordingLauncher {
        fn launch(&mut self, helper: Helper, args: Vec<OsString>) -> Result<(), String> {
            self.0.push((helper, args));
            Ok(())
        }
    }

    fn origin_button(screen: &Review) -> Option<WidgetId> {
        screen
            .actions
            .iter()
            .find_map(|(id, action)| (*action == Action::OpenOriginalTask).then_some(*id))
    }

    fn has_status_caption(screen: &Review, text: &str) -> bool {
        screen
            .tree
            .children(screen.status_trailing)
            .iter()
            .any(|&id| {
                screen
                    .tree
                    .get::<Label>(id)
                    .is_some_and(|label| label.text() == text)
            })
    }

    #[test]
    fn links_ac5_live_and_stored_return_button_opens_the_origin_issue() {
        let mut text = TextStore::new(Default::default());
        let dir = std::env::temp_dir().join(format!("noa-review-links-ui-{}", std::process::id()));
        let repo = dir.join("task repo");
        fs::create_dir_all(&repo).unwrap();
        let tasks_root = dir.join("tasks");
        let key_dir = tasks_root.join("key");
        fs::create_dir_all(&key_dir).unwrap();
        let review_dir = dir.join("reviews").join("rev-1");
        let batches = crate::demo::log();
        let mut store = store::Store::create(&review_dir, &batches[0]).unwrap();
        store.append_all(&batches[1..]).unwrap();
        drop(store);
        let session = crate::demo::session();
        let log = task_links::links_log(&key_dir);
        let mut origin = Origin {
            key_dir,
            tasks_repo: None,
            task: task_links::TaskKey(serde_json::json!({"github":{"repo":"o/n","number":12}})),
            run: "run-1".into(),
        };

        let snapshot = |origin| Snapshot {
            link: Link::Live,
            session: Some(Arc::new(session.clone())),
            reply_root: None,
            session_dir: Some(review_dir.clone()),
            origin,
            generation: 0,
        };
        assert!(origin_button(&Review::of_snapshot(snapshot(None))).is_none());
        let screen = Review::of_snapshot(snapshot(Some(origin.clone())));
        assert!(!screen.tree.is_enabled(origin_button(&screen).unwrap()));
        assert!(has_status_caption(
            &screen,
            "タスクのリポジトリがわかりません"
        ));
        origin.tasks_repo = Some(repo.clone());
        let screen = Review::of_snapshot(snapshot(Some(origin.clone())));
        assert!(screen.tree.is_enabled(origin_button(&screen).unwrap()));

        task_links::append(
            &log,
            &LinkEvent::Repo {
                path: repo.clone(),
                at: 1,
            },
        )
        .unwrap();
        task_links::append(
            &log,
            &LinkEvent::SessionLinked {
                kind: SessionKind::Review,
                session: session.id.to_string(),
                task: origin.task.clone(),
                run: origin.run.clone(),
                commit: Some("a".repeat(40)),
                uncommitted: false,
                by: task_links::MatchedBy::Branch,
                at: 2,
            },
        )
        .unwrap();
        let (channel, link) = Channel::fake();
        let mut screen = Review::live(channel, std::sync::mpsc::channel().1);
        screen
            .show_stored_at(&review_dir, &mut text, Some(&tasks_root))
            .unwrap();
        assert!(screen.read_only);
        let id = origin_button(&screen).unwrap();
        assert_eq!(
            screen.tree.get::<Button>(id).unwrap().label(),
            "元のタスクを開く"
        );
        assert!(screen.tree.is_enabled(id));
        screen.handle_action(Action::OpenOriginalTask, &mut text);
        let command = link.commands.try_iter().last().unwrap().1;
        assert_eq!(command, Command::OpenOriginalTask(origin.clone()));
        let Command::OpenOriginalTask(origin) = command else {
            unreachable!()
        };
        let mut launcher = RecordingLauncher::default();
        crate::live::open_original_task(&mut launcher, &origin).unwrap();
        assert_eq!(
            launcher.0,
            [(
                sibling::TASKS,
                vec![
                    format!("--repo={}", repo.display()).into(),
                    "--select=#12".into()
                ]
            )]
        );

        fs::remove_dir(&repo).unwrap();
        screen.rebuild(&mut text);
        assert!(!screen.tree.is_enabled(origin_button(&screen).unwrap()));
        assert!(has_status_caption(
            &screen,
            &format!("{} はもうありません", repo.display())
        ));
        assert!(
            crate::live::open_original_task(&mut launcher, &origin)
                .unwrap_err()
                .contains("はもうありません")
        );
        assert_eq!(launcher.0.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
}

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command as OsCommand;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use noa_gpui_core::style::{
    AlignItems, FlexDirection, FlexWrap, LengthPercentage, Overflow, Point as LayoutPoint,
    Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Alert, AlertResponse, AlertStyle, Button, ButtonClicked, Container, ControlSize, Label,
    MenuChosen, ScrollView, SelectionChanged, SplitPane, SplitView, StatusBar, Tag, TextChanged,
    TextField, Toast, Tone,
};
use noa_gpui_core::{Message as UiMessage, Role, Shortcut, Style, Theme, UiTree, WidgetId};
use noa_gpui_platform::{FileDialogRequest, FileDialogResult, Screen};
use noa_gpui_text::TextStore;

use review::model::export;
use review::model::types::{
    AgentKind, Agreement, Decision, FindingStatus, Millis, PartId, Phase, RoundNo, Session,
    Severity,
};
use review::store::{self, Listed};
use review::task_links::{self, Origin, SessionKind};

use crate::installed::Installed;
use crate::live::{Channel, Command, Link, Snapshot};
use crate::session::fix::FixRequest;

pub(crate) use fix::FixScope;
pub(crate) use start::Form as StartForm;

const LEFT_WIDTH: f32 = 280.0;
const RIGHT_WIDTH: f32 = 340.0;
const CENTER_MIN: f32 = 520.0;
pub(crate) const MIN_WINDOW: (f64, f64) = (
    (LEFT_WIDTH + CENTER_MIN + RIGHT_WIDTH + 2.0 * noa_gpui_core::widgets::DIVIDER) as f64,
    600.0,
);

/// The shortest time between two looks at a stored review on screen (its
/// log and its lock). The look is taken when the window wakes: on input,
/// on a redraw, and at least every `live::CLOCK_TICK` (10 s) when idle.
const BUSY_RECHECK: std::time::Duration = std::time::Duration::from_secs(1);

/// The rounds the "+N" buttons add.
const EXTEND_BY: [RoundNo; 2] = [1, 3];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Start,
    AutoAssign(usize),
    CancelAuto,
    AddPanelRow,
    RemovePanelRow(usize),
    /// Shows a stored review read-only (no runner, no agents).
    OpenPast(PathBuf),
    /// Continues a stored, unfinished review.
    ResumePast(PathBuf),
    ShowTab(CenterTab),
    ToggleSeverity(Severity),
    ToggleLens(String),
    ToggleStatus(FindingStatus),
    ToggleAgreement(Agreement),
    FilterReviewer(Option<PartId>),
    /// The board narrowed to the findings pointing into one target file
    /// (from the file coverage), or no longer.
    FilterFile(Option<String>),
    /// Opens the findings search (on the findings tab, focused), or
    /// closes it and drops its words.
    ToggleSearch,
    ClearFilters,
    /// A matrix cell: the board filtered to one lens (and severity).
    ShowCell(String, Option<Severity>),
    SelectFinding(Option<String>),
    /// Opens a snapshot file (or the snapshot folder) in the OS.
    OpenSnapshot(Option<String>),
    /// Opens a panel to choose the document file of a document review.
    PickDocument,
    CopyReport,
    SaveReport,
    Send,
    Retry,
    Close,
    NewReview,
    FocusPane(PartId),
    SetHold(bool),
    Continue,
    Extend(RoundNo),
    /// Closes the agents' panes of the ended review in Noa.
    ClosePanes,
    OpenOriginalTask,
    /// Takes up the unfinished stored review on screen (its agents start).
    ResumeShown,
    /// Writes the stored review's report again from its log.
    RegenerateReport,
    /// Fills the start view to review again what the review shown
    /// (`None`) or the one stored in the folder reviewed, as it was set up.
    ReviewAgain(Option<PathBuf>),
    /// Lists more of the past reviews.
    MorePast,
    /// Starts `agent` on fixing the findings of `scope` in a new tab.
    RequestFix {
        agent: AgentKind,
        scope: FixScope,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CenterTab {
    Findings,
    Matrix,
    Report,
}

impl CenterTab {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "findings" => CenterTab::Findings,
            "matrix" => CenterTab::Matrix,
            "report" => CenterTab::Report,
            _ => return None,
        })
    }
}

/// What the findings board shows. An empty set means every value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filters {
    pub severities: BTreeSet<Severity>,
    pub lenses: BTreeSet<String>,
    pub statuses: BTreeSet<FindingStatusKey>,
    pub agreements: BTreeSet<AgreementKey>,
    pub reviewer: Option<PartId>,
    /// Words a finding must hold (its id, claim, rationale, fix, location,
    /// or a source's claim, evidence or fix), case aside; empty: any.
    pub query: String,
    /// A target file a finding must point into (its own location or a
    /// source's).
    pub path: Option<String>,
}

/// [`FindingStatus`] in a set (by its order in `FindingStatus::ALL`).
pub type FindingStatusKey = usize;
/// [`Agreement`] in a set (by its order in `Agreement::ALL`).
pub type AgreementKey = usize;

pub fn status_key(status: FindingStatus) -> FindingStatusKey {
    FindingStatus::ALL
        .iter()
        .position(|s| *s == status)
        .unwrap_or(0)
}

pub fn agreement_key(agreement: Agreement) -> AgreementKey {
    Agreement::ALL
        .iter()
        .position(|a| *a == agreement)
        .unwrap_or(0)
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            severities: BTreeSet::new(),
            lenses: BTreeSet::new(),
            statuses: [
                FindingStatus::Open,
                FindingStatus::Agreed,
                FindingStatus::Disputed,
            ]
            .into_iter()
            .map(status_key)
            .collect(),
            agreements: BTreeSet::new(),
            reviewer: None,
            query: String::new(),
            path: None,
        }
    }
}

fn toggle<T: Ord>(set: &mut BTreeSet<T>, value: T) {
    if !set.remove(&value) {
        set.insert(value);
    }
}

enum Source {
    Demo,
    Snapshot,
    Live(Channel),
}

pub struct Review {
    tree: UiTree,
    source: Source,
    link: Link,
    session: Option<Arc<Session>>,
    /// Root of the snapshot and the reply files.
    reply_root: Option<PathBuf>,
    /// The session's store directory.
    session_dir: Option<PathBuf>,
    origin: Option<Origin>,
    tab: CenterTab,
    filters: Filters,
    selected: Option<String>,
    /// An intervention sent and not yet seen in the log.
    pending_intervention: Option<PendingIntervention>,
    file_dialogs: Vec<FileDialogRequest>,
    /// The session on screen was read from its log: no runner drives it.
    read_only: bool,
    past: Vec<Listed>,
    past_root: Option<PathBuf>,
    /// How many past reviews the list shows; さらに表示 raises it.
    past_limit: usize,
    /// How many there are in all.
    past_total: usize,
    /// The log of the read-only review on screen, when it is one.
    stored_dir: Option<PathBuf>,
    /// That log is free to write: no runner holds it.
    stored_free: bool,
    /// When whether a runner holds it was last asked (the view asks again
    /// every [`BUSY_RECHECK`] while the review is unfinished).
    busy_checked: Option<std::time::Instant>,
    /// The stored log's stamp when it was last read.
    stored_stamp: Option<store::LogStamp>,
    /// Rounds or a resume were asked for on the read-only review on
    /// screen: it stays shown until the runner's first snapshot of it
    /// arrives, then goes live.
    extending: bool,
    /// The stored report differs from what the log gives (missing, or the
    /// review stopped between writing its log and its report).
    report_stale: bool,
    /// A start was asked for: a refusal shows under the form.
    starting: bool,
    /// The open alert that asks before the review is ended.
    close_alert: Option<WidgetId>,
    built: Option<BuiltKey>,
    /// Which agents are installed: unknown until the background check
    /// reports (always "all" in the demo and in stored views, which stay
    /// deterministic and run no shell).
    detection: start::Detection,
    /// Where that check reports, until it has.
    installed: Option<Receiver<Installed>>,
    /// The version of the newest Noa connection that reported (its number
    /// in hello order, and the version): a report from an older connection
    /// that arrives later is not applied.
    noa_version: Option<(u64, String)>,

    auto: auto::Auto,
    start: start::Parts,
    board: WidgetId,
    left_host: WidgetId,
    header_host: WidgetId,
    progress_host: WidgetId,
    /// The center tabs and the findings summary.
    tabs_host: WidgetId,
    /// The findings search: kept across rebuilds, so typing in it goes on.
    search_row: WidgetId,
    search: WidgetId,
    /// The search row is open (it takes no line of the board otherwise).
    search_open: bool,
    center_host: WidgetId,
    right_host: WidgetId,
    composer: composer::Parts,
    status_leading: WidgetId,
    status_trailing: WidgetId,

    actions: Vec<(WidgetId, Action)>,
}

impl Review {
    pub fn demo() -> Self {
        Self::build(
            Source::Demo,
            Link::Live,
            Some(Arc::new(crate::demo::session())),
        )
    }

    /// The live screen. `installed` delivers which agents the login shell
    /// has; until it does, the start button waits.
    pub fn live(channel: Channel, installed: Receiver<Installed>) -> Self {
        let mut screen = Self::build(Source::Live(channel), Link::Connecting, None);
        screen.installed = Some(installed);
        screen.detection = start::Detection::Pending;
        let mut text = TextStore::new(Default::default());
        screen.rebuild(&mut text);
        screen
    }

    pub fn of_snapshot(snapshot: Snapshot) -> Self {
        let mut screen = Self::build(Source::Snapshot, snapshot.link, snapshot.session);
        screen.reply_root = snapshot.reply_root;
        screen.session_dir = snapshot.session_dir;
        screen.origin = snapshot.origin;
        screen.rebuild(&mut TextStore::new(Default::default()));
        screen
    }

    /// The demo's start view: a filled form above canned past reviews.
    pub fn demo_start() -> Self {
        let mut screen = Self::build(Source::Demo, Link::Live, None);
        let mut text = TextStore::new(Default::default());
        let form = crate::demo::start_form();
        start::fill(&mut screen.tree, &mut screen.start, &form, &mut text);
        screen.rebuild(&mut text);
        screen
    }

    /// The demo held after a consolidated round, waiting for the user.
    pub fn demo_held() -> Self {
        Self::build(
            Source::Demo,
            Link::Live,
            Some(Arc::new(crate::demo::held())),
        )
    }

    /// The review logged in `dir`, read-only.
    pub fn stored(dir: &Path) -> Result<Self, String> {
        let mut screen = Self::build(Source::Snapshot, Link::Live, None);
        let mut text = TextStore::new(Default::default());
        screen.show_stored(dir, &mut text)?;
        Ok(screen)
    }

    /// The live screen showing the review logged in `dir` read-only.
    pub fn live_stored(channel: Channel, installed: Receiver<Installed>, dir: &Path) -> Self {
        let mut screen = Self::live(channel, installed);
        let mut text = TextStore::new(Default::default());
        if let Err(why) = screen.show_stored(dir, &mut text) {
            screen.toast(why);
        }
        screen
    }

    /// Puts `dir` in the start view's repository, root and working
    /// directory fields (`--repo`: the focused pane's directory, so a
    /// review of where the person works needs no path typed).
    pub fn with_place(mut self, dir: &Path) -> Self {
        let mut text = TextStore::new(Default::default());
        let place = dir.display().to_string();
        let form = start::Form {
            repo: place.clone(),
            root: place.clone(),
            document_workdir: place,
            ..start::read(&self.tree, &self.start)
        };
        start::fill(&mut self.tree, &mut self.start, &form, &mut text);
        self.rebuild(&mut text);
        self
    }

    /// Picks the center tab and the selected finding (for `--tab` and
    /// `--finding`).
    pub fn with_view(mut self, tab: Option<CenterTab>, finding: Option<String>) -> Self {
        if let Some(tab) = tab {
            self.tab = tab;
        }
        if finding.is_some() {
            self.selected = finding;
        }
        let mut text = TextStore::new(Default::default());
        self.rebuild(&mut text);
        self
    }

    fn build(source: Source, link: Link, session: Option<Arc<Session>>) -> Self {
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
        let body = tree.insert(Some(root), Container::new(fill()));
        let start = start::insert(&mut tree, body);
        let auto = auto::Auto::new(&mut tree, start.auto_host);
        let board = tree.insert(Some(body), Container::new(fill()));
        let split = tree.insert(
            Some(board),
            SplitView::horizontal()
                .pane(SplitPane::fixed(LEFT_WIDTH).min(220.0))
                .pane(SplitPane::flexible().min(CENTER_MIN))
                .pane(SplitPane::fixed(RIGHT_WIDTH).min(260.0)),
        );
        let left_host = tree.insert(Some(split), pane_column(Role::Surface));
        let center_col = tree.insert(Some(split), pane_column(Role::Background));
        let header_host = tree.insert(Some(center_col), strip_host());
        let progress_host = tree.insert(Some(center_col), strip_host());
        let tabs_host = tree.insert(Some(center_col), strip_host());
        let search_row = tree.insert(
            Some(center_col),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::CENTER,
                flex_shrink: 0.0,
                padding: LayoutRect {
                    left: length(12.0),
                    right: length(12.0),
                    top: length(0.0),
                    bottom: length(6.0),
                },
                ..Default::default()
            }),
        );
        let search = tree.insert(
            Some(search_row),
            TextField::new("指摘を検索（ID・主張・根拠・修正案・パス）", 360.0),
        );
        tree.set_accessible_label(search, "指摘を検索");
        let center_host = tree.insert(Some(center_col), Container::new(fill()));
        let composer = composer::insert(&mut tree, center_col);
        let right_host = tree.insert(Some(split), pane_column(Role::Surface));
        let status = StatusBar::insert(&mut tree, root);

        let mut screen = Self {
            tree,
            source,
            link,
            session,
            reply_root: None,
            session_dir: None,
            origin: None,
            tab: CenterTab::Findings,
            filters: Filters::default(),
            selected: None,
            pending_intervention: None,
            file_dialogs: Vec::new(),
            read_only: false,
            past: Vec::new(),
            past_root: store::default_root(),
            past_limit: start::PAST_LIMIT,
            past_total: 0,
            stored_dir: None,
            stored_free: false,
            busy_checked: None,
            stored_stamp: None,
            extending: false,
            report_stale: false,
            starting: false,
            close_alert: None,
            built: None,
            detection: start::Detection::all_installed(),
            installed: None,
            noa_version: None,
            auto,
            start,
            board,
            left_host,
            header_host,
            progress_host,
            tabs_host,
            search_row,
            search,
            search_open: false,
            center_host,
            right_host,
            composer,
            status_leading: status.leading,
            status_trailing: status.trailing,
            actions: Vec::new(),
        };
        screen.refresh_past();
        let mut text = TextStore::new(Default::default());
        screen.rebuild(&mut text);
        screen
    }

    fn built_key(&self) -> BuiltKey {
        let session = self.session.as_ref();
        let minute = session
            .filter(|s| progress::has_running(s))
            .map(|_| system_now() / 60_000);
        (
            session.map(|s| (s.id.to_string(), s.rev)),
            self.link.clone(),
            minute,
        )
    }

    fn rebuild(&mut self, text: &mut TextStore) {
        // Rebuilding replaces the widgets, and focus would go with them: it
        // goes back to the widget that takes the focused one's place.
        let focused = self.focused_action();
        self.rebuild_widgets(text);
        if self.tree.focused().is_none()
            && let Some((action, nth)) = focused
        {
            self.focus_action(&action, nth);
        }
    }

    /// The action of the widget with keyboard focus, and which of the
    /// widgets carrying it (in tree order) that is.
    fn focused_action(&self) -> Option<(Action, usize)> {
        let focused = self.tree.focused()?;
        let at = self.actions.iter().position(|(id, _)| *id == focused)?;
        let action = self.actions[at].1.clone();
        let nth = self.actions[..at]
            .iter()
            .filter(|(_, a)| *a == action)
            .count();
        Some((action, nth))
    }

    /// Focuses the `nth` widget that carries `action` (the first, when there
    /// are fewer now).
    fn focus_action(&mut self, action: &Action, nth: usize) {
        let mut carrying = self.actions.iter().filter(|(_, a)| a == action);
        let first = carrying.clone().next().map(|(id, _)| *id);
        if let Some(id) = carrying.nth(nth).map(|(id, _)| *id).or(first) {
            self.tree.set_focus(Some(id));
        }
    }

    fn rebuild_widgets(&mut self, text: &mut TextStore) {
        self.built = Some(self.built_key());
        self.actions.clear();
        let started = self.session.is_some();
        self.tree.set_hidden(self.start.root, started);
        self.tree.set_hidden(self.board, !started);
        if started {
            self.rebuild_left(text);
            self.rebuild_header(text);
            self.rebuild_progress(text);
            self.rebuild_center(text);
            self.rebuild_right(text);
            self.rebuild_composer(text);
        } else {
            if !self.starting {
                start::rebuild_note(
                    &mut self.tree,
                    &self.start,
                    &self.link,
                    &self.detection,
                    text,
                );
            }
            self.render_auto(text);
            start::actions(&self.start, &mut self.actions);
            start::rebuild_past(
                &mut self.tree,
                &self.start,
                &self.past,
                self.past_total,
                system_now(),
                &mut self.actions,
                text,
            );
        }
        self.rebuild_status(text);
    }

    fn refresh_past(&mut self) {
        (self.past, self.past_total) = match &self.source {
            Source::Demo => {
                let past = crate::demo::past();
                let total = past.len();
                (past, total)
            }
            Source::Live(_) | Source::Snapshot => match self.past_root.as_deref() {
                Some(root) => store::page_sessions(root, &self.past, self.past_limit),
                None => (Vec::new(), 0),
            },
        };
    }

    fn load_stored(&self, dir: &Path) -> Result<Session, String> {
        if let Source::Demo = self.source {
            let id = dir.file_name().map(|n| n.to_string_lossy().into_owned());
            return crate::demo::stored(id.as_deref().unwrap_or_default())
                .ok_or_else(|| format!("{} を開けません", dir.display()));
        }
        store::read_session(dir).map_err(|e| format!("{} を開けません: {e}", dir.display()))
    }

    /// Leaves whatever review is on screen: its runner stops (an unfinished
    /// one stays resumable from the list).
    fn leave(&mut self) {
        self.auto.reset();
        if let Source::Live(channel) = &self.source {
            channel.leave();
            if !self.read_only && self.session.as_ref().is_some_and(|s| !s.closed) {
                self.toast("レビューを中断しました（過去のレビューから再開できます）");
            }
        }
        self.session = None;
        self.reply_root = None;
        self.session_dir = None;
        self.origin = None;
        self.read_only = false;
        self.stored_dir = None;
        self.extending = false;
        self.tab = CenterTab::Findings;
        self.filters = Filters::default();
        self.search_open = false;
        self.selected = None;
        self.pending_intervention = None;
        composer::reset(&mut self.tree, &mut self.composer);
        self.refresh_past();
    }

    fn show_stored(&mut self, dir: &Path, text: &mut TextStore) -> Result<(), String> {
        self.show_stored_at(dir, text, task_links::default_root().as_deref())
    }

    fn show_stored_at(
        &mut self,
        dir: &Path,
        text: &mut TextStore,
        tasks_root: Option<&Path>,
    ) -> Result<(), String> {
        // Taken just before the read, so it goes with the session read: a
        // write that lands after it shows as a change at the next look.
        let stamp = store::log_stamp(dir);
        let session = self.load_stored(dir)?;
        #[cfg(test)]
        tests::after_stored_read();
        self.leave();
        if !matches!(self.source, Source::Demo) {
            self.reply_root = Some(store::reply_dir(dir, &session));
            self.session_dir = Some(dir.to_owned());
            self.origin = tasks_root.and_then(|root| {
                task_links::find_origin(root, SessionKind::Review, session.id.as_str())
            });
        }
        self.session = Some(Arc::new(session));
        self.read_only = true;
        self.stored_dir = Some(dir.to_owned());
        self.stored_free = !store::is_busy(dir);
        self.stored_stamp = stamp;
        self.busy_checked = Some(std::time::Instant::now());
        self.report_stale = !matches!(self.source, Source::Demo)
            && store::report_is_stale(dir, self.session.as_deref().expect("just set"));
        self.rebuild(text);
        Ok(())
    }

    fn rebuild_left(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.left_host, text);
        let Some(session) = &self.session else {
            return;
        };
        let target = cards::insert(
            &mut self.tree,
            self.left_host,
            session,
            self.filters.reviewer.as_ref(),
            !self.read_only,
            system_now(),
            &mut self.actions,
        );
        // L1-5: the target card opens the snapshot, when there is one.
        if self.reply_root.is_some() {
            let row = self.tree.insert(Some(target), Container::row());
            let open = self.tree.insert(
                Some(row),
                Button::new("スナップショットを開く").control_size(ControlSize::Small),
            );
            self.actions.push((open, Action::OpenSnapshot(None)));
        }
    }

    fn rebuild_header(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.header_host, text);
        let Some(session) = &self.session else {
            return;
        };
        let row = self.tree.insert(
            Some(self.header_host),
            Container::row()
                .align_items(AlignItems::CENTER)
                .gap(8.0)
                .padding(10.0),
        );
        let title = self.tree.insert(
            Some(row),
            Container::new(Style {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                min_size: LayoutSize {
                    width: length(0.0),
                    height: auto(),
                },
                ..Default::default()
            }),
        );
        self.tree.insert(
            Some(title),
            Label::new(session.manifest.label.clone())
                .font_size(14.0)
                .weight(600)
                .truncation(noa_gpui_core::widgets::Truncation::Tail),
        );
        tag(
            &mut self.tree,
            row,
            session.target.kind.label(),
            Tone::Neutral,
        );
        let (status, tone) = if session.closed {
            ("完了", Tone::Neutral)
        } else if session.waits_to_continue() {
            ("停止中", Tone::Warning)
        } else {
            ("進行中", Tone::Accent)
        };
        tag(&mut self.tree, row, status, tone);
        if let Some(verdict) = session.verdict() {
            tag(
                &mut self.tree,
                row,
                format!("総合判定: {}", verdict.decision.label()),
                decision_tone(verdict.decision),
            );
        } else if session.ended_by_user() {
            tag(&mut self.tree, row, "判定なし", Tone::Neutral);
        }
        let small = |label: &str| Button::new(label).control_size(ControlSize::Small);
        // In the header (the title gives way), not the tab row, which the
        // smallest window fills: ⌘F works from every tab.
        let search = self.tree.insert(
            Some(row),
            small("検索")
                .selected(self.search_open)
                .shortcut(Shortcut::command('f'))
                .tooltip("指摘を言葉やパスで絞り込みます（⌘F）"),
        );
        self.tree.set_accessible_label(search, "指摘を検索");
        self.actions.push((search, Action::ToggleSearch));
        let copy = self.tree.insert(Some(row), small("コピー"));
        self.actions.push((copy, Action::CopyReport));
        let save = self.tree.insert(Some(row), small("レポート保存…"));
        self.actions.push((save, Action::SaveReport));
    }

    fn rebuild_progress(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.progress_host, text);
        let Some(session) = &self.session else {
            return;
        };
        progress::insert(&mut self.tree, self.progress_host, session, system_now());
    }

    fn rebuild_center(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.tabs_host, text);
        clear_children(&mut self.tree, self.center_host, text);
        self.tree.set_hidden(
            self.search_row,
            !(self.search_open && self.tab == CenterTab::Findings),
        );
        // The field shows the query the filters hold (cleared with them).
        if self.tree.get::<TextField>(self.search).map(|f| f.text()) != Some(&self.filters.query) {
            let query = self.filters.query.clone();
            self.tree
                .update::<TextField, _>(self.search, |f, cx| f.set_text(cx, query));
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        let tabs = self.tree.insert(
            Some(self.tabs_host),
            Container::new(Style {
                // As wide as the pane, so what does not fit wraps.
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                min_size: LayoutSize {
                    width: length(0.0),
                    height: auto(),
                },
                flex_direction: FlexDirection::Row,
                // Wraps when the tabs, the findings summary and the
                // snapshot button do not fit on one line.
                flex_wrap: FlexWrap::Wrap,
                align_items: AlignItems::CENTER,
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
        let live = session
            .findings
            .iter()
            .filter(|f| f.status.is_live())
            .count();
        for (label, tab) in [
            (format!("指摘 {live}"), CenterTab::Findings),
            ("観点×重大度".to_owned(), CenterTab::Matrix),
            (
                if session.verdict().is_some() || session.closed {
                    "レポート".to_owned()
                } else {
                    "レポート（暫定）".to_owned()
                },
                CenterTab::Report,
            ),
        ] {
            let btn = self.tree.insert(
                Some(tabs),
                Button::new(label)
                    .control_size(ControlSize::Small)
                    .selected(self.tab == tab),
            );
            self.actions.push((btn, Action::ShowTab(tab)));
        }
        spacer(&mut self.tree, tabs);
        if self.tab == CenterTab::Findings {
            board::summary(
                &mut self.tree,
                tabs,
                &session,
                &self.filters,
                &mut self.actions,
            );
        }
        match self.tab {
            CenterTab::Findings => board::insert(
                &mut self.tree,
                self.center_host,
                &session,
                &self.filters,
                self.selected.as_deref(),
                &mut self.actions,
            ),
            CenterTab::Matrix => matrix::insert(
                &mut self.tree,
                self.center_host,
                &session,
                &mut self.actions,
            ),
            CenterTab::Report => report::insert(&mut self.tree, self.center_host, &session),
        }
    }

    fn rebuild_right(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.right_host, text);
        let Some(session) = &self.session else {
            return;
        };
        let offer = self.fix_offer(session);
        thread::insert(
            &mut self.tree,
            self.right_host,
            session,
            self.selected.as_deref(),
            self.reply_root.is_some(),
            offer.as_ref(),
            &mut self.actions,
        );
    }

    /// What 修正を依頼 offers for `session`: nothing in a headless stored
    /// view (no Noa to start an agent in), else the installed agents and
    /// the scopes that hold findings, or why it cannot.
    fn fix_offer(&self, session: &Session) -> Option<fix::FixOffer> {
        if let Source::Snapshot = self.source {
            return None;
        }
        if let Err(why) = store::fix_workdir(session) {
            return Some(fix::FixOffer::Unavailable(why.to_owned()));
        }
        // A Noa known to be older than 0.9.13 would let the fix tab take
        // the global auto-approve setting (the request checks it again).
        if let Some(version) = self
            .noa_version
            .as_ref()
            .map(|(_, v)| v.as_str())
            .filter(|v| !crate::session::ipc::sets_tab_auto_approve(v))
        {
            return Some(fix::FixOffer::Unavailable(
                crate::session::ipc::no_tab_auto_approve(version),
            ));
        }
        let kinds = match self.detection.fix_kinds() {
            Ok(kinds) => kinds,
            Err(why) => return Some(fix::FixOffer::Unavailable(why)),
        };
        let scopes = match self.shown_selection() {
            Some(id) => vec![(FixScope::Finding(id.to_owned()), 1)],
            None => [FixScope::MustFix, FixScope::Shown]
                .into_iter()
                .map(|scope| {
                    // Counted unranked: a rebuild ranks once, for the board.
                    let n = match scope {
                        FixScope::Shown => board::count(session, &self.filters),
                        _ => self.fix_ids(session, &scope).len(),
                    };
                    (scope, n)
                })
                .filter(|(_, n)| *n > 0)
                .collect(),
        };
        // Running here, or (a stored view) in another window whose runner
        // holds the log.
        let running = !session.closed
            && (!self.read_only || (self.stored_dir.is_some() && !self.stored_free));
        Some(fix::FixOffer::Offered {
            kinds,
            scopes,
            running,
        })
    }

    /// The findings a fix request of `scope` takes.
    fn fix_ids(&self, session: &Session, scope: &FixScope) -> Vec<String> {
        match scope {
            FixScope::MustFix => session
                .verdict()
                .map(|v| {
                    v.must_fix
                        .iter()
                        .filter(|id| session.finding(id).is_some_and(|f| f.status.is_live()))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            FixScope::Shown => board::shown(session, &self.filters)
                .into_iter()
                .map(|f| f.id.clone())
                .collect(),
            // The selected one, while the board shows it.
            FixScope::Finding(id) => (self.shown_selection() == Some(id.as_str()))
                .then(|| id.clone())
                .into_iter()
                .collect(),
        }
    }

    fn rebuild_composer(&mut self, text: &mut TextStore) {
        let hide = self.read_only || self.session.as_ref().is_some_and(|s| s.closed);
        self.tree.set_hidden(self.composer.root, hide);
        let Some(session) = &self.session else {
            return;
        };
        if hide {
            return;
        }
        composer::sync(&mut self.tree, &mut self.composer, session, text);
        composer::set_send_default(
            &mut self.tree,
            &mut self.composer,
            !session.waits_to_continue(),
            text,
        );
        if let Some(pending) = &self.pending_intervention
            && session
                .messages
                .iter()
                .skip(pending.sent_at)
                .any(|m| pending.arrived_as(m))
        {
            // It arrived: the composer is emptied unless a new draft was
            // started meanwhile.
            if composer::text(&self.tree, &self.composer) == pending.body.trim() {
                composer::clear(&mut self.tree, &self.composer);
            }
            self.pending_intervention = None;
        }
        self.actions.push((self.composer.send, Action::Send));
    }

    fn rebuild_status(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.status_leading, text);
        clear_children(&mut self.tree, self.status_trailing, text);
        let (link_label, link_tone) = match &self.link {
            Link::Live => ("Noa 接続中", Tone::Success),
            Link::Connecting => ("Noa 接続待ち…", Tone::Warning),
            Link::Down(_) => ("Noa 未接続", Tone::Critical),
        };
        if !self.read_only {
            tag(&mut self.tree, self.status_leading, link_label, link_tone);
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        let phase_text = if session.closed {
            "終了".to_owned()
        } else if let Some(r) = session.current_round() {
            let phase = match r.phase {
                Phase::Briefing => "ブリーフ配信中",
                Phase::Reviewing => "レビュー中",
                Phase::Consolidating => "リードが取りまとめ中",
                Phase::Closed => "完了",
            };
            format!("R{} ({}) · {phase}", r.no, r.kind.label())
        } else {
            "準備中".to_owned()
        };
        caption(&mut self.tree, self.status_leading, phase_text);
        if self.read_only {
            tag(
                &mut self.tree,
                self.status_leading,
                "閲覧専用",
                Tone::Neutral,
            );
        }
        let small = |label: &str| Button::new(label).control_size(ControlSize::Small);
        let waits = session.waits_to_continue();
        if !self.read_only && !session.closed {
            let hold = self.tree.insert(
                Some(self.status_trailing),
                small(if session.hold {
                    "ラウンド後に停止: オン"
                } else {
                    "ラウンド後に停止: オフ"
                })
                .tooltip(
                    "オンにすると、リードの取りまとめのあと次のラウンドを始める前に止まります",
                ),
            );
            self.actions.push((hold, Action::SetHold(!session.hold)));
            if waits {
                let go = self.tree.insert(
                    Some(self.status_trailing),
                    small("次のラウンドへ").default_button(true),
                );
                self.actions.push((go, Action::Continue));
            }
        }
        if session.extendable().is_ok()
            && (!self.read_only
                || (session.closed
                    && self.stored_dir.is_some()
                    && self.stored_free
                    && !self.extending))
        {
            caption(
                &mut self.tree,
                self.status_trailing,
                format!("上限 {} ラウンド", session.max_rounds),
            );
            for by in EXTEND_BY {
                let more = self.tree.insert(
                    Some(self.status_trailing),
                    small(&format!("+{by}")).tooltip(if self.read_only {
                        "再開してラウンドを追加します（エージェントが起動します）"
                    } else {
                        "ラウンド上限を増やします。上限で終了していればレビューを続けます"
                    }),
                );
                self.tree
                    .set_accessible_label(more, format!("ラウンドを {by} 追加"));
                self.actions.push((more, Action::Extend(by)));
            }
        }
        let stored_free = self.read_only
            && self.stored_dir.is_some()
            && self.stored_free
            && !self.extending
            && matches!(self.source, Source::Live(_));
        if stored_free && !session.closed {
            let resume = self.tree.insert(
                Some(self.status_trailing),
                small("再開").tooltip("このレビューを続けます（エージェントが起動します）"),
            );
            self.actions.push((resume, Action::ResumeShown));
        }
        if self.read_only && self.report_stale && self.stored_free && !self.extending {
            let regenerate = self.tree.insert(
                Some(self.status_trailing),
                small("レポートを再生成").tooltip(
                    "保存されたレポートがログと一致しません（書き出し前に止まった可能性があります）。ログから書き直します",
                ),
            );
            self.actions.push((regenerate, Action::RegenerateReport));
        }
        if !self.read_only
            && !session.closed
            && session
                .current_round()
                .is_some_and(|r| !r.retryable().is_empty())
        {
            let retry = self
                .tree
                .insert(Some(self.status_trailing), small("失敗を再試行"));
            self.actions.push((retry, Action::Retry));
        }
        let again = self.tree.insert(
            Some(self.status_trailing),
            small("同じ設定で再レビュー").tooltip(
                "同じ対象・パネル・依頼で開始ビューを埋めます（前回の要修正を依頼に添えます）",
            ),
        );
        self.actions.push((again, Action::ReviewAgain(None)));
        let new = self
            .tree
            .insert(Some(self.status_trailing), small("新しいレビュー"));
        self.actions.push((new, Action::NewReview));
        if !self.read_only && !session.closed {
            let close = self
                .tree
                .insert(Some(self.status_trailing), small("レビューを終了"));
            self.actions.push((close, Action::Close));
        }
        if !self.read_only && session.closed {
            let close = self.tree.insert(
                Some(self.status_trailing),
                small("ペインをすべて閉じる")
                    .tooltip("リードとレビュアーのペインとボードタブを Noa でまとめて閉じます（修正依頼のタブは残します）"),
            );
            self.actions.push((close, Action::ClosePanes));
        }
        if let Some(origin) = &self.origin {
            let availability = task_links::tasks_args(origin);
            let open_task = self
                .tree
                .insert(Some(self.status_trailing), small("元のタスクを開く"));
            self.tree.set_enabled(open_task, availability.is_ok());
            self.actions.push((open_task, Action::OpenOriginalTask));
            if let Err(why) = availability {
                caption(&mut self.tree, self.status_trailing, why);
            }
        }
    }

    /// Takes the installed-agents report, once it has arrived: the menus
    /// then offer only those, and a form still untouched takes them as its
    /// defaults.
    fn poll_installed(&mut self, text: &mut TextStore) {
        let Some(rx) = &self.installed else {
            return;
        };
        let Ok(installed) = rx.try_recv() else {
            return;
        };
        self.installed = None;
        let before = start::read(&self.tree, &self.start);
        let clean = start::Form::for_kinds(self.start.kinds());
        let pristine =
            !self.start.refilled && before.lead == clean.lead && before.panel == clean.panel;
        self.detection = start::Detection::Done(installed);
        let kinds = self.detection.menu_kinds();
        let form = if pristine {
            let clean = start::Form::for_kinds(&kinds);
            start::Form {
                lead: clean.lead,
                panel: clean.panel,
                ..before
            }
        } else {
            before
        };
        let left_out = start::apply_kinds(&mut self.tree, &mut self.start, kinds, &form, text);
        self.built = None;
        if !left_out.is_empty() {
            self.toast(start::left_out_message(&left_out));
        }
    }

    /// A stored review on screen, when the window wakes (at most every
    /// [`BUSY_RECHECK`]): its log read again if it changed (another window
    /// finished it, or extended a finished one), and whether a runner holds
    /// it asked again, so its state, 再開, +N and 修正を依頼 with its
    /// running note follow together.
    fn recheck_stored(&mut self) {
        let Some(dir) = self.stored_dir.clone() else {
            return;
        };
        if !self.read_only
            || self.extending
            || self
                .busy_checked
                .is_some_and(|at| at.elapsed() < BUSY_RECHECK)
        {
            return;
        }
        self.busy_checked = Some(std::time::Instant::now());
        let stamp = store::log_stamp(&dir);
        // The stamp is kept only with a read that worked: a log that could
        // not be read is tried again at the next look.
        if stamp != self.stored_stamp
            && let Ok(session) = store::read_session(&dir)
        {
            self.stored_stamp = stamp;
            self.report_stale = store::report_is_stale(&dir, &session);
            self.session = Some(Arc::new(session));
            self.drop_hidden_selection();
            self.built = None;
        }
        let free = !store::is_busy(&dir);
        if free != self.stored_free {
            self.stored_free = free;
            // A writer that let go has written its report by then (it
            // writes the report after the log, before it releases it).
            if free && let Some(session) = &self.session {
                self.report_stale = store::report_is_stale(&dir, session);
            }
            self.built = None;
        }
    }

    fn poll_live(&mut self, text: &mut TextStore) {
        self.poll_installed(text);
        self.poll_auto(text);
        if matches!(self.source, Source::Live(_)) {
            self.recheck_stored();
        }
        let mut latest = None;
        let mut notices = Vec::new();
        let mut released = false;
        if let Source::Live(channel) = &self.source {
            let current = channel.generation();
            while let Ok((generation, notice)) = channel.notices.try_recv() {
                if generation >= current {
                    notices.push(notice);
                }
            }
            while let Ok(snapshot) = channel.snapshots.try_recv() {
                if snapshot.generation >= current {
                    latest = Some(snapshot);
                }
            }
            while channel.released.try_recv().is_ok() {
                released = true;
            }
            while let Ok(version) = channel.versions.try_recv() {
                if self
                    .noa_version
                    .as_ref()
                    .is_none_or(|(newest, _)| version.0 > *newest)
                {
                    self.noa_version = Some(version);
                }
                self.built = None;
            }
        }
        let refused_start = self.starting
            && self.session.is_none()
            && latest
                .as_ref()
                .is_none_or(|snapshot| snapshot.session.is_none())
            && !notices.is_empty();
        for notice in &notices {
            self.toast(notice.clone());
        }
        if refused_start {
            self.starting = false;
            self.render_auto(text);
            start::show_refusal(&mut self.tree, &self.start, notices.last().unwrap(), text);
        }
        if released && self.session.is_none() {
            self.refresh_past();
            self.built = None;
        }
        if let Some(snapshot) = latest {
            self.link = snapshot.link;
            if self.read_only {
                let shown = self.session.as_ref().map(|s| s.id.clone());
                match &snapshot.session {
                    Some(next) if self.extending && Some(&next.id) == shown.as_ref() => {
                        self.read_only = false;
                        self.extending = false;
                        self.stored_dir = None;
                        self.built = None;
                    }
                    None if self.extending && matches!(self.link, Link::Down(_)) => {
                        self.extending = false;
                        self.built = None;
                        return self.rebuild_if_changed(text);
                    }
                    _ => return self.rebuild_if_changed(text),
                }
            }
            if snapshot.session.is_some() {
                self.auto.reset();
                self.starting = false;
            }
            self.session = snapshot.session;
            if self.origin != snapshot.origin {
                self.origin = snapshot.origin;
                self.built = None;
            }
            self.drop_hidden_selection();
            if snapshot.reply_root.is_some() {
                self.reply_root = snapshot.reply_root;
                self.session_dir = snapshot.session_dir;
            }
        }
        self.rebuild_if_changed(text);
    }

    fn rebuild_if_changed(&mut self, text: &mut TextStore) {
        let next = self.built_key();
        if let Some(prev) = &self.built {
            if prev == &next {
                return;
            }
            if prev.0 == next.0 && prev.1 == next.1 {
                self.built = Some(next);
                self.rebuild_progress(text);
                return;
            }
        }
        self.rebuild(text);
    }

    /// Rebuilds after the filters changed: a selected finding they now
    /// hide is let go, so the thread and 修正を依頼 follow what is shown.
    fn refilter(&mut self, text: &mut TextStore) {
        self.drop_hidden_selection();
        self.rebuild(text);
    }

    /// The selected finding, while the board shows it (it exists and the
    /// filters let it through): what the thread and 修正を依頼 go by.
    fn shown_selection(&self) -> Option<&str> {
        let (session, id) = (self.session.as_ref()?, self.selected.as_deref()?);
        session
            .finding(id)
            .is_some_and(|f| board::passes(session, &self.filters, f))
            .then_some(id)
    }

    /// Lets the selection go once the board no longer shows it: after the
    /// filters change, or a new state of the review (a finding rejected,
    /// withdrawn or merged under filters that hide those).
    fn drop_hidden_selection(&mut self) {
        if self.selected.is_some() && self.shown_selection().is_none() {
            self.selected = None;
        }
    }

    fn toast(&mut self, message: impl Into<String>) {
        Toast::new(message.into()).show(&mut self.tree);
    }

    /// Why the stored review on screen cannot start its agents again
    /// (the check is pending, or one of them is not installed).
    fn agents_blocker(&self) -> Option<String> {
        self.session
            .as_ref()
            .and_then(|session| self.detection.resume_blocker(session))
    }

    fn send_cmd(&mut self, cmd: Command) {
        match &self.source {
            Source::Live(channel) => channel.send(cmd),
            Source::Demo | Source::Snapshot => {
                self.toast("デモ表示では操作を送信できません");
            }
        }
    }

    fn open_in_os(&mut self, path: &Path) {
        #[cfg(target_os = "macos")]
        let result = OsCommand::new("open").arg(path).spawn();
        #[cfg(target_os = "linux")]
        let result = OsCommand::new("xdg-open").arg(path).spawn();
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let result: std::io::Result<std::process::Child> =
            Err(std::io::Error::other("unsupported OS"));
        if let Err(e) = result {
            self.toast(format!("開けませんでした: {e}"));
        }
    }

    fn open_snapshot(&mut self, path: Option<String>) {
        let (Some(root), Some(session)) = (&self.reply_root, &self.session) else {
            self.toast("デモ表示には実体ファイルがありません");
            return;
        };
        let dir = store::target_dir(root);
        let target = match &path {
            Some(path) => review::capture::snapshot_path(&dir, &session.manifest, path),
            None => Some(dir.clone()),
        };
        match target.filter(|p| p.exists()) {
            Some(target) => self.open_in_os(&target),
            None => self.toast(format!(
                "スナップショットに {} はありません",
                path.unwrap_or_else(|| dir.display().to_string())
            )),
        }
    }

    fn handle_action(&mut self, action: Action, text: &mut TextStore) {
        match action {
            Action::AutoAssign(i) => self.begin_auto(i, text),
            Action::CancelAuto => {
                self.auto.cancel_running();
                self.render_auto(text);
            }
            Action::Start => {
                if !self.detection.startable() {
                    let why = self.detection.blocked_reason();
                    start::show_refusal(&mut self.tree, &self.start, &why, text);
                    return;
                }
                let form = start::read(&self.tree, &self.start);
                let target = match form.target() {
                    Ok(target) => target,
                    Err(why) => {
                        start::show_refusal(&mut self.tree, &self.start, &why, text);
                        return;
                    }
                };
                if let Err(why) = review::engine::check_panel(&form.lead, &form.panel) {
                    start::show_refusal(&mut self.tree, &self.start, &why, text);
                    return;
                }
                let auto = self.auto.record(&self.start, &form);
                self.auto.cancel_running();
                self.starting = true;
                self.render_auto(text);
                start::rebuild_note(
                    &mut self.tree,
                    &self.start,
                    &Link::Connecting,
                    &self.detection,
                    text,
                );
                self.send_cmd(Command::Start {
                    auto,
                    request: form.request,
                    target: Box::new(target),
                    max_rounds: form.max_rounds,
                    round_cap: form.round_cap,
                    lead: form.lead,
                    panel: form.panel,
                });
            }
            Action::AddPanelRow => {
                start::add_row(&mut self.tree, &mut self.start);
                self.rebuild(text);
            }
            Action::RemovePanelRow(index) => {
                start::remove_row(&mut self.tree, &mut self.start, index, text);
                self.rebuild(text);
            }
            Action::OpenPast(dir) => {
                if let Err(why) = self.show_stored(&dir, text) {
                    self.toast(why);
                }
            }
            Action::ResumePast(dir) => match self.load_stored(&dir) {
                Ok(session) if session.closed => {
                    if let Err(why) = self.show_stored(&dir, text) {
                        self.toast(why);
                    }
                }
                Ok(session) => match self.detection.resume_blocker(&session) {
                    Some(why) => self.toast(why),
                    None => self.send_cmd(Command::Resume { dir }),
                },
                Err(_) => self.send_cmd(Command::Resume { dir }),
            },
            Action::ShowTab(tab) => {
                self.tab = tab;
                self.rebuild(text);
            }
            Action::ToggleSeverity(s) => {
                toggle(&mut self.filters.severities, s);
                self.refilter(text);
            }
            Action::ToggleLens(id) => {
                toggle(&mut self.filters.lenses, id);
                self.refilter(text);
            }
            Action::ToggleStatus(s) => {
                toggle(&mut self.filters.statuses, status_key(s));
                self.refilter(text);
            }
            Action::ToggleAgreement(a) => {
                toggle(&mut self.filters.agreements, agreement_key(a));
                self.refilter(text);
            }
            Action::FilterReviewer(part) => {
                self.filters.reviewer = part;
                self.tab = CenterTab::Findings;
                self.refilter(text);
            }
            // Closes the search only where it shows; from another tab, or
            // closed, it opens on the findings tab with its words kept.
            Action::ToggleSearch => {
                if self.search_open && self.tab == CenterTab::Findings {
                    self.search_open = false;
                    self.filters.query.clear();
                    self.refilter(text);
                } else {
                    self.search_open = true;
                    self.tab = CenterTab::Findings;
                    self.refilter(text);
                    self.tree.set_focus(Some(self.search));
                }
            }
            Action::FilterFile(path) => {
                self.filters.path = path;
                self.tab = CenterTab::Findings;
                self.refilter(text);
            }
            Action::ClearFilters => {
                self.filters = Filters {
                    statuses: BTreeSet::new(),
                    ..Filters::default()
                };
                self.rebuild(text);
            }
            Action::ShowCell(lens, severity) => {
                self.filters = Filters {
                    lenses: [lens].into_iter().collect(),
                    severities: severity.into_iter().collect(),
                    ..Filters::default()
                };
                self.tab = CenterTab::Findings;
                self.refilter(text);
            }
            Action::SelectFinding(id) => {
                self.selected = id;
                self.rebuild(text);
            }
            Action::OpenSnapshot(path) => self.open_snapshot(path),
            Action::CopyReport => {
                if let Some(session) = &self.session {
                    let md = export::report_markdown(session);
                    // The clipboard reports no failure: what it holds
                    // afterwards tells whether the copy took, and one that
                    // cannot be read back leaves it unknown.
                    let clipboard = self.tree.clipboard();
                    clipboard.write_text(&md);
                    let message = match clipboard.read_text() {
                        Some(held) if held == md => "レポートをクリップボードにコピーしました",
                        Some(_) => "レポートをクリップボードにコピーできませんでした",
                        None => {
                            "レポートをコピーしましたが、クリップボードを読み返して確かめられませんでした"
                        }
                    };
                    self.toast(message);
                }
            }
            Action::PickDocument => self.file_dialogs.push(FileDialogRequest {
                save: false,
                title: "レビューする文書を選択".into(),
                file_name: String::new(),
                filters: Vec::new(),
            }),
            Action::SaveReport => {
                if let Some(session) = &self.session {
                    self.file_dialogs.push(FileDialogRequest {
                        save: true,
                        title: "レビューレポートを保存".into(),
                        file_name: format!("{}.md", session.id),
                        filters: vec![("Markdown".into(), vec!["md".into()])],
                    });
                }
            }
            Action::Send => {
                let Some(session) = &self.session else {
                    return;
                };
                let body = composer::text(&self.tree, &self.composer);
                if body.is_empty() {
                    return;
                }
                let (_, to) = composer::recipient(&self.tree, &self.composer, session);
                self.pending_intervention = Some(PendingIntervention {
                    body: body.clone(),
                    to: to.clone(),
                    sent_at: session.messages.len(),
                });
                self.send_cmd(Command::Intervene { to, body });
            }
            Action::Retry => self.send_cmd(Command::Retry),
            // Ending a review before its last round cannot be undone (it
            // can be neither resumed nor extended), so it is asked first;
            // Return and Escape keep it running.
            Action::Close => {
                self.close_alert = Some(
                    Alert::new("このレビューを終了しますか？")
                        .informative_text(
                            "総合判定なしで終わります。終了したレビューは再開もラウンドの追加もできません。",
                        )
                        .style(AlertStyle::Critical)
                        .button("キャンセル")
                        .button("レビューを終了")
                        .cancel_button(0)
                        .open(&mut self.tree),
                );
            }
            Action::NewReview => {
                self.leave();
                let form = start::Form::for_kinds(self.start.kinds());
                start::fill(&mut self.tree, &mut self.start, &form, text);
                self.start.refilled = false;
                self.rebuild(text);
            }
            Action::FocusPane(part) => self.send_cmd(Command::Focus { part }),
            Action::SetHold(hold) => self.send_cmd(Command::SetHold { hold }),
            Action::Continue => self.send_cmd(Command::Continue),
            Action::ClosePanes => self.send_cmd(Command::ClosePanes),
            Action::OpenOriginalTask => {
                if let Some(origin) = self.origin.clone() {
                    self.send_cmd(Command::OpenOriginalTask(origin));
                }
            }
            Action::Extend(by) => match self.stored_dir.clone() {
                Some(dir) if self.read_only => {
                    if !matches!(self.source, Source::Live(_)) {
                        self.toast("デモ表示では操作を送信できません");
                        return;
                    }
                    if let Some(why) = self.agents_blocker() {
                        self.toast(why);
                        return;
                    }
                    self.extending = true;
                    self.send_cmd(Command::ExtendStored { dir, by });
                    self.rebuild(text);
                }
                _ => self.send_cmd(Command::Extend { by }),
            },
            Action::ResumeShown => {
                if let Some(dir) = self.stored_dir.clone() {
                    if let Some(why) = self.agents_blocker() {
                        self.toast(why);
                        return;
                    }
                    self.extending = true;
                    self.send_cmd(Command::Resume { dir });
                    self.rebuild(text);
                }
            }
            // One more page, reconciled with the folder as it is now:
            // unchanged rows are kept (their lock asked again), changed and
            // new ones read, vanished ones dropped.
            Action::MorePast => {
                if let (Source::Live(_) | Source::Snapshot, Some(root)) =
                    (&self.source, self.past_root.clone())
                {
                    let want = self.past.len() + start::PAST_LIMIT;
                    (self.past, self.past_total) = store::page_sessions(&root, &self.past, want);
                    self.past_limit = self.past.len().max(start::PAST_LIMIT);
                }
                self.rebuild(text);
            }
            Action::ReviewAgain(dir) => {
                let session = match &dir {
                    None => match &self.session {
                        Some(session) => Session::clone(session),
                        None => return,
                    },
                    Some(dir) => match self.load_stored(dir) {
                        Ok(session) => session,
                        Err(why) => return self.toast(why),
                    },
                };
                self.auto.reset();
                let form = start::Form::from_session(&session);
                if self.session.is_some() {
                    self.leave();
                }
                let left_out = start::fill(&mut self.tree, &mut self.start, &form, text);
                // Set by the person: the agent check that reports later
                // only drops kinds that are not installed.
                self.start.refilled = true;
                if !left_out.is_empty() {
                    self.toast(start::left_out_message(&left_out));
                }
                self.toast(format!(
                    "{} と同じ設定を入れました。確かめてから「レビューを開始」を押してください",
                    session.id
                ));
                self.rebuild(text);
                start::show_top(&mut self.tree, &self.start, form.kind);
            }
            Action::RequestFix { agent, scope } => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                let ids = self.fix_ids(&session, &scope);
                if ids.is_empty() {
                    self.toast("修正を依頼する指摘が表示されていません");
                    return;
                }
                match (&self.source, self.session_dir.clone()) {
                    (Source::Live(_), Some(dir)) => {
                        self.send_cmd(Command::RequestFix(Box::new(FixRequest {
                            session,
                            dir,
                            agent,
                            ids,
                        })));
                        self.toast(format!("{} に修正を依頼しています…", agent.command()));
                    }
                    (Source::Live(_), None) => {
                        self.toast("このレビューの保存先が分からないため、修正を依頼できません");
                    }
                    (Source::Demo | Source::Snapshot, _) => {
                        self.toast("デモ表示では操作を送信できません");
                    }
                }
            }
            Action::RegenerateReport => {
                let Some(dir) = self.stored_dir.clone() else {
                    return;
                };
                match store::rewrite_report(&dir) {
                    Ok(()) => {
                        self.report_stale = false;
                        self.toast("レポートをログから書き直しました");
                    }
                    Err(why) => self.toast(format!("レポートを書き直せません: {why}")),
                }
                self.stored_free = !store::is_busy(&dir);
                self.rebuild(text);
            }
        }
    }
}

impl Screen for Review {
    fn tree(&self) -> &UiTree {
        &self.tree
    }

    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }

    fn before_frame(&mut self, text: &mut TextStore) {
        self.poll_live(text);
    }

    fn woken(&mut self, text: &mut TextStore) {
        self.poll_live(text);
    }

    fn handle(&mut self, message: UiMessage, text: &mut TextStore) {
        if let Some(answer) = message.downcast_ref::<AlertResponse>()
            && self.close_alert == Some(message.source)
        {
            self.close_alert = None;
            if answer.button == 1 {
                self.send_cmd(Command::Close);
            }
            return;
        }
        if let Some(TextChanged(query)) = message.downcast_ref::<TextChanged>()
            && message.source == self.search
        {
            if self.filters.query != *query {
                self.filters.query = query.clone();
                self.refilter(text);
            }
            return;
        }
        if self.session.is_none() {
            if self.auto.control() == message.source {
                if let Some(MenuChosen(i)) = message.downcast_ref() {
                    self.handle_action(Action::AutoAssign(*i), text);
                } else if message.downcast_ref::<ButtonClicked>().is_some() {
                    self.handle_action(Action::CancelAuto, text);
                }
                return;
            }
            if message.downcast_ref::<SelectionChanged>().is_some() {
                if message.source == self.start.lead {
                    start::lead_kind_changed(&mut self.tree, &mut self.start, text);
                } else if !start::target_kind_changed(&mut self.tree, &self.start, message.source) {
                    start::row_changed(&mut self.tree, &mut self.start, message.source, text);
                }
                self.render_auto(text);
                return;
            }
            if message.downcast_ref::<TextChanged>().is_some() {
                self.render_auto(text);
                return;
            }
        }
        if message.downcast_ref::<ButtonClicked>().is_some()
            && let Some((_, action)) = self
                .actions
                .iter()
                .find(|(id, _)| *id == message.source)
                .cloned()
        {
            let had_focus = self.tree.focused() == Some(message.source);
            let again = pressed_again(
                &action,
                self.selected.as_deref(),
                self.filters.reviewer.as_ref(),
            );
            self.handle_action(action, text);
            // A toggle pressed from the keyboard now carries another action
            // (the rebuild could not find it): focus the widget that does.
            if had_focus
                && self.tree.focused().is_none()
                && let Some(again) = again
            {
                self.focus_action(&again, 0);
            }
        }
    }

    fn take_file_dialogs(&mut self) -> Vec<FileDialogRequest> {
        std::mem::take(&mut self.file_dialogs)
    }

    fn file_dialog_finished(&mut self, result: FileDialogResult, text: &mut TextStore) {
        // Only 選択… opens a panel; レポート保存… saves.
        if let FileDialogResult::Opened(paths) = &result
            && let Some(path) = paths.first()
        {
            start::set_document_file(&mut self.tree, &self.start, path);
            self.render_auto(text);
            return;
        }
        if let FileDialogResult::Saved(path) = result
            && let Some(session) = &self.session
        {
            let md = export::report_markdown(session);
            match std::fs::write(&path, md) {
                Ok(()) => self.toast(format!("保存しました: {}", path.display())),
                Err(e) => self.toast(format!("保存できませんでした: {e}")),
            }
        }
    }
}

/// The action the widget pressed with `action` carries once the screen is
/// rebuilt. A finding's row and a card's 指摘を絞り込む toggle: pressed on,
/// they carry the clearing action; pressed off, the one for what was on
/// before (`selected`, `reviewer`).
fn pressed_again(
    action: &Action,
    selected: Option<&str>,
    reviewer: Option<&PartId>,
) -> Option<Action> {
    match action {
        Action::SelectFinding(Some(_)) => Some(Action::SelectFinding(None)),
        Action::SelectFinding(None) => {
            selected.map(|id| Action::SelectFinding(Some(id.to_owned())))
        }
        Action::FilterReviewer(Some(_)) => Some(Action::FilterReviewer(None)),
        Action::FilterReviewer(None) => reviewer.map(|p| Action::FilterReviewer(Some(p.clone()))),
        other => Some(other.clone()),
    }
}

/// An intervention sent and not yet seen in the log: what was sent, to
/// whom, and how many messages the log had then (only a later message can
/// be it arriving).
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingIntervention {
    body: String,
    to: Vec<PartId>,
    sent_at: usize,
}

impl PendingIntervention {
    /// Whether `message` is this intervention, logged.
    fn arrived_as(&self, message: &review::model::types::Message) -> bool {
        message.author == review::model::types::Author::User
            && message.kind == review::model::types::MsgKind::Intervention
            && message.to == self.to
            && message.body.trim() == self.body.trim()
    }
}

type BuiltKey = (Option<(String, u64)>, Link, Option<Millis>);

fn system_now() -> Millis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as Millis)
}

pub(super) fn severity_tone(severity: Severity) -> Tone {
    match severity {
        Severity::Critical | Severity::High => Tone::Critical,
        Severity::Medium => Tone::Warning,
        Severity::Low => Tone::Accent,
        Severity::Info => Tone::Neutral,
    }
}

pub(super) fn status_tone(status: FindingStatus) -> Tone {
    match status {
        FindingStatus::Agreed => Tone::Success,
        FindingStatus::Disputed => Tone::Warning,
        FindingStatus::Open => Tone::Accent,
        FindingStatus::Rejected | FindingStatus::Withdrawn | FindingStatus::Merged => Tone::Neutral,
    }
}

pub(super) fn decision_tone(decision: Decision) -> Tone {
    match decision {
        Decision::Approve => Tone::Success,
        Decision::ApproveWithFixes => Tone::Accent,
        Decision::RequestChanges => Tone::Warning,
        Decision::Reject => Tone::Critical,
    }
}

pub(super) fn full() -> LayoutSize<noa_gpui_core::style::Dimension> {
    LayoutSize {
        width: percent(1.0),
        height: percent(1.0),
    }
}

pub(super) fn fill() -> Style {
    Style {
        flex_direction: FlexDirection::Column,
        flex_grow: 1.0,
        flex_shrink: 1.0,
        flex_basis: length(0.0),
        min_size: LayoutSize {
            width: length(0.0),
            height: length(0.0),
        },
        ..Default::default()
    }
}

pub(super) fn gap(px: f32) -> LayoutSize<LengthPercentage> {
    LayoutSize {
        width: length(px),
        height: length(px),
    }
}

fn strip_host() -> Container {
    Container::new(Style {
        flex_shrink: 0.0,
        size: LayoutSize {
            width: percent(1.0),
            height: auto(),
        },
        ..Default::default()
    })
}

fn pane_column(bg: Role) -> Container {
    Container::new(Style {
        flex_direction: FlexDirection::Column,
        flex_grow: 1.0,
        flex_shrink: 1.0,
        flex_basis: length(0.0),
        min_size: LayoutSize {
            width: length(0.0),
            height: length(0.0),
        },
        overflow: LayoutPoint {
            x: Overflow::Hidden,
            y: Overflow::Hidden,
        },
        ..Default::default()
    })
    .background(bg)
}

pub(super) fn heading(tree: &mut UiTree, parent: WidgetId, text: &str) {
    let bar = tree.insert(
        Some(parent),
        Container::new(Style {
            flex_shrink: 0.0,
            padding: LayoutRect {
                left: length(12.0),
                right: length(12.0),
                top: length(10.0),
                bottom: length(6.0),
            },
            ..Default::default()
        }),
    );
    tree.insert(Some(bar), Label::new(text).font_size(14.0).weight(600));
}

pub(super) fn caption(tree: &mut UiTree, parent: WidgetId, text: impl Into<String>) {
    tree.insert(
        Some(parent),
        Label::new(text.into()).font_size(11.0).color(Role::Muted),
    );
}

pub(super) fn note(tree: &mut UiTree, parent: WidgetId, text: impl Into<String>) {
    tree.insert(
        Some(parent),
        Label::new(text.into())
            .font_size(11.5)
            .color(Role::Muted)
            .wrap(true),
    );
}

pub(super) fn body(tree: &mut UiTree, parent: WidgetId, text: impl Into<String>) {
    tree.insert(
        Some(parent),
        Label::new(text.into())
            .font_size(12.5)
            .wrap(true)
            .selectable(true),
    );
}

pub(super) fn tag(tree: &mut UiTree, parent: WidgetId, text: impl Into<String>, tone: Tone) {
    tree.insert(Some(parent), Tag::new(text.into()).tone(tone));
}

pub(super) fn spacer(tree: &mut UiTree, parent: WidgetId) {
    tree.insert(
        Some(parent),
        Container::new(Style {
            flex_grow: 1.0,
            ..Default::default()
        }),
    );
}

pub(super) fn scroll_column(tree: &mut UiTree, parent: WidgetId, spacing: f32) -> WidgetId {
    let sv = tree.insert(
        Some(parent),
        ScrollView::new(Style {
            overflow: LayoutPoint {
                x: Overflow::Hidden,
                y: Overflow::Scroll,
            },
            ..fill()
        }),
    );
    tree.insert(
        Some(sv),
        Container::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(spacing),
            padding: LayoutRect::length(12.0),
            size: LayoutSize {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        }),
    )
}

pub(super) fn keep_scroll(tree: &mut UiTree, list: WidgetId, session: &Session, key: &str) {
    if let Some(sv) = tree.parent(list) {
        tree.set_autosave_name(sv, format!("{}/{key}", session.id));
    }
}

pub(super) fn clear_children(tree: &mut UiTree, parent: WidgetId, text: &mut TextStore) {
    let children: Vec<WidgetId> = tree.children(parent).to_vec();
    for child in children {
        tree.remove(child, text);
    }
}

/// A card: a bordered surface column.
pub(super) fn card(tree: &mut UiTree, parent: WidgetId, accent: bool) -> WidgetId {
    tree.insert(
        Some(parent),
        Container::column()
            .gap(6.0)
            .padding(10.0)
            .background(Role::Background)
            .border(
                1.0,
                if accent {
                    Role::Accent
                } else {
                    Role::Separator
                },
            )
            .radius(8.0),
    )
}
