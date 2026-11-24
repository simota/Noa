//! The review window: reviewer cards | findings board, matrix or report |
//! finding thread, with a progress strip and a header above the center pane,
//! an intervention composer under it, and a start view before any session
//! exists.
//!
//! The screen holds no review state of its own: it draws the [`Session`] of
//! the latest [`Snapshot`] and sends [`Command`]s. What it keeps is view
//! state: the tab, the filters, the selected finding, the composer text.

mod board;
mod cards;
mod composer;
mod matrix;
mod progress;
mod report;
mod start;
mod thread;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command as OsCommand;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use noa_gpui_core::style::{
    AlignItems, FlexDirection, LengthPercentage, Overflow, Point as LayoutPoint,
    Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Button, ButtonClicked, Container, ControlSize, Label, ScrollView, SelectionChanged, SplitPane,
    SplitView, StatusBar, Tag, Toast, Tone,
};
use noa_gpui_core::{Message as UiMessage, Role, Style, Theme, UiTree, WidgetId};
use noa_gpui_platform::{FileDialogRequest, FileDialogResult, Screen};
use noa_gpui_text::TextStore;

use review::model::export;
use review::model::types::{
    Agreement, Decision, FindingStatus, Millis, PartId, Phase, RoundNo, Session, Severity,
};
use review::store::{self, Listed};

use crate::installed::Installed;
use crate::live::{Channel, Command, Link, Snapshot};

pub(crate) use start::Form as StartForm;

const LEFT_WIDTH: f32 = 280.0;
const RIGHT_WIDTH: f32 = 340.0;
const CENTER_MIN: f32 = 520.0;
pub(crate) const MIN_WINDOW: (f64, f64) = (
    (LEFT_WIDTH + CENTER_MIN + RIGHT_WIDTH + 2.0 * noa_gpui_core::widgets::DIVIDER) as f64,
    600.0,
);

