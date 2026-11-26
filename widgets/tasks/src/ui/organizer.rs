//! Compact task rows, workspace navigation and the task-property editor.
//!
//! Builders return every action with the widget that owns it. The screen
//! performs those actions through the workspace store, so a failed save
//! never leaves a successful-looking row on screen.

use std::collections::BTreeSet;

use noa_gpui_core::style::{
    FlexDirection, Rect as LayoutRect, Size as LayoutSize, auto, length, percent,
};
use noa_gpui_core::widgets::{
    Button, ButtonClicked, CheckState, Checkbox, Container, ControlSize, Label, PopUpButton,
    Pressable, ScrollView, SelectionChanged, Tag, TextArea, TextField, Tone,
};
use noa_gpui_core::{Message, Role, Style, UiTree, WidgetId};

use tasks::model::types::{
    AgentState, Liveness, Millis, ResultFact, RunView, State, Task, TaskRef,
};
use tasks::workspace::{Change, Priority, TaskMeta, Workflow, effective_workflow};

use super::columns::age;
use super::style::{self, DETAIL_WIDTH, gap};

/// A workspace navigation entry. Keys belong to the screen; the renderer
/// never guesses whether a project or saved view is named by one.
#[derive(Debug, Clone)]
pub(super) struct NavItem {
    pub key: String,
    pub label: String,
    pub count: usize,
    pub group: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Action {
    Navigate(String),
    ToggleSelection(TaskRef),
    Open(TaskRef),
    Edit(TaskRef),
    Apply {
        tasks: Vec<TaskRef>,
        changes: Vec<Change>,
    },
    NewTask,
    SaveView,
    Cancel,
}

/// Button actions and inline property choices. The menus' index is
/// resolved only against their original choices, never today's task list.
#[derive(Debug, Default)]
pub(super) struct Controls {
    pub buttons: Vec<(WidgetId, Action)>,
    workflows: Vec<(WidgetId, TaskRef)>,
    priorities: Vec<(WidgetId, TaskRef)>,
}

impl Controls {
    pub fn action(&self, _tree: &UiTree, message: &Message) -> Option<Action> {
        if message.downcast_ref::<ButtonClicked>().is_some() {
            return self
                .buttons
                .iter()
                .find(|(id, _)| *id == message.source)
                .map(|(_, action)| action.clone());
        }
        let SelectionChanged(index) = message.downcast_ref::<SelectionChanged>()?;
        if let Some((_, task)) = self.workflows.iter().find(|(id, _)| *id == message.source) {
            let workflow = if *index == 0 {
                None
            } else {
                Some(*Workflow::ALL.get(*index - 1)?)
            };
            return Some(Action::Apply {
                tasks: vec![task.clone()],
                changes: vec![Change::Workflow(workflow)],
            });
        }
        let (_, task) = self
            .priorities
            .iter()
            .find(|(id, _)| *id == message.source)?;
        Some(Action::Apply {
            tasks: vec![task.clone()],
            changes: vec![Change::Priority(*Priority::ALL.get(*index)?)],
        })
    }
}

fn heading(tree: &mut UiTree, host: WidgetId, text: &str) {
    tree.insert(
        Some(host),
        Label::new(text)
            .font_size(11.0)
            .weight(700)
            .color(Role::Muted),
    );
}

fn muted(tree: &mut UiTree, host: WidgetId, text: impl Into<String>) {
    tree.insert(
        Some(host),
        Label::new(text.into())
            .font_size(11.0)
            .color(Role::Muted)
            .wrap(true),
    );
}

/// A quiet sidebar: the active destination has a coloured rail, counts
/// stay beside their labels, and a project name never doubles as its key.
pub(super) fn sidebar(
    tree: &mut UiTree,
    host: WidgetId,
    repo_title: &str,
    entries: &[NavItem],
    active: &str,
) -> Controls {
    let scroll = tree.insert(
        Some(host),
        ScrollView::new(Style {
            flex_direction: FlexDirection::Column,
            padding: LayoutRect::length(10.0),
            gap: gap(6.0),
            ..style::fill()
        }),
    );
    tree.insert(
        Some(scroll),
        Label::new(repo_title)
            .font_size(14.0)
            .weight(700)
            .max_lines(2)
            .wrap(true),
    );
    muted(tree, scroll, "WORKSPACE");
    let mut controls = Controls::default();
    let create = tree.insert(
        Some(scroll),
        Button::new("＋ 新規タスク  C").control_size(ControlSize::Small),
    );
    controls.buttons.push((create, Action::NewTask));
    let mut group = String::new();
    for entry in entries {
        if entry.group != group {
            group = entry.group.clone();
            if !group.is_empty() {
                tree.insert(
                    Some(scroll),
                    Container::new(Style {
                        size: LayoutSize {
                            width: length(1.0),
                            height: length(8.0),
                        },
                        ..Default::default()
                    }),
                );
                heading(tree, scroll, &group);
            }
        }
        let chosen = entry.key == active;
        let row = tree.insert(
            Some(scroll),
            Pressable::new(Style {
                padding: LayoutRect::length(7.0),
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                ..style::row(6.0)
            })
            .label(format!("{} {} 件", entry.label, entry.count))
            .selected(chosen)
            .radius(5.0),
        );
        tree.insert(
            Some(row),
            Label::new(&entry.label)
                .font_size(12.0)
                .weight(if chosen { 700 } else { 400 })
                .max_lines(1),
        );
        style::spacer(tree, row);
        tree.insert(
            Some(row),
            Label::new(entry.count.to_string())
                .font_size(11.0)
                .color(Role::Muted),
        );
        controls
            .buttons
            .push((row, Action::Navigate(entry.key.clone())));
    }
    let save = tree.insert(
        Some(scroll),
        Button::new("現在のビューを保存").control_size(ControlSize::Small),
    );
    controls.buttons.push((save, Action::SaveView));
    controls
}

pub(super) struct Row<'a> {
    pub task: &'a Task,
    pub last: Option<&'a RunView>,
    pub meta: TaskMeta,
}

