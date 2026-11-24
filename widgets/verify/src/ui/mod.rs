//! The verification window: target and panel cards | criteria matrix,
//! evidence, defects or report | the selected criterion's thread, with a
//! header and a progress strip above the center pane, a composer under
//! it, the status bar at the bottom, and a start view before any
//! verification exists.
//!
//! The screen holds no verification state of its own: it draws the
//! [`Session`] of the latest [`Snapshot`] and sends [`Command`]s. What it
//! keeps is view state: the tab, the filters, the selection, the drafts,
//! the open log, the file waiting for its caption.

mod board;
mod cards;
mod composer;
mod criteria;
mod defects;
mod evidence;
mod fix;
mod matrix;
mod progress;
mod report;
mod start;
mod thread;

#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command as OsCommand;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use noa_gpui_core::style::{
    FlexDirection, LengthPercentage, Overflow, Point as LayoutPoint, Rect as LayoutRect,
    Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Alert, AlertResponse, AlertStyle, Container, Label, ScrollView, SelectionChanged, SplitPane,
    SplitView, StatusBar, Tag, TextChanged, Toast, Tone,
};
use noa_gpui_core::{
    DragData, Event as UiEvent, Message as UiMessage, Role, Style, Theme, UiTree, WidgetId,
};
use noa_gpui_platform::{FileDialogRequest, FileDialogResult, Screen};
use noa_gpui_text::TextStore;

use verify::model::export;
use verify::model::types::{
    AgentKind, Agreement, Decision, DefectStatus, EvidenceBody, EvidenceKey, MAX_LOG_BYTES, Millis,
    PartId, Rerun, RoundNo, RulingKind, Session, Severity, Stored, WorktreeState,
};
use verify::store::{self, Listed};

use crate::installed::Installed;
use crate::live::{Attachment, Channel, Command, Link, Snapshot};
use crate::session::fix::FixRequest;

pub(crate) use fix::FixScope;
pub(crate) use start::Form as StartForm;

const LEFT_WIDTH: f32 = 280.0;
const RIGHT_WIDTH: f32 = 340.0;
const CENTER_MIN: f32 = 520.0;
pub(crate) const MIN_WINDOW: (f64, f64) = (
    (LEFT_WIDTH + CENTER_MIN + RIGHT_WIDTH + 2.0 * noa_gpui_core::widgets::DIVIDER) as f64,
    640.0,
);

/// The shortest time between two looks at a stored verification on screen
/// (its log and its lock). The look is taken when the window wakes: on
/// input, on a redraw, and at least every `live::CLOCK_TICK` (10 s).
const BUSY_RECHECK: std::time::Duration = std::time::Duration::from_secs(1);

/// The rounds the "+N" buttons add.
const EXTEND_BY: [RoundNo; 2] = [1, 3];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Start,
    AddPanelRow,
    RemovePanelRow(usize),
    /// Opens a panel to choose the spec file.
    PickSpec,
    /// Shows a stored verification read-only (no runner, no agents).
    OpenPast(PathBuf),
    /// Continues a stored, unfinished verification.
    ResumePast(PathBuf),
    /// Fills the start view to verify again what the verification shown
    /// (`None`) or the one stored in the folder verified, as it was set up.
    VerifyAgain(Option<PathBuf>),
    /// Lists more of the past verifications.
    MorePast,
    ShowTab(CenterTab),
    SelectCriterion(Option<String>),
    SelectDefect(Option<String>),
    ToggleSeverity(Severity),
    ToggleCategory(String),
    ToggleStatus(DefectStatus),
    ToggleAgreement(Agreement),
    FilterReporter(Option<PartId>),
    ClearDefectFilters,
    FilterEvidence(evidence::Filter),
    ClearEvidenceFilters,
    /// 開く: the stored copy (a log in the window's text view, anything
    /// else in the system viewer).
    OpenEvidence(EvidenceKey),
    /// Finder で表示 on the stored copy.
    RevealEvidence(EvidenceKey),
    CloseLog,
    /// 証跡を追加: opens a panel to choose files.
    AttachEvidence,
    AttachSubmit,
    AttachCancel,
    SaveCriterion(String),
    RemoveCriterion(String),
    MoveCriterion(String, usize),
    AddCriterion,
    /// 基準を確定して検証を開始.
    StartVerify,
    /// Opens the snapshot folder in the OS.
    OpenSnapshot,
    /// ワークツリーを開く: the verifier's worktree in Finder.
    OpenWorktree(PartId),
    CopyReport,
    /// レポート保存 (レポートのみ).
    SaveReport,
    /// 証跡ごと書き出す.
    ExportBundle,
    NewVerification,
    Send,
    Retry,
    Close,
    FocusPane(PartId),
    SetHold(bool),
    Continue,
    Extend(RoundNo),
    /// ペインをすべて閉じる (asks first).
    ClosePanes,
    /// Takes up the unfinished stored verification on screen.
    ResumeShown,
    /// Writes the stored verification's report again from its log.
    RegenerateReport,
    /// ワークツリーを削除 for the stored verification on screen.
    RemoveWorktrees,
    /// Starts `agent` on fixing what `scope` takes, in a new tab.
    RequestFix {
        agent: AgentKind,
        scope: FixScope,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CenterTab {
    Criteria,
    Evidence,
    /// The evidence tab in ギャラリー mode.
    Gallery,
    Defects,
    Report,
}

impl CenterTab {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "criteria" => CenterTab::Criteria,
            "evidence" => CenterTab::Evidence,
            "gallery" => CenterTab::Gallery,
            "defects" => CenterTab::Defects,
            "report" => CenterTab::Report,
            _ => return None,
        })
    }
}

/// What the right pane follows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Selected {
    Criterion(String),
    Defect(String),
}

/// What a file dialog was asked for, in the order the requests went out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Spec,
    Attach,
    SaveReport,
    ExportBundle,
}

/// What the screen asks the OS to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Open {
    /// `open <path>`: the system viewer or player, or Finder on a folder.
    File(PathBuf),
    /// `open -R <path>`: Finder with the file selected.
    Reveal(PathBuf),
}

/// How [`Open`] requests are carried out: `open` as a subprocess, or a
/// test's recorder.
pub(crate) type Opener = Box<dyn FnMut(&Open) -> Result<(), String>>;

fn system_opener() -> Opener {
    Box::new(|open| {
        let mut cmd = if cfg!(target_os = "macos") {
            OsCommand::new("open")
        } else {
            OsCommand::new("xdg-open")
        };
        match open {
            Open::File(path) => cmd.arg(path),
            Open::Reveal(path) if cfg!(target_os = "macos") => cmd.arg("-R").arg(path),
            // No file manager selects a file on every desktop: its folder.
            Open::Reveal(path) => cmd.arg(path.parent().unwrap_or(path)),
        };
        cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
    })
}

