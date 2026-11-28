mod feed;
mod style;
mod views;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

use ember::model::{Attachment, Command, MemoryEntry, RunKind, Scope, Settings, View};
use noa_gpui_core::widgets::{
    ButtonClicked, CheckState, Checkbox, CheckboxChanged, Container, Label, PopUpButton,
    SelectionChanged, TextArea, TextChanged, TextField, TextView,
};
use noa_gpui_core::{Event, Key, Message, UiTree, WidgetId};
use noa_gpui_platform::Screen;
use noa_gpui_scene::Size;
use noa_gpui_text::TextStore;

use feed::{Feed, Job, Update};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Conversation,
    Requests,
    Memory,
    Settings,
}

impl Page {
    const ALL: [Self; 4] = [
        Self::Conversation,
        Self::Requests,
        Self::Memory,
        Self::Settings,
    ];
    fn index(self) -> usize {
        match self {
            Self::Conversation => 0,
            Self::Requests => 1,
            Self::Memory => 2,
            Self::Settings => 3,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Conversation => "会話",
            Self::Requests => "依頼",
            Self::Memory => "記憶",
            Self::Settings => "設定",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    Page(Page),
    Send,
    Start,
    StopConversation,
    Cancel(String),
    Attachments,
    PreviewFile,
    ClearAttachments,
    SharePreview,
    Artifact(String),
    ReadMessage(String),
    Remember(String),
    EditMemory(String),
    SaveMemory,
    NewMemory,
    DeleteMemory(String),
    SaveSettings,
    ApplyProject,
    Reconnect,
    RetryPending,
    Archive(String, bool),
    ShowArchived,
    DeleteRun(String),
    ClearConversation,
    PrepareHandoff(String),
    OpenHandoff,
    ClosePreview,
    Confirm,
    Export,
}

#[derive(Clone)]
enum Completion {
    Composer(String, Vec<String>),
    Objective(String, Vec<String>),
    Memory {
        id: Option<String>,
        scope: Scope,
        text: String,
    },
    Settings(Settings),
    Other,
}

struct Pending {
    id: String,
    command: Command,
    completion: Completion,
    in_flight: bool,
}

enum Preview {
    Text,
    Attachment {
        attachment: Attachment,
        scope: Scope,
    },
    Handoff(PathBuf),
    Confirm(Command),
}

struct Ids {
    shell: WidgetId,
    navigation: WidgetId,
    navigation_buttons: [WidgetId; 4],
    pages: [WidgetId; 4],
    status: WidgetId,
    notice: WidgetId,
    retry: WidgetId,
    scope: WidgetId,
    scope_label: WidgetId,
    history: WidgetId,
    history_scroll: WidgetId,
    composer: WidgetId,
    send: WidgetId,
    stop: WidgetId,
    request_list: WidgetId,
    request_scroll: WidgetId,
    objective: WidgetId,
    kind: WidgetId,
    start: WidgetId,
    archive_toggle: WidgetId,
    memory_list: WidgetId,
    memory_scroll: WidgetId,
    memory_text: WidgetId,
    memory_scope: WidgetId,
    memory_scope_label: WidgetId,
    memory_save: WidgetId,
    model: WidgetId,
    notifications: WidgetId,
    project_path: WidgetId,
    settings_save: WidgetId,
    backend_notice: WidgetId,
    share_panel: WidgetId,
    attachment_path: WidgetId,
    attachment_summary: WidgetId,
    preview_file: WidgetId,
    preview_page: WidgetId,
    preview_title: WidgetId,
    preview_body: WidgetId,
    preview_note: WidgetId,
    preview_share: WidgetId,
    preview_open: WidgetId,
    preview_confirm: WidgetId,
}

pub(crate) struct EmberScreen {
    tree: UiTree,
    ids: Ids,
    actions: Vec<(WidgetId, Action)>,
    view: View,
    feed: Option<Feed>,
    demo: bool,
    connected: bool,
    page: Page,
    scope: Scope,
    scope_chosen: bool,
    project: Option<Scope>,
    memory_scope: Scope,
    memory_edit: Option<String>,
    memory_revision: Option<u64>,
    settings_dirty: bool,
    attachments: Vec<Attachment>,
    share_open: bool,
    preview: Option<Preview>,
    pending: Option<Pending>,
    notice: String,
    show_archived: bool,
    refresh_needed: bool,
    narrow: Option<bool>,
}

impl EmberScreen {
    pub fn demo(kind: &str) -> Self {
        let mut screen = Self::new(crate::demo::view(kind), None, true);
        screen.project = Some(crate::demo::scope());
        screen.scope = crate::demo::scope();
        screen.memory_scope = crate::demo::scope();
        screen.page = match kind {
            "requests" | "error" => Page::Requests,
            "memory" => Page::Memory,
            "settings" => Page::Settings,
            _ => Page::Conversation,
        };
        screen.set_field(screen.ids.project_path, "/workspace/Noa".into());
        screen.set_area(screen.ids.memory_text, "説明は日本語で、要点から。".into());
        screen
    }

    pub fn live(
        root: PathBuf,
        executable: PathBuf,
        repository: Option<PathBuf>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let feed = Feed::start(root, executable, repository, wake);
        Self::new(View::default(), Some(feed), false)
    }

    fn new(view: View, feed: Option<Feed>, demo: bool) -> Self {
        let mut tree = UiTree::new();
        crate::appearance::install(&mut tree, style::theme());
        let (ids, actions) = views::build(&mut tree);
        let mut screen = Self {
            tree,
            ids,
            actions,
            view,
            feed,
            demo,
            connected: demo,
            page: Page::Conversation,
            scope: Scope::Global,
            scope_chosen: false,
            project: None,
            memory_scope: Scope::Global,
            memory_edit: None,
            memory_revision: None,
            settings_dirty: false,
            attachments: Vec::new(),
            share_open: false,
            preview: None,
            pending: None,
            notice: String::new(),
            show_archived: false,
            refresh_needed: true,
            narrow: None,
        };
        screen.sync_settings();
        screen.sync_chrome();
        screen
    }

    fn field(&self, id: WidgetId) -> String {
        self.tree
            .get::<TextField>(id)
            .map(|field| field.text().to_owned())
            .unwrap_or_default()
    }
    fn area(&self, id: WidgetId) -> String {
        self.tree
            .get::<TextArea>(id)
            .map(|area| area.text().to_owned())
            .unwrap_or_default()
    }
    fn set_field(&mut self, id: WidgetId, value: String) {
        self.tree
            .update::<TextField, _>(id, |field, cx| field.set_text(cx, value));
    }
    fn set_area(&mut self, id: WidgetId, value: String) {
        self.tree
            .update::<TextArea, _>(id, |area, cx| area.set_text(cx, value));
    }
    fn label(&mut self, id: WidgetId, value: impl Into<String>) {
        let value = value.into();
        self.tree
            .update::<Label, _>(id, |label, cx| label.set_text(cx, value));
    }

    fn job(&mut self, job: Job) -> Result<(), String> {
        if self.demo {
            return Err("デモ表示では外部への送信・実行を行いません".into());
        }
        self.feed
            .as_ref()
            .ok_or("サービスに接続していません")?
            .send(job)
    }

    fn command(&mut self, command: Command, completion: Completion) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("前の操作の受付を確認してから、次の操作を行ってください".into());
        }
        let id = ember::client::request_id();
        self.job(Job::Command {
            request_id: id.clone(),
            intent: command.clone(),
        })?;
        self.pending = Some(Pending {
            id,
            command,
            completion,
            in_flight: true,
        });
        self.notice = "操作を受け付けています…".into();
        Ok(())
    }