pub(super) struct ListBuilt {
    pub scroll: WidgetId,
    pub rows: Vec<(WidgetId, TaskRef)>,
    pub controls: Controls,
}

fn workflow_tone(workflow: Workflow) -> Tone {
    match workflow {
        Workflow::Backlog | Workflow::Todo | Workflow::Canceled => Tone::Neutral,
        Workflow::InProgress => Tone::Accent,
        Workflow::InReview => Tone::Warning,
        Workflow::Done => Tone::Success,
    }
}

fn task_id(task: &Task) -> String {
    match &task.r#ref {
        TaskRef::Local(id) => id.clone(),
        TaskRef::GitHub { number, .. } => format!("#{number}"),
        TaskRef::Code { path, .. } => path.rsplit('/').next().unwrap_or(path).to_owned(),
    }
}

/// Kept separate from workflow: an agent waiting for a person does not
/// become a different planning status, and a finished process is not Done.
pub(super) fn run_status(run: Option<&RunView>) -> Option<(&'static str, Tone)> {
    let run = run?;
    Some(match &run.liveness {
        Liveness::Starting => ("起動中", Tone::Accent),
        Liveness::Alive {
            agent: Some(AgentState::Permission),
        } => ("許可待ち", Tone::Warning),
        Liveness::Alive {
            agent: Some(AgentState::Input),
        } => ("入力待ち", Tone::Warning),
        Liveness::Alive {
            agent: Some(AgentState::Error),
        } => ("実行エラー", Tone::Warning),
        Liveness::Alive {
            agent: Some(AgentState::Finished),
        } => ("実行終了", Tone::Neutral),
        Liveness::Alive { .. } => ("実行中", Tone::Accent),
        Liveness::Gone if matches!(run.result, ResultFact::Missing) => {
            ("報告なしで終了", Tone::Warning)
        }
        Liveness::Gone => ("報告あり", Tone::Neutral),
    })
}

fn workflow_choice(meta: &TaskMeta) -> usize {
    meta.workflow
        .and_then(|w| Workflow::ALL.iter().position(|v| *v == w))
        .map_or(0, |i| i + 1)
}

fn priority_choice(meta: &TaskMeta) -> usize {
    Priority::ALL
        .iter()
        .position(|p| *p == meta.priority)
        .unwrap_or(0)
}