enum Source {
    Demo,
    Snapshot,
    Live(Channel),
}

fn toggle<T: Ord>(set: &mut BTreeSet<T>, value: T) {
    if !set.remove(&value) {
        set.insert(value);
    }
}

pub struct Verify {
    tree: UiTree,
    source: Source,
    link: Link,
    session: Option<Arc<Session>>,
    /// The judge's reply dir: the snapshot, the stored evidence.
    reply_root: Option<PathBuf>,
    /// The session's store directory.
    session_dir: Option<PathBuf>,
    tab: CenterTab,
    defect_filters: defects::Filters,
    evidence_filters: evidence::Filters,
    selected: Option<Selected>,
    /// An intervention sent and not yet seen in the log.
    pending_intervention: Option<PendingIntervention>,
    file_dialogs: Vec<FileDialogRequest>,
    dialog_purposes: VecDeque<Purpose>,
    /// The session on screen was read from its log: no runner drives it.
    read_only: bool,
    past: Vec<Listed>,
    past_root: Option<PathBuf>,
    past_limit: usize,
    past_total: usize,
    /// The log of the read-only verification on screen, when it is one.
    stored_dir: Option<PathBuf>,
    /// That log is free to write: no runner holds it.
    stored_free: bool,
    busy_checked: Option<std::time::Instant>,
    stored_stamp: Option<store::LogStamp>,
    /// Rounds or a resume were asked for on the read-only verification
    /// on screen: it stays shown until the runner's first snapshot of it.
    extending: bool,
    report_stale: bool,
    /// A start was asked for: a refusal shows under the form.
    starting: bool,
    /// Why the last start was refused, shown under the form until the
    /// next start or a refill.
    refusal: Option<String>,
    /// The open alert that asks before the verification is ended.
    close_alert: Option<WidgetId>,
    /// The open alert that asks before every agent pane is closed.
    panes_alert: Option<WidgetId>,
    /// How many panes Noa says the stored verification on screen still has
    /// open (its folder, the count), once asked.
    stored_panes: Option<(PathBuf, usize)>,
    built: Option<BuiltKey>,
    detection: start::Detection,
    installed: Option<Receiver<Installed>>,
    noa_version: Option<(u64, String)>,
    /// The command whose stored log the log view shows.
    log_open: Option<EvidenceKey>,
    /// The file the 証跡を追加 form is for, and those waiting after it.
    attach_current: Option<evidence::AttachDraft>,
    attach_queue: VecDeque<evidence::AttachDraft>,
    opener: Opener,

    start: start::Parts,
    editor: criteria::Editor,
    board: WidgetId,
    left_host: WidgetId,
    header_host: WidgetId,
    progress_host: WidgetId,
    tabs_host: WidgetId,
    center_host: WidgetId,
    log: evidence::LogParts,
    attach: evidence::AttachParts,
    right_host: WidgetId,
    composer: composer::Parts,
    status_leading: WidgetId,
    status_trailing: WidgetId,

    actions: Vec<(WidgetId, Action)>,
}

impl Verify {
    pub fn demo() -> Self {
        Self::build(
            Source::Demo,
            Link::Live,
            Some(Arc::new(crate::demo::session())),
        )
    }

    /// The demo's start view: a filled form above canned past
    /// verifications.
    pub fn demo_start() -> Self {
        let mut screen = Self::build(Source::Demo, Link::Live, None);
        let mut text = TextStore::new(Default::default());
        let form = crate::demo::start_form();
        start::fill(&mut screen.tree, &mut screen.start, &form, &mut text);
        screen.rebuild(&mut text);
        screen
    }

    /// The demo held after its first ruled round, waiting for the person.
    pub fn demo_held() -> Self {
        Self::build(
            Source::Demo,
            Link::Live,
            Some(Arc::new(crate::demo::held())),
        )
    }

    /// The demo's criteria, held for the person to confirm.
    pub fn demo_criteria() -> Self {
        Self::build(
            Source::Demo,
            Link::Live,
            Some(Arc::new(crate::demo::criteria_held())),
        )
    }

    /// The verification logged in `dir`, read-only.
    pub fn stored(dir: &Path) -> Result<Self, String> {
        let mut screen = Self::build(Source::Snapshot, Link::Live, None);
        let mut text = TextStore::new(Default::default());
        screen.show_stored(dir, &mut text)?;
        Ok(screen)
    }

    pub fn of_snapshot(snapshot: Snapshot) -> Self {
        let mut screen = Self::build(Source::Snapshot, snapshot.link, snapshot.session);
        screen.reply_root = snapshot.reply_root;
        screen.session_dir = snapshot.session_dir;
        screen
    }

    /// The live screen. `installed` delivers which agents the login shell
    /// has; until it does, the start button waits.
    pub fn live(channel: Channel, installed: Receiver<Installed>) -> Self {
        let mut screen = Self::build(Source::Live(channel), Link::Connecting, None);
        screen.installed = Some(installed);
        screen.detection = start::Detection::Pending;
        screen.rebuild(&mut TextStore::new(Default::default()));
        screen
    }

    /// The live screen showing the verification logged in `dir`
    /// read-only.
    pub fn live_stored(channel: Channel, installed: Receiver<Installed>, dir: &Path) -> Self {
        let mut screen = Self::live(channel, installed);
        let mut text = TextStore::new(Default::default());
        if let Err(why) = screen.show_stored(dir, &mut text) {
            screen.toast(why);
        }
        screen
    }

    /// Puts `dir` in the start view's repository field (`--repo`: the
    /// focused pane's directory, so verifying where the person works needs
    /// no path typed).
    pub fn with_place(mut self, dir: &Path) -> Self {
        let mut text = TextStore::new(Default::default());
        let form = start::Form {
            repo: dir.display().to_string(),
            ..start::read(&self.tree, &self.start)
        };
        start::fill(&mut self.tree, &mut self.start, &form, &mut text);
        self.rebuild(&mut text);
        self
    }

