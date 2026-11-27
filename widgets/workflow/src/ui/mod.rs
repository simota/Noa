mod canvas;
mod editor;
mod evidence;
mod style;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::PathBuf;

use noa_gpui_core::style::{AlignItems, FlexDirection, Size as LayoutSize, length};
use noa_gpui_core::widgets::{
    Button, ButtonClicked, Container, ControlSize, Label, PopUpButton, ScrollView,
    SelectionChanged, TextArea, TextChanged, TextField, TextView,
};
use noa_gpui_core::{Event, Key, Message, PointerButton, Role, Style, UiTree, WidgetId};
use noa_gpui_platform::Screen;
use noa_gpui_scene::{Point, Rect, Size};
use noa_gpui_text::TextStore;
use workflow::model::{AgentChoice, AgentPreset, NodeKind, NodeState, WorkflowDefinition};
use workflow::runner::{Channel, Connection, Intent, View};

use editor::Editor;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Build,
    Run,
    History,
}

#[derive(Clone, PartialEq)]
enum Action {
    Tab(Tab),
    Select(String),
    SavedRun,
    Settings,
    Add(NodeKind),
    Palette,
    Source(String),
    Connect(String),
    Disconnect(String, String),
    Delete(String),
    Undo,
    Redo,
    Arrange,
    Zoom(f32),
    Fit,
    Details,
    Apply,
    Save,
    Validate,
    Start,
    Pause,
    Resume,
    Cancel,
    ConfirmCancel,
    Approve(String, String),
    Retry(String),
    Repair,
    Reconcile(String),
    Focus(String),
    Answer(String),
    Refresh,
    Reload,
    Evidence(PathBuf, String, String),
    PreviewText(String, String),
    ClosePreview,
    SavePreset,
    ApplyPreset(usize),
    OpenTask,
}

#[derive(Clone)]
enum Field {
    Title,
    Instruction,
    Timeout,
    Retries,
    Name,
    Duration,
    Parallel,
    Repairs,
    Cost,
    Objective,
    Provider,
    Model,
    Effort,
    PresetName,
    Answer(String),
}

#[derive(Default, Clone)]
struct Form {
    node: Option<String>,
    title: String,
    instruction: String,
    timeout: String,
    retries: String,
    name: String,
    duration: String,
    parallel: String,
    repairs: String,
    cost: String,
    agent: AgentChoice,
    dirty: bool,
}

struct Drag {
    node: String,
    pointer: Point,
    origin: workflow::model::Point,
    before: WorkflowDefinition,
}

pub(crate) struct WorkflowScreen {
    tree: UiTree,
    header: WidgetId,
    status: WidgetId,
    footer: WidgetId,
    body: WidgetId,
    palette: WidgetId,
    canvas_host: Option<WidgetId>,
    canvas: Option<canvas::Canvas>,
    inspector: Option<WidgetId>,
    actions: Vec<(WidgetId, Action)>,
    fields: Vec<(WidgetId, Field)>,
    origin_task: Option<workflow::tasks::TaskOrigin>,
    preset_name: String,
    save_button: WidgetId,
    start_button: WidgetId,
    editor: Editor,
    view: View,
    channel: Option<Channel>,
    demo: bool,
    tab: Tab,
    selected_run: Option<String>,
    preparing_run: bool,
    start_pending: bool,
    objective: String,
    answers: BTreeMap<String, String>,
    form: Form,
    notice: String,
    zoom: f32,
    narrow: bool,
    detail_only: bool,
    show_palette: bool,
    viewport: Size,
    built: bool,
    deferred_inspector: bool,
    drag: Option<Drag>,
    connecting: bool,
    confirm_cancel: bool,
    evidence_preview: Option<(String, String)>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
fn short(value: &str) -> String {
    value.chars().take(12).collect()
}
fn clipped(value: &str, count: usize) -> String {
    let mut chars = value.chars();
    let part: String = chars.by_ref().take(count).collect();
    if chars.next().is_some() {
        format!("{part}…")
    } else {
        part
    }
}

impl WorkflowScreen {
    pub fn with_task(mut self, origin: workflow::tasks::TaskOrigin, objective: String) -> Self {
        self.origin_task = Some(origin);
        self.objective = objective;
        self.preparing_run = true;
        self.selected_run = None;
        self.start_pending = false;
        self.tab = Tab::Run;
        self.detail_only = true;
        self
    }

    pub fn saved(run: workflow::model::Run) -> Self {
        let mut screen = Self::new(
            View {
                repository: run.repository.clone(),
                definition: run.definition.clone(),
                version: 0,
                run: Some(run),
                connection: Connection::Disconnected("保存済みの実行を閲覧中".into()),
                notice: Some(
                    "保存済みの実行です。エージェントの起動や結果の再利用は行いません。".into(),
                ),
                can_control: false,
                deferred: false,
                presets: Vec::new(),
            },
            None,
            false,
        );
        screen.tab = Tab::Run;
        screen.preparing_run = false;
        screen
    }
    pub fn demo(kind: &str) -> Self {
        let run = match kind {
            "approval" => Some(workflow::engine::demo_waiting(now())),
            "failed" => Some(workflow::engine::demo_failed(now())),
            _ => None,
        };
        let definition = run
            .as_ref()
            .map_or_else(WorkflowDefinition::standard, |r| r.definition.clone());
        let view = View {
            repository: PathBuf::from("/workspace/Noa"),
            definition,
            version: 1,
            run,
            connection: Connection::Connected,
            notice: None,
            can_control: true,
            deferred: false,
            presets: Vec::new(),
        };
        let mut screen = Self::new(view, None, true);
        if kind != "editor" {
            screen.tab = Tab::Run;
            screen.selected_run = Some(
                if kind == "failed" {
                    "verify"
                } else {
                    "approval"
                }
                .into(),
            );
        }
        screen
    }

    pub fn live(repository: PathBuf, channel: Channel) -> Self {
        let view = channel.poll().unwrap_or(View {
            repository,
            definition: WorkflowDefinition::standard(),
            version: 0,
            run: None,
            connection: Connection::Connecting,
            notice: None,
            can_control: false,
            deferred: false,
            presets: Vec::new(),
        });
        Self::new(view, Some(channel), false)
    }

    pub fn unavailable(repository: PathBuf, error: String) -> Self {
        Self::new(
            View {
                repository,
                definition: WorkflowDefinition::standard(),
                version: 0,
                run: None,
                connection: Connection::Disconnected(error.clone()),
                notice: Some(error),
                can_control: false,
                deferred: false,
                presets: Vec::new(),
            },
            None,
            false,
        )
    }

    fn new(view: View, channel: Option<Channel>, demo: bool) -> Self {
        let mut tree = UiTree::new();
        crate::appearance::install(&mut tree, style::theme());
        let root = tree.insert(
            None,
            Container::new(style::root()).background(Role::Background),
        );
        let top = tree.insert(
            Some(root),
            Container::wrapping_row(20.0, 10.0)
                .padding(crate::appearance::space(20.0))
                .align_items(AlignItems::CENTER),
        );
        let title = tree.insert(
            Some(top),
            Container::column()
                .grow(1.0)
                .gap(crate::appearance::space(5.0)),
        );
        tree.insert(
            Some(title),
            Label::new("N O A   /   D E V E L O P M E N T")
                .font_size(crate::appearance::font(10.0))
                .weight(600)
                .color(Role::Muted),
        );
        let header = tree.insert(
            Some(title),
            Label::new("Workflow")
                .font_size(crate::appearance::font(26.0))
                .weight(650),
        );
        let buttons = tree.insert(
            Some(top),
            Container::row()
                .gap(crate::appearance::space(8.0))
                .align_items(AlignItems::CENTER),
        );
        let save_button = tree.insert(Some(buttons), Button::new("保存"));
        let validate = tree.insert(Some(buttons), Button::new("検証"));
        let start_button = tree.insert(Some(buttons), Button::new("実行を準備"));
        let nav = tree.insert(
            Some(root),
            Container::wrapping_row(8.0, 6.0)
                .padding(crate::appearance::space(12.0))
                .background(Role::Surface),
        );
        let build = tree.insert(Some(nav), Button::new("構築").selected(true));
        let run = tree.insert(Some(nav), Button::new("実行"));
        let history = tree.insert(Some(nav), Button::new("履歴"));
        let status = tree.insert(
            Some(nav),
            Label::new("")
                .font_size(crate::appearance::font(12.0))
                .color(Role::Muted),
        );
        let palette = tree.insert(
            Some(root),
            Container::wrapping_row(8.0, 8.0)
                .padding(crate::appearance::space(12.0))
                .background(Role::SurfaceAlt),
        );
        let body = tree.insert(Some(root), Container::new(style::column()));
        let bottom = tree.insert(
            Some(root),
            Container::row()
                .padding(crate::appearance::space(10.0))
                .background(Role::Surface),
        );
        let footer = tree.insert(
            Some(bottom),
            Label::new("")
                .font_size(crate::appearance::font(11.0))
                .color(Role::Muted)
                .wrap(true)
                .max_lines(3),
        );
        let mut actions = vec![
            (save_button, Action::Save),
            (validate, Action::Validate),
            (start_button, Action::Tab(Tab::Run)),
            (build, Action::Tab(Tab::Build)),
            (run, Action::Tab(Tab::Run)),
            (history, Action::Tab(Tab::History)),
        ];
        for kind in NodeKind::ALL {
            let button = tree.insert(
                Some(palette),
                Button::new(format!("＋ {}", kind.label())).control_size(ControlSize::Small),
            );
            actions.push((button, Action::Add(kind)));
        }
        tree.set_hidden(palette, true);
        let editor = Editor::new(view.definition.clone(), view.version);
        let notice = view.notice.clone().unwrap_or_default();
        let preparing_run = view.run.is_none();
        Self {
            tree,
            header,
            status,
            footer,
            body,
            palette,
            canvas_host: None,
            canvas: None,
            inspector: None,
            actions,
            fields: Vec::new(),
            origin_task: None,
            preset_name: String::new(),
            save_button,
            start_button,
            editor,
            view,
            channel,
            demo,
            tab: Tab::Build,
            selected_run: None,
            preparing_run,
            start_pending: false,
            objective: String::new(),
            answers: BTreeMap::new(),
            form: Form::default(),
            notice,
            zoom: 0.9,
            narrow: false,
            detail_only: false,
            show_palette: false,
            viewport: Size::new(1280.0, 860.0),
            built: false,
            deferred_inspector: false,
            drag: None,
            connecting: false,
            confirm_cancel: false,
            evidence_preview: None,
        }
    }