    fn attachment_paths(&self) -> Vec<String> {
        self.attachments
            .iter()
            .map(|attachment| attachment.path.clone())
            .collect()
    }

    fn active_conversation(&self) -> Option<String> {
        self.view
            .state
            .runs
            .iter()
            .rev()
            .find(|run| {
                run.kind == RunKind::Conversation
                    && run.scope == self.scope
                    && !run.status.terminal()
            })
            .map(|run| run.id.clone())
    }

    fn act(&mut self, action: Action, text: &mut TextStore) -> Result<(), String> {
        match action {
            Action::Page(page) => {
                self.page = page;
                self.preview = None;
                if page == Page::Memory
                    && self.memory_edit.is_none()
                    && self.area(self.ids.memory_text).is_empty()
                {
                    self.memory_scope = self.scope.clone();
                }
                self.refresh_needed = true;
            }
            Action::Send => {
                let draft = self.area(self.ids.composer);
                if draft.trim().is_empty() {
                    return Err("相談したい内容を入力してください".into());
                }
                if draft.len() > ember::model::MAX_TEXT_BYTES {
                    return Err("入力は64KiB以内に整理してください".into());
                }
                let paths = self.attachment_paths();
                self.command(
                    Command::Send {
                        text: draft.clone(),
                        scope: self.scope.clone(),
                        attachments: paths.clone(),
                    },
                    Completion::Composer(draft, paths),
                )?;
            }
            Action::Start => {
                let objective = self.area(self.ids.objective);
                if objective.trim().is_empty() {
                    return Err("依頼の目的を入力してください".into());
                }
                if objective.len() > ember::model::MAX_TEXT_BYTES {
                    return Err("依頼の目的は64KiB以内に整理してください".into());
                }
                let kind = if self
                    .tree
                    .get::<PopUpButton>(self.ids.kind)
                    .map_or(0, PopUpButton::selected)
                    == 0
                {
                    RunKind::Document
                } else {
                    RunKind::Research
                };
                let paths = self.attachment_paths();
                self.command(
                    Command::StartRun {
                        kind,
                        objective: objective.clone(),
                        scope: self.scope.clone(),
                        attachments: paths.clone(),
                    },
                    Completion::Objective(objective, paths),
                )?;
            }
            Action::StopConversation => {
                let run_id = self
                    .active_conversation()
                    .ok_or("この会話に進行中の応答はありません")?;
                self.command(Command::CancelRun { run_id }, Completion::Other)?;
            }
            Action::Cancel(run_id) => {
                self.command(Command::CancelRun { run_id }, Completion::Other)?
            }
            Action::Attachments => self.share_open = !self.share_open,
            Action::PreviewFile => {
                let path = self.field(self.ids.attachment_path);
                if path.trim().is_empty() {
                    return Err("共有する資料の絶対パスを入力してください".into());
                }
                self.job(Job::PreviewFile {
                    path,
                    scope: self.scope.clone(),
                })?;
                self.notice = "資料を読み込んでいます。まだ共有していません。".into();
            }
            Action::ClearAttachments => {
                self.attachments.clear();
                self.notice = "共有する資料を外しました".into();
            }
            Action::SharePreview => {
                let Some(Preview::Attachment { attachment, scope }) = self.preview.take() else {
                    return Err("共有前に資料を確認してください".into());
                };
                if scope != self.scope {
                    return Err("参照範囲が変わりました。資料をもう一度確認してください".into());
                }
                if self.attachments.len() >= ember::model::MAX_ATTACHMENTS
                    && !self.attachments.iter().any(|a| a.path == attachment.path)
                {
                    return Err("一度に共有できる資料は8件までです".into());
                }
                self.attachments.retain(|a| a.path != attachment.path);
                self.attachments.push(attachment);
                self.share_open = false;
                self.notice =
                    "次に送信する内容へ資料を添えます。送信時の内容が共有されます。".into();
            }
            Action::Artifact(id) => {
                let run = self
                    .view
                    .state
                    .runs
                    .iter()
                    .find(|run| run.id == id)
                    .ok_or("依頼が見つかりません")?;
                let title = run
                    .artifact
                    .as_ref()
                    .map_or_else(|| run.kind.label().to_owned(), |a| a.title.clone());
                if self.demo {
                    self.open_text(
                        title,
                        run.answer.clone().unwrap_or_default(),
                        "デモの成果物です。実際の調査結果ではありません。",
                    );
                } else {
                    self.job(Job::Artifact { run_id: id, title })?;
                    self.notice = "保存した成果物を確認しています…".into();
                }
            }
            Action::ReadMessage(id) => {
                let message = self
                    .view
                    .state
                    .messages
                    .iter()
                    .find(|message| message.id == id)
                    .ok_or("会話が見つかりません")?;
                self.open_text(
                    format!("{} · {}", message.role.label(), message.scope.label()),
                    message.content.clone(),
                    "読み取り専用 · テキストを選択してコピーできます",
                );
            }
            Action::Remember(id) => {
                let message = self
                    .view
                    .state
                    .messages
                    .iter()
                    .find(|message| message.id == id)
                    .ok_or("保存する会話が見つかりません")?;
                let value = message.content.clone();
                let scope = message.scope.clone();
                self.memory_edit = None;
                self.memory_revision = None;
                self.memory_scope = scope;
                self.set_area(self.ids.memory_text, value);
                self.page = Page::Memory;
                self.preview = None;
                self.notice = "保存内容と使う範囲を確認して、「記憶に保存」を選んでください".into();
            }
            Action::EditMemory(id) => {
                let memory = self
                    .view
                    .state
                    .memories
                    .iter()
                    .find(|memory| memory.id == id)
                    .ok_or("記憶が見つかりません")?
                    .clone();
                self.edit_memory(memory);
            }
            Action::SaveMemory => {
                let value = self.area(self.ids.memory_text);
                if value.trim().is_empty() || value.len() > 4096 {
                    return Err("記憶は空でない4KiB以内の文章にしてください".into());
                }
                self.command(
                    Command::SaveMemory {
                        id: self.memory_edit.clone(),
                        expected_revision: self.memory_revision,
                        scope: self.memory_scope.clone(),
                        text: value.clone(),
                    },
                    Completion::Memory {
                        id: self.memory_edit.clone(),
                        scope: self.memory_scope.clone(),
                        text: value,
                    },
                )?;
            }
            Action::NewMemory => {
                self.memory_edit = None;
                self.memory_revision = None;
                self.memory_scope = self.scope.clone();
                self.set_area(self.ids.memory_text, String::new());
            }
            Action::DeleteMemory(id) => {
                let expected_revision = self
                    .view
                    .state
                    .memories
                    .iter()
                    .find(|memory| memory.id == id)
                    .ok_or("記憶が見つかりません")?
                    .revision;
                self.command(
                    Command::DeleteMemory {
                        id,
                        expected_revision,
                    },
                    Completion::Other,
                )?;
            }
            Action::SaveSettings => {
                let settings = Settings {
                    model: self.field(self.ids.model).trim().into(),
                    notifications: self
                        .tree
                        .get::<Checkbox>(self.ids.notifications)
                        .is_some_and(|checkbox| checkbox.state() == CheckState::On),
                };
                settings.validate()?;
                self.command(
                    Command::UpdateSettings {
                        settings: settings.clone(),
                    },
                    Completion::Settings(settings),
                )?;
            }
            Action::ApplyProject => {
                let path = self.field(self.ids.project_path);
                if path.trim().is_empty() {
                    return Err("プロジェクトのフォルダーを指定してください".into());
                }
                self.job(Job::Project(path))?;
                self.scope_chosen = true;
                self.notice = "プロジェクトを確認しています…".into();
            }
            Action::Reconnect => {
                self.job(Job::Reconnect)?;
                self.notice = "再接続しています…".into();
            }
            Action::RetryPending => {
                let pending = self.pending.as_ref().ok_or("確認中の操作はありません")?;
                if !pending.in_flight {
                    self.job(Job::Command {
                        request_id: pending.id.clone(),
                        intent: pending.command.clone(),
                    })?;
                    if let Some(pending) = &mut self.pending {
                        pending.in_flight = true;
                    }
                    self.notice = "同じ操作の受付を再確認しています…".into();
                }
            }
            Action::Archive(run_id, archived) => {
                self.command(Command::ArchiveRun { run_id, archived }, Completion::Other)?
            }
            Action::ShowArchived => {
                self.show_archived = !self.show_archived;
                self.refresh_needed = true;
            }
            Action::DeleteRun(run_id) => {
                self.open_text(
                    "依頼を削除".into(),
                    "この依頼と保存した成果物を削除します。必要な内容は先にコピーしてください。"
                        .into(),
                    "削除する対象を確認してください。",
                );
                self.preview = Some(Preview::Confirm(Command::DeleteRun { run_id }));
            }
            Action::ClearConversation => {
                self.open_text("この範囲の会話を削除".into(),format!("対象: {}\n\nこの範囲の会話を削除し、今後の応答で参照しないようにします。保存した記憶は別に管理できます。",scope_description(&self.scope)),"実行中の依頼がある場合は、先に停止してください。");
                self.preview = Some(Preview::Confirm(Command::ClearConversation {
                    scope: self.scope.clone(),
                }));
            }
            Action::Confirm => {
                let Some(Preview::Confirm(command)) = self.preview.take() else {
                    return Ok(());
                };
                self.command(command, Completion::Other)?;
            }
            Action::PrepareHandoff(id) => {
                self.job(Job::PrepareHandoff(id))?;
                self.notice = "Workflowへの引継ぎ内容を準備しています…".into();
            }
            Action::OpenHandoff => {
                let Some(Preview::Handoff(path)) = &self.preview else {
                    return Err("引継ぎ内容を先に確認してください".into());
                };
                self.job(Job::OpenHandoff(path.clone()))?;
            }
            Action::ClosePreview => self.preview = None,
            Action::Export => {
                self.job(Job::Export)?;
                self.notice = "保存したデータを読み込んでいます…".into();
            }
        }
        self.sync_chrome();
        if self.refresh_needed && !self.in_overlay() {
            self.refresh_lists(text);
        }
        Ok(())
    }