/// The rounds the "+N" buttons add.
const EXTEND_BY: [RoundNo; 2] = [1, 3];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Start,
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
    ClearFilters,
    /// A matrix cell: the board filtered to one lens (and severity).
    ShowCell(String, Option<Severity>),
    SelectFinding(Option<String>),
    /// Opens a snapshot file (or the snapshot folder) in the OS.
    OpenSnapshot(Option<String>),
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
    /// Takes up the unfinished stored review on screen (its agents start).
    ResumeShown,
    /// Writes the stored review's report again from its log.
    RegenerateReport,
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
    tab: CenterTab,
    filters: Filters,
    selected: Option<String>,
    pending_intervention: Option<String>,
    file_dialogs: Vec<FileDialogRequest>,
    /// The session on screen was read from its log: no runner drives it.
    read_only: bool,
    past: Vec<Listed>,
    past_root: Option<PathBuf>,
    /// The log of the read-only review on screen, when it is one.
    stored_dir: Option<PathBuf>,
    /// That log is free to write: no runner holds it.
    stored_free: bool,
    /// Rounds or a resume were asked for on the read-only review on
    /// screen: it stays shown until the runner's first snapshot of it
    /// arrives, then goes live.
    extending: bool,
    /// The stored report differs from what the log gives (missing, or the
    /// review stopped between writing its log and its report).
    report_stale: bool,
    /// A start was asked for: a refusal shows under the form.
    starting: bool,
    built: Option<BuiltKey>,
    /// Which agents are installed: unknown until the background check
    /// reports (always "all" in the demo and in stored views, which stay
    /// deterministic and run no shell).
    detection: start::Detection,
    /// Where that check reports, until it has.
    installed: Option<Receiver<Installed>>,

    start: start::Parts,
    board: WidgetId,
    left_host: WidgetId,
    header_host: WidgetId,
    progress_host: WidgetId,
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
            tab: CenterTab::Findings,
            filters: Filters::default(),
            selected: None,
            pending_intervention: None,
            file_dialogs: Vec::new(),
            read_only: false,
            past: Vec::new(),
            past_root: store::default_root(),
            stored_dir: None,
            stored_free: false,
            extending: false,
            report_stale: false,
            starting: false,
            built: None,
            detection: start::Detection::all_installed(),
            installed: None,
            start,
            board,
            left_host,
            header_host,
            progress_host,
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
            start::actions(&self.start, &mut self.actions);
            start::rebuild_past(
                &mut self.tree,
                &self.start,
                &self.past,
                system_now(),
                &mut self.actions,
                text,
            );
        }
        self.rebuild_status(text);
    }

    fn refresh_past(&mut self) {
        self.past = match &self.source {
            Source::Demo => crate::demo::past(),
            Source::Live(_) | Source::Snapshot => self
                .past_root
                .as_deref()
                .map(|root| store::list_sessions(root, start::PAST_LIMIT))
                .unwrap_or_default(),
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
        if let Source::Live(channel) = &self.source {
            channel.leave();
            if !self.read_only && self.session.as_ref().is_some_and(|s| !s.closed) {
                self.toast("レビューを中断しました（過去のレビューから再開できます）");
            }
        }
        self.session = None;
        self.reply_root = None;
        self.session_dir = None;
        self.read_only = false;
        self.stored_dir = None;
        self.extending = false;
        self.tab = CenterTab::Findings;
        self.filters = Filters::default();
        self.selected = None;
        self.pending_intervention = None;
        composer::reset(&mut self.tree, &mut self.composer);
        self.refresh_past();
    }

    fn show_stored(&mut self, dir: &Path, text: &mut TextStore) -> Result<(), String> {
        let session = self.load_stored(dir)?;
        self.leave();
        if !matches!(self.source, Source::Demo) {
            self.reply_root = Some(store::reply_dir(dir, &session));
            self.session_dir = Some(dir.to_owned());
        }
        self.session = Some(Arc::new(session));
        self.read_only = true;
        self.stored_dir = Some(dir.to_owned());
        self.stored_free = !store::is_busy(dir);
        self.report_stale = !matches!(self.source, Source::Demo)
            && report_stale(dir, self.session.as_deref().expect("just set"));
        self.rebuild(text);
        Ok(())
    }

    fn rebuild_left(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.left_host, text);
        let Some(session) = &self.session else {
            return;
        };
        cards::insert(
            &mut self.tree,
            self.left_host,
            session,
            self.filters.reviewer.as_ref(),
            !self.read_only,
            system_now(),
            &mut self.actions,
        );
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
        clear_children(&mut self.tree, self.center_host, text);
        let Some(session) = self.session.clone() else {
            return;
        };
        let tabs = self.tree.insert(
            Some(self.center_host),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
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
                    .default_button(self.tab == tab),
            );
            self.actions.push((btn, Action::ShowTab(tab)));
        }
        spacer(&mut self.tree, tabs);
        if self.reply_root.is_some() {
            let open = self.tree.insert(
                Some(tabs),
                Button::new("スナップショットを開く").control_size(ControlSize::Small),
            );
            self.actions.push((open, Action::OpenSnapshot(None)));
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
        thread::insert(
            &mut self.tree,
            self.right_host,
            session,
            self.selected.as_deref(),
            self.reply_root.is_some(),
            &mut self.actions,
        );
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
        if let Some(pending) = &self.pending_intervention
            && session
                .messages
                .iter()
                .rev()
                .any(|m| m.body.trim() == pending.trim())
        {
            self.pending_intervention = None;
            composer::clear(&mut self.tree, &self.composer);
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
        let pristine = before.lead == clean.lead && before.panel == clean.panel;
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

    fn poll_live(&mut self, text: &mut TextStore) {
        self.poll_installed(text);
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
        }
        let refused_start = self.starting && self.session.is_none() && !notices.is_empty();
        for notice in &notices {
            self.toast(notice.clone());
        }
        if refused_start {
            self.starting = false;
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
                self.starting = false;
            }
            self.session = snapshot.session;
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
                self.starting = true;
                start::rebuild_note(
                    &mut self.tree,
                    &self.start,
                    &Link::Connecting,
                    &self.detection,
                    text,
                );
                self.send_cmd(Command::Start {
                    request: form.request,
                    target: Box::new(target),
                    max_rounds: form.max_rounds,
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
                self.rebuild(text);
            }
            Action::ToggleLens(id) => {
                toggle(&mut self.filters.lenses, id);
                self.rebuild(text);
            }
            Action::ToggleStatus(s) => {
                toggle(&mut self.filters.statuses, status_key(s));
                self.rebuild(text);
            }
            Action::ToggleAgreement(a) => {
                toggle(&mut self.filters.agreements, agreement_key(a));
                self.rebuild(text);
            }
            Action::FilterReviewer(part) => {
                self.filters.reviewer = part;
                self.tab = CenterTab::Findings;
                self.rebuild(text);
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
                self.rebuild(text);
            }
            Action::SelectFinding(id) => {
                self.selected = id;
                self.rebuild(text);
            }
            Action::OpenSnapshot(path) => self.open_snapshot(path),
            Action::CopyReport => {
                if let Some(session) = &self.session {
                    let md = export::report_markdown(session);
                    self.tree.clipboard().write_text(&md);
                    self.toast("レポートをクリップボードにコピーしました");
                }
            }
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
                self.pending_intervention = Some(body.clone());
                self.send_cmd(Command::Intervene { to, body });
            }
            Action::Retry => self.send_cmd(Command::Retry),
            Action::Close => self.send_cmd(Command::Close),
            Action::NewReview => {
                self.leave();
                let form = start::Form::for_kinds(self.start.kinds());
                start::fill(&mut self.tree, &mut self.start, &form, text);
                self.rebuild(text);
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
                match regenerate_report(&dir) {
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

/// Whether `dir`'s stored report is not what its log gives. A review not
/// yet consolidated once has no report to compare.
fn report_stale(dir: &Path, session: &Session) -> bool {
    let has_report = session.closed || session.rounds.iter().any(|r| r.outcome.is_some());
    has_report
        && std::fs::read_to_string(store::report_path(dir)).ok()
            != Some(export::report_markdown(session))
}

/// Writes `dir`'s report from its log, holding the log's lock while it
/// does (a runner that took it up meanwhile is not written under).
fn regenerate_report(dir: &Path) -> Result<(), String> {
    let (store, session) = store::Store::open(dir).map_err(|e| e.to_string())?;
    store::write_report(dir, &export::report_markdown(&session)).map_err(|e| e.to_string())?;
    drop(store);
    Ok(())
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
        if message.downcast_ref::<SelectionChanged>().is_some() {
            if message.source == self.start.lead {
                start::lead_kind_changed(&mut self.tree, &mut self.start, text);
                return;
            }
            if start::target_kind_changed(&mut self.tree, &self.start, message.source)
                || start::row_changed(&mut self.tree, &mut self.start, message.source, text)
            {
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
            self.handle_action(action, text);
        }
    }

    fn take_file_dialogs(&mut self) -> Vec<FileDialogRequest> {
        std::mem::take(&mut self.file_dialogs)
    }

    fn file_dialog_finished(&mut self, result: FileDialogResult, _text: &mut TextStore) {
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