fn workflow_menu(
    tree: &mut UiTree,
    host: WidgetId,
    task: &Task,
    last: Option<&RunView>,
    meta: &TaskMeta,
) -> WidgetId {
    let automatic = format!(
        "自動 · {}",
        effective_workflow(task, last, &TaskMeta::default()).label()
    );
    tree.insert(
        Some(host),
        PopUpButton::new(
            std::iter::once(automatic).chain(Workflow::ALL.iter().map(|w| w.label().to_owned())),
        )
        .selected_index(workflow_choice(meta))
        .control_size(ControlSize::Small),
    )
}

fn priority_menu(tree: &mut UiTree, host: WidgetId, meta: &TaskMeta) -> WidgetId {
    tree.insert(
        Some(host),
        PopUpButton::new(Priority::ALL.iter().map(|p| p.label()))
            .selected_index(priority_choice(meta))
            .control_size(ControlSize::Small),
    )
}

/// Dense rows with sibling controls: clicking a property never also
/// opens the issue, which would steal focus from the property menu.
pub(super) fn list(
    tree: &mut UiTree,
    host: WidgetId,
    cards: &[Row<'_>],
    selected: Option<&TaskRef>,
    checked: &BTreeSet<TaskRef>,
    now: Millis,
) -> ListBuilt {
    let scroll = tree.insert(
        Some(host),
        ScrollView::new(Style {
            flex_direction: FlexDirection::Column,
            gap: gap(1.0),
            ..style::fill()
        }),
    );
    let mut controls = Controls::default();
    let mut rows = Vec::new();
    if cards.is_empty() {
        let empty = tree.insert(
            Some(scroll),
            Container::new(Style {
                padding: LayoutRect::length(28.0),
                gap: gap(8.0),
                ..style::card_style()
            }),
        );
        tree.insert(
            Some(empty),
            Label::new("このビューにタスクはありません")
                .font_size(15.0)
                .weight(700),
        );
        muted(
            tree,
            empty,
            "検索条件を変えるか、新しいタスクを作成してください。",
        );
        let new = tree.insert(Some(empty), Button::new("タスクを作成"));
        controls.buttons.push((new, Action::NewTask));
    }
    for card in cards {
        let task = card.task;
        let is_selected = selected == Some(&task.r#ref);
        let frame = tree.insert(
            Some(scroll),
            Container::new(Style {
                flex_direction: FlexDirection::Column,
                flex_shrink: 0.0,
                size: LayoutSize {
                    width: percent(1.0),
                    height: auto(),
                },
                padding: LayoutRect {
                    left: length(8.0),
                    right: length(8.0),
                    top: length(5.0),
                    bottom: length(5.0),
                },
                gap: gap(4.0),
                ..Default::default()
            })
            .background(Role::Surface)
            .border(if is_selected { 1.0 } else { 0.0 }, Role::Accent)
            .radius(4.0),
        );
        let row = tree.insert(Some(frame), Container::new(style::row(6.0)));
        let toggle = tree.insert(
            Some(row),
            Button::new(if checked.contains(&task.r#ref) {
                "✓"
            } else {
                "□"
            })
            .control_size(ControlSize::Small)
            .selected(checked.contains(&task.r#ref))
            .tooltip(format!("{} の複数選択を切り替え", task.title)),
        );
        tree.set_accessible_label(toggle, format!("{} の複数選択を切り替え", task.title));
        controls
            .buttons
            .push((toggle, Action::ToggleSelection(task.r#ref.clone())));
        let open = tree.insert(
            Some(row),
            Pressable::new(Style {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: length(0.0),
                min_size: LayoutSize {
                    width: length(0.0),
                    height: auto(),
                },
                padding: LayoutRect {
                    left: length(2.0),
                    right: length(2.0),
                    top: length(5.0),
                    bottom: length(5.0),
                },
                ..style::row(8.0)
            })
            .label(task.title.clone())
            .selected(is_selected),
        );
        tree.insert(
            Some(open),
            Label::new(task_id(task))
                .font_size(10.0)
                .color(Role::Muted)
                .max_lines(1),
        );
        let title_host = tree.insert(
            Some(open),
            Container::new(Style {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: length(0.0),
                min_size: LayoutSize {
                    width: length(0.0),
                    height: auto(),
                },
                ..Default::default()
            }),
        );
        tree.insert(
            Some(title_host),
            Label::new(&task.title)
                .font_size(13.0)
                .weight(if is_selected { 600 } else { 400 })
                .max_lines(1),
        );
        controls
            .buttons
            .push((open, Action::Open(task.r#ref.clone())));
        rows.push((open, task.r#ref.clone()));
        let pin = tree.insert(
            Some(row),
            Button::new(if card.meta.pinned { "★" } else { "☆" })
                .control_size(ControlSize::Small)
                .selected(card.meta.pinned)
                .tooltip(if card.meta.pinned {
                    "ピン留めを外す"
                } else {
                    "ピン留めする"
                }),
        );
        tree.set_accessible_label(
            pin,
            format!(
                "{} を{}",
                task.title,
                if card.meta.pinned {
                    "ピン留めから外す"
                } else {
                    "ピン留めする"
                }
            ),
        );
        controls.buttons.push((
            pin,
            Action::Apply {
                tasks: vec![task.r#ref.clone()],
                changes: vec![Change::Pinned(!card.meta.pinned)],
            },
        ));

        // This second line wraps, keeping actions reachable in a narrow
        // pane without turning every task into a tall fixed-width card.
        let props = tree.insert(Some(frame), Container::wrapping_row(6.0, 4.0));
        let workflow = workflow_menu(tree, props, task, card.last, &card.meta);
        controls.workflows.push((workflow, task.r#ref.clone()));
        let priority = priority_menu(tree, props, &card.meta);
        controls.priorities.push((priority, task.r#ref.clone()));
        if let TaskRef::GitHub { .. } = &task.r#ref {
            tree.insert(
                Some(props),
                Tag::new(if task.state == State::Closed {
                    "GitHub: closed"
                } else {
                    "GitHub: open"
                })
                .tone(Tone::Neutral),
            );
        }
        for label in task.labels.iter().take(2) {
            tree.insert(Some(props), Tag::new(label.clone()).tone(Tone::Neutral));
        }
        if !card.meta.project.is_empty() {
            tree.insert(
                Some(props),
                Tag::new(card.meta.project.clone()).tone(Tone::Neutral),
            );
        }
        if !card.meta.assignee.is_empty() {
            muted(tree, props, &card.meta.assignee);
        }
        if let Some((status, tone)) = run_status(card.last) {
            let agent = card.last.map(|r| r.run.agent.command()).unwrap_or("");
            tree.insert(
                Some(props),
                Tag::new(format!("{agent} · {status}")).tone(tone),
            );
        }
        if !card.meta.blocked_by.is_empty() {
            tree.insert(
                Some(props),
                Tag::new(format!("依存 {}", card.meta.blocked_by.len())).tone(Tone::Warning),
            );
        }
        if !card.meta.due.is_empty() {
            muted(tree, props, format!("期限 {}", card.meta.due));
        }
        if let Some(problem) = &task.problem {
            tree.insert(Some(props), Tag::new(problem.clone()).tone(Tone::Warning));
        }
        muted(tree, props, age(now, task.updated));
        let edit = tree.insert(
            Some(props),
            Button::new("詳細設定").control_size(ControlSize::Small),
        );
        controls
            .buttons
            .push((edit, Action::Edit(task.r#ref.clone())));
    }
    ListBuilt {
        scroll,
        rows,
        controls,
    }
}

/// Compact properties above the existing task detail. It does not own a
/// scroll view, so the screen can place it beside the source and results.
pub(super) fn property_summary(
    tree: &mut UiTree,
    host: WidgetId,
    task: &Task,
    meta: &TaskMeta,
    last: Option<&RunView>,
) -> Controls {
    let mut controls = Controls::default();
    let row = tree.insert(Some(host), Container::wrapping_row(6.0, 4.0));
    let workflow = effective_workflow(task, last, meta);
    tree.insert(
        Some(row),
        Tag::new(workflow.label()).tone(workflow_tone(workflow)),
    );
    tree.insert(
        Some(row),
        Tag::new(meta.priority.label()).tone(if meta.priority == Priority::Urgent {
            Tone::Warning
        } else {
            Tone::Neutral
        }),
    );
    if !meta.project.is_empty() {
        tree.insert(
            Some(row),
            Tag::new(meta.project.clone()).tone(Tone::Neutral),
        );
    }
    if !meta.cycle.is_empty() {
        tree.insert(Some(row), Tag::new(meta.cycle.clone()).tone(Tone::Neutral));
    }
    if let Some((status, tone)) = run_status(last) {
        tree.insert(Some(row), Tag::new(status).tone(tone));
    }
    let edit = tree.insert(
        Some(row),
        Button::new("プロパティ・コメント").control_size(ControlSize::Small),
    );
    controls
        .buttons
        .push((edit, Action::Edit(task.r#ref.clone())));
    controls
}

#[derive(Debug)]
struct PropertyIds {
    workflow: WidgetId,
    priority: WidgetId,
    assignee: WidgetId,
    project: WidgetId,
    cycle: WidgetId,
    due: WidgetId,
    estimate: WidgetId,
    parent: WidgetId,
    blockers: Vec<(WidgetId, String)>,
    comment: WidgetId,
    save: WidgetId,
    cancel: WidgetId,
    error: WidgetId,
}

/// A property form is inserted once and kept while live task updates
/// arrive. It submits only changed properties, preserving later edits to
/// unrelated fields in another window.
pub(super) struct PropertyForm {
    task: TaskRef,
    title: String,
    original: TaskMeta,
    choices: Vec<(String, String)>,
    ids: Option<PropertyIds>,
}

impl PropertyForm {
    pub fn new(task: &Task, meta: TaskMeta, choices: Vec<(String, String)>) -> Self {
        let own = tasks::workspace::task_key(&task.r#ref);
        let mut choices: Vec<_> = choices.into_iter().filter(|(key, _)| key != &own).collect();
        for key in meta.parent.iter().chain(meta.blocked_by.iter()) {
            if !choices.iter().any(|(known, _)| known == key) {
                choices.push((
                    key.clone(),
                    format!("一覧外の関連タスク {}", choices.len() + 1),
                ));
            }
        }
        Self {
            task: task.r#ref.clone(),
            title: task.title.clone(),
            original: meta,
            choices,
            ids: None,
        }
    }

    pub fn insert(&mut self, tree: &mut UiTree, host: WidgetId) {
        let column = tree.insert(
            Some(host),
            ScrollView::new(Style {
                flex_direction: FlexDirection::Column,
                gap: gap(8.0),
                ..style::fill()
            }),
        );
        tree.insert(
            Some(column),
            Label::new(&self.title)
                .font_size(15.0)
                .weight(700)
                .wrap(true),
        );
        muted(tree, column, "Noa の作業管理 · GitHub の状態は変更しません");
        heading(tree, column, "作業状態");
        let workflow = tree.insert(
            Some(column),
            PopUpButton::new(
                std::iter::once("実行状況から自動".to_owned())
                    .chain(Workflow::ALL.iter().map(|w| w.label().to_owned())),
            )
            .selected_index(workflow_choice(&self.original))
            .control_size(ControlSize::Small),
        );
        heading(tree, column, "優先度");
        let priority = priority_menu(tree, column, &self.original);
        let width = DETAIL_WIDTH - 40.0;
        let field = |tree: &mut UiTree, label: &str, placeholder: &str, value: &str| {
            heading(tree, column, label);
            let id = tree.insert(Some(column), TextField::new(placeholder, width));
            tree.update::<TextField, _>(id, |f, cx| f.set_text(cx, value));
            id
        };
        let assignee = field(tree, "担当者", "例: 自分", &self.original.assignee);
        let project = field(
            tree,
            "プロジェクト",
            "例: Tasks 改善",
            &self.original.project,
        );
        let cycle = field(tree, "サイクル", "例: 2026-W48", &self.original.cycle);
        let due = field(tree, "期限", "YYYY-MM-DD（任意）", &self.original.due);
        let estimate = field(
            tree,
            "見積もり",
            "ポイント（0 は未設定）",
            &self.original.estimate.to_string(),
        );
        heading(tree, column, "親タスク");
        let selected_parent = self
            .original
            .parent
            .as_ref()
            .and_then(|key| self.choices.iter().position(|(k, _)| k == key))
            .map_or(0, |i| i + 1);
        let parent = tree.insert(
            Some(column),
            PopUpButton::new(
                std::iter::once("なし".to_owned())
                    .chain(self.choices.iter().map(|(_, label)| label.clone())),
            )
            .selected_index(selected_parent)
            .control_size(ControlSize::Small),
        );
        if self.original.parent.is_some() && selected_parent == 0 {
            muted(
                tree,
                column,
                "親タスクは現在の一覧にありません。選択を変えない場合は関連を保持します。",
            );
        }
        heading(tree, column, "先に完了するタスク");
        let mut blockers = Vec::new();
        for (key, label) in &self.choices {
            let id = tree.insert(
                Some(column),
                Checkbox::new(label.clone())
                    .checked(self.original.blocked_by.contains(key))
                    .control_size(ControlSize::Small),
            );
            blockers.push((id, key.clone()));
        }
        if self.choices.is_empty() {
            muted(tree, column, "関連付けられるタスクがありません");
        }
        heading(tree, column, "コメントを追加");
        let comment = tree.insert(
            Some(column),
            TextArea::new("今回の判断、補足、引き継ぎ事項")
                .width(width)
                .min_lines(3),
        );
        let error = tree.insert(
            Some(column),
            Label::new("")
                .font_size(12.0)
                .wrap(true)
                .color(super::FAILURE),
        );
        let buttons = tree.insert(Some(column), Container::wrapping_row(8.0, 4.0));
        let save = tree.insert(Some(buttons), Button::new("保存").default_button(true));
        let cancel = tree.insert(Some(buttons), Button::new("キャンセル (Esc)"));
        activity(tree, column, &self.original);
        self.ids = Some(PropertyIds {
            workflow,
            priority,
            assignee,
            project,
            cycle,
            due,
            estimate,
            parent,
            blockers,
            comment,
            save,
            cancel,
            error,
        });
        let hidden: Vec<_> = tree
            .ancestors(host)
            .filter(|id| tree.is_hidden(*id))
            .collect();
        for id in hidden {
            tree.set_hidden(id, false);
        }
        tree.set_focus(Some(workflow));
    }

    pub fn handle(&mut self, message: &Message, tree: &mut UiTree) -> Option<Action> {
        let ids = self.ids.as_ref()?;
        if message.downcast_ref::<ButtonClicked>().is_none() {
            return None;
        }
        if message.source == ids.cancel {
            return Some(Action::Cancel);
        }
        if message.source != ids.save {
            return None;
        }
        match self.changes(tree) {
            Ok(changes) => Some(Action::Apply {
                tasks: vec![self.task.clone()],
                changes,
            }),
            Err(why) => {
                tree.update::<Label, _>(ids.error, |l, cx| l.set_text(cx, why));
                None
            }
        }
    }

    /// A live refresh may have updated the same property while this
    /// draft stayed open. Unrelated changes and appended comments merge;
    /// a changed field must never silently replace another window's edit.
    pub fn conflicts(&self, current: &TaskMeta, changes: &[Change]) -> bool {
        fn changed<T: PartialEq>(before: &T, current: &T, proposed: &T) -> bool {
            before != current && current != proposed
        }
        changes.iter().any(|change| match change {
            Change::Workflow(value) => changed(&self.original.workflow, &current.workflow, value),
            Change::Priority(value) => changed(&self.original.priority, &current.priority, value),
            Change::Assignee(value) => changed(&self.original.assignee, &current.assignee, value),
            Change::Project(value) => changed(&self.original.project, &current.project, value),
            Change::Cycle(value) => changed(&self.original.cycle, &current.cycle, value),
            Change::Due(value) => changed(&self.original.due, &current.due, value),
            Change::Estimate(value) => changed(&self.original.estimate, &current.estimate, value),
            Change::Pinned(value) => changed(&self.original.pinned, &current.pinned, value),
            Change::Parent(value) => changed(&self.original.parent, &current.parent, value),
            Change::BlockedBy(value) => {
                changed(&self.original.blocked_by, &current.blocked_by, value)
            }
            Change::Rank(value) => changed(&self.original.rank, &current.rank, value),
            Change::Comment(_) | Change::Seen(_) => false,
        })
    }

    fn changes(&self, tree: &UiTree) -> Result<Vec<Change>, String> {
        let ids = self.ids.as_ref().ok_or("入力欄がありません")?;
        let mut changes = Vec::new();
        let selected = |id| {
            tree.get::<PopUpButton>(id)
                .map(PopUpButton::selected)
                .unwrap_or(0)
        };
        let text = |id| {
            tree.get::<TextField>(id)
                .map(|f| f.text().trim().to_owned())
                .unwrap_or_default()
        };
        let at = selected(ids.workflow);
        let workflow = if at == 0 {
            None
        } else {
            Workflow::ALL.get(at - 1).copied()
        };
        if workflow != self.original.workflow {
            changes.push(Change::Workflow(workflow));
        }
        let priority = Priority::ALL
            .get(selected(ids.priority))
            .copied()
            .unwrap_or(Priority::None);
        if priority != self.original.priority {
            changes.push(Change::Priority(priority));
        }
        let assignee = text(ids.assignee);
        if assignee != self.original.assignee {
            changes.push(Change::Assignee(assignee));
        }
        let project = text(ids.project);
        if project != self.original.project {
            changes.push(Change::Project(project));
        }
        let cycle = text(ids.cycle);
        if cycle != self.original.cycle {
            changes.push(Change::Cycle(cycle));
        }
        let due = text(ids.due);
        if due != self.original.due {
            changes.push(Change::Due(due));
        }
        let estimate = text(ids.estimate);
        let estimate: u32 = if estimate.is_empty() {
            0
        } else {
            estimate
                .parse()
                .map_err(|_| "見積もりは 0 以上の整数で入力してください")?
        };
        if estimate != self.original.estimate {
            changes.push(Change::Estimate(estimate));
        }
        let selected_parent = selected(ids.parent);
        let parent = selected_parent
            .checked_sub(1)
            .and_then(|i| self.choices.get(i))
            .map(|(key, _)| key.clone());
        // A task outside the currently fetched list stays associated until
        // the person chooses a different visible parent.
        let missing_parent = self
            .original
            .parent
            .as_ref()
            .is_some_and(|key| !self.choices.iter().any(|(k, _)| k == key));
        if !(missing_parent && selected_parent == 0) && parent != self.original.parent {
            changes.push(Change::Parent(parent));
        }
        let mut blockers: Vec<String> = self
            .original
            .blocked_by
            .iter()
            .filter(|key| !self.choices.iter().any(|(k, _)| k == *key))
            .cloned()
            .collect();
        blockers.extend(
            ids.blockers
                .iter()
                .filter(|(id, _)| {
                    tree.get::<Checkbox>(*id)
                        .is_some_and(|c| c.state() == CheckState::On)
                })
                .map(|(_, key)| key.clone()),
        );
        blockers.sort();
        blockers.dedup();
        let mut previous = self.original.blocked_by.clone();
        previous.sort();
        previous.dedup();
        if blockers != previous {
            changes.push(Change::BlockedBy(blockers));
        }
        let comment = tree
            .get::<TextArea>(ids.comment)
            .map(|a| a.text().trim().to_owned())
            .unwrap_or_default();
        if !comment.is_empty() {
            changes.push(Change::Comment(comment));
        }
        Ok(changes)
    }
}

/// Local conversation and property history. Values are shown as text,
/// so issue bodies and comments never acquire UI or command semantics.
pub(super) fn activity(tree: &mut UiTree, host: WidgetId, meta: &TaskMeta) {
    let date = |at| {
        let day = super::dates::day_of(at);
        format!("{}-{:02}-{:02}", day.year, day.month, day.day)
    };
    if !meta.comments.is_empty() {
        heading(tree, host, "コメント");
        if meta.comments.len() > 20 {
            muted(
                tree,
                host,
                format!("{} 件中、最新 20 件を表示", meta.comments.len()),
            );
        }
        for comment in meta.comments.iter().rev().take(20) {
            muted(tree, host, date(comment.at));
            tree.insert(
                Some(host),
                Label::new(&comment.body).font_size(12.0).wrap(true),
            );
        }
    }
    if !meta.activity.is_empty() {
        heading(tree, host, "アクティビティ");
        if meta.activity.len() > 20 {
            muted(
                tree,
                host,
                format!("{} 件中、最新 20 件を表示", meta.activity.len()),
            );
        }
        for item in meta.activity.iter().rev().take(20) {
            muted(tree, host, format!("{} · {}", date(item.at), item.text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tasks::model::types::{AgentKind, Run, RunId};

    fn local(id: &str, title: &str) -> Task {
        Task {
            r#ref: TaskRef::Local(id.into()),
            title: title.into(),
            body: None,
            state: State::Open,
            labels: Vec::new(),
            updated: 1_000,
            url: None,
            closed: None,
            problem: None,
        }
    }

    fn run_view(task: &TaskRef, liveness: Liveness, result: ResultFact) -> RunView {
        RunView {
            run: Run {
                run: RunId::new("run-ui"),
                task: task.clone(),
                agent: AgentKind::Codex,
                workdir: "/repo".into(),
                branch: None,
                pane: None,
                board: None,
                started: 1_000,
                closed: None,
            },
            liveness,
            result,
            report: None,
        }
    }

    #[test]
    fn waiting_and_failed_runs_keep_their_own_labels() {
        let task = local("t-ui", "task");
        for (agent, expected) in [
            (AgentState::Permission, "許可待ち"),
            (AgentState::Input, "入力待ち"),
            (AgentState::Error, "実行エラー"),
            (AgentState::Finished, "実行終了"),
        ] {
            let run = run_view(
                &task.r#ref,
                Liveness::Alive { agent: Some(agent) },
                ResultFact::Missing,
            );
            assert_eq!(
                run_status(Some(&run)).map(|(label, _)| label),
                Some(expected)
            );
        }
        let mut run = run_view(&task.r#ref, Liveness::Gone, ResultFact::Missing);
        run.run.agent = AgentKind::Codex;
        assert_eq!(
            run_status(Some(&run)).map(|(label, _)| label),
            Some("報告なしで終了")
        );
    }

    #[test]
    fn unchanged_properties_make_no_edits_and_missing_links_survive() {
        let task = local("t-ui", "task");
        let other = tasks::workspace::task_key(&TaskRef::Local("t-outside".into()));
        let meta = TaskMeta {
            parent: Some(other.clone()),
            blocked_by: vec![other],
            project: "プロジェクト".into(),
            ..TaskMeta::default()
        };
        let mut form = PropertyForm::new(&task, meta, Vec::new());
        let mut tree = UiTree::new();
        let host = tree.insert(None, Container::new(style::fill()));
        form.insert(&mut tree, host);
        assert!(form.changes(&tree).unwrap().is_empty());
        let ids = form.ids.as_ref().unwrap();
        tree.update::<PopUpButton, _>(ids.parent, |p, cx| p.set_selected(cx, 0));
        assert_eq!(form.changes(&tree).unwrap(), vec![Change::Parent(None)]);
        tree.update::<TextField, _>(ids.estimate, |f, cx| f.set_text(cx, "two"));
        assert!(form.changes(&tree).unwrap_err().contains("整数"));
    }

    #[test]
    fn property_form_only_submits_changed_fields_and_comment() {
        let task = local("t-ui", "task");
        let mut form = PropertyForm::new(&task, TaskMeta::default(), Vec::new());
        let mut tree = UiTree::new();
        let host = tree.insert(None, Container::new(style::fill()));
        form.insert(&mut tree, host);
        let ids = form.ids.as_ref().unwrap();
        tree.update::<TextField, _>(ids.project, |f, cx| f.set_text(cx, "改善"));
        tree.update::<TextArea, _>(ids.comment, |f, cx| f.set_text(cx, "まず検索を直す"));
        assert_eq!(
            form.changes(&tree).unwrap(),
            vec![
                Change::Project("改善".into()),
                Change::Comment("まず検索を直す".into())
            ]
        );
    }

    #[test]
    fn a_draft_refuses_overlapping_edits_but_merges_unrelated_properties() {
        let task = local("t-ui", "task");
        let form = PropertyForm::new(&task, TaskMeta::default(), Vec::new());
        let current = TaskMeta {
            project: "別の画面の編集".into(),
            priority: Priority::High,
            ..TaskMeta::default()
        };
        assert!(form.conflicts(&current, &[Change::Project("編集中の値".into())]));
        assert!(!form.conflicts(
            &current,
            &[
                Change::Assignee("担当者".into()),
                Change::Comment("追記".into())
            ]
        ));
        assert!(!form.conflicts(&current, &[Change::Project(current.project.clone())]));
    }
}