    fn edit_memory(&mut self, memory: MemoryEntry) {
        self.memory_edit = Some(memory.id);
        self.memory_revision = Some(memory.revision);
        self.memory_scope = memory.scope;
        self.set_area(self.ids.memory_text, memory.text);
        self.page = Page::Memory;
        self.preview = None;
        self.tree.set_focus(Some(self.ids.memory_text));
    }

    fn open_text(&mut self, title: String, body: String, note: &str) {
        self.label(self.ids.preview_title, visible(&title));
        self.label(self.ids.preview_note, note);
        self.tree
            .update::<TextView, _>(self.ids.preview_body, |view, cx| {
                view.set_text(cx, visible(&body))
            });
        self.preview = Some(Preview::Text);
    }

    fn complete(&mut self, pending: Pending, view: View) {
        match pending.completion {
            Completion::Composer(value, paths) => {
                if self.area(self.ids.composer) == value && !self.composing(self.ids.composer) {
                    self.set_area(self.ids.composer, String::new());
                }
                if self.attachment_paths() == paths {
                    self.attachments.clear();
                }
                self.notice = "相談を受け付けました".into();
            }
            Completion::Objective(value, paths) => {
                if self.area(self.ids.objective) == value && !self.composing(self.ids.objective) {
                    self.set_area(self.ids.objective, String::new());
                }
                if self.attachment_paths() == paths {
                    self.attachments.clear();
                }
                self.notice = "依頼を受け付けました。結果は依頼一覧に表示されます。".into();
            }
            Completion::Memory { id, scope, text } => {
                if self.memory_edit == id
                    && self.memory_scope == scope
                    && self.area(self.ids.memory_text) == text
                    && !self.composing(self.ids.memory_text)
                {
                    self.set_area(self.ids.memory_text, String::new());
                    self.memory_edit = None;
                    self.memory_revision = None;
                } else if self.memory_edit == id
                    && let Some(id) = id
                    && let Some(memory) = view.state.memories.iter().find(|memory| memory.id == id)
                {
                    self.memory_revision = Some(memory.revision);
                }
                self.notice = "記憶を保存しました".into();
            }
            Completion::Settings(settings) => {
                self.settings_dirty = self.field(self.ids.model).trim() != settings.model
                    || self
                        .tree
                        .get::<Checkbox>(self.ids.notifications)
                        .is_some_and(|checkbox| checkbox.state() == CheckState::On)
                        != settings.notifications;
                self.notice = "設定を保存しました".into();
            }
            Completion::Other => self.notice = "操作を反映しました".into(),
        }
        self.receive_view(view);
    }

