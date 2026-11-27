//! Workspace navigation, command dispatch and persisted personal views.
//! Source edits still go through Feed; planning changes use Workspace's
//! atomic transactions, including bulk operations and undo.

use std::collections::{BTreeMap, BTreeSet};

use noa_gpui_core::widgets::{
    Button, ButtonClicked, CommandChosen, CommandPalette, Container, ControlSize, Label,
    PaletteCommand, PopUpButton, SelectionChanged, TextField,
};
use noa_gpui_core::{Key, Message, Role, WidgetId};
use noa_gpui_text::TextStore;

use tasks::model::types::{Source, State, Task, TaskRef};
use tasks::workspace::{
    self, Change, Preferences, Priority, SavedView, Sort, TaskMeta, Workflow, Workspace,
};

use super::organizer::{self, Action, Controls, NavItem, PropertyForm};
use super::{Card, Feed, Form, Intent, Layout, Tasks, clear, style};

#[derive(Clone)]
enum Command {
    New,
    Search,
    Open(TaskRef),
    Properties(TaskRef),
    Run(TaskRef),
    Apply(Vec<TaskRef>, Change),
    Undo,
    SaveView,
    RemoveView(String),
    Sort(Sort),
    Navigate(String),
    Layout(Layout),
    SelectAll,
    ClearSelection,
    Move(isize),
    Refresh,
    ToggleSidebar,
    Help,
}

pub(super) struct Planning {
    pub workspace: Workspace,
    pub section: String,
    pub sort: Sort,
    pub checked: BTreeSet<TaskRef>,
    pub nav_host: WidgetId,
    pub properties_host: WidgetId,
    pub toolbar_host: WidgetId,
    pub palette_button: WidgetId,
    pub nav_controls: Controls,
    pub row_controls: Controls,
    pub properties_controls: Controls,
    pub property_form: Option<PropertyForm>,
    pub notice: Option<String>,
    pub sidebar_hidden: bool,
    toolbar_actions: Vec<(WidgetId, Command)>,
    bulk_status: Option<WidgetId>,
    bulk_priority: Option<WidgetId>,
    sort_control: Option<WidgetId>,
    active_palette: Option<(WidgetId, Vec<Command>)>,
    save_form: Option<(WidgetId, WidgetId, WidgetId)>,
    preferences_dirty: bool,
    pending_states: BTreeMap<TaskRef, (State, Option<Workflow>)>,
}

impl Planning {
    pub fn new(
        feed: &dyn Feed,
        nav_host: WidgetId,
        properties_host: WidgetId,
        toolbar_host: WidgetId,
        palette_button: WidgetId,
    ) -> Self {
        let (workspace, notice) = match feed.workspace_path() {
            Some(path) => match Workspace::open(path) {
                Ok(workspace) => (workspace, None),
                Err(error) => (
                    Workspace::memory(),
                    Some(format!(
                        "作業管理を読み込めません（この画面の変更は保存されません）: {error}"
                    )),
                ),
            },
            None => (Workspace::memory(), None),
        };
        let preferences = workspace.data().preferences.clone();
        Self {
            workspace,
            section: preferences.section,
            sort: preferences.sort,
            checked: BTreeSet::new(),
            nav_host,
            properties_host,
            toolbar_host,
            palette_button,
            nav_controls: Controls::default(),
            row_controls: Controls::default(),
            properties_controls: Controls::default(),
            property_form: None,
            notice,
            sidebar_hidden: false,
            toolbar_actions: Vec::new(),
            bulk_status: None,
            bulk_priority: None,
            sort_control: None,
            active_palette: None,
            save_form: None,
            preferences_dirty: false,
            pending_states: BTreeMap::new(),
        }
    }

    pub fn form_open(&self) -> bool {
        self.property_form.is_some() || self.save_form.is_some()
    }
}

impl Tasks {
    /// CLI layout overrides the saved view; a new workspace starts as a list.
    pub fn initialize_layout(&mut self, explicit: Option<Layout>, text: &mut TextStore) {
        let preferences = self.planning.workspace.data().preferences.clone();
        self.calendar_mode = super::views::CalendarMode::from_word(&preferences.calendar_mode);
        self.filter.text = preferences.query;
        self.filter.source = preferences.source.as_deref().and_then(source_from_word);
        self.filter.label = preferences.label;
        let query = self.filter.text.clone();
        self.tree
            .update::<TextField, _>(self.text, |field, cx| field.set_text(cx, query));
        let source = super::SOURCES
            .iter()
            .position(|(_, source)| *source == self.filter.source)
            .unwrap_or(0);
        self.tree
            .update::<super::SegmentedControl, _>(self.source, |control, cx| {
                control.set_selected(cx, &[source])
            });
        self.set_layout(
            explicit
                .or_else(|| Layout::from_word(&preferences.layout))
                .unwrap_or(Layout::List),
            text,
        );
    }

