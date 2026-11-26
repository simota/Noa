//! The competition window: leaderboard | compare / timeline | judge board,
//! with an intervention composer and a start view before any session exists.

mod auto;
mod compare;
mod composer;
mod judge;
mod leaderboard;
mod map;
mod progress;
mod start;
mod timeline;

use std::path::{Path, PathBuf};
use std::process::Command as OsCommand;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use noa_gpui_core::style::{
    AlignItems, FlexDirection, LengthPercentage, Overflow, Point as LayoutPoint,
    Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Button, ButtonClicked, Container, ControlSize, Label, MenuChosen, ScrollView, SelectionChanged,
    SplitPane, SplitView, StatusBar, Tag, TextChanged, Toast, Tone,
};
use noa_gpui_core::{Message as UiMessage, Role, Style, Theme, UiTree, WidgetId};
use noa_gpui_platform::{FileDialogRequest, FileDialogResult, Screen};
#[cfg(test)]
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

use competition::model::export;
use competition::model::types::{
    AgentChoice, AgentKind, AgentSetup, MAX_PANEL, Millis, PartId, Phase, RoundKind, RoundNo,
    Session,
};
use competition::store;

use crate::installed::Installed;
use crate::live::{Channel, Command, Link, Snapshot};
use competition::store::Listed;

const LEFT_WIDTH: f32 = 280.0;
const RIGHT_WIDTH: f32 = 310.0;
const CENTER_MIN: f32 = 520.0;
pub(crate) const MIN_WINDOW: (f64, f64) = (
    (LEFT_WIDTH + CENTER_MIN + RIGHT_WIDTH + 2.0 * noa_gpui_core::widgets::DIVIDER) as f64,
    600.0,
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Start,
    /// Plans the agent rows with the pull-down's `n`th planner.
    AutoAssign(usize),
    /// Stops that planning (中止).
    CancelAutoAssign,
    ShowCompare,
    ShowTimeline,
    ShowMap,
    SelectRound(RoundNo),
    FilterPart(PartId),
    ClearPartFilter,
    OpenViewer,
    OpenArtifact {
        round: RoundNo,
        part: PartId,
        rel_path: String,
    },
    SelectWinner(PartId),
    CopyResult,
    ExportReport,
    Adopt {
        winner: PartId,
        agent: AgentKind,
    },
    Send,
    Retry,
    Close,
    AddPanelRow,
    RemovePanelRow(usize),
    /// Adds a judge row to the start form (up to three judges).
    AddJudgeRow,
    /// Removes the start form's co-judge row (0 is `judge-2`).
    RemoveJudgeRow(usize),
    /// Shows a stored competition read-only (no runner, no agents).
    OpenPast(PathBuf),
    /// Continues a stored, unfinished competition.
    ResumePast(PathBuf),
    /// Fills the start form with a stored competition's settings.
    RematchPast(PathBuf),
    /// Leaves the competition on screen for a clean start view.
    NewCompetition,
    /// Leaves the competition on screen for the start view, filled with
    /// its settings.
    Rematch,
    /// Brings a participant's (or the judge's) pane to the front in Noa.
    FocusPane(PartId),
    /// Holds the competition after each judged round, or lets it go on.
    SetHold(bool),
    /// Starts the next round of the held competition.
    Continue,
    /// Gives the competition on screen this many more rounds (a stored one
    /// is resumed to go on).
    Extend(RoundNo),
    /// Gives the stored competition in the directory more rounds and goes
    /// on with it.
    ExtendPast(PathBuf, RoundNo),
    /// Closes every judge's and contestant's pane of the ended competition
    /// in Noa.
    ClosePanes,
}

/// The rounds the "+N" buttons add.
const EXTEND_BY: [RoundNo; 2] = [1, 3];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CenterTab {
    Compare,
    Timeline,
    Map,
}

enum Source {
    Demo,
    Snapshot,
    Live(Channel),
}

pub struct Competition {
    tree: UiTree,
    source: Source,
    link: Link,
    session: Option<Arc<Session>>,
    artifact_root: Option<PathBuf>,
    tab: CenterTab,
    selected_round: Option<RoundNo>,
    filter_part: Option<PartId>,
    pending_intervention: Option<String>,
    file_dialogs: Vec<FileDialogRequest>,
    /// The session on screen was read from its log: no runner drives it,
    /// so nothing is sent for it.
    read_only: bool,
    /// The past competitions the start view lists.
    past: Vec<Listed>,
    /// Where they are read from: the store root.
    past_root: Option<PathBuf>,
    /// The log of the read-only competition on screen, when it is one.
    stored_dir: Option<PathBuf>,
    /// That log is free to write: no runner holds it, and no widget from
    /// before the lock may (read when it was opened).
    stored_free: bool,
    /// Rounds were asked for on the read-only competition on screen: it
    /// stays shown until the runner's first snapshot of it arrives.
    extending: bool,
    /// Which agents are installed: unknown until the background check
    /// reports (always "all" in the demo and in stored views, which stay
    /// deterministic and run no shell).
    detection: start::Detection,
    /// Where that check reports, until it has.
    installed: Option<Receiver<Installed>>,
    /// What the board was last built from: the session revision, the link,
    /// and the clock minute while some agent's elapsed time is running.
    built: Option<BuiltKey>,

    /// The start form's 「自動で割り当て」.
    auto: auto::Auto,
    start: start::Parts,
    board: WidgetId,
    left_host: WidgetId,
    progress_host: WidgetId,
    center_host: WidgetId,
    right_host: WidgetId,
    composer: composer::Parts,
    status_leading: WidgetId,
    status_trailing: WidgetId,

    actions: Vec<(WidgetId, Action)>,
}

impl Competition {
    pub fn demo() -> Self {
        Self::build(
            Source::Demo,
            Link::Live,
            Some(Arc::new(crate::demo::session())),
            None,
        )
    }

    /// The live screen. `installed` delivers which agents the login shell
    /// has; until it does, the start button waits.
    pub fn live(channel: Channel, installed: Receiver<Installed>) -> Self {
        let mut screen = Self::build(Source::Live(channel), Link::Connecting, None, None);
        screen.installed = Some(installed);
        screen.detection = start::Detection::Pending;
        let mut text = TextStore::new(Default::default());
        screen.rebuild(&mut text);
        screen
    }

    pub fn of_snapshot(snapshot: Snapshot) -> Self {
        Self::build(
            Source::Snapshot,
            snapshot.link,
            snapshot.session,
            snapshot.artifact_root,
        )
    }

    /// The demo's start view: a panel with two claude entries above canned
    /// past competitions.
    pub fn demo_start() -> Self {
        let mut screen = Self::build(Source::Demo, Link::Live, None, None);
        let mut text = TextStore::new(Default::default());
        let form = start::Form {
            request: crate::demo::session().request,
            judge: AgentChoice {
                kind: AgentKind::Claude,
                setup: AgentSetup {
                    model: Some("opus".into()),
                    effort: None,
                },
            },
            panel: crate::demo::start_panel(),
            ..start::Form::default()
        };
        start::fill(&mut screen.tree, &mut screen.start, &form, &mut text);
        screen.rebuild(&mut text);
        screen
    }

    /// The demo held after its last judged round, waiting for the user.
    pub fn demo_held() -> Self {
        let mut session = crate::demo::session();
        session.hold = true;
        Self::build(Source::Demo, Link::Live, Some(Arc::new(session)), None)
    }

    /// The demo scored by a panel of three judges, waiting for the lead's
    /// final verdict.
    pub fn demo_panel() -> Self {
        Self::build(
            Source::Demo,
            Link::Live,
            Some(Arc::new(crate::demo::panel_session())),
            None,
        )
    }

    /// The competition logged in `dir`, read-only.
    pub fn stored(dir: &Path) -> Result<Self, String> {
        let mut screen = Self::build(Source::Snapshot, Link::Live, None, None);
        let mut text = TextStore::new(Default::default());
        screen.show_stored(dir, &mut text)?;
        Ok(screen)
    }

    /// The live screen, showing the competition logged in `dir` read-only
    /// (the start view, with why, when it cannot be read).
    pub fn live_stored(channel: Channel, installed: Receiver<Installed>, dir: &Path) -> Self {
        let mut screen = Self::live(channel, installed);
        let mut text = TextStore::new(Default::default());
        if let Err(why) = screen.show_stored(dir, &mut text) {
            screen.toast(why);
        }
        screen
    }