    fn receive_view(&mut self, view: View) {
        self.connected = true;
        self.view = view;
        self.sync_settings();
        self.refresh_needed = true;
    }

    fn receive(&mut self, update: Update) {
        match update {
            Update::Connected(view) => {
                self.notice.clear();
                self.receive_view(view);
            }
            Update::View(view) => self.receive_view(view),
            Update::Disconnected(error) => {
                self.connected = false;
                self.notice = error;
            }
            Update::Command { request_id, result } => {
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.id == request_id)
                {
                    match result {
                        Ok(view) => {
                            if let Some(pending) = self.pending.take() {
                                self.complete(pending, view);
                            }
                        }
                        Err(error) => {
                            if error.outcome_unknown {
                                if let Some(pending) = &mut self.pending {
                                    pending.in_flight = false;
                                }
                                self.notice = format!(
                                    "{} · 入力は保持しています。受付を再確認できます。",
                                    error.message
                                );
                            } else {
                                self.pending = None;
                                self.notice = error.message;
                            }
                        }
                    }
                }
            }
            Update::Attachment { scope, result } => match result {
                Ok(attachment) if scope == self.scope => {
                    self.open_text("共有する資料を確認".into(),attachment.text.clone(),"「この内容を共有」で次の送信に添付します。送信時点のファイル内容が共有されます。");
                    self.label(
                        self.ids.preview_note,
                        format!(
                            "{} · {} bytes\n送信時のファイル内容を共有します。",
                            visible(&attachment.path),
                            attachment.bytes
                        ),
                    );
                    self.preview = Some(Preview::Attachment { attachment, scope });
                }
                Ok(_) => {
                    self.notice = "参照範囲が変わったため、資料をもう一度確認してください".into()
                }
                Err(error) => self.notice = error,
            },
            Update::Text { title, result } => match result {
                Ok(body) => self.open_text(
                    title,
                    body,
                    "読み取り専用 · テキストを選択してコピーできます",
                ),
                Err(error) => self.notice = error,
            },
            Update::Project { initial, result } => match result {
                Ok(scope) => {
                    self.project = Some(scope.clone());
                    if !initial || !self.scope_chosen {
                        self.scope = scope.clone();
                        if self.memory_edit.is_none() {
                            self.memory_scope = scope.clone();
                        }
                        self.attachments.clear();
                    }
                    if let Scope::Project { root } = scope {
                        self.set_field(self.ids.project_path, root);
                    }
                    self.refresh_needed = true;
                    self.notice="プロジェクトを選びました。ファイルは明示的に添付したものだけを参照します。".into();
                }
                Err(error) => self.notice = error,
            },
            Update::Handoff(result) => match result {
                Ok(preview) => {
                    self.open_text(
                        preview.title,
                        preview.objective,
                        "Workflowの開始画面を開きます。ここでは開発を開始しません。",
                    );
                    self.label(
                        self.ids.preview_note,
                        format!(
                            "{}\nWorkflowの開始画面で目的を確認してから実行します。",
                            preview.repo.display()
                        ),
                    );
                    self.preview = Some(Preview::Handoff(preview.path));
                }
                Err(error) => self.notice = error,
            },
            Update::HandoffOpened(result) => {
                self.notice=match result {
                    Ok(())=>"Workflowの起動を要求しました。開いた開始画面で実行してください。状況・結果はWorkflowで確認できます。".into(),
                    Err(error)=>error,
                };
            }
        }
        self.sync_chrome();
    }

    fn sync_settings(&mut self) {
        if !self.settings_dirty
            && self.tree.focused() != Some(self.ids.model)
            && self
                .tree
                .get::<TextField>(self.ids.model)
                .is_none_or(|field| field.composition().is_empty())
        {
            self.set_field(self.ids.model, self.view.state.settings.model.clone());
            let state = if self.view.state.settings.notifications {
                CheckState::On
            } else {
                CheckState::Off
            };
            self.tree
                .update::<Checkbox, _>(self.ids.notifications, |checkbox, cx| {
                    checkbox.set_state(cx, state)
                });
        }
    }

    fn in_overlay(&self) -> bool {
        self.tree.focused().is_some_and(|focused| {
            let overlays: Vec<_> = self.tree.overlays().collect();
            self.tree
                .ancestors(focused)
                .any(|id| overlays.contains(&id))
        })
    }

    fn composing(&self, id: WidgetId) -> bool {
        self.tree
            .get::<TextArea>(id)
            .is_some_and(|area| !area.composition().is_empty())
    }
}