    pub(super) fn sync_filter_controls(&mut self) {
        let query = self.filter.text.clone();
        self.tree
            .update::<TextField, _>(self.text, |field, cx| field.set_text(cx, query));
        let source = super::SOURCES
            .iter()
            .position(|(_, source)| *source == self.filter.source)
            .unwrap_or(0);
        self.tree
            .update::<super::SegmentedControl, _>(self.source, |control, cx| {
                control.set_selected(cx, &[source])
            });
        let layout = Layout::ALL
            .iter()
            .position(|layout| *layout == self.layout)
            .unwrap_or(0);
        self.tree
            .update::<super::SegmentedControl, _>(self.layout_control, |control, cx| {
                control.set_selected(cx, &[layout])
            });
        let label = self
            .filter
            .label
            .as_ref()
            .and_then(|label| self.label_choices.iter().position(|value| value == label))
            .map_or(0, |i| i + 1);
        self.tree
            .update::<PopUpButton, _>(self.label_popup, |control, cx| {
                control.set_selected(cx, label)
            });
    }

    fn section_matches(&self, section: &str, task: &Task, meta: &TaskMeta) -> bool {
        let last = self.last_run(&task.r#ref);
        let workflow = workspace::effective_workflow(task, last, meta);
        match section {
            "all" => true,
            "inbox" => workspace::needs_attention(last),
            "triage" => {
                meta.workflow.is_none()
                    && last.is_none()
                    && task.r#ref.source() != Source::Local
                    && task.state == tasks::model::types::State::Open
            }
            "active" => matches!(
                workflow,
                Workflow::Todo | Workflow::InProgress | Workflow::InReview
            ),
            "backlog" => workflow == Workflow::Backlog,
            "review" => workflow == Workflow::InReview,
            "done" => workflow.is_finished(),
            "pinned" => meta.pinned,
            _ => {
                if let Some(project) = section.strip_prefix("project:") {
                    meta.project == project
                } else if let Some(cycle) = section.strip_prefix("cycle:") {
                    meta.cycle == cycle
                } else if let Some(id) = section.strip_prefix("saved:") {
                    self.planning
                        .workspace
                        .data()
                        .views
                        .iter()
                        .find(|v| v.id == id)
                        .is_some_and(|view| {
                            // Saved views cannot recursively refer to another saved view.
                            !view.section.starts_with("saved:")
                                && self.section_matches(&view.section, task, meta)
                        })
                } else {
                    true
                }
            }
        }
    }

    pub(super) fn planning_cards(&self, recent_done: bool) -> Vec<Card<'_>> {
        let mut cards: Vec<_> = self
            .all_tasks()
            .into_iter()
            .filter_map(|task| {
                let last = self.last_run(&task.r#ref);
                let meta = self.planning.workspace.meta(&task.r#ref);
                let mut source_filter = self.filter.clone();
                source_filter.text.clear();
                if !super::matches(task, &source_filter)
                    || !self.section_matches(&self.planning.section, task, &meta)
                {
                    return None;
                }
                let searchable = self.searchable_task(task);
                if !workspace::matches_query(&self.filter.text, &searchable, last, &meta) {
                    return None;
                }
                if recent_done
                    && super::column(task, last) == tasks::model::types::Column::Done
                    && self
                        .view
                        .now
                        .saturating_sub(task.closed.unwrap_or(task.updated))
                        > super::DONE_DAYS * super::DAY_MS
                {
                    return None;
                }
                Some(Card { task, last })
            })
            .collect();
        cards.sort_by(|a, b| {
            workspace::compare_tasks(
                self.planning.sort,
                (a.task, a.last, &self.planning.workspace.meta(&a.task.r#ref)),
                (b.task, b.last, &self.planning.workspace.meta(&b.task.r#ref)),
            )
        });
        cards
    }

    fn searchable_task(&self, task: &Task) -> Task {
        let mut searchable = task.clone();
        if let TaskRef::GitHub { number, .. } = &task.r#ref
            && let Some(Ok(detail)) = self.view.github.details.get(number)
        {
            let mut body = detail.body.clone();
            for comment in &detail.comments {
                body.push('\n');
                body.push_str(&comment.body);
            }
            searchable.body = Some(body);
        }
        searchable
    }

    pub(super) fn planning_lists(&self) -> [Vec<Card<'_>>; 4] {
        let mut lists: [Vec<Card<'_>>; 4] = Default::default();
        for card in self.planning_cards(true) {
            lists[super::column(card.task, card.last) as usize].push(card);
        }
        lists
    }

    fn navigation(&self) -> Vec<NavItem> {
        let tasks = self.all_tasks();
        let mut entries = Vec::new();
        for (key, label) in [
            ("all", "すべてのタスク"),
            ("inbox", "要対応"),
            ("triage", "Triage"),
            ("active", "アクティブ"),
            ("backlog", "バックログ"),
            ("review", "レビュー待ち"),
            ("done", "完了・キャンセル"),
            ("pinned", "ピン留め"),
        ] {
            let count = tasks
                .iter()
                .filter(|t| self.section_matches(key, t, &self.planning.workspace.meta(&t.r#ref)))
                .count();
            entries.push(NavItem {
                key: key.into(),
                label: label.into(),
                count,
                group: "タスク".into(),
            });
        }
        for (prefix, group) in [("project:", "プロジェクト"), ("cycle:", "サイクル")] {
            let values: BTreeSet<_> = tasks
                .iter()
                .map(|t| self.planning.workspace.meta(&t.r#ref))
                .map(|m| {
                    if prefix == "project:" {
                        m.project
                    } else {
                        m.cycle
                    }
                })
                .filter(|s| !s.is_empty())
                .collect();
            for value in values {
                let key = format!("{prefix}{value}");
                let count = tasks
                    .iter()
                    .filter(|t| {
                        self.section_matches(&key, t, &self.planning.workspace.meta(&t.r#ref))
                    })
                    .count();
                entries.push(NavItem {
                    key,
                    label: value,
                    count,
                    group: group.into(),
                });
            }
        }
        for view in &self.planning.workspace.data().views {
            let key = format!("saved:{}", view.id);
            let count = tasks
                .iter()
                .filter(|task| {
                    let meta = self.planning.workspace.meta(&task.r#ref);
                    self.section_matches(&key, task, &meta)
                        && view
                            .source
                            .as_deref()
                            .and_then(source_from_word)
                            .is_none_or(|source| task.r#ref.source() == source)
                        && view.label.as_ref().is_none_or(|label| {
                            task.labels
                                .iter()
                                .any(|value| value.eq_ignore_ascii_case(label))
                        })
                        && workspace::matches_query(
                            &view.query,
                            &self.searchable_task(task),
                            self.last_run(&task.r#ref),
                            &meta,
                        )
                })
                .count();
            entries.push(NavItem {
                key,
                label: view.name.clone(),
                count,
                group: "保存したビュー".into(),
            });
        }
        entries
    }

    pub(super) fn rebuild_planning(&mut self, text: &mut TextStore) {
        let visible: BTreeSet<_> = self.cards.iter().map(|(_, task)| task.clone()).collect();
        self.planning.checked.retain(|r| visible.contains(r));
        let entries = self.navigation();
        let repo_title = self
            .view
            .repo
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Tasks".into());
        clear(&mut self.tree, self.planning.nav_host, text);
        self.planning.nav_controls = organizer::sidebar(
            &mut self.tree,
            self.planning.nav_host,
            &repo_title,
            &entries,
            &self.planning.section,
        );
        self.apply_responsive();
        clear(&mut self.tree, self.planning.toolbar_host, text);
        self.planning.toolbar_actions.clear();
        self.planning.bulk_status = None;
        self.planning.bulk_priority = None;
        let host = self.planning.toolbar_host;
        let label = entries
            .iter()
            .find(|e| e.key == self.planning.section)
            .map(|e| e.label.as_str())
            .unwrap_or("すべてのタスク");
        self.tree.insert(
            Some(host),
            Label::new(format!("{label} · {} 件", visible.len()))
                .font_size(crate::appearance::font(12.0))
                .weight(700),
        );
        if !self.planning.checked.is_empty() {
            self.tree.insert(
                Some(host),
                Label::new(format!("{} 件を選択", self.planning.checked.len()))
                    .font_size(crate::appearance::font(12.0))
                    .color(Role::Accent),
            );
            self.planning.bulk_status = Some(
                self.tree.insert(
                    Some(host),
                    PopUpButton::new(
                        std::iter::once("状態を一括変更").chain(Workflow::ALL.map(Workflow::label)),
                    )
                    .control_size(ControlSize::Small),
                ),
            );
            self.planning.bulk_priority = Some(
                self.tree.insert(
                    Some(host),
                    PopUpButton::new(
                        std::iter::once("優先度を一括変更")
                            .chain(Priority::ALL.map(Priority::label)),
                    )
                    .control_size(ControlSize::Small),
                ),
            );
            let clear_selection = self.tree.insert(
                Some(host),
                Button::new("選択解除").control_size(ControlSize::Small),
            );
            self.planning
                .toolbar_actions
                .push((clear_selection, Command::ClearSelection));
        }
        style::spacer(&mut self.tree, host);
        self.planning.sort_control = Some(
            self.tree.insert(
                Some(host),
                PopUpButton::new(Sort::ALL.map(Sort::label))
                    .selected_index(
                        Sort::ALL
                            .iter()
                            .position(|s| *s == self.planning.sort)
                            .unwrap_or(0),
                    )
                    .control_size(ControlSize::Small),
            ),
        );
        if self.planning.workspace.can_undo() {
            let undo = self.tree.insert(
                Some(host),
                Button::new("取り消す").control_size(ControlSize::Small),
            );
            self.planning.toolbar_actions.push((undo, Command::Undo));
        }
    }

    pub(super) fn rebuild_properties(&mut self, text: &mut TextStore) {
        clear(&mut self.tree, self.planning.properties_host, text);
        self.planning.properties_controls = Controls::default();
        let shown = self.form.is_none() && !self.planning.form_open();
        if shown && let Some(task) = self.detail.as_ref().and_then(|r| self.task(r)).cloned() {
            let meta = self.planning.workspace.meta(&task.r#ref);
            let last = self.last_run(&task.r#ref).cloned();
            self.planning.properties_controls = organizer::property_summary(
                &mut self.tree,
                self.planning.properties_host,
                &task,
                &meta,
                last.as_ref(),
            );
        }
        self.tree.set_hidden(
            self.planning.properties_host,
            !shown || self.detail.is_none(),
        );
    }

    pub(super) fn mark_preferences_dirty(&mut self) {
        self.planning.preferences_dirty = true;
    }

    pub(super) fn persist_preferences(&mut self) {
        if !self.planning.preferences_dirty {
            return;
        }
        let preferences = Preferences {
            section: self.planning.section.clone(),
            query: self.filter.text.clone(),
            layout: self.layout.word().into(),
            sort: self.planning.sort,
            source: self.filter.source.map(|s| source_word(s).into()),
            label: self.filter.label.clone(),
            calendar_mode: self.calendar_mode.word().into(),
        };
        match self.planning.workspace.set_preferences(preferences) {
            Ok(_) => self.planning.preferences_dirty = false,
            Err(error) => self.own_notice = Some(error),
        }
    }

    fn command_targets(&self) -> Vec<TaskRef> {
        if self.planning.checked.is_empty() {
            self.selected.iter().cloned().collect()
        } else {
            self.planning.checked.iter().cloned().collect()
        }
    }

    fn apply_planning(&mut self, tasks: &[TaskRef], changes: &[Change], text: &mut TextStore) {
        match self.planning.workspace.apply(tasks, changes, self.view.now) {
            Ok(true) => {
                self.own_notice = Some(format!("{} 件を更新しました · 取り消し可能", tasks.len()))
            }
            Ok(false) => {}
            Err(error) => self.own_notice = Some(error),
        }
        self.rebuild(text);
    }

    fn navigate(&mut self, section: &str, text: &mut TextStore) {
        self.narrow_sidebar_open = false;
        self.detail = None;
        self.planning.section = section.into();
        self.planning.checked.clear();
        self.filter.text.clear();
        self.filter.source = None;
        self.filter.label = None;
        self.selected = None;
        if let Some(id) = section.strip_prefix("saved:")
            && let Some(view) = self
                .planning
                .workspace
                .data()
                .views
                .iter()
                .find(|v| v.id == id)
                .cloned()
        {
            self.filter.text = view.query;
            self.filter.source = view.source.as_deref().and_then(source_from_word);
            self.filter.label = view.label;
            self.planning.sort = view.sort;
            if let Some(layout) = Layout::from_word(&view.layout) {
                self.layout = layout;
            }
        }
        let query = self.filter.text.clone();
        self.tree
            .update::<TextField, _>(self.text, |field, cx| field.set_text(cx, query));
        self.sync_filter_controls();
        self.mark_preferences_dirty();
        self.persist_preferences();
        self.rebuild(text);
    }

    fn open_properties(&mut self, r: TaskRef, text: &mut TextStore) {
        let Some(task) = self.task(&r).cloned() else {
            return;
        };
        self.narrow_sidebar_open = false;
        self.form = None;
        self.planning.save_form = None;
        self.detail = Some(r.clone());
        let choices = self
            .all_tasks()
            .iter()
            .map(|t| {
                (
                    workspace::task_key(&t.r#ref),
                    format!("{} · {}", t.r#ref.badge(), t.title),
                )
            })
            .collect();
        let mut form = PropertyForm::new(&task, self.planning.workspace.meta(&r), choices);
        clear(&mut self.tree, self.form_host, text);
        form.insert(&mut self.tree, self.form_host);
        self.planning.property_form = Some(form);
        self.rebuild_detail(text);
    }

    pub(super) fn close_planning_form(&mut self, text: &mut TextStore) {
        self.planning.property_form = None;
        self.planning.save_form = None;
        clear(&mut self.tree, self.form_host, text);
        self.rebuild_detail(text);
    }

    fn open_save_view(&mut self, text: &mut TextStore) {
        self.narrow_sidebar_open = false;
        self.form = None;
        self.planning.property_form = None;
        clear(&mut self.tree, self.form_host, text);
        let host = self.form_host;
        self.tree.insert(
            Some(host),
            Label::new("現在のビューを保存")
                .font_size(crate::appearance::font(16.0))
                .weight(700),
        );
        self.tree.insert(
            Some(host),
            Label::new("検索・取得元・ラベル・表示形式・並び順を保存します。")
                .font_size(crate::appearance::font(12.0))
                .wrap(true)
                .color(Role::Muted),
        );
        let name = self
            .tree
            .insert(Some(host), TextField::new("ビューの名前", 300.0));
        let row = self
            .tree
            .insert(Some(host), Container::new(style::row(8.0)));
        let save = self
            .tree
            .insert(Some(row), Button::new("保存").default_button(true));
        let cancel = self.tree.insert(Some(row), Button::new("キャンセル"));
        self.planning.save_form = Some((name, save, cancel));
        self.rebuild_detail(text);
        self.tree.set_focus(Some(name));
    }

    fn save_view(&mut self, text: &mut TextStore) {
        let Some((name, _, _)) = self.planning.save_form else {
            return;
        };
        let name = self
            .tree
            .get::<TextField>(name)
            .map(|f| f.text().trim().to_owned())
            .unwrap_or_default();
        if name.is_empty() {
            self.own_notice = Some("ビューの名前を入力してください".into());
            self.show_notice();
            return;
        }
        let section = self
            .planning
            .section
            .strip_prefix("saved:")
            .and_then(|id| {
                self.planning
                    .workspace
                    .data()
                    .views
                    .iter()
                    .find(|v| v.id == id)
            })
            .map(|v| v.section.clone())
            .unwrap_or_else(|| self.planning.section.clone());
        let query = self.filter.text.clone();
        let id = format!(
            "view-{}-{}",
            self.view.now,
            self.planning.workspace.data().revision
        );
        let saved = SavedView {
            id: id.clone(),
            name,
            query: query.trim().into(),
            section,
            layout: self.layout.word().into(),
            sort: self.planning.sort,
            source: self.filter.source.map(|s| source_word(s).into()),
            label: self.filter.label.clone(),
        };
        match self.planning.workspace.save_view(saved) {
            Ok(_) => {
                self.close_planning_form(text);
                self.navigate(&format!("saved:{id}"), text);
            }
            Err(error) => {
                self.own_notice = Some(error);
                self.show_notice();
            }
        }
    }

    pub(super) fn planning_message(&mut self, message: &Message, text: &mut TextStore) -> bool {
        if let Some((id, commands)) = &self.planning.active_palette
            && message.source == *id
            && let Some(CommandChosen(index)) = message.downcast_ref::<CommandChosen>()
        {
            let command = commands.get(*index).cloned();
            self.planning.active_palette = None;
            if let Some(command) = command {
                self.run_command(command, text);
            }
            return true;
        }
        if let Some(form) = &mut self.planning.property_form
            && let Some(action) = form.handle(message, &mut self.tree)
        {
            if let Action::Apply { tasks, changes } = action {
                self.save_properties(&tasks, &changes, text);
            } else {
                self.organize_action(action, text);
            }
            return true;
        }
        if let Some((_, save, cancel)) = self.planning.save_form
            && message.downcast_ref::<ButtonClicked>().is_some()
        {
            if message.source == save {
                self.save_view(text);
                return true;
            }
            if message.source == cancel {
                self.close_planning_form(text);
                return true;
            }
        }
        let action = self
            .planning
            .nav_controls
            .action(&self.tree, message)
            .or_else(|| self.planning.row_controls.action(&self.tree, message))
            .or_else(|| {
                self.planning
                    .properties_controls
                    .action(&self.tree, message)
            });
        if let Some(action) = action {
            self.organize_action(action, text);
            return true;
        }
        if message.downcast_ref::<ButtonClicked>().is_some() {
            if message.source == self.planning.palette_button {
                self.open_commands(None);
                return true;
            }
            if let Some((_, command)) = self
                .planning
                .toolbar_actions
                .iter()
                .find(|(id, _)| *id == message.source)
                .cloned()
            {
                self.run_command(command, text);
                return true;
            }
        }
        if let Some(SelectionChanged(index)) = message.downcast_ref::<SelectionChanged>() {
            if self.planning.sort_control == Some(message.source) {
                if let Some(sort) = Sort::ALL.get(*index) {
                    self.run_command(Command::Sort(*sort), text);
                }
                return true;
            }
            let change = if self.planning.bulk_status == Some(message.source) {
                index
                    .checked_sub(1)
                    .and_then(|i| Workflow::ALL.get(i))
                    .map(|w| Change::Workflow(Some(*w)))
            } else if self.planning.bulk_priority == Some(message.source) {
                index
                    .checked_sub(1)
                    .and_then(|i| Priority::ALL.get(i))
                    .map(|p| Change::Priority(*p))
            } else {
                None
            };
            if let Some(change) = change {
                self.apply_planning(&self.command_targets(), &[change], text);
                return true;
            }
        }
        false
    }

    fn organize_action(&mut self, action: Action, text: &mut TextStore) {
        match action {
            Action::Navigate(section) => self.navigate(&section, text),
            Action::ToggleSelection(task) => {
                if !self.planning.checked.remove(&task) {
                    self.planning.checked.insert(task.clone());
                }
                self.selected = Some(task);
                self.rebuild(text);
            }
            Action::Open(task) => {
                self.select_ref(&task);
                self.open_detail(task, text);
            }
            Action::Edit(task) => self.open_properties(task, text),
            Action::Apply { tasks, changes } => {
                self.apply_planning(&tasks, &changes, text);
            }
            Action::NewTask => self.open_form(Form::new_task(), text),
            Action::SaveView => self.open_save_view(text),
            Action::Cancel => self.close_planning_form(text),
        }
    }

    fn save_properties(&mut self, tasks: &[TaskRef], changes: &[Change], text: &mut TextStore) {
        // A live reload must not turn a stale form into permission to
        // overwrite another window's edit to the same field.
        if let Some(form) = &self.planning.property_form
            && tasks
                .iter()
                .any(|task| form.conflicts(&self.planning.workspace.meta(task), changes))
        {
            self.own_notice = Some(
                "別の画面でこの属性が変更されました。入力内容を控えてから開き直してください".into(),
            );
            self.show_notice();
            return;
        }
        match self.planning.workspace.apply(tasks, changes, self.view.now) {
            Ok(_) => {
                self.close_planning_form(text);
                self.own_notice = Some(format!("{} 件を更新しました · 取り消し可能", tasks.len()));
                self.rebuild(text);
            }
            Err(error) => {
                // Failed validation or persistence keeps the draft visible.
                self.own_notice = Some(error);
                self.show_notice();
            }
        }
    }

    pub(super) fn planning_key(&mut self, key: &Key, text: &mut TextStore) -> bool {
        match key {
            Key::Char('c') => self.open_form(Form::new_task(), text),
            Key::Char('s') => self.open_commands(Some("status")),
            Key::Char('p') => self.open_commands(Some("priority")),
            Key::Char('e') => {
                if let Some(task) = self.selected.clone() {
                    self.open_properties(task, text);
                }
            }
            Key::Char('f') => {
                if let Some(task) = self.selected.clone() {
                    let pinned = self.planning.workspace.meta(&task).pinned;
                    self.apply_planning(&[task], &[Change::Pinned(!pinned)], text);
                }
            }
            Key::Space | Key::Char(' ') => {
                if let Some(task) = self.selected.clone() {
                    self.organize_action(Action::ToggleSelection(task), text);
                }
            }
            Key::Char('?') => self.run_command(Command::Help, text),
            _ => return false,
        }
        true
    }

    pub(super) fn planning_command_key(&mut self, key: &Key, text: &mut TextStore) -> bool {
        match key {
            Key::Char('k') => self.open_commands(None),
            Key::Char('z') if !self.typing() => self.run_command(Command::Undo, text),
            Key::Char('a') if !self.typing() => self.run_command(Command::SelectAll, text),
            Key::Char('b') if !self.typing() => self.run_command(Command::ToggleSidebar, text),
            Key::ArrowUp if !self.typing() => self.run_command(Command::Move(-1), text),
            Key::ArrowDown if !self.typing() => self.run_command(Command::Move(1), text),
            _ => return false,
        }
        true
    }

    fn open_commands(&mut self, only: Option<&str>) {
        let mut entries = Vec::new();
        let mut add = |label: String, keywords: &str, command: Command| {
            entries.push((PaletteCommand::new(label).keywords(keywords), command));
        };
        let targets = self.command_targets();
        if !targets.is_empty() {
            if only != Some("priority") {
                for workflow in Workflow::ALL {
                    add(
                        format!("状態: {}（{} 件）", workflow.label(), targets.len()),
                        "status workflow 状態",
                        Command::Apply(targets.clone(), Change::Workflow(Some(workflow))),
                    );
                }
                add(
                    "状態を実行状況から自動表示".into(),
                    "status auto",
                    Command::Apply(targets.clone(), Change::Workflow(None)),
                );
            }
            if only != Some("status") {
                for priority in Priority::ALL {
                    add(
                        format!("優先度: {}（{} 件）", priority.label(), targets.len()),
                        "priority 優先度",
                        Command::Apply(targets.clone(), Change::Priority(priority)),
                    );
                }
            }
        }
        if only.is_none() {
            add("新規タスク".into(), "create new task 新規", Command::New);
            add("タスクを検索".into(), "search find 検索", Command::Search);
            if let Some(task) = self.selected.clone() {
                add(
                    "プロパティを編集".into(),
                    "edit properties assignee project cycle due 親 依存 コメント",
                    Command::Properties(task.clone()),
                );
                add(
                    "エージェントで実行".into(),
                    "run agent 実行",
                    Command::Run(task.clone()),
                );
                let pin = !self.planning.workspace.meta(&task).pinned;
                add(
                    if pin {
                        "ピン留め"
                    } else {
                        "ピンを外す"
                    }
                    .into(),
                    "pin favorite お気に入り",
                    Command::Apply(vec![task], Change::Pinned(pin)),
                );
            }
            if self.planning.workspace.can_undo() {
                add(
                    "最後のプロパティ変更を取り消す".into(),
                    "undo 取り消し",
                    Command::Undo,
                );
            }
            add(
                "表示中のタスクをすべて選択".into(),
                "select all bulk",
                Command::SelectAll,
            );
            add(
                "選択解除".into(),
                "clear selection",
                Command::ClearSelection,
            );
            add(
                "現在のビューを保存".into(),
                "save view filter favorite",
                Command::SaveView,
            );
            if let Some(id) = self.planning.section.strip_prefix("saved:") {
                add(
                    "この保存ビューを削除".into(),
                    "delete view",
                    Command::RemoveView(id.into()),
                );
            }
            for sort in Sort::ALL {
                add(
                    format!("並び順: {}", sort.label()),
                    "sort 並べ替え",
                    Command::Sort(sort),
                );
            }
            add(
                "選択したタスクを一つ上へ".into(),
                "move up reorder",
                Command::Move(-1),
            );
            add(
                "選択したタスクを一つ下へ".into(),
                "move down reorder",
                Command::Move(1),
            );
            for layout in Layout::ALL {
                add(
                    format!("表示: {}", layout.label()),
                    "layout view",
                    Command::Layout(layout),
                );
            }
            for nav in self.navigation() {
                add(
                    format!("移動: {}", nav.label),
                    "navigate inbox backlog project cycle",
                    Command::Navigate(nav.key),
                );
            }
            add("今すぐ再取得".into(), "refresh reload", Command::Refresh);
            add("サイドバーを開閉".into(), "sidebar", Command::ToggleSidebar);
            add(
                "キーボード操作と検索のヘルプ".into(),
                "help shortcuts",
                Command::Help,
            );
            for task in self.all_tasks() {
                add(
                    format!("{} · {}", task.r#ref.badge(), task.title),
                    "task open",
                    Command::Open(task.r#ref.clone()),
                );
            }
        }
        let (commands, actions): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
        let id = CommandPalette::new(commands)
            .placeholder("タスク・プロパティ・操作を検索…")
            .open(&mut self.tree);
        self.planning.active_palette = Some((id, actions));
    }

    fn run_command(&mut self, command: Command, text: &mut TextStore) {
        match command {
            Command::New => self.open_form(Form::new_task(), text),
            Command::Search => self.tree.set_focus(Some(self.text)),
            Command::Open(task) => {
                self.filter = super::Filter::default();
                self.planning.section = "all".into();
                self.sync_filter_controls();
                self.mark_preferences_dirty();
                self.persist_preferences();
                self.rebuild(text);
                self.select_ref(&task);
                self.open_detail(task, text);
            }
            Command::Properties(task) => self.open_properties(task, text),
            Command::Run(task) => {
                if let Some(task) = self.task(&task).cloned() {
                    self.run_form(&task, text);
                }
            }
            Command::Apply(tasks, change) => self.apply_planning(&tasks, &[change], text),
            Command::Undo => {
                self.own_notice = match self.planning.workspace.undo(self.view.now) {
                    Ok(true) => Some("変更を取り消しました".into()),
                    Ok(false) => Some("取り消せる変更がありません".into()),
                    Err(error) => Some(error),
                };
                self.rebuild(text);
            }
            Command::SaveView => self.open_save_view(text),
            Command::RemoveView(id) => match self.planning.workspace.remove_view(&id) {
                Ok(_) => self.navigate("all", text),
                Err(error) => {
                    self.own_notice = Some(error);
                    self.show_notice();
                }
            },
            Command::Sort(sort) => {
                self.planning.sort = sort;
                self.mark_preferences_dirty();
                self.persist_preferences();
                self.rebuild(text);
            }
            Command::Navigate(section) => self.navigate(&section, text),
            Command::Layout(layout) => self.set_layout(layout, text),
            Command::SelectAll => {
                self.planning.checked = self.cards.iter().map(|(_, task)| task.clone()).collect();
                self.rebuild(text);
            }
            Command::ClearSelection => {
                self.planning.checked.clear();
                self.rebuild(text);
            }
            Command::Move(by) => self.move_task(by, text),
            Command::Refresh => {
                self.act(Intent::Refresh);
                if let Err(error) = self.planning.workspace.reload() {
                    self.own_notice = Some(error);
                }
                self.rebuild(text);
            }
            Command::ToggleSidebar => {
                self.toggle_sidebar();
            }
            Command::Help => {
                self.own_notice = Some("⌘/Ctrl K: 操作検索 · C/N: 新規 · J/K: 移動 · S: 状態 · P: 優先度 · E: 属性 · Space: 複数選択 · X: 実行 · R: 再取得 · ⌘/Ctrl Z: 取り消し · 検索 state: priority: label: project: agent: -語句".into());
                self.show_notice();
            }
        }
    }

    fn move_task(&mut self, by: isize, text: &mut TextStore) {
        if self.layout != Layout::List {
            self.own_notice = Some("並び替えはリスト表示で操作できます".into());
            self.show_notice();
            return;
        }
        let Some(selected) = self.selected.clone() else {
            return;
        };
        let mut order: Vec<_> = self
            .planning_cards(false)
            .into_iter()
            .map(|c| c.task.r#ref.clone())
            .collect();
        let Some(at) = order.iter().position(|r| *r == selected) else {
            return;
        };
        let Some(next) = at.checked_add_signed(by).filter(|i| *i < order.len()) else {
            return;
        };
        if self.planning.workspace.meta(&order[at]).pinned
            != self.planning.workspace.meta(&order[next]).pinned
        {
            self.own_notice =
                Some("ピン留めしたタスクは先頭に表示されます。ピンを外すと下へ移動できます".into());
            self.show_notice();
            return;
        }
        order.swap(at, next);
        // Reordering is one transaction and one undo entry.
        match self.planning.workspace.reorder(&order, self.view.now) {
            Ok(_) => {
                self.planning.sort = Sort::Manual;
                self.mark_preferences_dirty();
                self.persist_preferences();
            }
            Err(error) => self.own_notice = Some(error),
        }
        self.rebuild(text);
    }

    pub(super) fn record_source_change(&mut self, intent: &Intent) {
        if let Intent::SetState { id, state } = intent {
            let task = TaskRef::Local(id.clone());
            let workflow = self.planning.workspace.meta(&task).workflow;
            if workflow.is_some() {
                self.planning
                    .pending_states
                    .insert(task, (*state, workflow));
            }
        }
    }

    /// A legacy local close/reopen clears the manual override only after
    /// the source confirms it. A failed file write leaves planning intact.
    pub(super) fn sync_source_changes(&mut self) {
        let done: Vec<_> = self
            .planning
            .pending_states
            .iter()
            .filter_map(|(r, (state, before))| {
                self.task(r)
                    .filter(|task| task.state == *state)
                    .map(|_| (r.clone(), *before))
            })
            .collect();
        for (task, before) in done {
            self.planning.pending_states.remove(&task);
            if self.planning.workspace.meta(&task).workflow == before {
                if let Err(error) =
                    self.planning
                        .workspace
                        .apply(&[task], &[Change::Workflow(None)], self.view.now)
                {
                    self.own_notice = Some(error);
                }
            }
        }
    }

    pub(super) fn blocked_reason(&self, task: &TaskRef) -> Option<String> {
        let meta = self.planning.workspace.meta(task);
        if meta.workflow.is_some_and(Workflow::is_finished) {
            return Some(
                "完了・キャンセルしたタスクです。作業状態を変更してから実行してください".into(),
            );
        }
        let blocked: Vec<_> = meta
            .blocked_by
            .iter()
            .filter_map(|key| {
                match self
                    .all_tasks()
                    .into_iter()
                    .find(|t| workspace::task_key(&t.r#ref) == *key)
                {
                    Some(t)
                        if workspace::effective_workflow(
                            t,
                            self.last_run(&t.r#ref),
                            &self.planning.workspace.meta(&t.r#ref),
                        )
                        .is_finished() =>
                    {
                        None
                    }
                    Some(t) => Some(t.title.clone()),
                    None => Some("一覧にない依存タスク".into()),
                }
            })
            .collect();
        (!blocked.is_empty())
            .then(|| format!("先に完了するタスクがあります: {}", blocked.join("、")))
    }
}

fn source_word(source: Source) -> &'static str {
    match source {
        Source::Local => "local",
        Source::GitHub => "github",
        Source::Code => "code",
    }
}

fn source_from_word(word: &str) -> Option<Source> {
    match word {
        "local" => Some(Source::Local),
        "github" => Some(Source::GitHub),
        "code" => Some(Source::Code),
        _ => None,
    }
}