    fn build(
        source: Source,
        link: Link,
        session: Option<Arc<Session>>,
        artifact_root: Option<PathBuf>,
    ) -> Self {
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
        let assign = auto::Auto::new(&mut tree, start.auto_host);

        let board = tree.insert(Some(body), Container::new(fill()));
        let split = tree.insert(
            Some(board),
            SplitView::horizontal()
                .pane(SplitPane::fixed(LEFT_WIDTH).min(220.0))
                .pane(SplitPane::flexible().min(CENTER_MIN))
                .pane(SplitPane::fixed(RIGHT_WIDTH).min(240.0)),
        );
        let left_host = tree.insert(Some(split), pane_column(Role::Surface));
        let center_col = tree.insert(Some(split), pane_column(Role::Background));
        let progress_host = tree.insert(
            Some(center_col),
            Container::new(Style {
                flex_shrink: 0.0,
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                ..Default::default()
            }),
        );
        let center_host = tree.insert(Some(center_col), Container::new(fill()));
        let composer = composer::insert(&mut tree, center_col);
        let right_host = tree.insert(Some(split), pane_column(Role::Surface));

        let status = StatusBar::insert(&mut tree, root);

        let mut screen = Self {
            tree,
            source,
            link,
            session,
            artifact_root,
            tab: CenterTab::Compare,
            selected_round: None,
            filter_part: None,
            pending_intervention: None,
            file_dialogs: Vec::new(),
            read_only: false,
            past: Vec::new(),
            past_root: store::default_root(),
            stored_dir: None,
            stored_free: false,
            extending: false,
            detection: start::Detection::all_installed(),
            installed: None,
            built: None,
            auto: assign,
            start,
            board,
            left_host,
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
            // The start view is gone: a planner for its form has no use.
            self.auto.cancel_running();
            self.rebuild_left(text);
            self.rebuild_progress(text);
            self.rebuild_center(text);
            self.rebuild_right(text);
            self.rebuild_composer(text);
        } else {
            start::rebuild_note(
                &mut self.tree,
                &self.start,
                &self.link,
                &self.detection,
                text,
            );
            self.render_auto(text);
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

    /// Lists the past competitions under the store root (the demo's
    /// canned ones in the demo).
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

    /// The session logged in `dir` (the demo's own, in the demo).
    fn load_stored(&self, dir: &Path) -> Result<(Session, Option<PathBuf>), String> {
        if let Source::Demo = self.source {
            let id = dir.file_name().map(|n| n.to_string_lossy().into_owned());
            return crate::demo::stored(id.as_deref().unwrap_or_default())
                .map(|session| (session, None))
                .ok_or_else(|| format!("{} を開けません", dir.display()));
        }
        let session =
            store::read_session(dir).map_err(|e| format!("{} を開けません: {e}", dir.display()))?;
        let root = store::reply_dir(dir, &session);
        Ok((session, Some(root)))
    }

    /// Leaves whatever competition is on screen: its runner stops (an
    /// unfinished one stays resumable from the list).
    fn leave(&mut self) {
        self.auto.cancel_running();
        if let Source::Live(channel) = &self.source {
            channel.leave();
            if !self.read_only && self.session.as_ref().is_some_and(|s| !s.closed) {
                self.toast("コンペを中断しました（過去のコンペから再開できます）");
            }
        }
        self.session = None;
        self.artifact_root = None;
        self.read_only = false;
        self.stored_dir = None;
        self.extending = false;
        self.tab = CenterTab::Compare;
        self.selected_round = None;
        self.filter_part = None;
        self.pending_intervention = None;
        // A draft written for this competition must not reach the next.
        composer::reset(&mut self.tree, &mut self.composer);
        self.refresh_past();
    }

    /// Resumes the stored competition in `dir` with `by` more rounds. A
    /// read-only view of it stays until the runner's first snapshot of it
    /// (a refused extension leaves it as it was, with the notice why).
    fn extend_stored(&mut self, dir: PathBuf, by: RoundNo, text: &mut TextStore) {
        if !matches!(self.source, Source::Live(_)) {
            self.toast("デモ表示では操作を送信できません");
            return;
        }
        self.auto.cancel_running();
        self.extending = self.read_only;
        self.send_cmd(Command::ExtendStored { dir, by });
        self.rebuild(text);
    }

    fn show_stored(&mut self, dir: &Path, text: &mut TextStore) -> Result<(), String> {
        let (session, root) = self.load_stored(dir)?;
        self.leave();
        self.session = Some(Arc::new(session));
        self.artifact_root = root;
        self.read_only = true;
        self.stored_dir = Some(dir.to_owned());
        self.stored_free = !store::is_busy(dir) && !store::may_run_unlocked(dir);
        self.rebuild(text);
        Ok(())
    }

    fn rebuild_left(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.left_host, text);
        let Some(session) = &self.session else {
            return;
        };
        leaderboard::insert(
            &mut self.tree,
            self.left_host,
            session,
            self.selected_round,
            self.filter_part.as_ref(),
            !self.read_only,
            &mut self.actions,
        );
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
        let Some(session) = &self.session else {
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
        for (label, action, current) in [
            (
                "成果物を比較",
                Action::ShowCompare,
                self.tab == CenterTab::Compare,
            ),
            (
                "タイムライン",
                Action::ShowTimeline,
                self.tab == CenterTab::Timeline,
            ),
            ("方向性マップ", Action::ShowMap, self.tab == CenterTab::Map),
        ] {
            let btn = self.tree.insert(
                Some(tabs),
                Button::new(label)
                    .control_size(ControlSize::Small)
                    .default_button(current),
            );
            self.actions.push((btn, action));
        }
        if let Some(part) = &self.filter_part {
            tag(&mut self.tree, tabs, format!("絞込: {part}"), Tone::Accent);
            let clear = self.tree.insert(
                Some(tabs),
                Button::new("解除").control_size(ControlSize::Small),
            );
            self.actions.push((clear, Action::ClearPartFilter));
        }
        spacer(&mut self.tree, tabs);
        let open_viewer = self.tree.insert(
            Some(tabs),
            Button::new("ブラウザ比較ビューアー…").control_size(ControlSize::Small),
        );
        self.actions.push((open_viewer, Action::OpenViewer));

        match self.tab {
            CenterTab::Compare => compare::insert(
                &mut self.tree,
                self.center_host,
                session,
                self.selected_round,
                self.filter_part.as_ref(),
                !self.read_only,
                &mut self.actions,
            ),
            CenterTab::Timeline => timeline::insert(
                &mut self.tree,
                self.center_host,
                session,
                self.filter_part.as_ref(),
            ),
            CenterTab::Map => map::insert(
                &mut self.tree,
                self.center_host,
                session,
                self.filter_part.as_ref(),
            ),
        }
    }

    fn rebuild_right(&mut self, text: &mut TextStore) {
        clear_children(&mut self.tree, self.right_host, text);
        let Some(session) = &self.session else {
            return;
        };
        judge::insert(
            &mut self.tree,
            self.right_host,
            session,
            !self.read_only,
            &self.detection.available(),
            &mut self.actions,
        );
    }

    fn rebuild_composer(&mut self, text: &mut TextStore) {
        self.tree.set_hidden(self.composer.root, self.read_only);
        let Some(session) = &self.session else {
            return;
        };
        if self.read_only {
            return;
        }
        composer::sync(&mut self.tree, &mut self.composer, session, text);
        composer::sync_hint(&mut self.tree, &self.composer, session);
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
        // A read-only competition has no runner, so no link of its own.
        if !self.read_only {
            tag(&mut self.tree, self.status_leading, link_label, link_tone);
        }

        if let Some(session) = &self.session {
            let phase_text = if session.closed {
                "終了".to_owned()
            } else if let Some(r) = session.current_round() {
                let kind = match r.kind {
                    RoundKind::Blind => "🔒ブラインド",
                    RoundKind::Reveal => "👁相互閲覧",
                };
                let phase = match r.phase {
                    Phase::Briefing => "ブリーフ配信中",
                    Phase::Competing => "制作中",
                    Phase::Judging => "審判採点中",
                    Phase::Closed => "完了",
                };
                format!("R{} ({kind}) · {phase}", r.no)
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
            if waits {
                tag(
                    &mut self.tree,
                    self.status_leading,
                    "一時停止中",
                    Tone::Warning,
                );
            }
            if !self.read_only && !session.closed {
                let hold = self.tree.insert(
                    Some(self.status_trailing),
                    small(if session.hold {
                        "ラウンド後に停止: オン"
                    } else {
                        "ラウンド後に停止: オフ"
                    })
                    .tooltip("オンにすると、審判の採点のあと次のラウンドを始める前に止まります"),
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
            // A stored competition that ran all its rounds is resumed to go on.
            if session.extendable().is_ok()
                && (!self.read_only
                    || (session.closed
                        && self.stored_dir.is_some()
                        && self.stored_free
                        && !self.extending))
            {
                if let Some(max) = session.max_rounds {
                    caption(
                        &mut self.tree,
                        self.status_trailing,
                        format!("上限 {max} ラウンド"),
                    );
                }
                for by in EXTEND_BY {
                    let more = self.tree.insert(
                        Some(self.status_trailing),
                        small(&format!("+{by}")).tooltip(if self.read_only {
                            "再開してラウンドを追加します（エージェントが起動します）"
                        } else {
                            "ラウンド上限を増やします。上限で終了していればコンペを続けます"
                        }),
                    );
                    self.tree
                        .set_accessible_label(more, format!("ラウンドを {by} 追加"));
                    self.actions.push((more, Action::Extend(by)));
                }
            }
            let new = self
                .tree
                .insert(Some(self.status_trailing), small("新しいコンペ"));
            self.actions.push((new, Action::NewCompetition));
            let rematch = self
                .tree
                .insert(Some(self.status_trailing), small("同じ設定でリマッチ"));
            self.actions.push((rematch, Action::Rematch));

            if !self.read_only
                && !session.closed
                && session
                    .current_round()
                    .is_some_and(|r| !r.retryable().is_empty())
            {
                let retry = self.tree.insert(
                    Some(self.status_trailing),
                    Button::new("失敗を再試行").control_size(ControlSize::Small),
                );
                self.actions.push((retry, Action::Retry));
            }
            if !self.read_only && !session.closed {
                let close = self.tree.insert(
                    Some(self.status_trailing),
                    Button::new("コンペを終了").control_size(ControlSize::Small),
                );
                self.actions.push((close, Action::Close));
            }
            if !self.read_only && session.closed {
                let close = self.tree.insert(
                    Some(self.status_trailing),
                    small("ペインをすべて閉じる")
                        .tooltip("審判と参加者のペインを Noa でまとめて閉じます"),
                );
                self.actions.push((close, Action::ClosePanes));
            }
        }
    }

    /// Takes the installed-agents report, once it has arrived: the menus
    /// then offer only those, and the form drops what is not installed.
    fn poll_installed(&mut self, text: &mut TextStore) {
        let Some(rx) = &self.installed else {
            return;
        };
        let Ok(installed) = rx.try_recv() else {
            return;
        };
        self.installed = None;
        let before = start::read(&self.tree, &self.start);
        let pristine = before.panel == start::Form::for_kinds(self.start.kinds()).panel
            && before.judge == AgentChoice::from(self.start.kinds()[0])
            && before.co_judges.is_empty();
        self.detection = start::Detection::Done(installed);
        let kinds = self.detection.menu_kinds();
        let form = if pristine {
            start::Form {
                judge: AgentChoice::from(kinds[0]),
                panel: start::Form::for_kinds(&kinds).panel,
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
        self.poll_auto(text);
        let mut latest = None;
        let mut notices = Vec::new();
        let mut released = false;
        if let Source::Live(channel) = &self.source {
            // Snapshots and notices sent before the screen last left a
            // competition belong to that one.
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
        for notice in notices {
            self.toast(notice);
        }
        // A runner let go of its log: the list shows it resumable now.
        if released && self.session.is_none() {
            self.refresh_past();
            self.built = None;
        }
        if let Some(snapshot) = latest {
            self.link = snapshot.link;
            if self.read_only {
                let shown = self.session.as_ref().map(|s| s.id.clone());
                match &snapshot.session {
                    // The extended competition, now driven by its runner.
                    Some(next) if self.extending && Some(&next.id) == shown.as_ref() => {
                        self.read_only = false;
                        self.extending = false;
                        self.stored_dir = None;
                        self.built = None;
                    }
                    // The extension failed (the runner ended without it).
                    None if self.extending && matches!(self.link, Link::Down(_)) => {
                        self.extending = false;
                        self.built = None;
                        return self.rebuild_if_changed(text);
                    }
                    // No runner drives a read-only session.
                    _ => return self.rebuild_if_changed(text),
                }
            }
            self.session = snapshot.session;
            if snapshot.artifact_root.is_some() {
                self.artifact_root = snapshot.artifact_root;
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
        let msg = message.into();
        Toast::new(msg).show(&mut self.tree);
    }

    fn send_cmd(&mut self, cmd: Command) {
        match &self.source {
            Source::Live(channel) => channel.send(cmd),
            Source::Demo | Source::Snapshot => {
                self.toast("デモ表示では操作を送信できません");
            }
        }
    }

    fn open_in_os(&mut self, path: &std::path::Path) {
        #[cfg(target_os = "macos")]
        let result = OsCommand::new("open").arg(path).spawn();
        #[cfg(target_os = "linux")]
        let result = OsCommand::new("xdg-open").arg(path).spawn();
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let result: std::io::Result<std::process::Child> =
            Err(std::io::Error::other("unsupported OS"));

        match result {
            Ok(_) => self.toast(format!("開きました: {}", path.display())),
            Err(e) => self.toast(format!("開けませんでした: {e}")),
        }
    }

    fn open_viewer(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        match viewer_page(session, self.artifact_root.as_deref(), self.read_only) {
            Ok(path) => self.open_in_os(&path),
            Err(e) => self.toast(format!("比較ビューアーの生成に失敗しました: {e}")),
        }
    }

    fn handle_action(&mut self, action: Action, text: &mut TextStore) {
        match action {
            Action::Start => {
                if !self.detection.startable() {
                    self.toast(self.detection.blocked_reason());
                    return;
                }
                let form = start::read(&self.tree, &self.start);
                if form.request.is_empty() {
                    self.toast("依頼内容を入力してください");
                    return;
                }
                if form.panel.len() < 2 {
                    self.toast("参加パネルを2体以上選んでください");
                    return;
                }
                if form.panel.len() > MAX_PANEL {
                    self.toast(format!("参加パネルは{MAX_PANEL}体までです"));
                    return;
                }
                let workdir = match start::workdir(&form.workdir) {
                    Ok(w) => w,
                    Err(why) => {
                        self.toast(why);
                        return;
                    }
                };
                let judges = form.judges();
                // Starting while planning starts the rows as they are.
                let auto = self.auto.record(&form, &self.start);
                self.auto.cancel_running();
                self.render_auto(text);
                self.send_cmd(Command::Start {
                    request: form.request,
                    workdir,
                    max_rounds: Some(form.max_rounds),
                    reveal_interval: form.reveal_interval,
                    judges,
                    panel: form.panel,
                    auto,
                });
            }
            Action::AutoAssign(index) => self.begin_auto(index, text),
            Action::CancelAutoAssign => self.cancel_auto(text),
            Action::ShowCompare => {
                self.tab = CenterTab::Compare;
                self.rebuild(text);
            }
            Action::ShowTimeline => {
                self.tab = CenterTab::Timeline;
                self.rebuild(text);
            }
            Action::ShowMap => {
                self.tab = CenterTab::Map;
                self.rebuild(text);
            }
            Action::SelectRound(no) => {
                self.selected_round = Some(no);
                self.rebuild(text);
            }
            Action::FilterPart(part) => {
                self.filter_part = Some(part);
                self.rebuild(text);
            }
            Action::ClearPartFilter => {
                self.filter_part = None;
                self.rebuild(text);
            }
            Action::OpenViewer => self.open_viewer(),
            Action::OpenArtifact {
                round,
                part,
                rel_path,
            } => {
                let Some(root) = &self.artifact_root else {
                    self.toast("デモ表示には実体ファイルがありません（比較ビューアーは開けます）");
                    return;
                };
                let path = store::artifact_snapshot_dir(root, round, &part).join(&rel_path);
                if path.exists() {
                    self.open_in_os(&path);
                } else {
                    let live_path = store::entry_dir(root, &part).join(&rel_path);
                    self.open_in_os(&live_path);
                }
            }
            Action::SelectWinner(winner) => {
                self.send_cmd(Command::SelectWinner { winner });
            }
            Action::CopyResult => {
                if let Some(session) = &self.session {
                    let md = export::result_markdown(session);
                    self.tree.clipboard().write_text(&md);
                    self.toast("結果をクリップボードにコピーしました");
                }
            }
            Action::ExportReport => {
                if let Some(session) = &self.session {
                    self.file_dialogs.push(FileDialogRequest {
                        save: true,
                        title: "コンペティションレポートを保存".into(),
                        file_name: format!("{}.md", session.id),
                        filters: vec![("Markdown".into(), vec!["md".into()])],
                    });
                }
            }
            Action::Adopt { winner, agent } => {
                self.send_cmd(Command::Adopt { winner, agent });
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
            Action::AddPanelRow => {
                start::add_row(&mut self.tree, &mut self.start);
                self.rebuild(text);
            }
            Action::RemovePanelRow(index) => {
                start::remove_row(&mut self.tree, &mut self.start, index, text);
                self.rebuild(text);
            }
            Action::AddJudgeRow => {
                start::add_judge_row(&mut self.tree, &mut self.start);
                self.rebuild(text);
            }
            Action::RemoveJudgeRow(index) => {
                start::remove_judge_row(&mut self.tree, &mut self.start, index, text);
                self.rebuild(text);
            }
            Action::OpenPast(dir) => {
                if let Err(why) = self.show_stored(&dir, text) {
                    self.toast(why);
                }
            }
            Action::ResumePast(dir) => {
                // Leaving the start view: its planner stops.
                self.auto.cancel_running();
                match self.load_stored(&dir) {
                    // Finished since the list was read: there is nothing to
                    // resume, so it opens read-only.
                    Ok((session, _)) if session.closed => {
                        if let Err(why) = self.show_stored(&dir, text) {
                            self.toast(why);
                        }
                    }
                    _ => self.send_cmd(Command::Resume { dir }),
                }
                self.render_auto(text);
            }
            Action::RematchPast(dir) => match self.load_stored(&dir) {
                Ok((session, _)) => {
                    // A rematch is staffed by hand unless assigned again.
                    self.auto.forget();
                    let form = start::Form::of_session(&session);
                    let left_out = start::fill(&mut self.tree, &mut self.start, &form, text);
                    self.rebuild(text);
                    self.toast(if left_out.is_empty() {
                        "前回の設定をフォームに入れました".to_owned()
                    } else {
                        format!(
                            "前回の設定をフォームに入れました。{}",
                            start::left_out_message(&left_out)
                        )
                    });
                }
                Err(why) => self.toast(why),
            },
            Action::NewCompetition => {
                self.leave();
                self.auto.forget();
                let form = start::Form::for_kinds(self.start.kinds());
                start::fill(&mut self.tree, &mut self.start, &form, text);
                self.rebuild(text);
            }
            Action::Rematch => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                self.leave();
                self.auto.forget();
                let form = start::Form::of_session(&session);
                let left_out = start::fill(&mut self.tree, &mut self.start, &form, text);
                self.rebuild(text);
                if !left_out.is_empty() {
                    self.toast(start::left_out_message(&left_out));
                }
            }
            Action::FocusPane(part) => self.send_cmd(Command::Focus { part }),
            Action::SetHold(hold) => self.send_cmd(Command::SetHold { hold }),
            Action::Continue => self.send_cmd(Command::Continue),
            Action::Extend(by) => match self.stored_dir.clone() {
                Some(dir) if self.read_only => self.extend_stored(dir, by, text),
                _ => self.send_cmd(Command::Extend { by }),
            },
            Action::ExtendPast(dir, by) => self.extend_stored(dir, by, text),
            Action::ClosePanes => self.send_cmd(Command::ClosePanes),
        }
    }
}

impl Screen for Competition {
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
        if message.source == self.composer.recipients
            && message.downcast_ref::<SelectionChanged>().is_some()
        {
            if let Some(session) = &self.session {
                composer::sync_hint(&mut self.tree, &self.composer, session);
            }
            return;
        }
        if message.source == self.auto.menu
            && let Some(MenuChosen(index)) = message.downcast_ref()
        {
            let index = *index;
            return self.handle_action(Action::AutoAssign(index), text);
        }
        if message.source == self.auto.cancel && message.downcast_ref::<ButtonClicked>().is_some() {
            return self.handle_action(Action::CancelAutoAssign, text);
        }
        if message.source == self.start.judge
            && message.downcast_ref::<SelectionChanged>().is_some()
        {
            start::judge_kind_changed(&mut self.tree, &mut self.start, text);
            return self.render_auto(text);
        }
        if message.downcast_ref::<SelectionChanged>().is_some()
            && start::row_kind_changed(&mut self.tree, &mut self.start, message.source, text)
        {
            return self.render_auto(text);
        }
        // An edit of the start form: the block marks rows changed by hand,
        // and the pull-down follows whether there is a request to read.
        if self.session.is_none()
            && (message.downcast_ref::<TextChanged>().is_some()
                || message.downcast_ref::<SelectionChanged>().is_some())
        {
            self.render_auto(text);
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
            let md = export::result_markdown(session);
            match std::fs::write(&path, md) {
                Ok(()) => self.toast(format!("保存しました: {}", path.display())),
                Err(e) => self.toast(format!("保存できませんでした: {e}")),
            }
        }
    }
}

/// The comparison viewer page to open. A live competition's is written
/// afresh into its artifact root. A read-only one's is the page already
/// there (a runner elsewhere may have written a newer one, and an archive
/// may not be writable); without one, and in the demo, a page is written
/// into a temporary directory, framing the artifacts from where they are.
fn viewer_page(
    session: &Session,
    root: Option<&Path>,
    read_only: bool,
) -> std::io::Result<PathBuf> {
    match root {
        Some(root) if !read_only => store::write_viewer(root, session),
        Some(root) if store::viewer_path(root).is_file() => Ok(store::viewer_path(root)),
        _ => {
            let dir = std::env::temp_dir().join(format!("noa-comp-viewer-{}", session.id));
            std::fs::create_dir_all(&dir)?;
            let path = store::viewer_path(&dir);
            let html = match root {
                Some(root) => competition::model::viewer::render_html_elsewhere(session, root),
                None => competition::model::viewer::render_html(session, &dir),
            };
            std::fs::write(&path, html)?;
            Ok(path)
        }
    }
}

type BuiltKey = (Option<(String, u64)>, Link, Option<Millis>);

fn system_now() -> Millis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as Millis)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_screen_prepares_without_panic() {
        let mut screen = Competition::demo();
        let mut text = TextStore::new(Default::default());
        screen.prepare(Size::new(1440.0, 900.0), &mut text);
    }

    #[test]
    fn winner_buttons_stay_on_screen_above_long_reports() {
        let mut session = crate::demo::session();
        for round in &mut session.rounds {
            for sub in round.submissions.values_mut() {
                sub.body = "- a long line of a submission report\n".repeat(80);
            }
        }
        let mut screen =
            Competition::build(Source::Demo, Link::Live, Some(Arc::new(session)), None);
        let mut text = TextStore::new(Default::default());
        screen.prepare(Size::new(1440.0, 900.0), &mut text);

        let buttons: Vec<_> = screen
            .actions
            .iter()
            .filter(|(_, action)| matches!(action, Action::SelectWinner(_)))
            .map(|(id, _)| screen.tree.window_rect(*id).unwrap())
            .collect();
        assert_eq!(buttons.len(), 3);
        for rect in buttons {
            assert!(rect.bottom() < 900.0, "{rect:?}");
        }
    }

    #[test]
    fn minute_tick_updates_progress_and_preserves_actions() {
        let mut session = crate::demo::session();
        session.closed = false;
        let last = session.rounds.len() - 1;
        session.rounds[last].phase = Phase::Competing;
        session.rounds[last].briefs[0].receipt.sent = Some(system_now().saturating_sub(120_000));
        session.rounds[last].briefs[0].receipt.replied = None;
        session.rounds[last].briefs[0].receipt.failed = None;

        let mut screen =
            Competition::build(Source::Demo, Link::Live, Some(Arc::new(session)), None);
        let mut text = TextStore::new(Default::default());
        screen.prepare(Size::new(1440.0, 900.0), &mut text);
        let actions_before: Vec<Action> = screen.actions.iter().map(|(_, a)| a.clone()).collect();
        let progress_before = screen.tree.children(screen.progress_host).to_vec();
        let left_before = screen.tree.children(screen.left_host).to_vec();
        let center_before = screen.tree.children(screen.center_host).to_vec();
        let right_before = screen.tree.children(screen.right_host).to_vec();

        if let Some((_, _, minute)) = &mut screen.built {
            *minute = minute.map(|m| m.saturating_sub(1));
        }
        screen.poll_live(&mut text);
        screen.prepare(Size::new(1440.0, 900.0), &mut text);

        let actions_after: Vec<Action> = screen.actions.iter().map(|(_, a)| a.clone()).collect();
        assert_eq!(actions_after, actions_before);
        assert_ne!(
            screen.tree.children(screen.progress_host),
            progress_before.as_slice(),
            "progress strip must refresh on minute tick"
        );
        assert_eq!(
            screen.tree.children(screen.left_host),
            left_before.as_slice()
        );
        assert_eq!(
            screen.tree.children(screen.center_host),
            center_before.as_slice()
        );
        assert_eq!(
            screen.tree.children(screen.right_host),
            right_before.as_slice()
        );
    }

    use competition::engine::{Config, Engine, Input, Setup};
    use competition::model::types::{AgentChoice, AgentSetup, JUDGE_ID, SessionId};
    use competition::store::Store;

    fn temp_root(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "noa-competition-ui-{}-{name}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A finished one-round competition of claude on opus against claude
    /// on sonnet (`claude-2`), judged by codex, logged under `root`.
    fn stored_competition(root: &Path) -> PathBuf {
        stored_competition_of(root, 3)
    }

    /// [`stored_competition`] with a round limit of `max_rounds`: with 1,
    /// it ended at its limit (and can be extended); with more, the judge
    /// closed it early.
    fn stored_competition_of(root: &Path, max_rounds: RoundNo) -> PathBuf {
        let on = |model: &str| AgentChoice {
            kind: AgentKind::Claude,
            setup: AgentSetup {
                model: Some(model.into()),
                effort: None,
            },
        };
        let panel = [on("opus"), on("sonnet")];
        let id = SessionId::new("comp-1700000000000");
        let dir = store::session_dir(root, &id);
        let setup = Setup {
            request: "音楽プレイヤーのミニマルな UI を作る",
            max_rounds: Some(max_rounds),
            reveal_interval: 5,
            workdir: None,
            judges: &[AgentKind::Codex.into()],
            panel: &panel,
        };
        let (mut engine, out) =
            Engine::create_in(id, setup, &dir, Config::default(), 1_700_000_000_000).unwrap();
        let mut store = Store::create(&dir, &out.batches[0]).unwrap();
        let mut feed = |engine: &mut Engine, input| {
            let out = engine.handle(input).unwrap();
            store.append_all(&out.batches).unwrap();
        };
        let at = 1_700_000_000_000;
        feed(&mut engine, Input::Start { now: at });
        for part in ["claude", "claude-2"] {
            let part = PartId::new(part);
            feed(
                &mut engine,
                Input::Sent {
                    round: 1,
                    part: part.clone(),
                    at,
                },
            );
            feed(
                &mut engine,
                Input::ReplyFileWritten {
                    round: 1,
                    content: format!("# Summary: {part} の案\n\n{part} が作りました。"),
                    part,
                    artifacts: vec![],
                    at: at + 60_000,
                },
            );
        }
        let judge = PartId::new(JUDGE_ID);
        feed(
            &mut engine,
            Input::Sent {
                round: 1,
                part: judge.clone(),
                at: at + 61_000,
            },
        );
        let json = r#"{
  "overview": "sonnet の案が操作性で上回りました。",
  "evaluations": {
    "claude": { "score": 78, "strengths": "余白", "feedback": "CTA を強めてください" },
    "claude-2": { "score": 84, "strengths": "操作性", "feedback": "配色を詰めてください" }
  },
  "ranking": ["claude-2", "claude"],
  "winner": "claude-2",
  "final_verdict": "claude-2 が操作性と完成度で勝ちました。",
  "close": true
}"#;
        feed(
            &mut engine,
            Input::ReplyFileWritten {
                round: 1,
                part: judge,
                content: json.into(),
                artifacts: vec![],
                at: at + 120_000,
            },
        );
        assert!(engine.session().closed, "{:?}", engine.session().rounds);
        dir
    }

    fn actions(screen: &Competition) -> Vec<Action> {
        screen.actions.iter().map(|(_, a)| a.clone()).collect()
    }

    /// Where a fixture goes: a fresh temporary directory, or, when `keep`
    /// names a directory, a new directory of its own inside it. Nothing
    /// already in `keep` is touched.
    fn fixture_root(keep: Option<&Path>) -> PathBuf {
        let Some(keep) = keep else {
            return temp_root("stored");
        };
        std::fs::create_dir_all(keep).unwrap();
        for n in 0.. {
            let root = keep.join(format!("competition-fixture-{n}"));
            if std::fs::create_dir(&root).is_ok() {
                return root;
            }
        }
        unreachable!()
    }

    #[test]
    fn a_kept_fixture_leaves_what_was_there_alone() {
        let keep = temp_root("keep");
        std::fs::create_dir_all(keep.join("comp-1")).unwrap();
        std::fs::write(keep.join("comp-1/events.jsonl"), "mine").unwrap();
        let root = fixture_root(Some(&keep));
        stored_competition(&root);
        assert_eq!(
            std::fs::read_to_string(keep.join("comp-1/events.jsonl")).unwrap(),
            "mine"
        );
        assert!(root.starts_with(&keep) && root != keep);
        std::fs::remove_dir_all(&keep).unwrap();
    }

    /// The fixture stays on disk (in a directory of its own) when
    /// `COMPETITION_FIXTURE_OUT` names a directory, for
    /// `--session=DIR --snapshot=…`.
    #[test]
    fn a_stored_competition_opens_read_only() {
        let keep = std::env::var_os("COMPETITION_FIXTURE_OUT").map(PathBuf::from);
        let root = fixture_root(keep.as_deref());
        let dir = stored_competition(&root);
        let mut screen = Competition::stored(&dir).unwrap();
        let mut text = TextStore::new(Default::default());
        screen.prepare(Size::new(1440.0, 900.0), &mut text);

        let session = screen.session.clone().unwrap();
        assert_eq!(session.winner, Some(PartId::new("claude-2")));
        assert!(screen.read_only);
        assert_eq!(screen.artifact_root.as_deref(), Some(dir.as_path()));
        assert!(screen.tree.is_hidden(screen.start.root));
        assert!(screen.tree.is_hidden(screen.composer.root));
        let acts = actions(&screen);
        assert!(acts.contains(&Action::NewCompetition));
        assert!(acts.contains(&Action::Rematch));
        for a in &acts {
            assert!(
                !matches!(
                    a,
                    Action::SelectWinner(_)
                        | Action::Adopt { .. }
                        | Action::FocusPane(_)
                        | Action::Send
                        | Action::Retry
                        | Action::Close
                        | Action::ClosePanes
                ),
                "{a:?} on a read-only competition"
            );
        }
        if keep.is_none() {
            std::fs::remove_dir_all(&root).unwrap();
        } else {
            println!("fixture: {}", dir.display());
        }
    }

    #[test]
    fn a_rematch_fills_the_form_with_every_panel_row() {
        let root = temp_root("rematch");
        let dir = stored_competition(&root);
        let mut screen = Competition::stored(&dir).unwrap();
        let mut text = TextStore::new(Default::default());
        screen.handle_action(Action::Rematch, &mut text);

        assert!(screen.session.is_none() && !screen.read_only);
        assert!(!screen.tree.is_hidden(screen.start.root));
        let form = start::read(&screen.tree, &screen.start);
        let stored = store::read_session(&dir).unwrap();
        assert_eq!(form, start::Form::of_session(&stored));
        let models: Vec<_> = form
            .panel
            .iter()
            .map(|c| (c.kind, c.setup.model.as_deref()))
            .collect();
        assert_eq!(
            models,
            [
                (AgentKind::Claude, Some("opus")),
                (AgentKind::Claude, Some("sonnet"))
            ]
        );
        assert_eq!(form.judge.kind, AgentKind::Codex);
        assert_eq!(form.max_rounds, 3);
        let contestants =
            competition::engine::participants(std::slice::from_ref(&form.judge), &form.panel)
                .contestants;
        let ids: Vec<&str> = contestants.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["claude", "claude-2"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn new_competition_returns_to_a_clean_start_view() {
        let mut screen = Competition::demo();
        let mut text = TextStore::new(Default::default());
        assert!(screen.tree.is_hidden(screen.start.root));
        screen.handle_action(Action::NewCompetition, &mut text);
        assert!(screen.session.is_none());
        assert!(!screen.tree.is_hidden(screen.start.root));
        assert!(screen.tree.is_hidden(screen.board));
        assert_eq!(
            start::read(&screen.tree, &screen.start),
            start::Form::default()
        );
        let acts = actions(&screen);
        assert!(acts.contains(&Action::Start));
        // The demo's past competitions are listed with what each allows.
        assert!(acts.iter().any(|a| matches!(a, Action::ResumePast(_))));
        assert!(acts.iter().any(|a| matches!(a, Action::OpenPast(_))));
        assert!(acts.iter().any(|a| matches!(a, Action::RematchPast(_))));
    }

    #[test]
    fn opening_a_past_competition_shows_it_read_only() {
        let mut screen = Competition::demo();
        let mut text = TextStore::new(Default::default());
        screen.handle_action(Action::NewCompetition, &mut text);
        screen.handle_action(Action::OpenPast(PathBuf::from("/demo/comp-2")), &mut text);
        assert!(screen.read_only);
        assert!(!screen.tree.is_hidden(screen.board));
        // The row's own competition: finished, won by claude.
        let session = screen.session.clone().unwrap();
        let listed = crate::demo::past();
        let row = listed[2].summary.as_ref().unwrap();
        assert_eq!(session.id, row.id);
        assert!(session.closed && row.closed);
        assert_eq!(session.winner, row.winner);
        assert_eq!(session.winner, Some(PartId::new("claude")));

        // A rematch of the interrupted one takes its own panel.
        screen.handle_action(Action::NewCompetition, &mut text);
        screen.handle_action(
            Action::RematchPast(PathBuf::from("/demo/comp-4")),
            &mut text,
        );
        let form = start::read(&screen.tree, &screen.start);
        assert!(
            form.request.starts_with("音楽プレイヤー"),
            "{}",
            form.request
        );
        assert_eq!(form.panel[0].setup.model.as_deref(), Some("opus"));
    }

    #[test]
    fn panel_rows_are_added_and_removed_within_the_limits() {
        let mut screen = Competition::demo();
        let mut text = TextStore::new(Default::default());
        screen.handle_action(Action::NewCompetition, &mut text);
        let rows = |s: &Competition| start::read(&s.tree, &s.start).panel.len();
        assert_eq!(rows(&screen), 3);
        for _ in 0..5 {
            screen.handle_action(Action::AddPanelRow, &mut text);
        }
        assert_eq!(rows(&screen), MAX_PANEL);
        assert!(!screen.tree.is_enabled(screen.start.add_row));
        let panel = start::read(&screen.tree, &screen.start).panel;
        // A new row takes the last row's kind.
        assert!(panel[3..].iter().all(|c| c.kind == AgentKind::Agy));
        let contestants = competition::engine::participants(&panel[..1], &panel).contestants;
        assert_eq!(contestants[5].id.as_str(), "agy-4");
        // The remove buttons are wired to their rows.
        assert_eq!(
            actions(&screen)
                .iter()
                .filter(|a| matches!(a, Action::RemovePanelRow(_)))
                .count(),
            MAX_PANEL
        );

        screen.handle_action(Action::RemovePanelRow(0), &mut text);
        let panel = start::read(&screen.tree, &screen.start).panel;
        assert_eq!(panel.len(), MAX_PANEL - 1);
        assert_eq!(panel[0].kind, AgentKind::Codex);
        assert!(screen.tree.is_enabled(screen.start.add_row));
        for _ in 0..10 {
            screen.handle_action(Action::RemovePanelRow(0), &mut text);
        }
        assert_eq!(rows(&screen), 2);
        assert!(
            screen
                .start
                .panel
                .iter()
                .all(|row| !screen.tree.is_enabled(row.remove))
        );
    }

    const VIEW: Size = Size::new(1440.0, 1700.0);

    /// Clicks `button` as a pointer would, and hands the messages that
    /// raises to the screen.
    fn click(screen: &mut Competition, button: WidgetId, text: &mut TextStore) {
        screen.prepare(VIEW, text);
        let rect = screen.tree.window_rect(button).unwrap();
        let pos = noa_gpui_scene::Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        for event in [
            noa_gpui_core::Event::PointerMove { pos },
            noa_gpui_core::Event::PointerDown {
                pos,
                button: noa_gpui_core::PointerButton::Primary,
            },
            noa_gpui_core::Event::PointerUp {
                pos,
                button: noa_gpui_core::PointerButton::Primary,
            },
        ] {
            screen.dispatch(event, text);
        }
    }

    #[test]
    fn a_row_kind_change_offers_that_kinds_models() {
        use noa_gpui_core::widgets::ComboBox;
        let mut screen = Competition::demo_start();
        let mut text = TextStore::new(Default::default());
        screen.prepare(VIEW, &mut text);
        // The second row (claude) goes to codex by keyboard.
        let row = screen.start.panel[1].kind;
        screen.tree.set_focus(Some(row));
        screen.dispatch(
            noa_gpui_core::Event::KeyDown {
                key: noa_gpui_core::Key::ArrowDown,
                modifiers: Default::default(),
            },
            &mut text,
        );
        let models = |screen: &Competition, index: usize| {
            let model = screen.start.panel[index].setup.model;
            screen.tree.get::<ComboBox>(model).unwrap().items().to_vec()
        };
        let codex: Vec<String> = AgentKind::Codex
            .models()
            .iter()
            .map(|m| m.to_string())
            .collect();
        let claude: Vec<String> = AgentKind::Claude
            .models()
            .iter()
            .map(|m| m.to_string())
            .collect();
        assert_eq!(models(&screen, 1), codex);
        assert_eq!(models(&screen, 0), claude);
        let panel = start::read(&screen.tree, &screen.start).panel;
        assert_eq!(panel[1], AgentChoice::from(AgentKind::Codex));
        assert_eq!(panel[0].setup.model.as_deref(), Some("opus"));
    }

    #[test]
    fn the_remove_button_of_a_middle_row_removes_that_row() {
        let mut screen = Competition::demo_start();
        let mut text = TextStore::new(Default::default());
        // opus, sonnet, gpt-6.2-sol: the middle row goes.
        let remove = screen.start.panel[1].remove;
        click(&mut screen, remove, &mut text);
        let models: Vec<_> = start::read(&screen.tree, &screen.start)
            .panel
            .into_iter()
            .map(|c| c.setup.model)
            .collect();
        assert_eq!(
            models,
            [Some("opus".to_owned()), Some("gpt-6.2-sol".to_owned())]
        );
    }

    #[test]
    fn a_draft_does_not_follow_into_the_next_competition() {
        let mut screen = Competition::demo();
        let mut text = TextStore::new(Default::default());
        let area = screen.composer.area;
        screen
            .tree
            .update::<noa_gpui_core::widgets::TextArea, _>(area, |a, cx| {
                a.set_text(cx, "codex だけに: 配色を変えて")
            });
        screen.handle_action(Action::Rematch, &mut text);
        screen.session = Some(Arc::new(crate::demo::session()));
        screen.rebuild(&mut text);
        assert_eq!(composer::text(&screen.tree, &screen.composer), "");
    }

    #[test]
    fn six_entries_compare_within_the_center_pane() {
        let mut session = crate::demo::session();
        let last = session.rounds.len() - 1;
        let round = &mut session.rounds[last];
        round.outcome = None;
        round.phase = Phase::Judging;
        let template = round.submissions[&PartId::new("claude")].clone();
        let base = session.contestants[0].clone();
        for n in 2..=4 {
            let id = PartId::new(format!("claude-{n}"));
            session
                .contestants
                .push(competition::model::types::Participant {
                    id: id.clone(),
                    ..base.clone()
                });
            let mut sub = template.clone();
            sub.part = id.clone();
            session.rounds[last].submissions.insert(id, sub);
        }
        assert_eq!(session.contestants.len(), MAX_PANEL);
        let mut screen =
            Competition::build(Source::Demo, Link::Live, Some(Arc::new(session)), None);
        let mut text = TextStore::new(Default::default());
        screen.prepare(Size::new(1440.0, 900.0), &mut text);
        let center = screen.tree.window_rect(screen.center_host).unwrap();
        let winners: Vec<_> = screen
            .actions
            .iter()
            .filter(|(_, a)| matches!(a, Action::SelectWinner(_)))
            .map(|(id, _)| screen.tree.window_rect(*id).unwrap())
            .collect();
        assert_eq!(winners.len(), MAX_PANEL);
        for rect in winners {
            assert!(rect.right() <= center.right(), "{rect:?} past {center:?}");
        }
    }

    /// A report that every kind is installed, already delivered.
    fn installed_all() -> Receiver<Installed> {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Installed::Found(AgentKind::ALL.to_vec())).unwrap();
        rx
    }

    /// A screen on the live channel whose list reads `root`.
    fn live_on(root: &Path) -> (Competition, crate::live::FakeLink, TextStore) {
        let (channel, link) = Channel::fake();
        let mut screen = Competition::live(channel, installed_all());
        screen.past_root = Some(root.to_owned());
        screen.refresh_past();
        let mut text = TextStore::new(Default::default());
        screen.rebuild(&mut text);
        (screen, link, text)
    }

    /// A live screen still waiting for the installed-agents report, and
    /// the end that delivers it.
    fn live_pending() -> (
        Competition,
        crate::live::FakeLink,
        std::sync::mpsc::Sender<Installed>,
        TextStore,
    ) {
        let (channel, link) = Channel::fake();
        let (tx, rx) = std::sync::mpsc::channel();
        let screen = Competition::live(channel, rx);
        (screen, link, tx, TextStore::new(Default::default()))
    }

    /// `screen` once the check reported `installed`.
    fn report(
        screen: &mut Competition,
        tx: &std::sync::mpsc::Sender<Installed>,
        installed: Installed,
        text: &mut TextStore,
    ) {
        tx.send(installed).unwrap();
        screen.poll_live(text);
    }

    fn found(kinds: &[AgentKind]) -> Installed {
        Installed::Found(kinds.to_vec())
    }

    /// How many entries `popup` has, by walking down its menu with the
    /// keyboard to the end.
    fn menu_len(screen: &mut Competition, popup: WidgetId, text: &mut TextStore) -> usize {
        screen.tree.set_focus(Some(popup));
        for _ in 0..AgentKind::ALL.len() + 2 {
            screen.dispatch(
                noa_gpui_core::Event::KeyDown {
                    key: noa_gpui_core::Key::ArrowDown,
                    modifiers: Default::default(),
                },
                text,
            );
        }
        screen
            .tree
            .get::<noa_gpui_core::widgets::PopUpButton>(popup)
            .unwrap()
            .selected()
            + 1
    }

    fn press_down(screen: &mut Competition, popup: WidgetId, text: &mut TextStore) {
        screen.tree.set_focus(Some(popup));
        screen.dispatch(
            noa_gpui_core::Event::KeyDown {
                key: noa_gpui_core::Key::ArrowDown,
                modifiers: Default::default(),
            },
            text,
        );
    }

    fn kinds_of(choices: &[AgentChoice]) -> Vec<AgentKind> {
        choices.iter().map(|c| c.kind).collect()
    }

    #[test]
    fn the_start_button_waits_for_the_installed_check() {
        let (mut screen, link, tx, mut text) = live_pending();
        let start = screen.start.start_button;
        assert!(!screen.tree.is_enabled(start));
        // A start that gets through anyway sends nothing.
        screen.handle_action(Action::Start, &mut text);
        assert!(link.commands.try_recv().is_err());

        report(&mut screen, &tx, found(&[AgentKind::Codex]), &mut text);
        assert!(screen.tree.is_enabled(start));
    }

    #[test]
    fn the_start_button_stays_off_when_no_agent_is_installed() {
        let (mut screen, link, tx, mut text) = live_pending();
        report(&mut screen, &tx, found(&[]), &mut text);
        assert!(!screen.tree.is_enabled(screen.start.start_button));
        assert_eq!(
            screen.detection.blocked_reason(),
            "claude / codex / agy のいずれもログインシェルの PATH に見つかりません"
        );
        screen.handle_action(Action::Start, &mut text);
        assert!(link.commands.try_recv().is_err());
    }

    #[test]
    fn an_unknown_check_offers_every_kind_and_allows_starting() {
        let (mut screen, _link, tx, mut text) = live_pending();
        report(
            &mut screen,
            &tx,
            Installed::Unknown("zsh".into()),
            &mut text,
        );
        assert!(screen.tree.is_enabled(screen.start.start_button));
        assert_eq!(screen.start.kinds(), AgentKind::ALL);
        let judge = screen.start.judge;
        assert_eq!(
            menu_len(&mut screen, judge, &mut text),
            AgentKind::ALL.len()
        );
        assert!(
            screen
                .detection
                .blocked_reason()
                .contains("すべて表示しています")
        );
    }

    #[test]
    fn menus_offer_only_the_installed_kinds_and_map_by_position() {
        let (mut screen, _link, tx, mut text) = live_pending();
        report(
            &mut screen,
            &tx,
            found(&[AgentKind::Codex, AgentKind::Agy]),
            &mut text,
        );
        let form = start::read(&screen.tree, &screen.start);
        // One row per installed kind, led by the first.
        assert_eq!(form.judge.kind, AgentKind::Codex);
        assert_eq!(kinds_of(&form.panel), [AgentKind::Codex, AgentKind::Agy]);

        let judge = screen.start.judge;
        assert_eq!(menu_len(&mut screen, judge, &mut text), 2);
        for row in 0..2 {
            let popup = screen.start.panel[row].kind;
            assert_eq!(menu_len(&mut screen, popup, &mut text), 2);
        }
        // The judge's second entry is agy (`ALL[1]` would be codex).
        let form = start::read(&screen.tree, &screen.start);
        assert_eq!(form.judge.kind, AgentKind::Agy);
        assert_eq!(kinds_of(&form.panel), [AgentKind::Agy, AgentKind::Agy]);

        // The model box of a changed row follows its real kind.
        let models = |screen: &Competition, index: usize| {
            let model = screen.start.panel[index].setup.model;
            screen
                .tree
                .get::<noa_gpui_core::widgets::ComboBox>(model)
                .unwrap()
                .items()
                .to_vec()
        };
        let agy: Vec<String> = AgentKind::Agy
            .models()
            .iter()
            .map(|m| m.to_string())
            .collect();
        assert_eq!(models(&screen, 0), agy);

        // A new row takes the last row's kind.
        screen.handle_action(Action::AddPanelRow, &mut text);
        let panel = start::read(&screen.tree, &screen.start).panel;
        assert_eq!(panel.last().unwrap().kind, AgentKind::Agy);
    }

    #[test]
    fn a_menu_step_lands_on_the_next_installed_kind() {
        let (mut screen, _link, tx, mut text) = live_pending();
        report(
            &mut screen,
            &tx,
            found(&[AgentKind::Codex, AgentKind::Agy]),
            &mut text,
        );
        let popup = screen.start.panel[0].kind;
        press_down(&mut screen, popup, &mut text);
        let panel = start::read(&screen.tree, &screen.start).panel;
        assert_eq!(kinds_of(&panel), [AgentKind::Agy, AgentKind::Agy]);
    }

    #[test]
    fn a_form_filled_with_uninstalled_kinds_drops_them() {
        let (mut screen, _link, tx, mut text) = live_pending();
        report(
            &mut screen,
            &tx,
            found(&[AgentKind::Codex, AgentKind::Agy]),
            &mut text,
        );
        let form = start::Form {
            judge: AgentKind::Claude.into(),
            panel: vec![
                AgentKind::Claude.into(),
                AgentKind::Codex.into(),
                AgentKind::Agy.into(),
                AgentKind::Claude.into(),
            ],
            ..start::Form::default()
        };
        let left_out = start::fill(&mut screen.tree, &mut screen.start, &form, &mut text);
        assert_eq!(left_out, [AgentKind::Claude]);
        let read = start::read(&screen.tree, &screen.start);
        assert_eq!(read.judge.kind, AgentKind::Codex);
        assert_eq!(kinds_of(&read.panel), [AgentKind::Codex, AgentKind::Agy]);
        assert_eq!(
            start::left_out_message(&left_out),
            "インストールされていないため除外: claude"
        );

        // Too few left: the panel is padded to two with the first kind.
        let form = start::Form {
            panel: vec![
                AgentKind::Claude.into(),
                AgentKind::Claude.into(),
                AgentKind::Agy.into(),
            ],
            ..start::Form::default()
        };
        start::fill(&mut screen.tree, &mut screen.start, &form, &mut text);
        let read = start::read(&screen.tree, &screen.start);
        assert_eq!(kinds_of(&read.panel), [AgentKind::Agy, AgentKind::Codex]);
    }

    #[test]
    fn a_rematch_leaves_out_what_is_not_installed() {
        let mut screen = Competition::demo();
        let mut text = TextStore::new(Default::default());
        let kinds = vec![AgentKind::Codex, AgentKind::Agy];
        screen.detection = start::Detection::Done(Installed::Found(kinds.clone()));
        let form = start::Form::for_kinds(&kinds);
        start::apply_kinds(&mut screen.tree, &mut screen.start, kinds, &form, &mut text);
        screen.handle_action(
            Action::RematchPast(PathBuf::from("/demo/comp-4")),
            &mut text,
        );
        let read = start::read(&screen.tree, &screen.start);
        let offered = [AgentKind::Codex, AgentKind::Agy];
        assert!(offered.contains(&read.judge.kind));
        assert!(read.panel.len() >= 2);
        assert!(read.panel.iter().all(|c| offered.contains(&c.kind)));
    }

    #[test]
    fn a_form_the_user_changed_is_kept_when_the_check_reports() {
        let (mut screen, _link, tx, mut text) = live_pending();
        // Pending offers every kind; a fourth row is the user's own.
        screen.handle_action(Action::AddPanelRow, &mut text);
        report(&mut screen, &tx, found(&[AgentKind::Codex]), &mut text);
        let panel = start::read(&screen.tree, &screen.start).panel;
        assert_eq!(kinds_of(&panel), [AgentKind::Codex, AgentKind::Codex]);
    }

    #[test]
    fn the_adopt_buttons_offer_only_installed_kinds() {
        let (mut screen, _link, tx, mut text) = live_pending();
        report(&mut screen, &tx, found(&[AgentKind::Agy]), &mut text);
        assert_eq!(screen.detection.available(), [AgentKind::Agy]);
        assert!(!screen.detection.available().contains(&AgentKind::Claude));
    }

    #[test]
    fn the_list_is_read_again_once_a_runner_lets_go_of_its_log() {
        let root = temp_root("released");
        let dir = stored_competition(&root);
        // Unfinished, and still held by a runner on its way out.
        let log = dir.join(store::EVENTS_FILE);
        let text_log = std::fs::read_to_string(&log).unwrap();
        let kept: Vec<&str> = text_log.lines().take(2).collect();
        std::fs::write(&log, kept.join("\n") + "\n").unwrap();
        let (held, _) = Store::open(&dir).unwrap();
        let (mut screen, link, mut text) = live_on(&root);
        let resumable = |screen: &Competition| {
            actions(screen)
                .iter()
                .any(|a| matches!(a, Action::ResumePast(d) if *d == dir))
        };
        assert!(!resumable(&screen));
        drop(held);
        link.released.send(()).unwrap();
        screen.poll_live(&mut text);
        assert!(resumable(&screen));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resuming_a_competition_finished_meanwhile_opens_it_read_only() {
        let root = temp_root("finished");
        let dir = stored_competition(&root);
        let (mut screen, link, mut text) = live_on(&root);
        screen.handle_action(Action::ResumePast(dir.clone()), &mut text);
        assert!(screen.read_only && screen.session.is_some());
        // Nothing was asked of a runner.
        assert!(
            link.commands
                .try_iter()
                .all(|(_, cmd)| !matches!(cmd, Command::Resume { .. }))
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_read_only_competition_opens_the_viewer_page_already_there() {
        let root = temp_root("viewer");
        let dir = stored_competition(&root);
        let session = store::read_session(&dir).unwrap();
        let page = store::viewer_path(&dir);
        std::fs::write(&page, "R2 written by the runner").unwrap();
        assert_eq!(viewer_page(&session, Some(&dir), true).unwrap(), page);
        assert_eq!(
            std::fs::read_to_string(&page).unwrap(),
            "R2 written by the runner"
        );
        // Without one, a page is written elsewhere.
        std::fs::remove_file(&page).unwrap();
        let other = viewer_page(&session, Some(&dir), true).unwrap();
        assert!(!other.starts_with(&dir) && other.is_file());
        assert!(!page.exists());
        // A live competition's page is written afresh where it belongs.
        assert_eq!(viewer_page(&session, Some(&dir), false).unwrap(), page);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_competition_opened_through_a_link_to_its_folder_can_go_on() {
        // `--session=DIR` names the folder another way than the log's reply
        // paths do (a symlink here; a relative path is made absolute first).
        let root = temp_root("resume-alias");
        let dir = stored_competition(&root);
        let session = store::read_session(&dir).unwrap();
        let alias = root.join("alias");
        std::fs::create_dir_all(&alias).unwrap();
        let linked = alias.join(dir.file_name().unwrap());
        std::os::unix::fs::symlink(&dir, &linked).unwrap();
        assert_eq!(
            competition::engine::check_resumable(&session, &linked),
            Ok(())
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Where the page's relative `href`s point: under its `<base href>`,
    /// else next to the page.
    fn link_targets(page: &Path) -> Vec<PathBuf> {
        let html = std::fs::read_to_string(page).unwrap();
        let base = html
            .split_once("<base href=\"file://")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(dir, _)| PathBuf::from(dir))
            .unwrap_or_else(|| page.parent().unwrap().to_owned());
        html.split("\"href\":\"")
            .skip(1)
            .filter_map(|rest| rest.split_once('"'))
            .map(|(href, _)| base.join(href))
            .collect()
    }

    #[test]
    fn a_viewer_page_written_elsewhere_links_the_artifacts_where_they_are() {
        let root = temp_root("viewer-links");
        let dir = stored_competition(&root);
        let mut session = store::read_session(&dir).unwrap();
        let entry = store::artifact_snapshot_dir(&dir, 1, &PartId::new("claude"));
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("index.html"), "<p>claude</p>").unwrap();
        let sub = session.rounds[0]
            .submissions
            .get_mut(&PartId::new("claude"))
            .unwrap();
        sub.artifacts.push(competition::model::types::ArtifactFile {
            rel_path: "index.html".into(),
            kind: competition::model::types::ArtifactKind::Html,
            bytes: 13,
        });
        let page = viewer_page(&session, Some(&dir), true).unwrap();
        assert!(!page.starts_with(&dir));
        let targets = link_targets(&page);
        assert!(!targets.is_empty(), "the fixture has artifacts");
        for target in targets {
            assert!(target.is_file(), "{}", target.display());
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn notices_about_a_left_competition_are_dropped() {
        let (channel, link) = Channel::fake();
        let mut screen = Competition::live(channel, installed_all());
        let mut text = TextStore::new(Default::default());
        screen.handle_action(Action::NewCompetition, &mut text);
        link.notices
            .send((0, "コンペティションを続けられません: old".into()))
            .unwrap();
        link.notices.send((1, "new".into())).unwrap();
        screen.poll_live(&mut text);
        let tree = &screen.tree;
        let toasts: Vec<&str> = tree
            .overlays()
            .flat_map(|stack| tree.children(stack).iter())
            .filter_map(|&id| tree.get::<noa_gpui_core::widgets::Toast>(id))
            .map(|toast| toast.message())
            .collect();
        assert_eq!(toasts, ["new"]);
    }

    #[test]
    fn every_agent_and_the_judge_has_a_pane_button() {
        let screen = Competition::demo();
        let focus: Vec<String> = actions(&screen)
            .into_iter()
            .filter_map(|a| match a {
                Action::FocusPane(part) => Some(part.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(focus, ["claude", "codex", "agy", "judge"]);
    }

    #[test]
    fn snapshots_of_a_left_competition_are_dropped() {
        let (channel, link) = Channel::fake();
        let snapshots = link.snapshots;
        let mut screen = Competition::live(channel, installed_all());
        let mut text = TextStore::new(Default::default());
        let snapshot = |generation| Snapshot {
            link: Link::Live,
            session: Some(Arc::new(crate::demo::session())),
            artifact_root: None,
            generation,
        };
        snapshots.send(snapshot(0)).unwrap();
        screen.poll_live(&mut text);
        assert!(screen.session.is_some());

        screen.handle_action(Action::NewCompetition, &mut text);
        assert!(screen.session.is_none());
        // Still in flight from the runner that was stopped.
        snapshots.send(snapshot(0)).unwrap();
        screen.poll_live(&mut text);
        assert!(screen.session.is_none());
        assert!(!screen.tree.is_hidden(screen.start.root));
        // The next competition's are taken.
        snapshots.send(snapshot(1)).unwrap();
        screen.poll_live(&mut text);
        assert!(screen.session.is_some());
    }

    /// The ids of the screen's actions (the widgets that raise them).
    fn widget_for(screen: &Competition, action: &Action) -> Option<WidgetId> {
        screen
            .actions
            .iter()
            .find(|(_, a)| a == action)
            .map(|(id, _)| *id)
    }

    /// The generation the screen's commands go out in (what the runner
    /// they start stamps its snapshots and notices with).
    fn generation(screen: &Competition) -> u64 {
        match &screen.source {
            Source::Live(channel) => channel.generation(),
            _ => 0,
        }
    }

    fn commands(link: &crate::live::FakeLink) -> Vec<Command> {
        link.commands.try_iter().map(|(_, cmd)| cmd).collect()
    }

    #[test]
    fn the_panes_close_together_once_the_competition_has_ended() {
        let (channel, link) = Channel::fake();
        let mut screen = Competition::live(channel, installed_all());
        let mut text = TextStore::new(Default::default());
        let mut session = crate::demo::session();
        session.closed = false;
        let send = |session: &Session| {
            link.snapshots
                .send(Snapshot {
                    link: Link::Live,
                    session: Some(Arc::new(session.clone())),
                    artifact_root: None,
                    generation: 0,
                })
                .unwrap();
        };
        send(&session);
        screen.poll_live(&mut text);
        assert!(widget_for(&screen, &Action::ClosePanes).is_none());

        // Ended at its round limit, so "+N" is offered beside it: the whole
        // status bar still fits the smallest window.
        session.closed = true;
        session.max_rounds = Some(session.rounds.len() as RoundNo);
        session.rev += 1;
        send(&session);
        screen.poll_live(&mut text);
        let (w, h) = MIN_WINDOW;
        let size = Size::new(w as f32, h as f32);
        screen.prepare(size, &mut text);
        assert!(widget_for(&screen, &Action::Extend(3)).is_some());
        for &child in screen.tree.children(screen.status_trailing) {
            let origin = screen.tree.window_origin(child).unwrap();
            let width = screen.tree.layout(child).unwrap().rect.width;
            assert!(
                origin.x >= 0.0 && origin.x + width <= size.width + 0.5,
                "{}+{width}",
                origin.x
            );
        }
        let close = widget_for(&screen, &Action::ClosePanes).expect("ペインをすべて閉じる");
        click(&mut screen, close, &mut text);
        assert_eq!(commands(&link), [Command::ClosePanes]);
    }

    #[test]
    fn a_held_competition_offers_the_next_round_and_the_toggle() {
        let (channel, link) = Channel::fake();
        let mut screen = Competition::live(channel, installed_all());
        let mut text = TextStore::new(Default::default());
        let mut session = crate::demo::session();
        session.hold = true;
        link.snapshots
            .send(Snapshot {
                link: Link::Live,
                session: Some(Arc::new(session)),
                artifact_root: None,
                generation: 0,
            })
            .unwrap();
        screen.poll_live(&mut text);
        let go = widget_for(&screen, &Action::Continue).expect("次のラウンドへ");
        click(&mut screen, go, &mut text);
        assert_eq!(commands(&link), [Command::Continue]);
        let toggle = widget_for(&screen, &Action::SetHold(false)).expect("the toggle");
        click(&mut screen, toggle, &mut text);
        assert_eq!(commands(&link), [Command::SetHold { hold: false }]);
        let more = widget_for(&screen, &Action::Extend(3)).expect("+3");
        click(&mut screen, more, &mut text);
        assert_eq!(commands(&link), [Command::Extend { by: 3 }]);

        // Not held: no next-round button, and the toggle turns it on.
        // As the runner sends it after `HoldSet { hold: false }`: a new rev.
        let mut running = crate::demo::session();
        running.hold = false;
        running.rev += 1;
        link.snapshots
            .send(Snapshot {
                link: Link::Live,
                session: Some(Arc::new(running)),
                artifact_root: None,
                generation: 0,
            })
            .unwrap();
        screen.poll_live(&mut text);
        assert!(widget_for(&screen, &Action::Continue).is_none());
        assert!(widget_for(&screen, &Action::SetHold(true)).is_some());
    }

    #[test]
    fn rounds_are_offered_only_where_they_can_be_added() {
        // The rounds each "+N" shown adds: all of them, or none.
        let offered = |session: Session| {
            let screen =
                Competition::build(Source::Demo, Link::Live, Some(Arc::new(session)), None);
            let shown: Vec<RoundNo> = actions(&screen)
                .into_iter()
                .filter_map(|a| match a {
                    Action::Extend(by) => Some(by),
                    _ => None,
                })
                .collect();
            match shown.as_slice() {
                [] => false,
                [1, 3] => true,
                other => panic!("only some of the rounds offered: {other:?}"),
            }
        };
        let base = crate::demo::session();
        // Running, with a limit.
        assert!(offered(base.clone()));
        // Ended at its limit.
        let mut ran_out = base.clone();
        ran_out.max_rounds = Some(ran_out.rounds.len() as RoundNo);
        ran_out.closed = true;
        assert!(offered(ran_out));
        // Ended before it (by the judge or the user).
        let mut early = base.clone();
        early.closed = true;
        assert!(!offered(early));
        // No limit.
        let mut unlimited = base.clone();
        unlimited.max_rounds = None;
        assert!(!offered(unlimited));
        // The final round under way: judged, or still with its contestants.
        for phase in [Phase::Judging, Phase::Competing, Phase::Briefing] {
            let mut last_round = base.clone();
            last_round.max_rounds = Some(last_round.rounds.len() as RoundNo);
            let last = last_round.rounds.len() - 1;
            last_round.rounds[last].phase = phase;
            assert!(!offered(last_round), "{phase:?}");
        }
    }

    #[test]
    fn a_stored_competition_that_ran_out_of_rounds_is_extended_from_its_view() {
        let root = temp_root("extend-view");
        let dir = stored_competition_of(&root, 1);
        let (mut screen, link, mut text) = live_on(&root);
        screen.handle_action(Action::OpenPast(dir.clone()), &mut text);
        assert!(screen.read_only);
        let _ = commands(&link);
        let more = widget_for(&screen, &Action::Extend(3)).expect("+3 on the stored view");
        click(&mut screen, more, &mut text);
        assert_eq!(
            commands(&link),
            [Command::ExtendStored {
                dir: dir.clone(),
                by: 3
            }]
        );
        // Still shown, read-only, until the runner has it; no second "+3".
        assert!(screen.read_only && screen.session.is_some());
        assert!(widget_for(&screen, &Action::Extend(3)).is_none());

        // The runner's first snapshot of it: the board is live.
        let mut extended = store::read_session(&dir).unwrap();
        extended.closed = false;
        extended.rev += 1;
        link.snapshots
            .send(Snapshot {
                link: Link::Live,
                session: Some(Arc::new(extended)),
                artifact_root: None,
                generation: generation(&screen),
            })
            .unwrap();
        screen.poll_live(&mut text);
        assert!(!screen.read_only);
        assert!(widget_for(&screen, &Action::SetHold(true)).is_some());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_refused_extension_keeps_the_stored_view() {
        let root = temp_root("extend-refused");
        let dir = stored_competition_of(&root, 1);
        let (mut screen, link, mut text) = live_on(&root);
        screen.handle_action(Action::OpenPast(dir.clone()), &mut text);
        screen.handle_action(Action::Extend(1), &mut text);
        // The runner could not open it: its notice, then its last snapshot.
        link.notices
            .send((
                generation(&screen),
                "コンペティションを続けられません: busy".into(),
            ))
            .unwrap();
        link.snapshots
            .send(Snapshot {
                link: Link::Down("busy".into()),
                session: None,
                artifact_root: None,
                generation: generation(&screen),
            })
            .unwrap();
        screen.poll_live(&mut text);
        assert!(screen.read_only);
        assert_eq!(
            screen.session.as_ref().map(|s| s.id.clone()),
            Some(store::read_session(&dir).unwrap().id)
        );
        // It can be asked for again.
        assert!(widget_for(&screen, &Action::Extend(1)).is_some());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_stored_view_of_a_log_in_use_offers_no_rounds() {
        let root = temp_root("extend-busy");
        let dir = stored_competition_of(&root, 1);
        // The window that ran it still holds its log.
        let (held, _) = Store::open(&dir).unwrap();
        let (mut screen, _link, mut text) = live_on(&root);
        assert!(
            !actions(&screen)
                .iter()
                .any(|a| matches!(a, Action::ExtendPast(..)))
        );
        screen.handle_action(Action::OpenPast(dir), &mut text);
        assert!(screen.read_only);
        assert!(widget_for(&screen, &Action::Extend(1)).is_none());
        drop(held);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_stored_competition_closed_early_offers_no_rounds() {
        let root = temp_root("extend-early");
        let dir = stored_competition_of(&root, 3);
        let (mut screen, _link, mut text) = live_on(&root);
        assert!(
            !actions(&screen)
                .iter()
                .any(|a| matches!(a, Action::ExtendPast(..)))
        );
        screen.handle_action(Action::OpenPast(dir), &mut text);
        assert!(widget_for(&screen, &Action::Extend(1)).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_listed_competition_that_ran_out_of_rounds_is_extended_from_the_list() {
        let root = temp_root("extend-list");
        let dir = stored_competition_of(&root, 1);
        let (mut screen, link, mut text) = live_on(&root);
        let more = widget_for(&screen, &Action::ExtendPast(dir.clone(), 1)).expect("+1 row");
        click(&mut screen, more, &mut text);
        assert_eq!(commands(&link), [Command::ExtendStored { dir, by: 1 }]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A rematch takes the limit the competition last had (an extended
    /// one included), within what the form takes (1 to 50).
    #[test]
    fn a_rematch_takes_the_last_limit_within_the_form() {
        let with_limit = |max| {
            let mut session = crate::demo::session();
            session.max_rounds = Some(max);
            start::Form::of_session(&session).max_rounds
        };
        assert_eq!(with_limit(12), 12);
        assert_eq!(with_limit(53), 50);
        let mut unlimited = crate::demo::session();
        unlimited.max_rounds = None;
        assert_eq!(
            start::Form::of_session(&unlimited).max_rounds,
            competition::model::types::DEFAULT_MAX_ROUNDS
        );
    }

    #[test]
    #[ignore = "perf probe: times competition UI minute-tick poll_live on 10-round session"]
    fn probe_ui_minute_tick() {
        let mut session = crate::demo::session();
        let base_round = session.rounds.last().unwrap().clone();
        while session.rounds.len() < 10 {
            let mut r = base_round.clone();
            r.no = session.rounds.len() as RoundNo + 1;
            for sub in r.submissions.values_mut() {
                sub.body = "Detailed evaluation and design report paragraph for performance measurement.\n"
                    .repeat(280);
            }
            session.rounds.push(r);
        }
        session.closed = false;
        let last = session.rounds.len() - 1;
        session.rounds[last].phase = Phase::Competing;
        session.rounds[last].briefs[0].receipt.sent = Some(system_now().saturating_sub(180_000));
        session.rounds[last].briefs[0].receipt.replied = None;
        session.rounds[last].briefs[0].receipt.failed = None;

        let mut screen =
            Competition::build(Source::Demo, Link::Live, Some(Arc::new(session)), None);
        let mut text = TextStore::new(Default::default());
        let viewport = Size::new(1440.0, 900.0);
        screen.prepare(viewport, &mut text);

        let iters = 20usize;
        let mut times_us = Vec::with_capacity(iters);
        for _ in 0..iters {
            if let Some((_, _, minute)) = &mut screen.built {
                *minute = minute.map(|m| m.saturating_sub(1));
            }
            let t0 = std::time::Instant::now();
            screen.poll_live(&mut text);
            screen.prepare(viewport, &mut text);
            times_us.push(t0.elapsed().as_secs_f64() * 1_000_000.0);
        }
        times_us.sort_by(|a, b| a.total_cmp(b));
        println!(
            "competition ui_minute_tick (10r x 3 x 20KB, n={iters}): median={:.1} us  min={:.1} us  max={:.1} us",
            times_us[iters / 2],
            times_us[0],
            times_us[iters - 1]
        );
    }

    /// Every label and tag text under `root`, depth first.
    fn texts_under(tree: &UiTree, root: WidgetId) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if let Some(label) = tree.get::<Label>(id) {
                out.push(label.text().to_owned());
            }
            if let Some(tag) = tree.get::<Tag>(id) {
                out.push(tag.text().to_owned());
            }
            stack.extend(tree.children(id).iter().rev());
        }
        out
    }

    fn toasts(screen: &Competition) -> Vec<String> {
        let tree = &screen.tree;
        tree.overlays()
            .flat_map(|stack| tree.children(stack).iter())
            .filter_map(|&id| tree.get::<noa_gpui_core::widgets::Toast>(id))
            .map(|toast| toast.message().to_owned())
            .collect()
    }

    fn setups(choices: &[AgentChoice]) -> Vec<(AgentKind, Option<&str>)> {
        choices
            .iter()
            .map(|c| (c.kind, c.setup.model.as_deref()))
            .collect()
    }

    #[test]
    fn a_rematch_fills_every_judge_row() {
        let mut screen = Competition::demo_panel();
        let mut text = TextStore::new(Default::default());
        let session = screen.session.clone().unwrap();
        screen.handle_action(Action::Rematch, &mut text);
        let form = start::read(&screen.tree, &screen.start);
        assert_eq!(form, start::Form::of_session(&session));
        assert_eq!(
            setups(&form.judges()),
            [
                (AgentKind::Claude, Some("opus")),
                (AgentKind::Codex, Some("gpt-6.2-sol")),
                (AgentKind::Agy, None),
            ]
        );
        assert_eq!(screen.start.co_judges.len(), 2);
        // Three judges: the add button is off, both co-judge rows can go.
        assert!(!screen.tree.is_enabled(screen.start.add_judge));
        let acts = actions(&screen);
        assert!(acts.contains(&Action::RemoveJudgeRow(0)));
        assert!(acts.contains(&Action::RemoveJudgeRow(1)));
        let seats = competition::engine::participants(&form.judges(), &form.panel);
        let ids: Vec<&str> = std::iter::once(&seats.judge)
            .chain(&seats.co_judges)
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(ids, ["judge", "judge-2", "judge-3"]);
    }

    #[test]
    fn a_rematch_leaves_out_judges_not_installed() {
        let mut screen = Competition::demo_panel();
        let mut text = TextStore::new(Default::default());
        let kinds = vec![AgentKind::Codex, AgentKind::Agy];
        screen.detection = start::Detection::Done(Installed::Found(kinds.clone()));
        let form = start::Form::for_kinds(&kinds);
        start::apply_kinds(&mut screen.tree, &mut screen.start, kinds, &form, &mut text);
        screen.handle_action(Action::Rematch, &mut text);
        // The lead (claude) is not installed: the first co-judge leads.
        let read = start::read(&screen.tree, &screen.start);
        assert_eq!(
            setups(&read.judges()),
            [
                (AgentKind::Codex, Some("gpt-6.2-sol")),
                (AgentKind::Agy, None)
            ]
        );
        assert!(
            toasts(&screen)
                .iter()
                .any(|t| t == "インストールされていないため除外: claude"),
            "{:?}",
            toasts(&screen)
        );

        // With none of the judges installed, one judge stays: the first kind.
        let mut session = crate::demo::panel_session();
        for judge in session.co_judges.iter_mut() {
            judge.kind = AgentKind::Claude;
        }
        let form = start::Form::of_session(&session);
        let left_out = start::fill(&mut screen.tree, &mut screen.start, &form, &mut text);
        assert_eq!(left_out, [AgentKind::Claude]);
        let read = start::read(&screen.tree, &screen.start);
        assert_eq!(read.judges(), [AgentChoice::from(AgentKind::Codex)]);
    }

    #[test]
    fn a_pristine_form_keeps_one_judge_after_the_agent_check() {
        let (mut screen, _link, tx, mut text) = live_pending();
        assert!(
            start::read(&screen.tree, &screen.start)
                .co_judges
                .is_empty()
        );
        report(
            &mut screen,
            &tx,
            found(&[AgentKind::Codex, AgentKind::Agy]),
            &mut text,
        );
        let form = start::read(&screen.tree, &screen.start);
        assert_eq!(form.judges(), [AgentChoice::from(AgentKind::Codex)]);
        assert!(screen.tree.is_enabled(screen.start.add_judge));

        // A judge the user added is the user's form: it is kept.
        let (mut screen, _link, tx, mut text) = live_pending();
        screen.handle_action(Action::AddJudgeRow, &mut text);
        report(&mut screen, &tx, found(&[AgentKind::Claude]), &mut text);
        let form = start::read(&screen.tree, &screen.start);
        assert_eq!(
            kinds_of(&form.judges()),
            [AgentKind::Claude, AgentKind::Claude]
        );
    }

    #[test]
    fn every_agent_and_every_judge_has_a_pane_button() {
        let screen = Competition::demo_panel();
        let focus: Vec<String> = actions(&screen)
            .into_iter()
            .filter_map(|a| match a {
                Action::FocusPane(part) => Some(part.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            focus,
            ["claude", "agy", "codex", "judge", "judge-2", "judge-3"]
        );
    }

    #[test]
    fn the_leaderboard_shows_each_judges_score_and_the_spread() {
        let screen = Competition::demo_panel();
        let texts = texts_under(&screen.tree, screen.left_host);
        for want in [
            "93.7 pt",
            "R1:72.3 → R2:78.3 → R3:82.3 → R4:85.3 → R5:88.3 → R6:93.7",
            "judge 93 · judge-2 96 · judge-3 92 · 差 4",
            // codex's spread of 20 is flagged.
            "judge 91 · judge-2 95 · judge-3 75",
            "⚠ 差 20",
            "リード審判 · claude (opus)",
            "審判 2 · codex (gpt-6.2-sol)",
            "審判 3 · agy",
            // The lead's card follows its verdict brief, not yet sent.
            "R6 待機中",
        ] {
            assert!(texts.iter().any(|t| t == want), "{want:?} in {texts:?}");
        }
        // A single judge's leaderboard is as it was.
        let single = Competition::demo();
        let texts = texts_under(&single.tree, single.left_host);
        assert!(texts.iter().any(|t| t == "93 pt"));
        assert!(texts.iter().any(|t| t == "審判 · claude"));
        assert!(
            !texts
                .iter()
                .any(|t| t.contains('差') || t.contains("リード"))
        );
    }

    #[test]
    fn the_judge_board_lists_scores_by_judge() {
        let screen = Competition::demo_panel();
        let texts = texts_under(&screen.tree, screen.right_host);
        for want in [
            "#1 claude(93.7) · #2 agy(88.7) · #3 codex(87.0)",
            "審判別スコア",
            "judge-2",
            "差",
            "⚠ 差 20",
            "審判 2 (codex) の総評",
            "R6: codex の見立て: 操作性と情報設計を重く見ました。",
            "審判 3 (agy) の総評",
            "最終講評を待っています（judge）",
        ] {
            assert!(texts.iter().any(|t| t == want), "{want:?} in {texts:?}");
        }
        // Only the round still waiting says so.
        let waiting = texts
            .iter()
            .filter(|t| t.starts_with("最終講評を待って"))
            .count();
        assert_eq!(waiting, 1);
        let single = Competition::demo();
        let texts = texts_under(&single.tree, single.right_host);
        assert!(
            texts
                .iter()
                .any(|t| t == "#1 claude(93) · #2 codex(91) · #3 agy(89)")
        );
        assert!(!texts.iter().any(|t| t == "審判別スコア"));
    }

    #[test]
    fn the_progress_strip_has_a_chip_per_judge_and_the_verdict() {
        let screen = Competition::demo_panel();
        let texts = texts_under(&screen.tree, screen.progress_host);
        for want in [
            "● 採点 3/3",
            "審判 (judge)",
            "審判 (judge-2)",
            "審判 (judge-3)",
            "講評 (judge)",
        ] {
            assert!(texts.iter().any(|t| t == want), "{want:?} in {texts:?}");
        }

        // Mid-scoring: judge-2 failed, judge-3 is still out.
        let mut session = crate::demo::panel_session();
        let last = session.rounds.len() - 1;
        let round = &mut session.rounds[last];
        round.verdict_brief = None;
        round.outcome = None;
        round
            .scorecards
            .retain(|judge, _| judge.as_str() == JUDGE_ID);
        round.co_judge_briefs[0].receipt.replied = None;
        round.co_judge_briefs[0].receipt.failed = Some("返答がありません".into());
        round.co_judge_briefs[1].receipt.replied = None;
        let screen = Competition::build(Source::Demo, Link::Live, Some(Arc::new(session)), None);
        let texts = texts_under(&screen.tree, screen.progress_host);
        for want in ["● 採点 1/3", "✗ judge-2: 返答がありません"] {
            assert!(texts.iter().any(|t| t == want), "{want:?} in {texts:?}");
        }
        assert!(!texts.iter().any(|t| t == "講評 (judge)"));
        // The retry button is offered for the failed judge.
        assert!(actions(&screen).contains(&Action::Retry));

        let single = Competition::demo();
        let texts = texts_under(&single.tree, single.progress_host);
        let judges: Vec<&String> = texts.iter().filter(|t| t.contains("(judge")).collect();
        assert_eq!(judges, ["審判 (judge)"]);
    }

    #[test]
    fn a_tie_the_verdict_breaks_shows_its_winner_first() {
        use competition::model::aggregate;
        use competition::model::reduce::{Event, apply};
        // Every judge scores claude and agy 90 in the last round, ranking
        // claude first; the lead's verdict names agy.
        let mut session = crate::demo::panel_session();
        let last = session.rounds.len();
        for card in session.rounds[last - 1].scorecards.values_mut() {
            for (part, ev) in &mut card.evaluations {
                ev.score = if part.as_str() == "codex" { 80 } else { 90 };
            }
            card.ranking = ["claude", "agy", "codex"].map(PartId::new).to_vec();
        }
        let cards = &session.rounds[last - 1].scorecards;
        let combined = aggregate::combine(&session, last as RoundNo, cards);
        assert_eq!(combined.ranking[0].as_str(), "claude");
        session.rounds[last - 1].outcome = Some(combined);
        apply(
            &mut session,
            &[Event::VerdictAccepted {
                round: last as RoundNo,
                winner: PartId::new("agy"),
                final_verdict: "agy breaks the tie.".into(),
            }],
        )
        .unwrap();
        let outcome = session.rounds[last - 1].outcome.as_ref().unwrap();
        assert_eq!(outcome.ranking, ["agy", "claude", "codex"].map(PartId::new));

        let report = export::result_markdown(&session);
        assert!(report.contains("1. **agy** 🏆"), "{report}");
        assert!(report.contains("2. **claude** — "), "{report}");
        let screen = Competition::build(Source::Demo, Link::Live, Some(Arc::new(session)), None);
        let texts = texts_under(&screen.tree, screen.left_host);
        for want in ["🏆 #1 agy", "#2 claude", "#3 codex"] {
            assert!(texts.iter().any(|t| t == want), "{want:?} in {texts:?}");
        }
    }

    #[test]
    fn a_verdict_the_competition_ended_without_is_not_awaited() {
        use competition::model::reduce::{Event, apply};
        let winner = PartId::new("claude");
        for (how, batch) in [
            ("終了", vec![Event::SessionClosed]),
            (
                "a winner chosen by hand",
                vec![Event::WinnerSelected { winner }, Event::SessionClosed],
            ),
        ] {
            // Every judge scored, the verdict asked of the lead, then ended.
            let mut session = crate::demo::panel_session();
            apply(&mut session, &batch).unwrap();
            let screen =
                Competition::build(Source::Demo, Link::Live, Some(Arc::new(session)), None);
            let right = texts_under(&screen.tree, screen.right_host);
            assert!(
                !right.iter().any(|t| t.starts_with("最終講評を待って")),
                "{how}: {right:?}"
            );
            // The lead's card shows its scorecard, not the verdict it
            // will not write.
            let left = texts_under(&screen.tree, screen.left_host);
            assert!(left.iter().any(|t| t == "R6 提出済"), "{how}: {left:?}");
            assert!(!left.iter().any(|t| t == "R6 待機中"), "{how}: {left:?}");
            let progress = texts_under(&screen.tree, screen.progress_host);
            assert!(
                !progress.iter().any(|t| t == "講評 (judge)"),
                "{how}: {progress:?}"
            );
        }
    }

    #[test]
    fn an_intervention_in_a_deciding_round_reaches_the_leads_verdict_brief() {
        let ids = |v: &[&str]| v.iter().map(|x| PartId::new(*x)).collect::<Vec<_>>();
        let next = "追加指示は次のラウンドのブリーフで宛先パネルに届きます（他パネルへの個別指示はブラインド維持）";
        let verdict = "追加指示はこのラウンドでこのあと出る最終講評のブリーフでリード審判に届きます。延長した場合は次のラウンドのブリーフで宛先パネルに届きます";
        let verdict_too = "追加指示は宛先にリード審判がいなくても、このラウンドでこのあと出る最終講評のブリーフでリード審判に届きます。延長した場合は次のラウンドのブリーフで宛先パネルに届きます";
        let early_close =
            "追加指示はこのラウンドでこのあと出る最終講評のブリーフでリード審判に届きます";
        // The last round, being scored, its verdict brief not issued yet.
        let mut session = crate::demo::panel_session();
        let last = session.rounds.len() - 1;
        session.rounds[last].verdict_brief = None;
        for to in [
            ids(&[]),
            ids(&["judge"]),
            ids(&["judge", "judge-2", "judge-3"]),
        ] {
            assert_eq!(composer::hint(&session, &to), verdict, "{to:?}");
        }
        // The verdict brief carries every intervention, whatever its
        // addressees: one to co-judges or contestants reaches them next
        // round, and the lead now. Nothing claims blindness.
        for to in [
            ids(&["judge-2"]),
            ids(&["judge-2", "judge-3"]),
            ids(&["claude"]),
            ids(&["claude", "judge-3"]),
        ] {
            assert_eq!(composer::hint(&session, &to), verdict_too, "{to:?}");
        }
        assert!(!verdict.contains("ブラインド") && !verdict_too.contains("ブラインド"));
        // The last round has no next round unless extended: delivery to the
        // lead is stated as certain, to the recipients as conditional.
        for hint in [verdict, verdict_too] {
            assert!(
                hint.contains("最終講評のブリーフでリード審判に届きます"),
                "{hint}"
            );
            assert!(
                hint.contains("延長した場合は次のラウンドのブリーフで宛先パネルに届きます"),
                "{hint}"
            );
        }

        // A round before the last: the next round's briefs, until the
        // lead's scorecard closes the competition. Closed early, it cannot
        // be extended: there is no next round, so whoever it is addressed
        // to, it reaches only the lead's verdict brief.
        let mut early = session.clone();
        early.max_rounds = Some(last as RoundNo + 3);
        assert_eq!(composer::hint(&early, &ids(&["judge"])), next);
        assert_eq!(composer::hint(&early, &ids(&["judge-2"])), next);
        early.rounds[last]
            .scorecards
            .get_mut(&PartId::new(JUDGE_ID))
            .unwrap()
            .close = true;
        let every = [
            ids(&[]),
            ids(&["judge"]),
            ids(&["judge", "judge-2", "judge-3"]),
            ids(&["judge-2"]),
            ids(&["claude"]),
            ids(&["claude", "judge-3"]),
        ];
        for to in &every {
            assert_eq!(composer::hint(&early, to), early_close, "{to:?}");
        }
        // Without a round limit, the lead's close is the only end either.
        let mut unlimited = early.clone();
        unlimited.max_rounds = None;
        for to in &every {
            assert_eq!(composer::hint(&unlimited, to), early_close, "{to:?}");
        }
        assert!(!early_close.contains("次のラウンド"));

        // Before judging, and once the verdict brief is out, it is not.
        let mut competing = session.clone();
        competing.rounds[last].phase = Phase::Competing;
        assert_eq!(composer::hint(&competing, &[]), next);
        assert_eq!(composer::hint(&crate::demo::panel_session(), &[]), next);

        // A single judge's hint is as it was, in its deciding round too.
        let mut single = crate::demo::session();
        let last_single = single.rounds.len() - 1;
        single.max_rounds = Some(single.rounds.len() as RoundNo);
        single.rounds[last_single].phase = Phase::Judging;
        assert_eq!(composer::hint(&single, &[]), next);
        assert_eq!(composer::hint(&single, &ids(&["judge"])), next);
        assert_eq!(composer::hint(&single, &ids(&["claude"])), next);

        // On screen, the hint follows the chosen recipients.
        let mut text = TextStore::new(Default::default());
        let mut screen =
            Competition::build(Source::Demo, Link::Live, Some(Arc::new(session)), None);
        screen.prepare(VIEW, &mut text);
        let hint = |screen: &Competition| {
            screen
                .tree
                .get::<Label>(screen.composer.hint)
                .unwrap()
                .text()
                .to_owned()
        };
        assert_eq!(hint(&screen), verdict, "全員");
        let popup = screen.composer.recipients;
        for want in [verdict, verdict, verdict_too] {
            // 審判全員, 審判 (Judge), then judge-2.
            press_down(&mut screen, popup, &mut text);
            assert_eq!(hint(&screen), want);
        }
    }

    #[test]
    fn interventions_go_to_one_judge_or_every_judge() {
        let ids = |to: &[PartId]| to.iter().map(PartId::to_string).collect::<Vec<_>>();
        let panel = crate::demo::panel_session();
        let choices: Vec<(String, Vec<String>)> = composer::choices(&panel)
            .into_iter()
            .map(|(label, to)| (label, ids(&to)))
            .collect();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            choices,
            [
                ("全員".to_owned(), s(&[])),
                ("審判全員".to_owned(), s(&["judge", "judge-2", "judge-3"])),
                ("審判 (Judge)".to_owned(), s(&["judge"])),
                ("審判 2 (judge-2)".to_owned(), s(&["judge-2"])),
                ("審判 3 (judge-3)".to_owned(), s(&["judge-3"])),
                ("claude".to_owned(), s(&["claude"])),
                ("codex".to_owned(), s(&["codex"])),
                ("agy".to_owned(), s(&["agy"])),
            ]
        );
        // A single judge's choices are as they were.
        let single: Vec<String> = composer::choices(&crate::demo::session())
            .into_iter()
            .map(|(label, _)| label)
            .collect();
        assert_eq!(single, ["全員", "審判 (Judge)", "claude", "codex", "agy"]);

        // Sent from the composer, to every judge and then to judge-2.
        let (channel, link) = Channel::fake();
        let mut screen = Competition::live(channel, installed_all());
        let mut text = TextStore::new(Default::default());
        link.snapshots
            .send(Snapshot {
                link: Link::Live,
                session: Some(Arc::new(panel)),
                artifact_root: None,
                generation: 0,
            })
            .unwrap();
        screen.poll_live(&mut text);
        for (index, want) in [
            (1, vec!["judge", "judge-2", "judge-3"]),
            (3, vec!["judge-2"]),
        ] {
            let popup = screen.composer.recipients;
            screen
                .tree
                .update::<noa_gpui_core::widgets::PopUpButton, _>(popup, |p, cx| {
                    p.set_selected(cx, index)
                });
            let area = screen.composer.area;
            screen
                .tree
                .update::<noa_gpui_core::widgets::TextArea, _>(area, |a, cx| {
                    a.set_text(cx, "採点基準を揃えてください")
                });
            screen.handle_action(Action::Send, &mut text);
            let sent: Vec<Vec<String>> = commands(&link)
                .into_iter()
                .filter_map(|cmd| match cmd {
                    Command::Intervene { to, .. } => Some(ids(&to)),
                    _ => None,
                })
                .collect();
            assert_eq!(sent, [s(&want)]);
        }
    }

    // ---- 「自動で割り当て」 (docs/specs/auto-assign.md, unit U5) ----

    use competition::auto_assign::{
        self as assign, Assignment, AutoRecord, Cancel, Job, Pick, PlanError, Planning, RawPick,
        Request,
    };
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A planner run the screen asked for: its job, where its result goes,
    /// and the flag it is told to stop by.
    struct Planned {
        job: Job,
        reply: std::sync::mpsc::Sender<Result<Assignment, PlanError>>,
        cancel: Cancel,
    }

    type Queue = Rc<RefCell<Vec<Planned>>>;

    /// Swaps the screen's planner for one that runs nothing: each run is
    /// queued for the test to answer.
    fn fake_planner(screen: &mut Competition) -> Queue {
        let queue: Queue = Rc::default();
        let runs = Rc::clone(&queue);
        screen.auto.spawn = Box::new(move |job, _wake| {
            let (reply, rx) = std::sync::mpsc::channel();
            let cancel = Cancel::default();
            runs.borrow_mut().push(Planned {
                job,
                reply,
                cancel: Arc::clone(&cancel),
            });
            Planning { rx, cancel }
        });
        queue
    }

    const REQUEST: &str = "コーヒー焙煎所の LP を作る";

    fn set_request(screen: &mut Competition, request: &str, text: &mut TextStore) {
        let request = request.to_owned();
        let area = screen.start.request;
        screen
            .tree
            .update::<noa_gpui_core::widgets::TextArea, _>(area, |a, cx| a.set_text(cx, request));
        screen.rebuild(text);
    }

    /// A live screen with `kinds` installed, a request typed and a fake
    /// planner.
    fn auto_screen(kinds: &[AgentKind]) -> (Competition, crate::live::FakeLink, Queue, TextStore) {
        let (mut screen, link, tx, mut text) = live_pending();
        let queue = fake_planner(&mut screen);
        report(&mut screen, &tx, found(kinds), &mut text);
        set_request(&mut screen, REQUEST, &mut text);
        (screen, link, queue, text)
    }

    /// [`auto_screen`] with claude and codex, two judges (claude, claude)
    /// and three contestants (claude, codex, codex).
    fn five_rows() -> (Competition, crate::live::FakeLink, Queue, TextStore) {
        let (mut screen, link, queue, mut text) =
            auto_screen(&[AgentKind::Claude, AgentKind::Codex]);
        screen.handle_action(Action::AddJudgeRow, &mut text);
        screen.handle_action(Action::AddPanelRow, &mut text);
        let form = start::read(&screen.tree, &screen.start);
        assert_eq!(
            kinds_of(&form.judges()),
            [AgentKind::Claude, AgentKind::Claude]
        );
        assert_eq!(
            kinds_of(&form.panel),
            [AgentKind::Claude, AgentKind::Codex, AgentKind::Codex]
        );
        (screen, link, queue, text)
    }

    type Answer<'a> = (&'a str, &'a str, Option<&'a str>, Option<&'a str>);

    /// What the planner answers for [`five_rows`].
    const FIVE: [Answer<'static>; 5] = [
        ("judge-1", "claude", Some("opus"), Some("high")),
        ("judge-2", "codex", Some("gpt-6.2-sol"), Some("xhigh")),
        ("contestant-1", "codex", Some("gpt-6.2-astra"), None),
        ("contestant-2", "claude", Some("sonnet"), Some("medium")),
        ("contestant-3", "claude", None, None),
    ];

    fn on(kind: AgentKind, model: Option<&str>, effort: Option<&str>) -> AgentChoice {
        AgentChoice {
            kind,
            setup: AgentSetup {
                model: model.map(Into::into),
                effort: effort.map(Into::into),
            },
        }
    }

    fn reason(slot: &str) -> String {
        format!("{slot} にはこの組み合わせが向いています")
    }

    /// Answers the oldest queued run with `picks`, through the shared
    /// validation and the widget's own check, and lets the screen take it.
    /// The run's request comes back.
    fn answer(
        screen: &mut Competition,
        queue: &Queue,
        picks: &[Answer<'_>],
        text: &mut TextStore,
    ) -> Request {
        let Planned { job, reply, .. } = queue.borrow_mut().remove(0);
        let request = (job.request)();
        let raw = picks
            .iter()
            .map(|(slot, agent, model, effort)| RawPick {
                slot: (*slot).into(),
                agent: (*agent).into(),
                model: model.map(Into::into),
                effort: effort.map(Into::into),
                reason: reason(slot),
            })
            .collect();
        let assignment = assign::validate(raw, &request, &job.planner, &*job.check)
            .unwrap_or_else(|e| panic!("{e:?}"));
        reply.send(Ok(assignment)).unwrap();
        screen.poll_live(text);
        request
    }

    /// Fails the oldest queued run with `error`.
    fn fail(screen: &mut Competition, queue: &Queue, error: PlanError, text: &mut TextStore) {
        let Planned { reply, .. } = queue.borrow_mut().remove(0);
        reply.send(Err(error)).unwrap();
        screen.poll_live(text);
    }

    fn auto_notes(screen: &Competition) -> Vec<String> {
        texts_under(&screen.tree, screen.start.auto_note)
    }

    const HEADING: &str = "自動割り当て（claude (sonnet · medium) が決定）— 内容を確認して「コンペティションを開始」を押してください";

    #[test]
    fn the_auto_assign_pull_down_offers_only_installed_planners() {
        // Waiting for the installed check: nothing to pick.
        let (mut screen, _link, tx, mut text) = live_pending();
        let _queue = fake_planner(&mut screen);
        assert!(!screen.tree.is_enabled(screen.auto.menu));
        assert!(screen.auto.planner_agents().is_empty());
        report(
            &mut screen,
            &tx,
            found(&[AgentKind::Codex, AgentKind::Agy]),
            &mut text,
        );
        assert_eq!(screen.auto.planner_agents(), ["codex"]);
        // No request yet: nothing to assign by, and the note says so.
        assert!(!screen.tree.is_enabled(screen.auto.menu));
        assert_eq!(auto_notes(&screen), ["依頼内容を入力してください"]);
        // Typing one turns it on.
        let area = screen.start.request;
        screen.tree.set_focus(Some(area));
        screen.dispatch(noa_gpui_core::Event::Text("LP".into()), &mut text);
        assert!(screen.tree.is_enabled(screen.auto.menu));
        assert!(auto_notes(&screen).is_empty());

        let (screen, _link, _queue, _text) = auto_screen(&AgentKind::ALL);
        assert_eq!(screen.auto.planner_agents(), ["claude", "codex"]);
        assert!(screen.tree.is_enabled(screen.auto.menu));

        // agy is never a planner.
        let (screen, _link, _queue, _text) = auto_screen(&[AgentKind::Agy]);
        assert!(screen.auto.planner_agents().is_empty());
        assert!(!screen.tree.is_enabled(screen.auto.menu));
        assert_eq!(
            auto_notes(&screen),
            ["自動割り当てに使えるエージェントがインストールされていません"]
        );
    }

    #[test]
    fn an_assignment_fills_every_judge_and_contestant_and_shows_each_reason() {
        let (mut screen, _link, queue, mut text) = five_rows();
        let menu = screen.auto.menu;
        screen.handle_action(Action::AutoAssign(0), &mut text);
        assert!(screen.auto.running());
        assert!(screen.tree.is_hidden(menu));
        assert!(!screen.tree.is_hidden(screen.auto.cancel));
        assert!(
            texts_under(&screen.tree, screen.start.auto_host)
                .contains(&"claude が割り当てを決めています…".to_owned())
        );
        assert_eq!(queue.borrow()[0].job.planner.agent, "claude");
        // The form stays editable while planning.
        assert!(screen.tree.is_enabled(screen.start.start_button));
        // The widget's own check is the one the start makes.
        let bad = Pick {
            slot: "judge-1".into(),
            agent: "claude".into(),
            model: Some("-rm".into()),
            effort: None,
            reason: "x".into(),
        };
        let why = (queue.borrow()[0].job.check)(&bad).unwrap_err();
        assert!(why.contains("のモデル名"), "{why}");

        let request = answer(&mut screen, &queue, &FIVE, &mut text);
        assert_eq!(request.widget, "competition");
        assert_eq!(
            request.run,
            "agents compete blind on the same task and judges score them"
        );
        let slots: Vec<(&str, &str)> = request
            .slots
            .iter()
            .map(|s| (s.id.as_str(), s.role.as_str()))
            .collect();
        assert_eq!(
            slots,
            [
                (
                    "judge-1",
                    "lead judge: scores every round, decides early finish and writes the final review"
                ),
                ("judge-2", "co-judge: scores every round"),
                (
                    "contestant-1",
                    "contestant: does the task blind, competing with the others"
                ),
                (
                    "contestant-2",
                    "contestant: does the task blind, competing with the others"
                ),
                (
                    "contestant-3",
                    "contestant: does the task blind, competing with the others"
                ),
            ]
        );
        let agents: Vec<&str> = request
            .candidates
            .iter()
            .map(|c| c.agent.as_str())
            .collect();
        assert_eq!(agents, ["claude", "codex"]);
        assert_eq!(request.candidates[1].models, AgentKind::Codex.models());
        assert!(request.content.contains(REQUEST), "{}", request.content);

        let form = start::read(&screen.tree, &screen.start);
        assert_eq!(
            form.judges(),
            [
                on(AgentKind::Claude, Some("opus"), Some("high")),
                on(AgentKind::Codex, Some("gpt-6.2-sol"), Some("xhigh")),
            ]
        );
        assert_eq!(
            form.panel,
            [
                on(AgentKind::Codex, Some("gpt-6.2-astra"), None),
                on(AgentKind::Claude, Some("sonnet"), Some("medium")),
                on(AgentKind::Claude, None, None),
            ]
        );
        assert_eq!(
            auto_notes(&screen),
            [
                HEADING.to_owned(),
                format!("審判 1: claude (opus · high) — {}", reason("judge-1")),
                format!(
                    "審判 2: codex (gpt-6.2-sol · xhigh) — {}",
                    reason("judge-2")
                ),
                format!(
                    "参加者 1: codex (gpt-6.2-astra) — {}",
                    reason("contestant-1")
                ),
                format!(
                    "参加者 2: claude (sonnet · medium) — {}",
                    reason("contestant-2")
                ),
                format!("参加者 3: claude — {}", reason("contestant-3")),
            ]
        );
        assert!(!screen.auto.running());
        assert!(!screen.tree.is_hidden(screen.auto.menu));
        // Each row's controls are the filled kind's: codex's efforts here.
        let effort = screen.co_judge_effort_items();
        assert_eq!(effort, AgentKind::Codex.efforts().len() + 1);
    }

    impl Competition {
        /// How many entries the second judge's effort menu has.
        fn co_judge_effort_items(&mut self) -> usize {
            let effort = self.start.co_judges[0].setup.effort;
            let mut text = TextStore::new(Default::default());
            menu_len(self, effort, &mut text)
        }
    }

    #[test]
    fn a_failed_assignment_leaves_the_form_and_names_its_cause() {
        let (mut screen, link, queue, mut text) = five_rows();
        let before = start::read(&screen.tree, &screen.start);
        screen.handle_action(Action::AutoAssign(1), &mut text);
        assert_eq!(queue.borrow()[0].job.planner.agent, "codex");
        fail(
            &mut screen,
            &queue,
            PlanError::Exit {
                code: Some(2),
                stderr_tail: "warming up\nnot logged in\n".into(),
            },
            &mut text,
        );
        assert_eq!(start::read(&screen.tree, &screen.start), before);
        assert_eq!(
            auto_notes(&screen),
            [
                "自動割り当てに失敗しました: codex (gpt-6.2-sol · medium) が終了コード 2 で終了しました（not logged in）。手動で選ぶか、別のエージェントで決めてください"
            ]
        );
        // An invalid answer is a failure too: nothing of it reaches the form.
        screen.handle_action(Action::AutoAssign(0), &mut text);
        let Planned { job, reply, .. } = queue.borrow_mut().remove(0);
        let request = (job.request)();
        let raw = vec![RawPick {
            slot: "judge-1".into(),
            agent: "claude".into(),
            model: Some("opus-9".into()),
            effort: None,
            reason: "x".into(),
        }];
        let error = assign::validate(raw, &request, &job.planner, &*job.check).unwrap_err();
        reply.send(Err(error)).unwrap();
        screen.poll_live(&mut text);
        assert_eq!(start::read(&screen.tree, &screen.start), before);
        let notes = auto_notes(&screen);
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].starts_with(
                "自動割り当てに失敗しました: claude (sonnet · medium) の割り当てが不正です: "
            ),
            "{notes:?}"
        );
        // A cancelled run says nothing, and nothing was started.
        screen.handle_action(Action::AutoAssign(0), &mut text);
        screen.handle_action(Action::CancelAutoAssign, &mut text);
        assert!(
            queue.borrow()[0]
                .cancel
                .load(std::sync::atomic::Ordering::Acquire)
        );
        assert!(auto_notes(&screen).is_empty());
        assert!(commands(&link).is_empty());
    }

    #[test]
    fn an_assignment_for_rows_changed_meanwhile_is_not_applied() {
        const CHANGED: &str = "割り当て中にエージェントの選択が変わったため反映しませんでした。もう一度「自動で割り当て」を押してください";
        // A row's kind changed by hand.
        let (mut screen, _link, queue, mut text) = five_rows();
        screen.handle_action(Action::AutoAssign(0), &mut text);
        let row = screen.start.panel[0].kind;
        press_down(&mut screen, row, &mut text);
        let edited = start::read(&screen.tree, &screen.start);
        assert_eq!(edited.panel[0].kind, AgentKind::Codex);
        answer(&mut screen, &queue, &FIVE, &mut text);
        assert_eq!(start::read(&screen.tree, &screen.start), edited);
        assert_eq!(auto_notes(&screen), [CHANGED]);

        // A row removed and another added: the same choices, other rows.
        let (mut screen, _link, queue, mut text) = five_rows();
        screen.handle_action(Action::AutoAssign(0), &mut text);
        screen.handle_action(Action::RemovePanelRow(2), &mut text);
        screen.handle_action(Action::AddPanelRow, &mut text);
        let edited = start::read(&screen.tree, &screen.start);
        answer(&mut screen, &queue, &FIVE, &mut text);
        assert_eq!(start::read(&screen.tree, &screen.start), edited);
        assert_eq!(auto_notes(&screen), [CHANGED]);
    }

    /// The whole `Command::Start` after a fill: rows still as filled are
    /// recorded with their reasons, by row (a removed row's slot is
    /// overridden; the rows after it keep theirs), and rows changed by hand
    /// are overridden.
    #[test]
    fn the_start_records_the_rows_still_as_filled_and_the_overridden_slots() {
        let (mut screen, link, queue, mut text) = five_rows();
        screen.handle_action(Action::AutoAssign(0), &mut text);
        answer(&mut screen, &queue, &FIVE, &mut text);
        // contestant-1's row goes; judge-2's kind is changed by hand.
        screen.handle_action(Action::RemovePanelRow(0), &mut text);
        // codex → claude, one up its menu.
        let judge_2 = screen.start.co_judges[0].kind;
        screen.tree.set_focus(Some(judge_2));
        screen.dispatch(
            noa_gpui_core::Event::KeyDown {
                key: noa_gpui_core::Key::ArrowUp,
                modifiers: Default::default(),
            },
            &mut text,
        );
        let notes = auto_notes(&screen);
        assert!(notes[2].ends_with("（手動で変更）"), "{notes:?}");
        assert!(notes[3].ends_with("（手動で変更）"), "{notes:?}");
        assert!(!notes[1].ends_with("（手動で変更）"), "{notes:?}");
        assert!(!notes[4].ends_with("（手動で変更）"), "{notes:?}");

        screen.handle_action(Action::Start, &mut text);
        let sent = commands(&link);
        assert_eq!(sent.len(), 1);
        let Command::Start {
            auto: Some(record), ..
        } = &sent[0]
        else {
            panic!("{sent:?}");
        };
        let pick = |i: usize| {
            let (slot, agent, model, effort) = FIVE[i];
            Pick {
                slot: slot.into(),
                agent: agent.into(),
                model: model.map(Into::into),
                effort: effort.map(Into::into),
                reason: reason(slot),
            }
        };
        let judge_2 = start::read(&screen.tree, &screen.start).co_judges[0].clone();
        assert_eq!(judge_2.kind, AgentKind::Claude);
        assert_eq!(
            sent[0],
            Command::Start {
                request: REQUEST.into(),
                workdir: None,
                max_rounds: Some(competition::model::types::DEFAULT_MAX_ROUNDS),
                reveal_interval: competition::model::types::DEFAULT_REVEAL_INTERVAL,
                judges: vec![on(AgentKind::Claude, Some("opus"), Some("high")), judge_2],
                panel: vec![
                    on(AgentKind::Claude, Some("sonnet"), Some("medium")),
                    on(AgentKind::Claude, None, None),
                ],
                auto: Some(AutoRecord {
                    planner: "claude (sonnet · medium)".into(),
                    at: record.at,
                    picks: vec![pick(0), pick(3), pick(4)],
                    overridden: vec!["judge-2".into(), "contestant-1".into()],
                }),
            }
        );
        assert!(record.at > 0);
    }

    #[test]
    fn a_start_without_an_assignment_or_after_a_new_form_records_none() {
        let (mut screen, link, queue, mut text) =
            auto_screen(&[AgentKind::Claude, AgentKind::Codex]);
        let starts = |link: &crate::live::FakeLink| -> Vec<Command> {
            commands(link)
                .into_iter()
                .filter(|c| matches!(c, Command::Start { .. }))
                .collect()
        };
        screen.handle_action(Action::Start, &mut text);
        let sent = starts(&link);
        assert!(
            matches!(&sent[..], [Command::Start { auto: None, .. }]),
            "{sent:?}"
        );
        // A fill, then a clean form: the lead's row is the same row, but the
        // form is new and staffed by hand.
        screen.handle_action(Action::AutoAssign(0), &mut text);
        answer(
            &mut screen,
            &queue,
            &[
                ("judge-1", "claude", None, None),
                ("contestant-1", "claude", None, None),
                ("contestant-2", "codex", None, None),
            ],
            &mut text,
        );
        screen.handle_action(Action::NewCompetition, &mut text);
        assert!(
            auto_notes(&screen)
                .iter()
                .all(|n| !n.starts_with("自動割り当て（"))
        );
        set_request(&mut screen, REQUEST, &mut text);
        screen.handle_action(Action::Start, &mut text);
        let sent = starts(&link);
        assert!(
            matches!(&sent[..], [Command::Start { auto: None, .. }]),
            "{sent:?}"
        );
    }

    /// Empties `field` and types `typed` into it, key by key, so each edit
    /// reaches the screen as the person's would.
    fn retype(screen: &mut Competition, field: WidgetId, typed: &str, text: &mut TextStore) {
        screen.tree.set_focus(Some(field));
        let erase = screen
            .tree
            .get::<noa_gpui_core::widgets::TextField>(field)
            .map_or(0, |f| f.text().chars().count());
        for _ in 0..erase {
            screen.dispatch(
                noa_gpui_core::Event::KeyDown {
                    key: noa_gpui_core::Key::Backspace,
                    modifiers: Default::default(),
                },
                text,
            );
        }
        if !typed.is_empty() {
            screen.dispatch(noa_gpui_core::Event::Text(typed.into()), text);
        }
        let form = start::read(&screen.tree, &screen.start);
        assert_eq!(form.workdir, typed);
    }

    /// Whether the pull-down is on, and the auto note.
    fn auto_control(screen: &Competition) -> (bool, Vec<String>) {
        (screen.tree.is_enabled(screen.auto.menu), auto_notes(screen))
    }

    #[test]
    fn an_unreadable_workdir_turns_the_pull_down_off_until_it_is_fixed() {
        const RELATIVE: &str = "foo は絶対パスではありません";
        let (mut screen, _link, queue, mut text) = auto_screen(&[AgentKind::Claude]);
        let field = screen.start.workdir;
        assert_eq!(auto_control(&screen), (true, vec![]));
        // A relative path: off, with the start's own message.
        retype(&mut screen, field, "foo", &mut text);
        assert_eq!(auto_control(&screen), (false, vec![RELATIVE.to_owned()]));
        // Asked anyway: no planner, and the message once.
        screen.handle_action(Action::AutoAssign(0), &mut text);
        assert!(queue.borrow().is_empty());
        assert_eq!(auto_control(&screen), (false, vec![RELATIVE.to_owned()]));
        // Cleared: on again, nothing left over.
        retype(&mut screen, field, "", &mut text);
        assert_eq!(auto_control(&screen), (true, vec![]));

        // A folder that is not there, then made: on again once it is.
        let root = temp_root("auto-workdir");
        let path = root.to_str().unwrap().to_owned();
        retype(&mut screen, field, &path, &mut text);
        assert_eq!(
            auto_control(&screen),
            (false, vec![format!("{path} はディレクトリではありません")])
        );
        std::fs::create_dir_all(&root).unwrap();
        retype(&mut screen, field, "", &mut text);
        retype(&mut screen, field, &path, &mut text);
        assert_eq!(auto_control(&screen), (true, vec![]));
        // The planner runs there.
        screen.handle_action(Action::AutoAssign(0), &mut text);
        assert_eq!(queue.borrow()[0].job.cwd, root);

        // A planner's failure gives way to the form's message, and fixing
        // the form leaves neither.
        fail(&mut screen, &queue, PlanError::Timeout(180), &mut text);
        let (on, notes) = auto_control(&screen);
        assert!(on);
        assert!(
            matches!(&notes[..], [n] if n.starts_with("自動割り当てに失敗しました: ")),
            "{notes:?}"
        );
        retype(&mut screen, field, "foo", &mut text);
        assert_eq!(auto_control(&screen), (false, vec![RELATIVE.to_owned()]));
        retype(&mut screen, field, &path, &mut text);
        assert_eq!(auto_control(&screen), (true, vec![]));

        // An empty request asked anyway: its message once, gone once one
        // is typed.
        set_request(&mut screen, "", &mut text);
        screen.handle_action(Action::AutoAssign(0), &mut text);
        assert_eq!(
            auto_control(&screen),
            (false, vec!["依頼内容を入力してください".to_owned()])
        );
        set_request(&mut screen, REQUEST, &mut text);
        assert_eq!(auto_control(&screen), (true, vec![]));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_later_planning_run_keeps_the_applied_assignment_until_the_start() {
        let (mut screen, link, queue, mut text) = five_rows();
        screen.handle_action(Action::AutoAssign(0), &mut text);
        answer(&mut screen, &queue, &FIVE, &mut text);
        let filled = start::read(&screen.tree, &screen.start);
        // A second run fails: the block of the first stays, with the cause.
        screen.handle_action(Action::AutoAssign(1), &mut text);
        assert!(auto_notes(&screen).contains(&HEADING.to_owned()));
        fail(&mut screen, &queue, PlanError::Timeout(180), &mut text);
        let notes = auto_notes(&screen);
        assert!(notes[0].contains("180 秒以内に"), "{notes:?}");
        assert_eq!(notes[1], HEADING);
        // A third is cancelled; a fourth is still running at the start,
        // which stops it and records the first fill.
        screen.handle_action(Action::AutoAssign(0), &mut text);
        screen.handle_action(Action::CancelAutoAssign, &mut text);
        assert_eq!(auto_notes(&screen)[0], HEADING);
        screen.handle_action(Action::AutoAssign(0), &mut text);
        screen.handle_action(Action::Start, &mut text);
        let running = queue.borrow_mut().pop().unwrap();
        assert!(running.cancel.load(std::sync::atomic::Ordering::Acquire));
        assert!(!screen.auto.running());
        assert_eq!(start::read(&screen.tree, &screen.start), filled);
        let sent = commands(&link);
        let [
            Command::Start {
                auto: Some(record), ..
            },
        ] = &sent[..]
        else {
            panic!("{sent:?}");
        };
        assert_eq!(record.picks.len(), FIVE.len());
        assert!(record.overridden.is_empty());
        // Its late answer is not taken.
        let _ = running.reply.send(Err(PlanError::Timeout(1)));
        screen.poll_live(&mut text);
        assert_eq!(auto_notes(&screen)[0], HEADING);
    }

    #[test]
    fn planning_takes_focus_to_cancel_and_back_on_controls_made_once() {
        let (mut screen, _link, queue, mut text) = five_rows();
        let (menu, cancel) = (screen.auto.menu, screen.auto.cancel);
        screen.rebuild(&mut text);
        assert_eq!((screen.auto.menu, screen.auto.cancel), (menu, cancel));
        // Chosen from the keyboard: the menu opens, a rebuild comes in
        // meanwhile, and the choice still reaches the screen.
        screen.prepare(VIEW, &mut text);
        screen.tree.set_focus(Some(menu));
        let key = |screen: &mut Competition, key, text: &mut TextStore| {
            screen.dispatch(
                noa_gpui_core::Event::KeyDown {
                    key,
                    modifiers: Default::default(),
                },
                text,
            );
        };
        key(&mut screen, noa_gpui_core::Key::ArrowDown, &mut text);
        screen.rebuild(&mut text);
        screen.prepare(VIEW, &mut text);
        key(&mut screen, noa_gpui_core::Key::ArrowDown, &mut text);
        key(&mut screen, noa_gpui_core::Key::Enter, &mut text);
        assert_eq!(queue.borrow().len(), 1, "the choice was dropped");
        assert_eq!(queue.borrow()[0].job.planner.agent, "claude");
        assert_eq!(screen.tree.focused(), Some(cancel));
        screen.rebuild(&mut text);
        assert_eq!(screen.tree.focused(), Some(cancel));
        answer(&mut screen, &queue, &FIVE, &mut text);
        assert_eq!(screen.tree.focused(), Some(menu));
        // 中止 too gives the focus back.
        screen.handle_action(Action::AutoAssign(0), &mut text);
        assert_eq!(screen.tree.focused(), Some(cancel));
        screen.handle_action(Action::CancelAutoAssign, &mut text);
        assert_eq!(screen.tree.focused(), Some(menu));
        assert_eq!(screen.auto.menu, menu);
    }

    #[test]
    fn leaving_the_start_view_stops_the_planner() {
        let root = temp_root("auto-leave");
        let dir = stored_competition(&root);
        let (mut screen, _link, mut text) = live_on(&root);
        let queue = fake_planner(&mut screen);
        screen.poll_live(&mut text);
        set_request(&mut screen, REQUEST, &mut text);
        screen.handle_action(Action::AutoAssign(0), &mut text);
        assert!(screen.auto.running());
        screen.handle_action(Action::OpenPast(dir.clone()), &mut text);
        assert!(screen.session.is_some());
        assert!(!screen.auto.running());
        assert!(
            queue.borrow()[0]
                .cancel
                .load(std::sync::atomic::Ordering::Acquire)
        );
        // Resuming one leaves too.
        screen.handle_action(Action::NewCompetition, &mut text);
        set_request(&mut screen, REQUEST, &mut text);
        screen.handle_action(Action::AutoAssign(0), &mut text);
        screen.handle_action(Action::ResumePast(dir), &mut text);
        assert!(!screen.auto.running());
        assert!(
            queue.borrow()[1]
                .cancel
                .load(std::sync::atomic::Ordering::Acquire)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_demo_starts_no_planner() {
        let mut screen = Competition::demo_start();
        let queue = fake_planner(&mut screen);
        let mut text = TextStore::new(Default::default());
        screen.rebuild(&mut text);
        assert!(!screen.tree.is_enabled(screen.auto.menu));
        screen.handle_action(Action::AutoAssign(0), &mut text);
        assert!(queue.borrow().is_empty());
        assert!(!screen.auto.running());
    }
}