    /// Picks the center tab and the selected criterion (for `--tab` and
    /// `--criterion`).
    pub fn with_view(mut self, tab: Option<CenterTab>, criterion: Option<String>) -> Self {
        if let Some(tab) = tab {
            self.tab = tab;
        }
        if let Some(id) = criterion {
            self.selected = Some(Selected::Criterion(id));
        }
        self.rebuild(&mut TextStore::new(Default::default()));
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
        let center_host = tree.insert(Some(center_col), Container::new(fill()));
        let log = evidence::insert_log(&mut tree, center_col);
        let composer = composer::insert(&mut tree, center_col);
        let right_col = tree.insert(Some(split), pane_column(Role::Surface));
        let attach_host = tree.insert(
            Some(right_col),
            Container::new(Style {
                flex_shrink: 0.0,
                padding: LayoutRect::length(8.0),
                ..Default::default()
            }),
        );
        let attach = evidence::insert_attach(&mut tree, attach_host);
        let right_host = tree.insert(Some(right_col), Container::new(fill()));
        let status = StatusBar::insert(&mut tree, root);

        let mut screen = Self {
            tree,
            source,
            link,
            session,
            reply_root: None,
            session_dir: None,
            tab: CenterTab::Criteria,
            defect_filters: defects::Filters::default(),
            evidence_filters: evidence::Filters::default(),
            selected: None,
            pending_intervention: None,
            file_dialogs: Vec::new(),
            dialog_purposes: VecDeque::new(),
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
            refusal: None,
            close_alert: None,
            panes_alert: None,
            stored_panes: None,
            built: None,
            detection: start::Detection::all_installed(),
            installed: None,
            noa_version: None,
            log_open: None,
            attach_current: None,
            attach_queue: VecDeque::new(),
            opener: system_opener(),
            start,
            editor: criteria::Editor::default(),
            board,
            left_host,
            header_host,
            progress_host,
            tabs_host,
            center_host,
            log,
            attach,
            right_host,
            composer,
            status_leading: status.leading,
            status_trailing: status.trailing,
            actions: Vec::new(),
        };
        screen.refresh_past();
        screen.rebuild(&mut TextStore::new(Default::default()));
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

    /// Focuses the `nth` widget that carries `action` (the first, when
    /// there are fewer now).
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
            self.attach_actions();
        } else {
            self.tree
                .set_enabled(self.start.start_button, self.detection.startable());
            match (&self.refusal, self.starting) {
                (_, true) => {}
                (Some(why), false) => {
                    let why = why.clone();
                    start::show_refusal(&mut self.tree, &self.start, &why, text);
                }
                (None, false) => start::rebuild_note(
                    &mut self.tree,
                    &self.start,
                    &self.link,
                    &self.detection,
                    text,
                ),
            }
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

    /// Leaves whatever verification is on screen: its runner stops (an
    /// unfinished one stays resumable from the list).
    fn leave(&mut self) {
        if let Source::Live(channel) = &self.source {
            channel.leave();
            if !self.read_only && self.session.as_ref().is_some_and(|s| !s.closed) {
                self.toast("検証を中断しました（過去の検証から再開できます）");
            }
        }
        self.session = None;
        self.reply_root = None;
        self.session_dir = None;
        self.read_only = false;
        self.stored_dir = None;
        self.stored_panes = None;
        self.extending = false;
        self.tab = CenterTab::Criteria;
        self.defect_filters = defects::Filters::default();
        self.evidence_filters = evidence::Filters::default();
        self.selected = None;
        self.pending_intervention = None;
        self.editor.reset();
        self.close_log();
        self.attach_current = None;
        self.attach_queue.clear();
        evidence::hide_attach(&mut self.tree, &self.attach);
        composer::reset(&mut self.tree, &mut self.composer);
        self.refresh_past();
    }

    fn show_stored(&mut self, dir: &Path, text: &mut TextStore) -> Result<(), String> {
        // Taken just before the read, so it goes with the session read: a
        // write that lands after it shows as a change at the next look.
        let stamp = store::log_stamp(dir);
        let session = self.load_stored(dir)?;
        #[cfg(test)]
        tests::after_stored_read();
        self.leave();
        if !matches!(self.source, Source::Demo) {
            self.reply_root = Some(store::reply_dir(&session));
            self.session_dir = Some(dir.to_owned());
        }
        let closed = session.closed;
        self.session = Some(Arc::new(session));
        self.read_only = true;
        self.stored_dir = Some(dir.to_owned());
        self.stored_free = !store::is_busy(dir);
        self.stored_stamp = stamp;
        self.busy_checked = Some(std::time::Instant::now());
        self.report_stale = !matches!(self.source, Source::Demo)
            && store::report_is_stale(dir, self.session.as_deref().expect("just set"));
        if closed {
            self.probe_panes(dir);
        }
        self.rebuild(text);
        Ok(())
    }

    /// Asks Noa how many panes of the finished stored verification in
    /// `dir` are still open (ペインをすべて閉じる is offered on the answer).
    fn probe_panes(&mut self, dir: &Path) {
        if let Source::Live(channel) = &self.source {
            channel.send(Command::ProbePanes {
                dir: dir.to_owned(),
            });
        }
    }

    fn rebuild_progress(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.progress_host, text);
        let Some(session) = &self.session else {
            return;
        };
        progress::insert(&mut self.tree, self.progress_host, session, system_now());
    }

    /// 証跡を追加 can be used: the verification has started round 1 and
    /// not closed, and a runner holds it here (or it is the demo).
    fn can_attach(&self) -> bool {
        !self.read_only
            && !matches!(self.source, Source::Snapshot)
            && self
                .session
                .as_ref()
                .is_some_and(|s| !s.closed && !s.rounds.is_empty())
    }

