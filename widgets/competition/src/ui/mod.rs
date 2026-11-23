//! The competition window: leaderboard | compare / timeline | judge board,
//! with an intervention composer and a start view before any session exists.

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
#[cfg(test)]
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

use competition::model::export;
use competition::model::types::{
    AgentChoice, AgentKind, AgentSetup, MAX_PANEL, Millis, PartId, Phase, RoundKind, RoundNo,
    Session,
};
use competition::store;

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
}

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
    /// What the board was last built from: the session revision, the link,
    /// and the clock minute while some agent's elapsed time is running.
    built: Option<BuiltKey>,

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

    pub fn live(channel: Channel) -> Self {
        Self::build(Source::Live(channel), Link::Connecting, None, None)
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

    /// The competition logged in `dir`, read-only.
    pub fn stored(dir: &Path) -> Result<Self, String> {
        let mut screen = Self::build(Source::Snapshot, Link::Live, None, None);
        let mut text = TextStore::new(Default::default());
        screen.show_stored(dir, &mut text)?;
        Ok(screen)
    }

    /// The live screen, showing the competition logged in `dir` read-only
    /// (the start view, with why, when it cannot be read).
    pub fn live_stored(channel: Channel, dir: &Path) -> Self {
        let mut screen = Self::live(channel);
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
            built: None,
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
            self.rebuild_left(text);
            self.rebuild_progress(text);
            self.rebuild_center(text);
            self.rebuild_right(text);
            self.rebuild_composer(text);
        } else {
            start::rebuild_note(&mut self.tree, &self.start, &self.link, text);
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
            Source::Live(_) | Source::Snapshot => store::default_root()
                .map(|root| store::list_sessions(&root, start::PAST_LIMIT))
                .unwrap_or_default(),
        };
    }

    /// The session logged in `dir` (the demo's own, in the demo).
    fn load_stored(&self, dir: &Path) -> Result<(Session, Option<PathBuf>), String> {
        if let Source::Demo = self.source {
            return Ok((crate::demo::session(), None));
        }
        let session =
            store::read_session(dir).map_err(|e| format!("{} を開けません: {e}", dir.display()))?;
        let root = store::reply_dir(dir, &session);
        Ok((session, Some(root)))
    }

    /// Leaves whatever competition is on screen: its runner stops (an
    /// unfinished one stays resumable from the list).
    fn leave(&mut self) {
        if let Source::Live(channel) = &self.source {
            channel.leave();
            if !self.read_only && self.session.as_ref().is_some_and(|s| !s.closed) {
                self.toast("コンペを中断しました（過去のコンペから再開できます）");
            }
        }
        self.session = None;
        self.artifact_root = None;
        self.read_only = false;
        self.tab = CenterTab::Compare;
        self.selected_round = None;
        self.filter_part = None;
        self.pending_intervention = None;
        self.refresh_past();
    }

    fn show_stored(&mut self, dir: &Path, text: &mut TextStore) -> Result<(), String> {
        let (session, root) = self.load_stored(dir)?;
        self.leave();
        self.session = Some(Arc::new(session));
        self.artifact_root = root;
        self.read_only = true;
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
        }
    }

    fn poll_live(&mut self, text: &mut TextStore) {
        let mut latest = None;
        let mut notices = Vec::new();
        if let Source::Live(channel) = &self.source {
            // Snapshots sent before the screen last left a competition
            // belong to that one.
            let current = channel.generation();
            while let Ok(snapshot) = channel.snapshots.try_recv() {
                if snapshot.generation >= current {
                    latest = Some(snapshot);
                }
            }
            while let Ok(notice) = channel.notices.try_recv() {
                notices.push(notice);
            }
        }
        for notice in notices {
            self.toast(notice);
        }
        if let Some(snapshot) = latest {
            self.link = snapshot.link;
            if self.read_only {
                // No runner drives a read-only session.
                return self.rebuild_if_changed(text);
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
        let base_dir = match &self.artifact_root {
            Some(root) => root.clone(),
            None => {
                // Demo mode: write into a temporary directory so the user can still
                // preview the comparison viewer UI immediately.
                let dir = std::env::temp_dir().join(format!("noa-comp-viewer-{}", session.id));
                dir
            }
        };
        match store::write_viewer(&base_dir, session) {
            Ok(path) => self.open_in_os(&path),
            Err(e) => self.toast(format!("比較ビューアーの生成に失敗しました: {e}")),
        }
    }

    fn handle_action(&mut self, action: Action, text: &mut TextStore) {
        match action {
            Action::Start => {
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
                self.send_cmd(Command::Start {
                    request: form.request,
                    workdir,
                    max_rounds: Some(form.max_rounds),
                    reveal_interval: form.reveal_interval,
                    judge: form.judge,
                    panel: form.panel,
                });
            }
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
            Action::OpenPast(dir) => {
                if let Err(why) = self.show_stored(&dir, text) {
                    self.toast(why);
                }
            }
            Action::ResumePast(dir) => self.send_cmd(Command::Resume { dir }),
            Action::RematchPast(dir) => match self.load_stored(&dir) {
                Ok((session, _)) => {
                    let form = start::Form::of_session(&session);
                    start::fill(&mut self.tree, &mut self.start, &form, text);
                    self.rebuild(text);
                    self.toast("前回の設定をフォームに入れました");
                }
                Err(why) => self.toast(why),
            },
            Action::NewCompetition => {
                self.leave();
                start::fill(
                    &mut self.tree,
                    &mut self.start,
                    &start::Form::default(),
                    text,
                );
                self.rebuild(text);
            }
            Action::Rematch => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                self.leave();
                let form = start::Form::of_session(&session);
                start::fill(&mut self.tree, &mut self.start, &form, text);
                self.rebuild(text);
            }
            Action::FocusPane(part) => self.send_cmd(Command::Focus { part }),
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
        if message.source == self.start.judge
            && message.downcast_ref::<SelectionChanged>().is_some()
        {
            start::judge_kind_changed(&mut self.tree, &mut self.start, text);
            return;
        }
        if message.downcast_ref::<SelectionChanged>().is_some()
            && start::row_kind_changed(&mut self.tree, &mut self.start, message.source, text)
        {
            return;
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
            max_rounds: Some(3),
            reveal_interval: 5,
            workdir: None,
            judge: AgentKind::Codex.into(),
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

    /// The fixture stays on disk when `COMPETITION_FIXTURE_OUT` names a
    /// directory, for `--session=DIR --snapshot=…`.
    #[test]
    fn a_stored_competition_opens_read_only() {
        let keep = std::env::var_os("COMPETITION_FIXTURE_OUT").map(PathBuf::from);
        let root = keep.clone().unwrap_or_else(|| temp_root("stored"));
        let _ = std::fs::remove_dir_all(&root);
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
        let (_, contestants) = competition::engine::participants(&form.judge, &form.panel);
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
        assert!(screen.read_only && screen.session.is_some());
        assert!(!screen.tree.is_hidden(screen.board));
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
        let (_, contestants) = competition::engine::participants(&panel[0], &panel);
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

    #[test]
    fn a_row_kind_change_offers_that_kinds_models() {
        let mut screen = Competition::demo_start();
        let mut text = TextStore::new(Default::default());
        let row = screen.start.panel[1].kind;
        screen
            .tree
            .update::<noa_gpui_core::widgets::PopUpButton, _>(row, |p, cx| p.set_selected(cx, 1));
        assert!(start::row_kind_changed(
            &mut screen.tree,
            &mut screen.start,
            row,
            &mut text
        ));
        let panel = start::read(&screen.tree, &screen.start).panel;
        assert_eq!(panel[1], AgentChoice::from(AgentKind::Codex));
        assert_eq!(panel[0].setup.model.as_deref(), Some("opus"));
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
        let (channel, snapshots) = Channel::fake();
        let mut screen = Competition::live(channel);
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
}