    fn label(
        &mut self,
        parent: WidgetId,
        text: impl Into<String>,
        size: f32,
        muted: bool,
    ) -> WidgetId {
        self.tree.insert(
            Some(parent),
            Label::new(text)
                .font_size(crate::appearance::font(size))
                .color(if muted { Role::Muted } else { Role::Text })
                .wrap(true),
        )
    }
    fn button(&mut self, parent: WidgetId, title: impl Into<String>, action: Action) -> WidgetId {
        let id = self.tree.insert(
            Some(parent),
            Button::new(title).control_size(ControlSize::Small),
        );
        self.actions.push((id, action));
        id
    }
    fn section(&mut self, parent: WidgetId, title: &str) {
        self.tree.insert(
            Some(parent),
            Label::new(title)
                .font_size(crate::appearance::font(11.0))
                .weight(650)
                .color(Role::Muted),
        );
    }

    fn evidence_buttons(&mut self, host: WidgetId, artifact: &workflow::model::Artifact) {
        if let (Some(path), Some(hash)) = (
            &artifact.path,
            artifact.data.get("report_sha256").and_then(|v| v.as_str()),
        ) {
            self.button(
                host,
                "構造化された報告を読む",
                Action::Evidence(path.clone(), hash.into(), "報告書".into()),
            );
        }
        if let Some(files) = artifact.data.get("evidence").and_then(|v| v.as_array()) {
            for file in files {
                let (Some(path), Some(hash)) = (
                    file.get("path").and_then(|v| v.as_str()),
                    file.get("sha256").and_then(|v| v.as_str()),
                ) else {
                    continue;
                };
                let label = file
                    .get("criterion")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        PathBuf::from(path)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string()
                    });
                let label =
                    if file.get("source").and_then(|v| v.as_str()) == Some("captured_git_diff") {
                        "開始時からのcommit差分".to_string()
                    } else {
                        label
                    };
                if let Some(exit) = file.get("exit_code").and_then(|v| v.as_i64()) {
                    self.label(
                        host,
                        format!("終了コード {exit} · {}", clipped(&label, 80)),
                        11.0,
                        true,
                    );
                }
                self.button(
                    host,
                    format!("根拠を読む · {}", clipped(&label, 22)),
                    Action::Evidence(PathBuf::from(path), hash.into(), label),
                );
            }
        }
    }
    fn input(&mut self, parent: WidgetId, label: &str, value: String, field: Field) -> WidgetId {
        self.label(parent, label, 11.0, true);
        let id = self.tree.insert(Some(parent), TextField::new(label, 260.0));
        self.tree
            .update::<TextField, _>(id, |f, cx| f.set_text(cx, value));
        self.tree.set_accessible_label(id, label);
        self.fields.push((id, field));
        id
    }
    fn area(
        &mut self,
        parent: WidgetId,
        label: &str,
        value: String,
        field: Field,
        lines: u32,
    ) -> WidgetId {
        self.label(parent, label, 11.0, true);
        let id = self.tree.insert(
            Some(parent),
            TextArea::new(label).width(260.0).min_lines(lines),
        );
        self.tree
            .update::<TextArea, _>(id, |f, cx| f.set_text(cx, value));
        self.tree.set_accessible_label(id, label);
        self.fields.push((id, field));
        id
    }
    fn clear(&mut self, parent: WidgetId, text: &mut TextStore) {
        for child in self.tree.children(parent).to_vec() {
            self.tree.remove(child, text);
        }
        self.actions.retain(|(id, _)| self.tree.contains(*id));
        self.fields.retain(|(id, _)| self.tree.contains(*id));
    }

    fn status(&mut self) {
        let repo = self
            .view
            .repository
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let changed = self.editor.dirty() || self.form.dirty;
        let title = format!(
            "{}{}",
            self.editor.definition.name,
            if changed { "  ·" } else { "" }
        );
        self.tree
            .update::<Label, _>(self.header, |label, cx| label.set_text(cx, title));
        let connection = if self.demo {
            "表示サンプル"
        } else if !self.view.can_control && self.channel.is_some() {
            "監視中 · 実行サービスの操作権を待機"
        } else if self.view.deferred {
            "開始待ち · 保存済み実行は自動再開しません"
        } else {
            match self.view.connection {
                Connection::Connected => "Noa 接続済み",
                Connection::Connecting => "接続中",
                Connection::Disconnected(_) => "Noa 未接続",
            }
        };
        let state = if self.preparing_run {
            "新しい実行の準備"
        } else {
            self.view
                .run
                .as_ref()
                .map_or("実行なし", |r| r.state.label())
        };
        let line = format!("{repo}   /   {connection}   /   {state}");
        self.tree
            .update::<Label, _>(self.status, |label, cx| label.set_text(cx, line));
        let validation = self.editor.definition.validate();
        let default = if self.editor.conflict {
            "保存競合があります。ドラフトを保持しています。設定から保存済みの内容を確認してください。".into()
        } else if let Some(source) = &self.editor.source {
            format!("接続元: {source}  ·  接続先の入力ポートを選択  ·  Escで解除")
        } else if !validation.is_empty() {
            format!(
                "実行前に修正が必要: {}件  ·  {}",
                validation.len(),
                validation[0].message
            )
        } else if self.demo {
            "表示サンプル · 編集操作を試せます。実エージェントの起動・PR作成は行いません。".into()
        } else if changed {
            "未保存の編集があります · ⌘/Ctrl S で保存 · 実行中のフローには影響しません".into()
        } else {
            "保存済み · ポートをつないで構築 · ノードをドラッグ · 矢印キーで移動 · ⌘/Ctrl Zで戻す"
                .into()
        };
        let footer = if self.notice.is_empty() {
            default
        } else {
            self.notice.clone()
        };
        self.tree
            .update::<Label, _>(self.footer, |label, cx| label.set_text(cx, footer));
        self.tree.set_enabled(
            self.save_button,
            (self.demo || self.channel.is_some())
                && self.view.can_control
                && changed
                && !self.editor.conflict
                && self.editor.pending_save.is_none(),
        );
        self.tree
            .update::<Button, _>(self.start_button, |button, cx| {
                button.set_label(
                    cx,
                    if self
                        .view
                        .run
                        .as_ref()
                        .is_some_and(|r| r.nodes.values().any(|n| n.state.active()))
                    {
                        "実行を表示"
                    } else {
                        "実行を準備"
                    },
                )
            });
        for (id, action) in &self.actions {
            match action {
                Action::Tab(tab) => {
                    self.tree
                        .update::<Button, _>(*id, |b, cx| b.set_selected(cx, *tab == self.tab));
                }
                Action::Undo => self
                    .tree
                    .set_enabled(*id, self.editor.can_undo() || self.form.dirty),
                Action::Redo => self.tree.set_enabled(*id, self.editor.can_redo()),
                Action::Start => self.tree.set_enabled(
                    *id,
                    !self.demo
                        && self.channel.is_some()
                        && self.view.can_control
                        && self.can_start_run()
                        && !self.start_pending
                        && !changed
                        && !self.editor.conflict
                        && self.editor.definition.validate().is_empty(),
                ),
                Action::SavePreset => self.tree.set_enabled(
                    *id,
                    self.demo || (self.channel.is_some() && self.view.can_control),
                ),
                Action::Resume => self.tree.set_enabled(
                    *id,
                    !self.demo && self.channel.is_some() && self.view.can_control,
                ),
                Action::Pause
                | Action::Cancel
                | Action::ConfirmCancel
                | Action::Approve(..)
                | Action::Retry(_)
                | Action::Repair
                | Action::Reconcile(_)
                | Action::Focus(_)
                | Action::Answer(_) => self.tree.set_enabled(
                    *id,
                    !self.demo
                        && self.channel.is_some()
                        && self.view.can_control
                        && !self.view.deferred,
                ),
                _ => {}
            }
        }
        self.tree
            .set_hidden(self.palette, !self.show_palette || self.tab != Tab::Build);
    }

    fn build_body(&mut self, text: &mut TextStore) {
        self.clear(self.body, text);
        self.canvas_host = None;
        self.canvas = None;
        self.inspector = None;
        if let Some((title, body)) = self.evidence_preview.clone() {
            let bar = self.tree.insert(
                Some(self.body),
                Container::row()
                    .padding(crate::appearance::space(16.0))
                    .gap(crate::appearance::space(16.0))
                    .align_items(AlignItems::CENTER),
            );
            self.button(bar, "← ワークフローへ戻る", Action::ClosePreview);
            self.label(bar, title, 17.0, false);
            let pane = self.tree.insert(
                Some(self.body),
                Container::new(style::column()).padding(crate::appearance::space(20.0)),
            );
            self.label(
                pane,
                "読み取り専用 · 内容を選択してコピーできます",
                11.0,
                true,
            );
            let view = self.tree.insert(
                Some(pane),
                TextView::new(400.0).fill_height().read_only(true),
            );
            self.tree
                .update::<TextView, _>(view, |v, cx| v.set_text(cx, body));
            self.tree.set_accessible_label(view, "成果物と検証ログ");
            self.tree.set_focus(Some(view));
            self.status();
            return;
        }
        if self.channel.is_none() && !self.demo {
            let content = self.tree.insert(
                Some(self.body),
                Container::column()
                    .padding(crate::appearance::space(32.0))
                    .gap(crate::appearance::space(16.0)),
            );
            self.label(content, "ワークフローを開けませんでした", 24.0, false);
            self.label(
                content,
                self.view
                    .notice
                    .clone()
                    .unwrap_or_else(|| "実行サービスに接続できません".into()),
                14.0,
                true,
            );
            self.label(content,"保存済みのデータは上書きしていません。原因を解消して、NoaからOpen Workflowをもう一度開いてください。",13.0,true);
            self.status();
            return;
        }
        if self.tab == Tab::History {
            self.build_history();
            self.status();
            return;
        }
        let toolbar = self.tree.insert(
            Some(self.body),
            Container::wrapping_row(6.0, 6.0).padding(crate::appearance::space(10.0)),
        );
        if self.tab == Tab::Build {
            self.button(toolbar, "＋ 工程", Action::Palette);
            self.button(toolbar, "設定", Action::Settings);
            self.button(toolbar, "元に戻す", Action::Undo);
            self.button(toolbar, "やり直す", Action::Redo);
            self.button(toolbar, "整列", Action::Arrange);
        } else {
            self.button(toolbar, "再接続", Action::Refresh);
            if let Some(run) = &self.view.run {
                let state = run.state;
                if self.view.deferred {
                    if !self.preparing_run
                        && !run.cancel_requested
                        && !matches!(state, NodeState::Succeeded | NodeState::Canceled)
                    {
                        self.button(toolbar, "以前の実行を再開", Action::Resume);
                    }
                } else if matches!(state, NodeState::Paused) && run.limit_reason.is_none() {
                    self.button(toolbar, "再開", Action::Resume);
                } else if matches!(state, NodeState::Running | NodeState::Waiting) {
                    self.button(toolbar, "一時停止", Action::Pause);
                }
                if !self.view.deferred
                    && !matches!(state, NodeState::Succeeded | NodeState::Canceled)
                {
                    self.button(toolbar, "停止", Action::Cancel);
                }
            }
        }
        self.button(toolbar, "−", Action::Zoom(-0.1));
        self.label(
            toolbar,
            format!("{}%", (self.zoom * 100.0).round() as u32),
            11.0,
            true,
        );
        self.button(toolbar, "＋", Action::Zoom(0.1));
        self.button(toolbar, "全体", Action::Fit);
        if self.narrow {
            self.button(
                toolbar,
                if self.detail_only {
                    "キャンバス"
                } else {
                    "詳細"
                },
                Action::Details,
            );
        }
        let row = self.tree.insert(
            Some(self.body),
            Container::new(Style {
                flex_direction: FlexDirection::Row,
                gap: LayoutSize {
                    width: length(1.0),
                    height: length(0.0),
                },
                ..style::fill()
            }),
        );
        if !self.narrow || !self.detail_only {
            let host = self.tree.insert(
                Some(row),
                ScrollView::new(style::fill())
                    .horizontal(true)
                    .vertical(true),
            );
            self.tree.set_autosave_name(host, "workflow/canvas");
            self.canvas_host = Some(host);
            self.build_canvas(text);
        }
        if !self.narrow || self.detail_only {
            let pane = self.tree.insert(
                Some(row),
                Container::new(Style {
                    flex_direction: FlexDirection::Column,
                    flex_grow: if self.narrow { 1.0 } else { 0.0 },
                    flex_shrink: 0.0,
                    size: LayoutSize {
                        width: length(if self.narrow {
                            self.viewport.width
                        } else {
                            308.0
                        }),
                        height: noa_gpui_core::style::auto(),
                    },
                    min_size: LayoutSize {
                        width: length(0.0),
                        height: length(0.0),
                    },
                    ..Default::default()
                })
                .background(Role::Surface),
            );
            let scroll = self
                .tree
                .insert(Some(pane), ScrollView::new(style::fill()).vertical(true));
            self.tree.set_autosave_name(scroll, "workflow/inspector");
            let content = self.tree.insert(
                Some(scroll),
                Container::column()
                    .padding(crate::appearance::space(18.0))
                    .gap(crate::appearance::space(10.0)),
            );
            self.inspector = Some(content);
            self.build_inspector(text);
        }
        self.status();
    }

    fn build_canvas(&mut self, text: &mut TextStore) {
        let Some(host) = self.canvas_host else {
            return;
        };
        let focus = self.tree.focused().and_then(|id| {
            self.actions
                .iter()
                .find(|(widget, a)| {
                    *widget == id
                        && matches!(
                            a,
                            Action::Select(_) | Action::Source(_) | Action::Connect(_)
                        )
                })
                .map(|(_, a)| a.clone())
        });
        self.clear(host, text);
        let definition = if self.tab == Tab::Build || self.preparing_run {
            &self.editor.definition
        } else {
            self.view
                .run
                .as_ref()
                .map_or(&self.editor.definition, |r| &r.definition)
        };
        let selected = if self.tab == Tab::Build {
            self.editor.selected.as_deref()
        } else {
            self.selected_run.as_deref()
        };
        let run = if self.tab == Tab::Build || self.preparing_run {
            None
        } else {
            self.view.run.as_ref()
        };
        let canvas = canvas::insert(
            &mut self.tree,
            host,
            canvas::Options {
                definition,
                run,
                selected,
                source: self.editor.source.as_deref(),
                zoom: self.zoom,
                editable: self.tab == Tab::Build,
            },
            &mut self.actions,
        );
        if let Some(focus) = focus
            && let Some((id, _)) = self.actions.iter().find(|(_, a)| *a == focus)
        {
            self.tree.set_focus(Some(*id));
        }
        self.canvas = Some(canvas);
    }

    fn build_inspector(&mut self, text: &mut TextStore) {
        let Some(host) = self.inspector else {
            return;
        };
        self.clear(host, text);
        self.deferred_inspector = false;
        if self.tab == Tab::Build {
            self.build_edit_inspector(host);
        } else {
            self.build_run_inspector(host);
        }
    }

    fn build_edit_inspector(&mut self, host: WidgetId) {
        let node = self
            .editor
            .selected
            .as_ref()
            .and_then(|id| self.editor.definition.node(id))
            .cloned();
        if let Some(node) = node {
            self.section(host, "NODE / 工程設定");
            self.label(host, node.kind.label(), 21.0, false);
            self.label(host, node.kind.description(), 12.0, true);
            if !self.form.dirty || self.form.node.as_deref() != Some(&node.id) {
                self.form = Form {
                    node: Some(node.id.clone()),
                    title: node.title.clone(),
                    instruction: node.instruction.clone(),
                    timeout: (node.timeout_ms / 60_000).to_string(),
                    retries: node.max_retries.to_string(),
                    agent: node.agent.clone(),
                    ..Default::default()
                };
            }
            self.input(host, "名前", self.form.title.clone(), Field::Title);
            if node.kind.uses_agent() {
                self.label(host, "担当エージェント", 11.0, true);
                let selected = AgentChoice::PROVIDERS
                    .iter()
                    .position(|p| *p == self.form.agent.provider)
                    .unwrap_or(0);
                let provider = self.tree.insert(
                    Some(host),
                    PopUpButton::new(AgentChoice::PROVIDERS).selected_index(selected),
                );
                self.tree.set_accessible_label(provider, "担当エージェント");
                self.fields.push((provider, Field::Provider));
                self.input(
                    host,
                    "モデル（空欄はCLIの既定）",
                    self.form.agent.model.clone().unwrap_or_default(),
                    Field::Model,
                );
                self.label(host, "推論の強さ（effort）", 11.0, true);
                let levels = AgentChoice::efforts(&self.form.agent.provider);
                let selected = self
                    .form
                    .agent
                    .effort
                    .as_deref()
                    .and_then(|value| levels.iter().position(|v| *v == value))
                    .map_or(0, |i| i + 1);
                let effort = self.tree.insert(
                    Some(host),
                    PopUpButton::new(std::iter::once("CLIの既定").chain(levels.iter().copied()))
                        .selected_index(selected),
                );
                self.tree.set_accessible_label(effort, "推論の強さ");
                self.fields.push((effort, Field::Effort));
                self.label(host, "autoは開始時に一度だけ検出します。開始後の担当・モデル・effortは再試行でも固定されます。", 11.0, true);
            }
            self.area(
                host,
                "工程への指示",
                self.form.instruction.clone(),
                Field::Instruction,
                4,
            );
            self.input(
                host,
                "タイムアウト（分）",
                self.form.timeout.clone(),
                Field::Timeout,
            );
            self.input(
                host,
                "再試行の上限",
                self.form.retries.clone(),
                Field::Retries,
            );
            self.button(host, "設定を適用", Action::Apply);
            self.section(host, "CONNECTIONS / 接続");
            let edges: Vec<_> = self
                .editor
                .definition
                .edges
                .iter()
                .filter(|e| e.from == node.id || e.to == node.id)
                .cloned()
                .collect();
            if edges.is_empty() {
                self.label(host, "接続がありません", 12.0, true);
            }
            for edge in edges {
                let from = self
                    .editor
                    .definition
                    .node(&edge.from)
                    .map_or(edge.from.as_str(), |n| n.title.as_str());
                let to = self
                    .editor
                    .definition
                    .node(&edge.to)
                    .map_or(edge.to.as_str(), |n| n.title.as_str());
                self.label(
                    host,
                    format!("{from} → {to}\n{}", edge.artifact.label()),
                    11.0,
                    true,
                );
                self.button(
                    host,
                    "この接続を解除",
                    Action::Disconnect(edge.from, edge.to),
                );
            }
            if node.kind != NodeKind::PullRequest {
                self.button(host, "この工程から接続", Action::Source(node.id.clone()));
            }
            if let Some(source) = self.editor.source.clone()
                && source != node.id
            {
                self.button(
                    host,
                    format!("{source} → この工程"),
                    Action::Connect(node.id.clone()),
                );
            }
            self.button(host, "工程を削除", Action::Delete(node.id));
        } else {
            self.section(host, "WORKFLOW / 実行ポリシー");
            self.label(host, "自律性に境界を", 21.0, false);
            let d = &self.editor.definition;
            if !self.form.dirty || self.form.node.is_some() {
                self.form = Form {
                    name: d.name.clone(),
                    duration: (d.limits.max_duration_ms / 60_000).to_string(),
                    parallel: d.limits.max_parallel_nodes.to_string(),
                    repairs: d.limits.max_repair_attempts.to_string(),
                    cost: d
                        .limits
                        .max_cost_microusd
                        .map_or_else(String::new, |v| format!("{:.6}", v as f64 / 1_000_000.0)),
                    ..Default::default()
                };
            }
            self.input(host, "ワークフロー名", self.form.name.clone(), Field::Name);
            self.input(
                host,
                "実行時間の上限（分）",
                self.form.duration.clone(),
                Field::Duration,
            );
            self.input(
                host,
                "同時実行する工程数",
                self.form.parallel.clone(),
                Field::Parallel,
            );
            self.input(
                host,
                "修正サイクルの上限",
                self.form.repairs.clone(),
                Field::Repairs,
            );
            self.input(
                host,
                "費用上限（USD、空欄は未設定）",
                self.form.cost.clone(),
                Field::Cost,
            );
            self.label(host,"費用が取得できないエージェントでは上限を保証できません。上限を設定すると、費用不明の状態では開始を保留します。",11.0,true);
            self.button(host, "設定を適用", Action::Apply);
            self.section(host, "VALIDATION / 検証結果");
            self.section(host, "担当構成のプリセット");
            self.input(
                host,
                "構成名（同じ名前で更新）",
                self.preset_name.clone(),
                Field::PresetName,
            );
            self.button(host, "現在の担当構成を保存", Action::SavePreset);
            for (index, preset) in self.view.presets.clone().iter().enumerate() {
                self.button(
                    host,
                    format!("読み込む · {}", preset.name),
                    Action::ApplyPreset(index),
                );
            }
            self.label(host, "計画・実装・レビュー・検証の担当設定だけを保存します。読み込み後はフローを保存して適用してください。", 11.0, true);
            let issues = self.editor.definition.validate();
            if issues.is_empty() {
                self.label(host, "✓ 接続と実行条件を確認済み", 12.0, false);
            }
            for issue in issues {
                self.label(
                    host,
                    format!(
                        "{}: {}",
                        issue.node_id.unwrap_or_else(|| "全体".into()),
                        issue.message
                    ),
                    12.0,
                    true,
                );
            }
            if self.editor.conflict {
                self.button(host, "保存済みの内容に戻す", Action::Reload);
            }
            self.label(
                host,
                "編集内容は保存後の新しい実行に使われます。実行中の定義と承認対象は固定されます。",
                11.0,
                true,
            );
        }
    }

    fn can_start_run(&self) -> bool {
        self.view.run.as_ref().is_none_or(|r| {
            !r.nodes.values().any(|n| n.state.active())
                && r.state != NodeState::Running
                && (r.state != NodeState::Paused || r.limit_reason.is_some())
        })
    }

    fn build_run_inspector(&mut self, host: WidgetId) {
        if self.confirm_cancel {
            self.section(host, "STOP / 停止の確認");
            self.label(host, "この実行を停止しますか？", 20.0, false);
            self.label(host,"動作中の工程には中断を要求します。外部処理の終了を確認するまで停止済みとは扱いません。成果物と履歴は残ります。",12.0,true);
            self.button(host, "停止を要求する", Action::ConfirmCancel);
            self.button(host, "実行に戻る", Action::Cancel);
            return;
        }
        let run = self.view.run.clone();
        let can_start = self.can_start_run();
        if self.preparing_run || run.is_none() {
            self.section(host, "START / 新しい実行");
            self.label(host, "目的から始める", 21.0, false);
            if let Some(origin) = self.origin_task.clone() {
                self.label(
                    host,
                    format!("元タスク: {} · {}", origin.task, origin.title),
                    13.0,
                    false,
                );
                self.button(host, "Tasksで元タスクを開く", Action::OpenTask);
            }
            self.label(host,"独立した作業領域で計画・実装・レビュー・検証を進めます。変更を確認してからPR作成を承認できます。",12.0,true);
            self.area(
                host,
                "達成したいこと・完了条件",
                self.objective.clone(),
                Field::Objective,
                7,
            );
            let start = self.button(host, "このフローで開始", Action::Start);
            self.tree.set_enabled(
                start,
                !self.demo
                    && self.channel.is_some()
                    && can_start
                    && !self.start_pending
                    && !self.editor.dirty()
                    && !self.form.dirty
                    && self.editor.definition.validate().is_empty(),
            );
            if self.editor.dirty() || self.form.dirty {
                self.label(host, "先に編集内容を保存してください。", 12.0, true);
            }
            if let Some(previous) = &run {
                self.section(host, "PREVIOUS RUN / 以前の実行");
                self.label(
                    host,
                    format!("{} · {}", previous.id, previous.state.label()),
                    12.0,
                    false,
                );
                self.label(host, clipped(&previous.objective, 240), 11.0, true);
                self.label(
                    host,
                    if can_start {
                        "上のタスクは未開始です。新しい実行を開始すると、以前の実行は履歴に残ります。"
                    } else {
                        "上のタスクは未開始です。以前の実行に動作中・判断待ちの工程があるため、新しい実行は開始できません。"
                    },
                    11.0,
                    true,
                );
                self.button(host, "以前の実行を表示", Action::SavedRun);
            }
            if let Connection::Disconnected(message) = &self.view.connection {
                self.label(host, message.clone(), 11.0, true);
            }
            return;
        }
        let Some(run) = run else {
            return;
        };
        self.section(host, "RUN / 固定された実行");
        if self.view.deferred {
            self.label(host, "保存済みの実行を閲覧しています。このウィンドウからは自動再開しません。「以前の実行を再開」を押すと、この保存済みの工程が動作します。", 12.0, true);
        }
        if self.origin_task.is_some() {
            self.button(
                host,
                "引継ぎタスクの開始フォームへ",
                Action::Select(String::new()),
            );
        }
        if let Some(origin) = &run.origin_task {
            self.label(
                host,
                format!("元タスク: {} · {}", origin.task, origin.title),
                12.0,
                false,
            );
            self.button(host, "Tasksで元タスクを開く", Action::OpenTask);
        }
        self.label(host, "結果の出所: Workflow の構造化エージェント報告。Review/Verify ウィジェットのセッションとは別の実行です。", 11.0, true);
        for node in &run.definition.nodes {
            if node.kind.uses_agent() {
                self.label(
                    host,
                    format!("{}: {}", node.title, node.agent.label()),
                    11.0,
                    true,
                );
            }
        }
        self.label(
            host,
            format!("{}  {}", style::state_symbol(run.state), run.state.label()),
            20.0,
            false,
        );
        self.label(
            host,
            format!(
                "{}\n対象 {}  ·  定義 r{}",
                run.id,
                short(&run.target.head_sha),
                run.definition.revision
            ),
            11.0,
            true,
        );
        self.label(
            host,
            format!(
                "修正 {}/{}  ·  費用 {}",
                run.repair_attempts,
                run.definition.limits.max_repair_attempts,
                run.cost_microusd.map_or_else(
                    || "未取得".into(),
                    |c| format!("${:.3}", c as f64 / 1_000_000.0)
                )
            ),
            11.0,
            true,
        );
        if let Some(reason) = &run.limit_reason {
            self.label(host, reason, 12.0, false);
        }
        if can_start {
            self.button(host, "次の実行を準備", Action::Select(String::new()));
        }
        let id = self.selected_run.clone().or_else(|| {
            run.nodes
                .values()
                .find(|n| {
                    matches!(
                        n.state,
                        NodeState::Waiting | NodeState::Failed | NodeState::Uncertain
                    )
                })
                .map(|n| n.node_id.clone())
        });
        let Some(id) = id else {
            self.label(host, &run.objective, 12.0, true);
            self.label(
                host,
                "工程を選ぶと、進行状況と成果物を確認できます。",
                12.0,
                true,
            );
            return;
        };
        let (Some(node), Some(state)) = (run.definition.node(&id), run.nodes.get(&id)) else {
            return;
        };
        self.label(host, &node.title, 19.0, false);
        self.label(
            host,
            format!(
                "{}  {}  ·  試行 {}",
                style::state_symbol(state.state),
                state.state.label(),
                state.attempt
            ),
            12.0,
            false,
        );
        self.label(host, &state.message, 12.0, true);
        if node.kind == NodeKind::Approval && state.state == NodeState::Waiting {
            self.section(host, "EVIDENCE / この変更の根拠");
            for candidate in &run.definition.nodes {
                if matches!(
                    candidate.kind,
                    NodeKind::Implement | NodeKind::Review | NodeKind::Verify
                ) && let Some(result) = run.nodes.get(&candidate.id)
                {
                    self.label(
                        host,
                        format!(
                            "{} {} · {}",
                            style::state_symbol(result.state),
                            candidate.title,
                            result.state.label()
                        ),
                        12.0,
                        false,
                    );
                    for output in &result.outputs {
                        self.label(host, clipped(&output.summary, 700), 11.0, true);
                        self.evidence_buttons(host, output);
                        if let Some(publication) = output.data.get("publication") {
                            self.section(host, "PUBLICATION / 公開内容");
                            self.label(
                                host,
                                format!(
                                    "公開先: {}\n{} → {}",
                                    publication
                                        .get("repository")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("未確定"),
                                    publication
                                        .get("branch")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("未確定"),
                                    publication
                                        .get("base")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("未確定")
                                ),
                                11.0,
                                true,
                            );
                            if let Some(body) = publication.get("body").and_then(|v| v.as_str()) {
                                self.button(
                                    host,
                                    "公開するPR本文を読む",
                                    Action::PreviewText("公開するPR本文".into(), body.into()),
                                );
                            }
                            self.label(
                                host,
                                publication
                                    .get("title")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("PRタイトル未確定"),
                                13.0,
                                false,
                            );
                            self.label(
                                host,
                                clipped(
                                    publication
                                        .get("body")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("PR本文未確定"),
                                    6000,
                                ),
                                11.0,
                                true,
                            );
                        }
                    }
                }
            }
            self.label(
                host,
                format!("承認対象: {}", run.target.digest()),
                10.0,
                true,
            );
            let publishes = run
                .definition
                .nodes
                .iter()
                .any(|n| n.kind == NodeKind::PullRequest);
            self.label(host,if publishes {"承認後、この対象版からブランチを公開してPRを作成します。変更が加わった場合、この承認は利用できません。"}else{"この対象版と検証結果を承認します。変更が加わった場合、この承認は利用できません。"},12.0,true);
            if let Some(digest) = &state.approval_digest {
                let approve = self.button(
                    host,
                    if publishes {
                        "この変更を承認してPR作成"
                    } else {
                        "この変更を承認"
                    },
                    Action::Approve(id.clone(), digest.clone()),
                );
                self.tree
                    .set_enabled(approve, !self.demo && self.channel.is_some());
            }
        } else if state.state == NodeState::Waiting {
            self.area(
                host,
                "エージェントへの回答",
                self.answers.get(&id).cloned().unwrap_or_default(),
                Field::Answer(id.clone()),
                4,
            );
            self.button(host, "回答を送る", Action::Answer(id.clone()));
        }
        if state.state == NodeState::Failed {
            self.button(host, "この工程を再試行", Action::Retry(id.clone()));
            if matches!(node.kind, NodeKind::Review | NodeKind::Verify) {
                self.button(host, "根拠を渡して修正", Action::Repair);
            }
        }
        if node.kind != NodeKind::Approval
            && matches!(
                state.state,
                NodeState::Uncertain | NodeState::Running | NodeState::Waiting
            )
        {
            self.button(host, "外部処理の状態を照合", Action::Reconcile(id.clone()));
        }
        if state.external_ref.is_some() {
            self.button(host, "エージェントの詳細を表示", Action::Focus(id.clone()));
        }
        if !state.outputs.is_empty() {
            self.section(host, "ARTIFACTS / 成果物");
            for artifact in &state.outputs {
                self.label(host, artifact.kind.label(), 12.0, false);
                self.label(host, clipped(&artifact.summary, 3000), 12.0, true);
                self.evidence_buttons(host, artifact);
                if let Some(path) = &artifact.path {
                    let field = self.tree.insert(
                        Some(host),
                        TextField::new("成果物パス", 260.0).read_only(true),
                    );
                    self.tree.update::<TextField, _>(field, |f, cx| {
                        f.set_text(cx, path.display().to_string())
                    });
                    self.tree
                        .set_accessible_label(field, "成果物パス（コピー可能）");
                }
                if !artifact.data.is_null() {
                    self.label(
                        host,
                        clipped(
                            &serde_json::to_string_pretty(&artifact.data).unwrap_or_default(),
                            5000,
                        ),
                        11.0,
                        true,
                    );
                }
            }
        }
        if !state.history.is_empty() {
            self.section(host, "ATTEMPTS / 過去の試行");
            for previous in state.history.iter().rev() {
                self.label(
                    host,
                    format!(
                        "#{} {} · {}",
                        previous.attempt,
                        previous.state.label(),
                        previous.message
                    ),
                    11.0,
                    true,
                );
            }
        }
    }

    fn build_history(&mut self) {
        let scroll = self.tree.insert(
            Some(self.body),
            ScrollView::new(style::fill()).vertical(true),
        );
        let content = self.tree.insert(
            Some(scroll),
            Container::column()
                .padding(crate::appearance::space(24.0))
                .gap(crate::appearance::space(12.0)),
        );
        self.section(content, "TIMELINE / この実行の記録");
        let Some(run) = self.view.run.clone() else {
            self.label(
                content,
                "実行すると、判断・成果物・復旧の経緯がここに残ります。",
                15.0,
                true,
            );
            return;
        };
        self.label(content, &run.objective, 21.0, false);
        self.label(
            content,
            format!(
                "{}  ·  定義 {}  ·  対象 {}",
                run.id,
                short(&run.definition_digest),
                short(&run.target.digest())
            ),
            12.0,
            true,
        );
        for event in run.events.iter().rev().take(500) {
            let row = self.tree.insert(
                Some(content),
                Container::column()
                    .padding(crate::appearance::space(14.0))
                    .gap(crate::appearance::space(6.0))
                    .background(Role::Surface)
                    .radius(8.0),
            );
            let elapsed = event.at.saturating_sub(run.started_at) / 1000;
            let node = event
                .node_id
                .as_ref()
                .and_then(|id| run.definition.node(id))
                .map_or("実行全体", |n| n.title.as_str());
            self.label(
                row,
                format!(
                    "#{:03}   +{}:{:02}   {}   /   {}",
                    event.seq,
                    elapsed / 60,
                    elapsed % 60,
                    node,
                    event.kind
                ),
                11.0,
                true,
            );
            self.label(row, &event.message, 13.0, false);
            if let Some(id) = &event.node_id {
                self.button(row, "工程を見る", Action::Select(id.clone()));
            }
        }
        if run.events.len() > 500 {
            self.label(
                content,
                "最新500件を表示しています。全記録は保存済みの実行データに保持されています。",
                11.0,
                true,
            );
        }
    }

    fn apply_form(&mut self) -> Result<(), String> {
        if !self.form.dirty {
            return Ok(());
        }
        if let Some(id) = self.form.node.clone() {
            let minutes = self
                .form
                .timeout
                .trim()
                .parse::<u64>()
                .map_err(|_| "タイムアウトは整数の分数で指定してください")?;
            let retries = self
                .form
                .retries
                .trim()
                .parse::<u8>()
                .map_err(|_| "再試行の上限は0〜5で指定してください")?;
            if !(1..=1440).contains(&minutes) || retries > 5 {
                return Err("タイムアウトは1〜1440分、再試行は0〜5回で指定してください".into());
            }
            let title = self.form.title.trim().to_string();
            if title.is_empty() || title.chars().count() > 100 {
                return Err("工程名は1〜100文字で指定してください".into());
            }
            let instruction = self.form.instruction.clone();
            let agent = self.form.agent.clone();
            agent.validate()?;
            self.editor.change(|d| {
                if let Some(node) = d.nodes.iter_mut().find(|n| n.id == id) {
                    node.title = title;
                    node.instruction = instruction;
                    node.timeout_ms = minutes * 60_000;
                    node.max_retries = retries;
                    node.agent = agent;
                }
            });
        } else {
            let duration = self
                .form
                .duration
                .trim()
                .parse::<u64>()
                .map_err(|_| "実行時間は整数の分数で指定してください")?;
            let parallel = self
                .form
                .parallel
                .trim()
                .parse::<u8>()
                .map_err(|_| "並行数は1〜8で指定してください")?;
            let repairs = self
                .form
                .repairs
                .trim()
                .parse::<u8>()
                .map_err(|_| "修正回数は0〜10で指定してください")?;
            if !(1..=1440).contains(&duration) || !(1..=8).contains(&parallel) || repairs > 10 {
                return Err(
                    "実行時間は1〜1440分、並行数は1〜8、修正は0〜10回で指定してください".into(),
                );
            }
            let cost = if self.form.cost.trim().is_empty() {
                None
            } else {
                let cost = self
                    .form
                    .cost
                    .trim()
                    .parse::<f64>()
                    .map_err(|_| "費用はUSDの数値で指定してください")?;
                if !cost.is_finite() || cost <= 0.0 || cost > 1_000_000.0 {
                    return Err("費用上限が不正です".into());
                }
                Some((cost * 1_000_000.0).round() as u64)
            };
            let name = self.form.name.trim().to_string();
            if name.is_empty() || name.chars().count() > 100 {
                return Err("ワークフロー名は1〜100文字で指定してください".into());
            }
            self.editor.change(|d| {
                d.name = name;
                d.limits.max_duration_ms = duration * 60_000;
                d.limits.max_parallel_nodes = parallel;
                d.limits.max_repair_attempts = repairs;
                d.limits.max_cost_microusd = cost;
                d.limits.allow_unknown_cost = cost.is_none();
            });
        }
        self.form.dirty = false;
        Ok(())
    }

    fn send(&mut self, intent: Intent) -> Result<(), String> {
        if !self.view.can_control && !matches!(intent, Intent::Refresh) {
            return Err(
                "別の実行サービスを監視中です。操作権が移るまで変更操作はできません。".into(),
            );
        }
        let channel = self.channel.as_ref().ok_or(if self.demo {
            "表示サンプルでは実処理を実行しません"
        } else {
            "実行サービスを開けませんでした。保存先や起動エラーを確認して開き直してください"
        })?;
        channel.act(intent)?;
        self.notice = "操作を受け付けました。実行サービスからの確定を待っています。".into();
        Ok(())
    }

    fn act(&mut self, action: Action, text: &mut TextStore) -> Result<(), String> {
        if !matches!(
            action,
            Action::Source(_)
                | Action::Connect(_)
                | Action::Zoom(_)
                | Action::Fit
                | Action::Refresh
                | Action::Cancel
                | Action::ConfirmCancel
                | Action::Approve(..)
                | Action::Retry(_)
                | Action::Reconcile(_)
                | Action::Focus(_)
                | Action::Answer(_)
                | Action::Pause
                | Action::Resume
                | Action::Repair
        ) {
            self.apply_form()?;
        }
        self.notice.clear();
        match action {
            Action::SavePreset => {
                let preset = AgentPreset::from_definition(
                    self.preset_name.trim().to_owned(),
                    &self.editor.definition,
                );
                preset.validate()?;
                if self.demo {
                    if let Some(existing) =
                        self.view.presets.iter_mut().find(|p| p.name == preset.name)
                    {
                        *existing = preset;
                    } else {
                        self.view.presets.push(preset);
                    }
                    self.notice = "サンプル内で担当構成を保存しました".into();
                    self.build_inspector(text);
                } else {
                    self.send(Intent::SavePreset(preset))?;
                }
            }
            Action::ApplyPreset(index) => {
                let preset = self
                    .view
                    .presets
                    .get(index)
                    .cloned()
                    .ok_or("担当構成が見つかりません")?;
                preset.validate()?;
                let mut next = self.editor.definition.clone();
                preset.apply(&mut next)?;
                self.editor.change(|definition| *definition = next);
                self.form = Form::default();
                self.build_body(text);
                self.notice = format!(
                    "{} を読み込みました。保存後の新しい実行に適用されます。",
                    preset.name
                );
            }
            Action::OpenTask => {
                let origin = if self.preparing_run {
                    self.origin_task.as_ref()
                } else {
                    self.view.run.as_ref().and_then(|r| r.origin_task.as_ref())
                }
                .ok_or("元タスクの参照がありません")?;
                if self.demo {
                    self.notice = "サンプルではTasksを起動しません".into();
                } else {
                    workflow::tasks::open_origin(origin)?;
                }
            }
            Action::Tab(tab) => {
                self.evidence_preview = None;
                self.tab = tab;
                self.confirm_cancel = false;
                self.build_body(text);
            }
            Action::SavedRun => {
                self.preparing_run = false;
                self.selected_run = None;
                self.tab = Tab::Run;
                self.build_body(text);
            }
            Action::Select(id) => {
                if self.tab == Tab::Run && self.preparing_run && !id.is_empty() {
                    self.tab = Tab::Build;
                    self.editor.selected = Some(id);
                    self.build_body(text);
                    return Ok(());
                }
                if self.tab == Tab::Build {
                    self.editor.selected = Some(id);
                } else {
                    self.preparing_run = id.is_empty();
                    self.selected_run = if id.is_empty() { None } else { Some(id) };
                    if self.tab == Tab::History {
                        self.tab = Tab::Run;
                        self.build_body(text);
                    }
                }
                if self.narrow {
                    self.detail_only = true;
                    self.build_body(text);
                } else {
                    self.build_canvas(text);
                    self.build_inspector(text);
                }
            }
            Action::Settings => {
                self.editor.selected = None;
                if self.narrow {
                    self.detail_only = true;
                    self.build_body(text);
                } else {
                    self.build_inspector(text);
                }
            }
            Action::Add(kind) => {
                self.editor.add(kind)?;
                self.show_palette = false;
                self.build_canvas(text);
                self.build_inspector(text);
            }
            Action::Palette => {
                self.show_palette = !self.show_palette;
            }
            Action::Source(id) => {
                self.editor.source = Some(id);
                self.build_canvas(text);
            }
            Action::Connect(to) => {
                let from = self
                    .editor
                    .source
                    .clone()
                    .ok_or("先に接続元の出力ポートを選んでください")?;
                self.editor.connect(&from, &to)?;
                self.build_canvas(text);
                self.build_inspector(text);
            }
            Action::Disconnect(from, to) => {
                self.editor
                    .change(|d| d.edges.retain(|e| e.from != from || e.to != to));
                self.build_canvas(text);
                self.build_inspector(text);
            }
            Action::Delete(id) => {
                self.editor.remove(&id);
                self.build_canvas(text);
                self.build_inspector(text);
            }
            Action::Undo => {
                self.editor.undo();
                self.build_canvas(text);
                self.build_inspector(text);
            }
            Action::Redo => {
                self.editor.redo();
                self.build_canvas(text);
                self.build_inspector(text);
            }
            Action::Arrange => {
                self.editor.arrange();
                self.build_canvas(text);
            }
            Action::Zoom(delta) => {
                self.zoom = (self.zoom + delta).clamp(0.5, 1.5);
                self.build_body(text);
            }
            Action::Fit => {
                let definition = if self.tab == Tab::Build {
                    &self.editor.definition
                } else {
                    self.view
                        .run
                        .as_ref()
                        .map_or(&self.editor.definition, |r| &r.definition)
                };
                let (w, h) = canvas::extent(definition);
                let width = if self.narrow {
                    self.viewport.width
                } else {
                    self.viewport.width - 310.0
                };
                self.zoom = ((width - 24.0) / w)
                    .min((self.viewport.height - 250.0) / h)
                    .clamp(0.5, 1.5);
                self.build_body(text);
                if let Some(host) = self.canvas_host {
                    self.tree.update::<ScrollView, _>(host, |scroll, cx| {
                        scroll.set_scroll_position(cx, Point::new(0.0, 0.0))
                    });
                }
            }
            Action::Details => {
                self.detail_only = !self.detail_only;
                self.build_body(text);
            }
            Action::Apply => {
                self.build_canvas(text);
                self.notice = "工程設定を反映しました。保存すると次の実行で使えます。".into();
            }
            Action::Save => {
                if !self.editor.dirty() {
                    self.notice = "保存済みです。".into();
                } else if self.demo {
                    self.editor.saved = self.editor.definition.clone();
                    self.notice =
                        "サンプル内で保存しました。ファイルや実リポジトリは変更していません。"
                            .into();
                } else {
                    let (definition, expected_version) = self.editor.begin_save()?;
                    if let Err(error) = self.send(Intent::Save {
                        definition,
                        expected_version,
                    }) {
                        self.editor.reject_save();
                        return Err(error);
                    }
                }
            }
            Action::Validate => {
                self.editor.selected = None;
                self.tab = Tab::Build;
                self.detail_only = self.narrow;
                self.build_body(text);
                self.notice = if self.editor.definition.validate().is_empty() {
                    "✓ 型・接続・到達性・承認条件・上限を確認しました。".into()
                } else {
                    format!(
                        "{}件の修正が必要です。詳細に理由を表示しています。",
                        self.editor.definition.validate().len()
                    )
                };
            }
            Action::Start => {
                if self.start_pending || !self.can_start_run() {
                    return Err("以前の実行を確認するか、開始の確定を待ってください".into());
                }
                if self.editor.dirty() {
                    return Err("実行する前に編集内容を保存してください".into());
                }
                if self.objective.trim().is_empty() {
                    return Err("達成したいことと完了条件を入力してください".into());
                }
                self.send(Intent::Start {
                    objective: self.objective.clone(),
                    origin_task: self.origin_task.clone().map(|mut origin| {
                        origin.objective_digest =
                            workflow::model::digest_bytes(self.objective.as_bytes());
                        origin
                    }),
                })?;
                self.start_pending = true;
            }
            Action::Pause => self.send(Intent::Pause)?,
            Action::Resume => self.send(Intent::Resume)?,
            Action::Cancel => {
                self.confirm_cancel = !self.confirm_cancel;
                if self.narrow {
                    self.detail_only = true;
                    self.build_body(text);
                } else {
                    self.build_inspector(text);
                }
            }
            Action::ConfirmCancel => {
                self.send(Intent::Cancel)?;
                self.confirm_cancel = false;
                self.build_inspector(text);
            }
            Action::Approve(node_id, digest) => self.send(Intent::Approve { node_id, digest })?,
            Action::Retry(node_id) => self.send(Intent::Retry { node_id })?,
            Action::Repair => self.send(Intent::Repair)?,
            Action::Reconcile(node_id) => self.send(Intent::Reconcile { node_id })?,
            Action::Focus(node_id) => self.send(Intent::Focus { node_id })?,
            Action::Answer(node_id) => {
                let answer = self.answers.get(&node_id).cloned().unwrap_or_default();
                if answer.trim().is_empty() {
                    return Err("回答を入力してください".into());
                }
                self.send(Intent::Answer { node_id, answer })?;
            }
            Action::Refresh => self.send(Intent::Refresh)?,
            Action::Reload => {
                self.editor = Editor::new(self.view.definition.clone(), self.view.version);
                self.form = Form::default();
                self.build_body(text);
            }
            Action::Evidence(path, digest, title) => {
                self.evidence_preview = Some((title, evidence::preview(&path, &digest)?));
                self.build_body(text);
            }
            Action::PreviewText(title, body) => {
                self.evidence_preview = Some((title, clipped(&body, 120_000)));
                self.build_body(text);
            }
            Action::ClosePreview => {
                self.evidence_preview = None;
                self.build_body(text);
            }
        }
        self.status();
        Ok(())
    }

    fn do_action(&mut self, action: Action, text: &mut TextStore) {
        if let Err(message) = self.act(action, text) {
            self.notice = message;
        }
        self.status();
    }

    fn receive(&mut self, view: View, text: &mut TextStore) {
        let before = self.editor.definition.clone();
        self.editor
            .receive(&view.definition, view.version, self.form.dirty);
        if view.notice.as_ref().is_some_and(|notice| {
            [
                "表示後に状態",
                "保存",
                "状態を保存",
                "ワークフローを保存",
                "別の処理が",
            ]
            .iter()
            .any(|prefix| notice.starts_with(prefix))
        }) {
            self.editor.reject_save();
        }
        if let Some(notice) = &view.notice {
            self.notice = notice.clone();
        } else if self.notice.starts_with("操作を受け付けました") {
            self.notice.clear();
        }
        let changed = self.view.run != view.run || self.view.deferred != view.deferred;
        if self.start_pending {
            if view.run.as_ref().map(|run| &run.id) != self.view.run.as_ref().map(|run| &run.id) {
                self.start_pending = false;
                self.preparing_run = false;
                self.selected_run = None;
            } else if view.notice.is_some() {
                // A rejected start keeps the task objective visible for correction.
                self.start_pending = false;
            }
        }
        self.view = view;
        let mut adopted = self.editor.definition.clone();
        adopted.revision = before.revision;
        if adopted != before
            && self.built
            && self.tab == Tab::Build
            && self.evidence_preview.is_none()
        {
            self.build_canvas(text);
            self.build_inspector(text);
        }
        if changed && self.built && self.evidence_preview.is_none() {
            if self.tab == Tab::History {
                self.build_body(text);
            } else if self.tab == Tab::Run {
                self.build_canvas(text);
                if self.text_focused() {
                    self.deferred_inspector = true;
                } else {
                    self.build_body(text);
                }
            }
        }
        self.status();
    }

    fn text_focused(&self) -> bool {
        self.tree.focused().is_some_and(|id| {
            self.tree.get::<TextField>(id).is_some() || self.tree.get::<TextArea>(id).is_some()
        })
    }
    fn bounds(&self, id: WidgetId) -> Option<Rect> {
        let origin = self.tree.window_origin(id)?;
        let layout = self.tree.layout(id)?;
        Some(Rect::new(
            origin.x,
            origin.y,
            layout.rect.width,
            layout.rect.height,
        ))
    }
    fn hit(&self, id: WidgetId, p: Point) -> bool {
        self.bounds(id).is_some_and(|r| {
            p.x >= r.x && p.x <= r.x + r.width && p.y >= r.y && p.y <= r.y + r.height
        })
    }
    fn on_canvas(&self, p: Point) -> bool {
        self.canvas_host.is_some_and(|id| self.hit(id, p))
    }

    fn pointer(&mut self, event: &Event, text: &mut TextStore) -> bool {
        if self.tab != Tab::Build {
            return false;
        }
        match event {
            Event::PointerDown {
                pos,
                button: PointerButton::Primary,
            } if self.on_canvas(*pos) => {
                if let Some(source) = self.actions.iter().find_map(|(id, a)| {
                    if self.hit(*id, *pos) {
                        if let Action::Source(from) = a {
                            Some(from.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }) {
                    self.editor.source = Some(source);
                    self.connecting = true;
                    self.build_canvas(text);
                    self.status();
                    return true;
                }
                if self
                    .actions
                    .iter()
                    .any(|(id, a)| matches!(a, Action::Connect(_)) && self.hit(*id, *pos))
                {
                    return false;
                }
                let node = self.canvas.as_ref().and_then(|c| {
                    c.cards
                        .iter()
                        .find(|c| self.hit(c.widget, *pos))
                        .map(|c| c.id.clone())
                });
                if let Some(id) = node {
                    if let Err(e) = self.apply_form() {
                        self.notice = e;
                        self.status();
                        return true;
                    }
                    if let Some(node) = self.editor.definition.node(&id) {
                        self.drag = Some(Drag {
                            node: id.clone(),
                            pointer: *pos,
                            origin: node.position,
                            before: self.editor.definition.clone(),
                        });
                    }
                    self.editor.selected = Some(id.clone());
                    self.build_canvas(text);
                    self.build_inspector(text);
                    if let Some(card) = self
                        .canvas
                        .as_ref()
                        .and_then(|c| c.cards.iter().find(|c| c.id == id))
                    {
                        self.tree.set_focus(Some(card.widget));
                    }
                    self.status();
                    return true;
                }
            }
            Event::PointerMove { pos } => {
                if let Some(drag) = &self.drag {
                    let delta = Point::new(
                        (pos.x - drag.pointer.x) / self.zoom,
                        (pos.y - drag.pointer.y) / self.zoom,
                    );
                    if let Some(node) = self
                        .editor
                        .definition
                        .nodes
                        .iter_mut()
                        .find(|n| n.id == drag.node)
                    {
                        node.position = workflow::model::Point {
                            x: ((drag.origin.x + delta.x) / 8.0).round().clamp(2.0, 1000.0) * 8.0,
                            y: ((drag.origin.y + delta.y) / 8.0).round().clamp(2.0, 1000.0) * 8.0,
                        };
                    }
                    self.build_canvas(text);
                    self.status();
                    return true;
                }
                if self.connecting {
                    return true;
                }
            }
            Event::PointerUp {
                pos,
                button: PointerButton::Primary,
            } => {
                if let Some(drag) = self.drag.take() {
                    self.editor.record(drag.before);
                    self.status();
                    return true;
                }
                if self.connecting {
                    self.connecting = false;
                    if self.on_canvas(*pos)
                        && let Some(to) = self.actions.iter().find_map(|(id, a)| {
                            if self.hit(*id, *pos) {
                                if let Action::Connect(to) = a {
                                    Some(to.clone())
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        })
                    {
                        self.do_action(Action::Connect(to), text);
                    }
                    self.status();
                    return true;
                }
            }
            Event::PointerLeave => {
                if let Some(drag) = self.drag.take() {
                    self.editor.record(drag.before);
                    self.status();
                }
                self.connecting = false;
            }
            _ => {}
        }
        false
    }
}

impl Screen for WorkflowScreen {
    fn set_system_appearance(&mut self, appearance: noa_gpui_core::Appearance) {
        crate::appearance::system(&mut self.tree, appearance);
    }
    fn tree(&self) -> &UiTree {
        &self.tree
    }
    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }
    fn prepare(&mut self, viewport: Size, text: &mut TextStore) -> bool {
        self.viewport = viewport;
        let narrow = viewport.width < 1040.0;
        if !self.built || self.narrow != narrow {
            self.narrow = narrow;
            self.built = true;
            self.build_body(text);
        }
        if self.deferred_inspector && !self.text_focused() {
            self.build_inspector(text);
        }
        self.tree.prepare(viewport, text).is_some()
    }
    fn woken(&mut self, text: &mut TextStore) {
        if let Some(view) = self.channel.as_ref().and_then(Channel::poll) {
            self.receive(view, text);
        }
    }
    fn dispatch(&mut self, event: Event, text: &mut TextStore) {
        if self.pointer(&event, text) {
            return;
        }
        if let Event::KeyDown { key, modifiers } = &event {
            if *key == Key::Escape && !self.text_focused() {
                if self.evidence_preview.is_some() {
                    self.evidence_preview = None;
                    self.build_body(text);
                    return;
                }
                if let Some(drag) = self.drag.take() {
                    self.editor.definition = drag.before;
                }
                self.editor.source = None;
                self.connecting = false;
                if self.confirm_cancel {
                    self.confirm_cancel = false;
                    self.build_inspector(text);
                }
                self.build_canvas(text);
                self.status();
                return;
            }
            if modifiers.command {
                if *key == Key::Char('s') {
                    self.do_action(Action::Save, text);
                    return;
                }
                if !self.text_focused() && self.tab == Tab::Build && *key == Key::Char('z') {
                    self.do_action(
                        if modifiers.shift {
                            Action::Redo
                        } else {
                            Action::Undo
                        },
                        text,
                    );
                    return;
                }
            }
            if self.tab == Tab::Build && !self.text_focused() {
                let selected = self.tree.focused().and_then(|id| {
                    self.canvas
                        .as_ref()?
                        .cards
                        .iter()
                        .find(|c| c.widget == id)
                        .map(|c| c.id.clone())
                });
                if let Some(id) = selected {
                    let step = if modifiers.shift { 40.0 } else { 8.0 };
                    let delta = match key {
                        Key::ArrowLeft => Some((-step, 0.0)),
                        Key::ArrowRight => Some((step, 0.0)),
                        Key::ArrowUp => Some((0.0, -step)),
                        Key::ArrowDown => Some((0.0, step)),
                        _ => None,
                    };
                    if let Some((x, y)) = delta {
                        self.editor.change(|d| {
                            if let Some(n) = d.nodes.iter_mut().find(|n| n.id == id) {
                                n.position.x = (n.position.x + x).clamp(16.0, 8000.0);
                                n.position.y = (n.position.y + y).clamp(16.0, 8000.0);
                            }
                        });
                        self.build_canvas(text);
                        self.status();
                        return;
                    }
                    if matches!(key, Key::Delete | Key::Backspace) {
                        self.do_action(Action::Delete(id), text);
                        return;
                    }
                }
            }
        }
        self.tree.dispatch(event, text);
        for message in self.tree.take_messages() {
            self.handle(message, text);
        }
    }
    fn handle(&mut self, message: Message, text: &mut TextStore) {
        if message.downcast_ref::<SelectionChanged>().is_some() {
            if let Some(field) = self
                .fields
                .iter()
                .find(|(id, _)| *id == message.source)
                .map(|(_, f)| f.clone())
            {
                let selected = self
                    .tree
                    .get::<PopUpButton>(message.source)
                    .map_or(0, PopUpButton::selected);
                match field {
                    Field::Provider => {
                        self.form.agent = AgentChoice {
                            provider: AgentChoice::PROVIDERS
                                .get(selected)
                                .unwrap_or(&"auto")
                                .to_string(),
                            model: None,
                            effort: None,
                        };
                        self.form.dirty = true;
                        self.build_inspector(text);
                    }
                    Field::Effort => {
                        self.form.agent.effort = selected
                            .checked_sub(1)
                            .and_then(|i| AgentChoice::efforts(&self.form.agent.provider).get(i))
                            .map(|s| s.to_string());
                        self.form.dirty = true;
                    }
                    _ => {}
                }
                self.status();
            }
        } else if message.downcast_ref::<TextChanged>().is_some() {
            if let Some(field) = self
                .fields
                .iter()
                .find(|(id, _)| *id == message.source)
                .map(|(_, field)| field.clone())
            {
                let value = self
                    .tree
                    .get::<TextField>(message.source)
                    .map(|f| f.text().to_string())
                    .or_else(|| {
                        self.tree
                            .get::<TextArea>(message.source)
                            .map(|f| f.text().to_string())
                    })
                    .unwrap_or_default();
                match field {
                    Field::Model => {
                        self.form.agent.model =
                            (!value.trim().is_empty()).then(|| value.trim().to_owned());
                        self.form.dirty = true;
                    }
                    Field::PresetName => self.preset_name = value,
                    Field::Provider | Field::Effort => {}
                    Field::Objective => self.objective = value,
                    Field::Answer(node_id) => {
                        self.answers.insert(node_id, value);
                    }
                    Field::Title => {
                        self.form.title = value;
                        self.form.dirty = true;
                    }
                    Field::Instruction => {
                        self.form.instruction = value;
                        self.form.dirty = true;
                    }
                    Field::Timeout => {
                        self.form.timeout = value;
                        self.form.dirty = true;
                    }
                    Field::Retries => {
                        self.form.retries = value;
                        self.form.dirty = true;
                    }
                    Field::Name => {
                        self.form.name = value;
                        self.form.dirty = true;
                    }
                    Field::Duration => {
                        self.form.duration = value;
                        self.form.dirty = true;
                    }
                    Field::Parallel => {
                        self.form.parallel = value;
                        self.form.dirty = true;
                    }
                    Field::Repairs => {
                        self.form.repairs = value;
                        self.form.dirty = true;
                    }
                    Field::Cost => {
                        self.form.cost = value;
                        self.form.dirty = true;
                    }
                }
                self.notice.clear();
                self.status();
            }
        } else if message.downcast_ref::<ButtonClicked>().is_some()
            && let Some(action) = self
                .actions
                .iter()
                .find(|(id, _)| *id == message.source)
                .map(|(_, a)| a.clone())
        {
            self.do_action(action, text);
        }
    }
}