    fn rebuild_center(&mut self, text: &mut TextStore) {
        self.editor.harvest(&self.tree);
        clear_children(&mut self.tree, self.tabs_host, text);
        clear_children(&mut self.tree, self.center_host, text);
        let Some(session) = self.session.clone() else {
            return;
        };
        let tabs = self.tree.insert(
            Some(self.tabs_host),
            Container::new(Style {
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                min_size: LayoutSize {
                    width: length(0.0),
                    height: auto(),
                },
                flex_direction: FlexDirection::Row,
                flex_wrap: noa_gpui_core::style::FlexWrap::Wrap,
                align_items: noa_gpui_core::style::AlignItems::CENTER,
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
        let live_defects = session
            .defects
            .iter()
            .filter(|d| d.status.is_live())
            .count();
        let evidence_tab = if self.tab == CenterTab::Gallery {
            CenterTab::Gallery
        } else {
            CenterTab::Evidence
        };
        for (label, tab) in [
            (
                format!("基準 {}", session.criteria.len()),
                CenterTab::Criteria,
            ),
            (format!("証跡 {}", session.evidence.len()), evidence_tab),
            (format!("不具合 {live_defects}"), CenterTab::Defects),
            (
                if session.verdict().is_some() {
                    "レポート".to_owned()
                } else {
                    "レポート（暫定）".to_owned()
                },
                CenterTab::Report,
            ),
        ] {
            let on = self.tab == tab
                || (tab == evidence_tab
                    && matches!(self.tab, CenterTab::Evidence | CenterTab::Gallery));
            let btn = self.tree.insert(
                Some(tabs),
                noa_gpui_core::widgets::Button::new(label)
                    .control_size(noa_gpui_core::widgets::ControlSize::Small)
                    .selected(on),
            );
            self.actions.push((btn, Action::ShowTab(tab)));
        }
        spacer(&mut self.tree, tabs);
        if self.tab == CenterTab::Defects {
            defects::summary(
                &mut self.tree,
                tabs,
                &session,
                &self.defect_filters,
                &mut self.actions,
            );
        }
        let log_open = self.log_open.is_some();
        self.tree.set_hidden(self.center_host, log_open);
        self.tree.set_hidden(self.log.root, !log_open);
        if log_open {
            self.actions.push((self.log.close, Action::CloseLog));
            return;
        }
        let can_attach = self.can_attach();
        let selected_criterion = match &self.selected {
            Some(Selected::Criterion(id)) => Some(id.as_str()),
            _ => None,
        };
        let selected_defect = match &self.selected {
            Some(Selected::Defect(id)) => Some(id.as_str()),
            _ => None,
        };
        match self.tab {
            CenterTab::Criteria if session.rounds.is_empty() && session.criteria_accepted => {
                let editable = !self.read_only && !session.closed && session.frozen_at.is_none();
                criteria::insert(
                    &mut self.tree,
                    self.center_host,
                    &session,
                    &mut self.editor,
                    editable,
                    &mut self.actions,
                );
            }
            CenterTab::Criteria if session.rounds.is_empty() => {
                let list = scroll_column(&mut self.tree, self.center_host, 10.0);
                note(
                    &mut self.tree,
                    list,
                    if session.closed {
                        "受け入れ基準ができる前に終了しました"
                    } else {
                        "判定役が依頼・仕様・あなたの基準から受け入れ基準を作っています…"
                    },
                );
            }
            CenterTab::Criteria => matrix::insert(
                &mut self.tree,
                self.center_host,
                &session,
                selected_criterion,
                &mut self.actions,
            ),
            CenterTab::Evidence | CenterTab::Gallery => evidence::insert(
                &mut self.tree,
                self.center_host,
                &session,
                &self.evidence_filters,
                self.tab == CenterTab::Gallery,
                can_attach,
                &mut self.actions,
            ),
            CenterTab::Defects => defects::insert(
                &mut self.tree,
                self.center_host,
                &session,
                &self.defect_filters,
                selected_defect,
                &mut self.actions,
            ),
            CenterTab::Report => report::insert(&mut self.tree, self.center_host, &session),
        }
    }

    fn rebuild_right(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.right_host, text);
        let Some(session) = self.session.clone() else {
            return;
        };
        let offer = self.fix_offer(&session);
        let selection = self.shown_selection().cloned();
        let shown = match &selection {
            Some(Selected::Criterion(id)) => thread::Shown::Criterion(id),
            Some(Selected::Defect(id)) => thread::Shown::Defect(id),
            None => thread::Shown::Nothing,
        };
        let cx = thread::Context {
            offer: offer.as_ref(),
            can_attach: self.can_attach(),
        };
        thread::insert(
            &mut self.tree,
            self.right_host,
            &session,
            shown,
            &cx,
            &mut self.actions,
        );
    }

    fn attach_actions(&mut self) {
        if self.attach_current.is_some() {
            self.actions
                .push((self.attach.submit, Action::AttachSubmit));
            self.actions
                .push((self.attach.cancel, Action::AttachCancel));
        }
    }

    /// What 修正を依頼 offers for `session`: nothing in a headless stored
    /// view (no Noa to start an agent in), else the installed agents and
    /// the scopes that hold something, or why it cannot.
    fn fix_offer(&self, session: &Session) -> Option<fix::FixOffer> {
        if let Source::Snapshot = self.source {
            return None;
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
            Some(Selected::Criterion(id)) => vec![(FixScope::Criterion(id.clone()), 1)],
            Some(Selected::Defect(id)) => vec![(FixScope::Defect(id.clone()), 1)],
            None => [FixScope::FailedCriteria, FixScope::Defects]
                .into_iter()
                .map(|scope| {
                    let (criteria, defects) = self.fix_ids(session, &scope);
                    let n = criteria.len() + defects.len();
                    (scope, n)
                })
                .filter(|(_, n)| *n > 0)
                .collect(),
        };
        // Running here, or (a stored view) in another window whose runner
        // holds the log.
        let running = (!session.closed
            && (!self.read_only || (self.stored_dir.is_some() && !self.stored_free)))
            .then_some(session.isolation);
        Some(fix::FixOffer::Offered {
            kinds,
            scopes,
            running,
        })
    }

    /// The failed criteria and the defects a fix request of `scope` takes.
    fn fix_ids(&self, session: &Session, scope: &FixScope) -> (Vec<String>, Vec<String>) {
        match scope {
            FixScope::FailedCriteria => (
                session
                    .criteria
                    .iter()
                    .filter(|c| {
                        session
                            .ruling(&c.id)
                            .is_some_and(|r| r.kind == RulingKind::Failed)
                    })
                    .map(|c| c.id.clone())
                    .collect(),
                Vec::new(),
            ),
            FixScope::Defects => (
                Vec::new(),
                session
                    .ranked()
                    .into_iter()
                    .filter(|d| d.status.is_live())
                    .map(|d| d.id.clone())
                    .collect(),
            ),
            // The selected one, while it is shown.
            FixScope::Criterion(id) => (
                (self.shown_selection() == Some(&Selected::Criterion(id.clone())))
                    .then(|| id.clone())
                    .into_iter()
                    .collect(),
                Vec::new(),
            ),
            FixScope::Defect(id) => (
                Vec::new(),
                (self.shown_selection() == Some(&Selected::Defect(id.clone())))
                    .then(|| id.clone())
                    .into_iter()
                    .collect(),
            ),
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
        // Held, Return goes on (次のラウンドへ, or the criteria's 確定).
        let held = session.waits_to_continue()
            || session.phase() == verify::model::types::Phase::Confirming;
        composer::set_send_default(&mut self.tree, &mut self.composer, !held, text);
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
        // What was refused while the check ran may start now.
        self.refusal = None;
        let before = start::read(&self.tree, &self.start);
        let clean = start::Form::for_kinds(self.start.kinds());
        let pristine =
            !self.start.refilled && before.judge == clean.judge && before.panel == clean.panel;
        self.detection = start::Detection::Done(installed);
        let kinds = self.detection.menu_kinds();
        let form = if pristine {
            let clean = start::Form::for_kinds(&kinds);
            start::Form {
                judge: clean.judge,
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

    /// A stored verification on screen, when the window wakes (at most
    /// every [`BUSY_RECHECK`]): its log read again if it changed, and
    /// whether a runner holds it asked again.
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
        if stamp != self.stored_stamp
            && let Ok(session) = store::read_session(&dir)
        {
            let was_closed = self.session.as_ref().is_some_and(|s| s.closed);
            self.stored_stamp = stamp;
            self.report_stale = store::report_is_stale(&dir, &session);
            let closed = session.closed;
            self.session = Some(Arc::new(session));
            if closed && !was_closed {
                self.probe_panes(&dir);
            } else if !closed {
                self.stored_panes = None;
            }
            self.drop_hidden_selection();
            self.built = None;
        }
        let free = !store::is_busy(&dir);
        if free != self.stored_free {
            self.stored_free = free;
            if free && let Some(session) = &self.session {
                self.report_stale = store::report_is_stale(&dir, session);
            }
            self.built = None;
        }
    }

    fn poll_live(&mut self, text: &mut TextStore) {
        self.poll_installed(text);
        if matches!(self.source, Source::Live(_)) {
            self.recheck_stored();
        }
        let mut latest = None;
        let mut notices = Vec::new();
        let mut released = false;
        if let Source::Live(channel) = &self.source {
            let current = channel.generation();
            while let Ok(snapshot) = channel.snapshots.try_recv() {
                if snapshot.generation >= current {
                    latest = Some(snapshot);
                }
            }
            while let Ok((generation, notice)) = channel.notices.try_recv() {
                if generation >= current {
                    notices.push(notice);
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
            while let Ok(answer) = channel.panes.try_recv() {
                if answer.generation >= current && self.stored_dir.as_ref() == Some(&answer.dir) {
                    self.stored_panes = Some((answer.dir, answer.count));
                    self.built = None;
                }
            }
        }
        let refused_start = self.starting && self.session.is_none() && !notices.is_empty();
        for notice in &notices {
            self.toast(notice.clone());
        }
        if refused_start {
            self.starting = false;
            self.refuse(notices.last().expect("not empty").clone(), text);
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
                        self.stored_panes = None;
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
                self.starting = false;
            }
            // A new verification opens on its criteria.
            let new_id = match (&self.session, &snapshot.session) {
                (Some(before), Some(next)) => before.id != next.id,
                (None, Some(_)) => true,
                _ => false,
            };
            if new_id {
                self.tab = CenterTab::Criteria;
                self.editor.reset();
            }
            self.session = snapshot.session;
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

    /// The selection, while the pane that holds it shows it: a criterion
    /// that exists, a defect the defect filters let through.
    fn shown_selection(&self) -> Option<&Selected> {
        let session = self.session.as_ref()?;
        let selected = self.selected.as_ref()?;
        let shown = match selected {
            Selected::Criterion(id) => session.criterion(id).is_some(),
            Selected::Defect(id) => session
                .defect(id)
                .is_some_and(|d| defects::passes(session, &self.defect_filters, d)),
        };
        shown.then_some(selected)
    }

    fn drop_hidden_selection(&mut self) {
        if self.selected.is_some() && self.shown_selection().is_none() {
            self.selected = None;
        }
    }

    fn refilter(&mut self, text: &mut TextStore) {
        self.drop_hidden_selection();
        self.rebuild(text);
    }

    fn toast(&mut self, message: impl Into<String>) {
        Toast::new(message.into()).show(&mut self.tree);
    }

    /// Why the stored verification on screen cannot start its agents
    /// again (the check is pending, or one of them is not installed).
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

    fn open(&mut self, open: Open) {
        let path = match &open {
            Open::File(path) | Open::Reveal(path) => path.clone(),
        };
        if std::fs::symlink_metadata(&path).is_err() {
            self.toast(format!("{} が見つかりません", path.display()));
            return;
        }
        if let Err(e) = (self.opener)(&open) {
            self.toast(format!("開けませんでした: {e}"));
        }
    }

    fn open_snapshot(&mut self) {
        match &self.reply_root {
            Some(root) => self.open(Open::File(store::target_dir(root))),
            None => self.toast("デモ表示には実体ファイルがありません"),
        }
    }

    /// The stored copy of `key` and where it lives, or why there is none.
    fn stored_copy(&self, key: &EvidenceKey) -> Result<(Arc<Session>, PathBuf, Stored), String> {
        let session = self.session.clone().ok_or("検証が表示されていません")?;
        let item = session
            .evidence_item(key)
            .ok_or_else(|| format!("{key} という証跡はありません"))?;
        let stored = item.stored.clone().ok_or_else(|| {
            format!("{key} の保存されたコピーはありません（取り込み前に消えたか、まだ取り込まれていません）")
        })?;
        let root = self
            .reply_root
            .clone()
            .ok_or("デモ表示には実体ファイルがありません")?;
        Ok((session, root, stored))
    }

    /// 開く (L1-5a): a command's stored log in the window's text view; any
    /// other item's stored copy in the system viewer or player. The
    /// worktree's file is never opened: it may have changed or gone.
    fn open_evidence(&mut self, key: &EvidenceKey, text: &mut TextStore) {
        let (session, root, stored) = match self.stored_copy(key) {
            Ok(found) => found,
            Err(why) => return self.toast(why),
        };
        let item = session.evidence_item(key).expect("found above");
        if let EvidenceBody::Command { cmd, .. } = &item.body {
            let read = verify::evidence::read_stored(
                &store::reply_base(&session),
                &root,
                &stored,
                MAX_LOG_BYTES + 4096,
            );
            match read {
                Ok((bytes, _)) => {
                    let title = format!("{key} のログ — $ {cmd}");
                    evidence::show_log(
                        &mut self.tree,
                        &self.log,
                        title,
                        String::from_utf8_lossy(&bytes).into_owned(),
                    );
                    self.log_open = Some(key.clone());
                    self.rebuild(text);
                }
                Err(e) => self.toast(format!("{} を読めません: {e}", stored.path.display())),
            }
            return;
        }
        self.open(Open::File(root.join(&stored.path)));
    }

    fn reveal_evidence(&mut self, key: &EvidenceKey) {
        match self.stored_copy(key) {
            Ok((_, root, stored)) => self.open(Open::Reveal(root.join(&stored.path))),
            Err(why) => self.toast(why),
        }
    }

    fn close_log(&mut self) {
        self.log_open = None;
        self.tree.set_hidden(self.log.root, true);
        self.tree.set_hidden(self.center_host, false);
    }

    /// Files chosen or dropped for 証跡を追加: each waits its turn in the
    /// form.
    fn queue_attachments(&mut self, paths: Vec<PathBuf>, text: &mut TextStore) {
        if !self.can_attach() {
            self.toast("証跡は、検証が始まってから終わるまでのあいだ追加できます");
            return;
        }
        for source in paths {
            let media = source.to_str().and_then(verify::media::by_extension);
            self.attach_queue
                .push_back(evidence::AttachDraft { source, media });
        }
        if self.attach_current.is_none() {
            self.next_attachment();
        }
        self.rebuild(text);
    }

    fn next_attachment(&mut self) {
        self.attach_current = self.attach_queue.pop_front();
        match &self.attach_current {
            Some(draft) => {
                let shows = match &self.selected {
                    Some(Selected::Criterion(id)) => id.clone(),
                    _ => String::new(),
                };
                evidence::show_attach(
                    &mut self.tree,
                    &self.attach,
                    draft,
                    shows,
                    self.attach_queue.len(),
                );
            }
            None => evidence::hide_attach(&mut self.tree, &self.attach),
        }
    }

    fn submit_attachment(&mut self, text: &mut TextStore) {
        let (Some(draft), Some(session)) = (self.attach_current.clone(), self.session.clone())
        else {
            return;
        };
        match evidence::read_attach(&self.tree, &self.attach, &draft, &session) {
            Ok((caption, marks, shows)) => {
                self.send_cmd(Command::Attach(Box::new(Attachment {
                    source: draft.source,
                    caption,
                    marks,
                    shows,
                })));
                self.next_attachment();
                self.rebuild(text);
            }
            Err(why) => evidence::attach_error(&mut self.tree, &self.attach, why),
        }
    }

    fn save_report(&mut self, path: &Path) {
        let Some(session) = &self.session else {
            return;
        };
        match std::fs::write(path, store::report_text(session)) {
            Ok(()) => self.toast(format!("保存しました: {}", path.display())),
            Err(e) => self.toast(format!("保存できませんでした: {e}")),
        }
    }

    /// 証跡ごと書き出す (L1-8): the report with links relative to `dest`,
    /// and every stored copy (re-run logs too) at its path relative to the
    /// reply dir.
    fn export_bundle(&mut self, dest: &Path) {
        let (Some(session), Some(root)) = (self.session.clone(), self.reply_root.clone()) else {
            return self.toast("デモ表示には実体ファイルがありません");
        };
        let stored: Vec<&Stored> = session
            .evidence
            .iter()
            .flat_map(|e| e.stored.iter().chain(e.rerun.as_ref().and_then(rerun_log)))
            .collect();
        let report = export::report_markdown(&session, None);
        match verify::evidence::export_bundle(
            &store::reply_base(&session),
            &root,
            dest,
            &report,
            &stored,
        ) {
            Ok(()) => self.toast(format!("証跡ごと書き出しました: {}", dest.display())),
            Err(why) => self.toast(format!("書き出せませんでした: {why}")),
        }
    }

    fn ask(&mut self, purpose: Purpose, request: FileDialogRequest) {
        self.dialog_purposes.push_back(purpose);
        self.file_dialogs.push(request);
    }

    fn send_edit(&mut self, edit: verify::model::types::CriteriaEdit) {
        self.send_cmd(Command::EditCriteria { edit });
    }

    fn handle_action(&mut self, action: Action, text: &mut TextStore) {
        match action {
            Action::Start => self.start_verification(text),
            Action::AddPanelRow => {
                start::add_row(&mut self.tree, &mut self.start);
                self.rebuild(text);
            }
            Action::RemovePanelRow(index) => {
                start::remove_row(&mut self.tree, &mut self.start, index, text);
                self.rebuild(text);
            }
            Action::PickSpec => self.ask(
                Purpose::Spec,
                FileDialogRequest {
                    save: false,
                    title: "仕様ファイルを選択".into(),
                    file_name: String::new(),
                    filters: Vec::new(),
                },
            ),
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
            Action::VerifyAgain(dir) => self.verify_again(dir, text),
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
            Action::ShowTab(tab) => {
                self.tab = tab;
                self.close_log();
                self.rebuild(text);
            }
            Action::SelectCriterion(id) => {
                self.selected = id.map(Selected::Criterion);
                self.rebuild(text);
            }
            Action::SelectDefect(id) => {
                self.selected = id.map(Selected::Defect);
                self.rebuild(text);
            }
            Action::ToggleSeverity(s) => {
                toggle(&mut self.defect_filters.severities, s);
                self.refilter(text);
            }
            Action::ToggleCategory(id) => {
                toggle(&mut self.defect_filters.categories, id);
                self.refilter(text);
            }
            Action::ToggleStatus(s) => {
                toggle(&mut self.defect_filters.statuses, defects::status_key(s));
                self.refilter(text);
            }
            Action::ToggleAgreement(a) => {
                toggle(
                    &mut self.defect_filters.agreements,
                    defects::agreement_key(a),
                );
                self.refilter(text);
            }
            Action::FilterReporter(part) => {
                self.defect_filters.reporter = part;
                self.tab = CenterTab::Defects;
                self.refilter(text);
            }
            Action::ClearDefectFilters => {
                self.defect_filters = defects::Filters {
                    statuses: BTreeSet::new(),
                    ..defects::Filters::default()
                };
                self.refilter(text);
            }
            Action::FilterEvidence(filter) => {
                self.evidence_filters.set(filter);
                self.rebuild(text);
            }
            Action::ClearEvidenceFilters => {
                self.evidence_filters = evidence::Filters::default();
                self.rebuild(text);
            }
            Action::OpenEvidence(key) => self.open_evidence(&key, text),
            Action::RevealEvidence(key) => self.reveal_evidence(&key),
            Action::CloseLog => {
                self.close_log();
                self.rebuild(text);
            }
            Action::AttachEvidence => {
                if !self.can_attach() {
                    self.toast("証跡は、検証が始まってから終わるまでのあいだ追加できます");
                    return;
                }
                self.ask(
                    Purpose::Attach,
                    FileDialogRequest {
                        save: false,
                        title: "証跡を追加（スクリーンショット・画面収録）".into(),
                        file_name: String::new(),
                        filters: vec![(
                            "画像・動画".into(),
                            ["png", "jpg", "jpeg", "gif", "webp", "mp4", "mov", "webm"]
                                .iter()
                                .map(|e| (*e).to_owned())
                                .collect(),
                        )],
                    },
                );
            }
            Action::AttachSubmit => self.submit_attachment(text),
            Action::AttachCancel => {
                self.next_attachment();
                self.rebuild(text);
            }
            Action::SaveCriterion(id) => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                match self.editor.edit(&self.tree, &session, &id) {
                    Ok(edit) => self.send_edit(edit),
                    Err(why) => self.toast(why),
                }
            }
            Action::RemoveCriterion(id) => {
                let removable = self
                    .session
                    .as_ref()
                    .and_then(|s| s.criterion(&id))
                    .is_some_and(criteria::removable);
                if removable {
                    self.send_edit(verify::model::types::CriteriaEdit::Remove { id });
                } else {
                    self.toast("あなたの基準と仕様の基準は削除できません（文言は直せます）");
                }
            }
            Action::MoveCriterion(id, to) => {
                self.send_edit(verify::model::types::CriteriaEdit::Move { id, to })
            }
            Action::AddCriterion => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                match self.editor.addition(&self.tree, &session) {
                    Ok(edit) => {
                        self.send_edit(edit);
                        self.editor.clear_addition(&mut self.tree);
                    }
                    Err(why) => self.toast(why),
                }
            }
            Action::StartVerify => self.send_cmd(Command::StartVerify),
            Action::OpenSnapshot => self.open_snapshot(),
            Action::OpenWorktree(part) => {
                let path = self
                    .session
                    .as_ref()
                    .and_then(|s| s.worktrees.get(&part))
                    .filter(|w| w.state == WorktreeState::Made)
                    .map(|w| w.path.clone());
                match path {
                    Some(path) => self.open(Open::File(path)),
                    None => self.toast(format!("{part} のワークツリーはありません")),
                }
            }
            Action::CopyReport => {
                if let Some(session) = &self.session {
                    let md = store::report_text(session);
                    // The clipboard reports no failure: what it holds
                    // afterwards tells whether the copy took.
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
            Action::SaveReport => {
                if let Some(session) = &self.session {
                    let name = format!("{}.md", session.id);
                    self.ask(
                        Purpose::SaveReport,
                        FileDialogRequest {
                            save: true,
                            title: "検証レポートを保存（レポートのみ）".into(),
                            file_name: name,
                            filters: vec![("Markdown".into(), vec!["md".into()])],
                        },
                    );
                }
            }
            Action::ExportBundle => {
                if self.reply_root.is_none() {
                    return self.toast("デモ表示には実体ファイルがありません");
                }
                if let Some(session) = &self.session {
                    let name = format!("{}-report", session.id);
                    self.ask(
                        Purpose::ExportBundle,
                        FileDialogRequest {
                            save: true,
                            title: "証跡ごと書き出す（新しいフォルダ名）".into(),
                            file_name: name,
                            filters: Vec::new(),
                        },
                    );
                }
            }
            Action::NewVerification => {
                self.leave();
                self.refusal = None;
                let form = start::Form::for_kinds(self.start.kinds());
                start::fill(&mut self.tree, &mut self.start, &form, text);
                self.start.refilled = false;
                self.rebuild(text);
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
            // Ending before the last round cannot be undone, so it is asked
            // first; Return and Escape keep it running.
            Action::Close => {
                self.close_alert = Some(
                    Alert::new("この検証を終了しますか？")
                        .informative_text(
                            "総合判定なしで終わります。終了した検証は再開もラウンドの追加もできません。",
                        )
                        .style(AlertStyle::Critical)
                        .button("キャンセル")
                        .button("検証を終了")
                        .cancel_button(0)
                        .open(&mut self.tree),
                );
            }
            Action::ClosePanes => {
                self.panes_alert = Some(
                    Alert::new("エージェントのペインをすべて閉じますか？")
                        .informative_text(
                            "判定役と検証者のペインを Noa でまとめて閉じます（修正依頼のタブは残します）。閉じたペインは元に戻せません。",
                        )
                        .style(AlertStyle::Warning)
                        .button("キャンセル")
                        .button("ペインをすべて閉じる")
                        .cancel_button(0)
                        .open(&mut self.tree),
                );
            }
            Action::FocusPane(part) => self.send_cmd(Command::Focus { part }),
            Action::SetHold(hold) => self.send_cmd(Command::SetHold { hold }),
            Action::Continue => self.send_cmd(Command::Continue),
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
            Action::RemoveWorktrees => {
                if let Some(dir) = self.stored_dir.clone() {
                    self.send_cmd(Command::RemoveWorktrees { dir });
                }
            }
            Action::RequestFix { agent, scope } => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                let (criteria, defects) = self.fix_ids(&session, &scope);
                if criteria.is_empty() && defects.is_empty() {
                    self.toast("修正を依頼する基準・不具合が表示されていません");
                    return;
                }
                match (&self.source, self.session_dir.clone()) {
                    (Source::Live(_), Some(dir)) => {
                        self.send_cmd(Command::RequestFix(Box::new(FixRequest {
                            session,
                            dir,
                            agent,
                            criteria,
                            defects,
                        })));
                        self.toast(format!("{} に修正を依頼しています…", agent.command()));
                    }
                    (Source::Live(_), None) => {
                        self.toast("この検証の保存先が分からないため、修正を依頼できません");
                    }
                    (Source::Demo | Source::Snapshot, _) => {
                        self.toast("デモ表示では操作を送信できません");
                    }
                }
            }
        }
    }

    /// Shows why a start was refused under the form, until the next one.
    fn refuse(&mut self, why: String, text: &mut TextStore) {
        start::show_refusal(&mut self.tree, &self.start, &why, text);
        self.refusal = Some(why);
    }

    fn start_verification(&mut self, text: &mut TextStore) {
        if !self.detection.startable() {
            let why = self.detection.blocked_reason();
            return self.refuse(why, text);
        }
        let form = start::read(&self.tree, &self.start);
        let target = match form.target() {
            Ok(target) => target,
            Err(why) => return self.refuse(why, text),
        };
        if let Err(why) = verify::engine::check_panel(&form.judge, &form.panel) {
            return self.refuse(why, text);
        }
        self.refusal = None;
        self.starting = true;
        start::rebuild_note(
            &mut self.tree,
            &self.start,
            &Link::Connecting,
            &self.detection,
            text,
        );
        self.send_cmd(Command::Start {
            request: form.request.clone(),
            target: Box::new(target),
            isolation: form.isolation,
            user_criteria: form.user_criteria(),
            confirm: form.confirm,
            max_rounds: form.max_rounds,
            round_cap: form.round_cap,
            judge: form.judge,
            panel: form.panel,
        });
    }

    fn verify_again(&mut self, dir: Option<PathBuf>, text: &mut TextStore) {
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
        let form = start::Form::from_session(&session);
        if self.session.is_some() {
            self.leave();
        }
        self.refusal = None;
        let left_out = start::fill(&mut self.tree, &mut self.start, &form, text);
        // Set by the person: the agent check that reports later only drops
        // kinds that are not installed.
        self.start.refilled = true;
        if !left_out.is_empty() {
            self.toast(start::left_out_message(&left_out));
        }
        self.toast(format!(
            "{} と同じ設定を入れました。確かめてから「検証を開始」を押してください",
            session.id
        ));
        self.rebuild(text);
        start::show_top(&mut self.tree, &self.start);
    }
}

/// The stored copy of a re-run's log, when one was kept.
fn rerun_log(rerun: &Rerun) -> Option<&Stored> {
    match rerun {
        Rerun::Matched { log, .. }
        | Rerun::Differed { log, .. }
        | Rerun::TimedOut { log }
        | Rerun::NotRun { log, .. } => log.as_ref(),
    }
}

impl Screen for Verify {
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

    /// Files dropped anywhere on the board go to 証跡を追加 (L1-5a); every
    /// other event goes to the tree as usual.
    fn dispatch(&mut self, event: UiEvent, text: &mut TextStore) {
        if let UiEvent::Drop {
            data: DragData::Files(files),
            ..
        } = &event
            && self.session.is_some()
        {
            let files = files.clone();
            self.queue_attachments(files, text);
            return;
        }
        self.tree.dispatch(event, text);
        for message in self.tree.take_messages() {
            self.handle(message, text);
        }
        self.after_input();
    }

    fn handle(&mut self, message: UiMessage, text: &mut TextStore) {
        if let Some(answer) = message.downcast_ref::<AlertResponse>() {
            if self.close_alert == Some(message.source) {
                self.close_alert = None;
                if answer.button == 1 {
                    self.send_cmd(Command::Close);
                }
                return;
            }
            if self.panes_alert == Some(message.source) {
                self.panes_alert = None;
                if answer.button == 1 {
                    match (&self.stored_dir, self.read_only) {
                        (Some(dir), true) => {
                            let dir = dir.clone();
                            self.send_cmd(Command::ClosePanesStored { dir });
                            // Asked again only when it is shown again: the
                            // close's outcome comes as a notice.
                            self.stored_panes = None;
                            self.built = None;
                            self.rebuild(text);
                        }
                        _ => self.send_cmd(Command::ClosePanes),
                    }
                }
                return;
            }
        }
        if let Some(TextChanged(query)) = message.downcast_ref::<TextChanged>()
            && message.source == self.log.search
        {
            let query = query.clone();
            evidence::search_log(&mut self.tree, &self.log, &query);
            return;
        }
        if message.downcast_ref::<SelectionChanged>().is_some() {
            if message.source == self.start.judge {
                start::judge_kind_changed(&mut self.tree, &mut self.start, text);
                return;
            }
            if let Some(id) = self
                .editor
                .priority_source(message.source)
                .map(str::to_owned)
            {
                let priority = self.editor.priority(&self.tree, &id);
                let changed = self
                    .session
                    .as_ref()
                    .and_then(|s| s.criterion(&id))
                    .is_some_and(|c| Some(c.priority) != priority);
                if changed {
                    self.send_edit(verify::model::types::CriteriaEdit::Edit {
                        id,
                        statement: None,
                        how: None,
                        priority,
                    });
                }
                return;
            }
            if start::target_kind_changed(&mut self.tree, &self.start, message.source)
                || start::row_changed(&mut self.tree, &mut self.start, message.source, text)
            {
                return;
            }
        }
        if message
            .downcast_ref::<noa_gpui_core::widgets::ButtonClicked>()
            .is_some()
            && let Some((_, action)) = self
                .actions
                .iter()
                .find(|(id, _)| *id == message.source)
                .cloned()
        {
            let had_focus = self.tree.focused() == Some(message.source);
            let again = pressed_again(&action, self.selected.as_ref());
            self.handle_action(action, text);
            // A toggle pressed from the keyboard now carries another
            // action (the rebuild could not find it): focus the widget
            // that does.
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
        let purpose = self.dialog_purposes.pop_front();
        match (purpose, result) {
            (Some(Purpose::Spec), FileDialogResult::Opened(paths)) => {
                if let Some(path) = paths.first() {
                    start::set_spec_file(&mut self.tree, &self.start, path);
                }
            }
            (Some(Purpose::Attach), FileDialogResult::Opened(paths)) => {
                self.queue_attachments(paths, text)
            }
            (Some(Purpose::SaveReport), FileDialogResult::Saved(path)) => self.save_report(&path),
            (Some(Purpose::ExportBundle), FileDialogResult::Saved(path)) => {
                self.export_bundle(&path)
            }
            _ => {}
        }
    }
}

/// The action the widget pressed with `action` carries once the screen is
/// rebuilt: a row that selects carries the clearing action, and the
/// clearing one the action for what was selected before.
fn pressed_again(action: &Action, selected: Option<&Selected>) -> Option<Action> {
    match action {
        Action::SelectCriterion(Some(_)) => Some(Action::SelectCriterion(None)),
        Action::SelectDefect(Some(_)) => Some(Action::SelectDefect(None)),
        Action::SelectCriterion(None) => match selected {
            Some(Selected::Criterion(id)) => Some(Action::SelectCriterion(Some(id.clone()))),
            _ => None,
        },
        Action::SelectDefect(None) => match selected {
            Some(Selected::Defect(id)) => Some(Action::SelectDefect(Some(id.clone()))),
            _ => None,
        },
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
    fn arrived_as(&self, message: &verify::model::types::Message) -> bool {
        message.author == verify::model::types::Author::User
            && message.kind == verify::model::types::MsgKind::Intervention
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

pub(super) fn status_tone(status: DefectStatus) -> Tone {
    match status {
        DefectStatus::Agreed => Tone::Success,
        DefectStatus::Disputed => Tone::Warning,
        DefectStatus::Open => Tone::Accent,
        DefectStatus::Rejected | DefectStatus::Withdrawn | DefectStatus::Merged => Tone::Neutral,
    }
}

pub(super) fn decision_tone(decision: Decision) -> Tone {
    match decision {
        Decision::Pass => Tone::Success,
        Decision::PassWithIssues => Tone::Accent,
        Decision::Fail => Tone::Critical,
        Decision::Inconclusive => Tone::Warning,
    }
}

pub(super) fn ruling_tone(kind: RulingKind) -> Tone {
    match kind {
        RulingKind::Verified => Tone::Success,
        RulingKind::Failed => Tone::Critical,
        RulingKind::Inconclusive => Tone::Warning,
        RulingKind::NotApplicable => Tone::Neutral,
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