impl Screen for EmberScreen {
    fn tree(&self) -> &UiTree {
        &self.tree
    }
    fn tree_mut(&mut self) -> &mut UiTree {
        &mut self.tree
    }
    fn set_system_appearance(&mut self, appearance: noa_gpui_core::Appearance) {
        crate::appearance::system(&mut self.tree, appearance);
    }
    fn prepare(&mut self, viewport: Size, text: &mut TextStore) -> bool {
        let narrow = viewport.width < 940.0;
        if self.narrow != Some(narrow) {
            self.narrow = Some(narrow);
            self.tree
                .update::<Container, _>(self.ids.shell, |container, cx| {
                    container.set_style(cx, views::shell_style(narrow))
                });
            self.tree
                .update::<Container, _>(self.ids.navigation, |container, cx| {
                    container.set_style(cx, views::navigation_style(narrow))
                });
        }
        if self.refresh_needed && !self.in_overlay() {
            self.refresh_lists(text);
        }
        self.sync_settings();
        self.sync_chrome();
        self.tree.prepare(viewport, text).is_some()
    }
    fn woken(&mut self, text: &mut TextStore) {
        let updates = self.feed.as_ref().map(Feed::drain).unwrap_or_default();
        for update in updates {
            self.receive(update);
        }
        if self.refresh_needed && !self.in_overlay() {
            self.refresh_lists(text);
        }
    }
    fn dispatch(&mut self, event: Event, text: &mut TextStore) {
        if let Event::KeyDown {
            key: Key::Enter,
            modifiers,
        } = &event
            && modifiers.command
            && !self.in_overlay()
            && self.preview.is_none()
        {
            let focus = self.tree.focused();
            let action = if focus == Some(self.ids.composer) && !self.composing(self.ids.composer) {
                Some(Action::Send)
            } else if focus == Some(self.ids.objective) && !self.composing(self.ids.objective) {
                Some(Action::Start)
            } else {
                None
            };
            if let Some(action) = action {
                if let Err(error) = self.act(action, text) {
                    self.notice = error;
                    self.sync_chrome();
                }
                return;
            }
        }
        self.tree.dispatch(event, text);
        for message in self.tree.take_messages() {
            self.handle(message, text);
        }
    }
    fn handle(&mut self, message: Message, text: &mut TextStore) {
        if message.downcast_ref::<ButtonClicked>().is_some() {
            if let Some(action) = self
                .actions
                .iter()
                .find(|(id, _)| *id == message.source)
                .map(|(_, action)| action.clone())
                && let Err(error) = self.act(action, text)
            {
                self.notice = error;
            }
        } else if message.downcast_ref::<SelectionChanged>().is_some() {
            let selected = self
                .tree
                .get::<PopUpButton>(message.source)
                .map_or(0, PopUpButton::selected);
            if message.source == self.ids.scope {
                self.scope_chosen = true;
                let scope = if selected == 0 {
                    Some(Scope::Global)
                } else {
                    self.project.clone()
                };
                if let Some(scope) = scope {
                    if scope != self.scope {
                        self.attachments.clear();
                        self.notice = "参照範囲を切り替え、共有資料を外しました".into();
                    }
                    self.scope = scope;
                    self.refresh_needed = true;
                    if self.memory_edit.is_none() {
                        self.memory_scope = self.scope.clone();
                    }
                } else {
                    self.notice = "設定でプロジェクトのフォルダーを選んでください".into();
                }
            } else if message.source == self.ids.memory_scope {
                if selected == 0 {
                    self.memory_scope = Scope::Global;
                } else if let Some(project) = &self.project {
                    self.memory_scope = project.clone();
                } else {
                    self.notice =
                        "プロジェクト限定の記憶には、先にプロジェクトを選んでください".into();
                }
            }
        } else if message.downcast_ref::<TextChanged>().is_some() {
            if message.source == self.ids.model {
                self.settings_dirty = true;
            }
        } else if message.downcast_ref::<CheckboxChanged>().is_some()
            && message.source == self.ids.notifications
        {
            self.settings_dirty = true;
        }
        self.sync_chrome();
    }
}

fn scope_description(scope: &Scope) -> String {
    match scope {
        Scope::Global => "すべての会話（個人）".into(),
        Scope::Project { root } => format!("このプロジェクト · {}", visible(root)),
    }
}

fn excerpt(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let prefix: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        format!("{}…", visible(&prefix))
    } else {
        visible(&prefix)
    }
}

fn visible(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        if (character.is_control() && !matches!(character, '\n' | '\t'))
            || matches!(character,'\u{061c}'|'\u{200e}'|'\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}'|'\u{feff}')
        {
            output.push_str(&format!("[U+{:04X}]", character as u32));
        } else {
            output.push(character);
        }
    }
    output
}
