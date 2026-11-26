//! Personal planning state for Tasks. Source tasks and agent runs stay in
//! their original stores; this file records the person's workflow, planning
//! fields, comments and views. A source closing remains visible independently
//! of this workflow. Writes are atomic, locked and compare the bytes last read
//! so another window's edits (including a hand edit) are never overwritten.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::{self, File};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fsx;
use crate::model::column::column;
use crate::model::types::{
    AgentState, Column, Liveness, Millis, ResultFact, RunView, Source, Task, TaskRef,
};

pub const FILE_NAME: &str = "workspace.json";
const FORMAT_VERSION: u32 = 1;
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const HISTORY_LIMIT: usize = 200;
const UNDO_LIMIT: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Workflow {
    Backlog,
    Todo,
    InProgress,
    InReview,
    Done,
    Canceled,
}

impl Workflow {
    pub const ALL: [Self; 6] = [
        Self::Backlog,
        Self::Todo,
        Self::InProgress,
        Self::InReview,
        Self::Done,
        Self::Canceled,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Backlog => "バックログ",
            Self::Todo => "未着手",
            Self::InProgress => "進行中",
            Self::InReview => "レビュー中",
            Self::Done => "完了",
            Self::Canceled => "キャンセル",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::InReview => "in_review",
            Self::Done => "done",
            Self::Canceled => "canceled",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        let word = word.to_lowercase().replace('-', "_");
        Self::ALL
            .into_iter()
            .find(|v| v.word() == word || v.label() == word)
    }

    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Canceled)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    #[default]
    None,
    Urgent,
    High,
    Normal,
    Low,
}

impl Priority {
    pub const ALL: [Self; 5] = [
        Self::None,
        Self::Urgent,
        Self::High,
        Self::Normal,
        Self::Low,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "優先度なし",
            Self::Urgent => "緊急",
            Self::High => "高",
            Self::Normal => "通常",
            Self::Low => "低",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Urgent => "urgent",
            Self::High => "high",
            Self::Normal => "normal",
            Self::Low => "low",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        let word = word.to_lowercase();
        Self::ALL
            .into_iter()
            .find(|v| v.word() == word || v.label() == word)
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::Urgent => 0,
            Self::High => 1,
            Self::Normal => 2,
            Self::Low => 3,
            Self::None => 4,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Comment {
    pub id: String,
    pub body: String,
    pub at: Millis,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Activity {
    pub at: Millis,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TaskMeta {
    /// None keeps the old fact-derived workflow until a person chooses one.
    pub workflow: Option<Workflow>,
    pub priority: Priority,
    pub assignee: String,
    pub project: String,
    pub cycle: String,
    /// A date without an implicit timezone, YYYY-MM-DD, or empty.
    pub due: String,
    pub estimate: u32,
    pub pinned: bool,
    /// Keys returned by task_key, never paths to read from the filesystem.
    pub parent: Option<String>,
    pub blocked_by: Vec<String>,
    pub comments: Vec<Comment>,
    pub activity: Vec<Activity>,
    pub rank: i64,
    pub seen: Millis,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sort {
    #[default]
    Activity,
    Priority,
    Title,
    Manual,
}

impl Sort {
    pub const ALL: [Self; 4] = [Self::Activity, Self::Priority, Self::Title, Self::Manual];

    pub fn label(self) -> &'static str {
        match self {
            Self::Activity => "更新順",
            Self::Priority => "優先度順",
            Self::Title => "タイトル順",
            Self::Manual => "手動順",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Activity => "activity",
            Self::Priority => "priority",
            Self::Title => "title",
            Self::Manual => "manual",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.word() == word)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub section: String,
    pub query: String,
    pub layout: String,
    pub sort: Sort,
    pub source: Option<String>,
    pub label: Option<String>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            section: "all".into(),
            query: String::new(),
            layout: "list".into(),
            sort: Sort::Activity,
            source: None,
            label: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SavedView {
    pub id: String,
    pub name: String,
    pub query: String,
    pub section: String,
    pub layout: String,
    pub sort: Sort,
    pub source: Option<String>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceData {
    pub version: u32,
    pub revision: u64,
    pub tasks: BTreeMap<String, TaskMeta>,
    pub preferences: Preferences,
    pub views: Vec<SavedView>,
}

impl Default for WorkspaceData {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            revision: 0,
            tasks: BTreeMap::new(),
            preferences: Preferences::default(),
            views: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Workflow(Option<Workflow>),
    Priority(Priority),
    Assignee(String),
    Project(String),
    Cycle(String),
    Due(String),
    Estimate(u32),
    Pinned(bool),
    Parent(Option<String>),
    BlockedBy(Vec<String>),
    Comment(String),
    Rank(i64),
    Seen(Millis),
}

#[derive(Debug, Clone)]
struct UndoTask {
    key: String,
    before: TaskMeta,
    after: TaskMeta,
}

/// One window's view of the planning file. The undo stack is session-local;
/// reload keeps it, and an undo refuses if one of its tasks changed meanwhile.
#[derive(Debug)]
pub struct Workspace {
    path: Option<PathBuf>,
    data: WorkspaceData,
    observed: Option<Vec<u8>>,
    undo: Vec<Vec<UndoTask>>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self::memory()
    }
}

impl Workspace {
    pub fn memory() -> Self {
        Self {
            path: None,
            data: WorkspaceData::default(),
            observed: None,
            undo: Vec::new(),
        }
    }

    /// A missing file is an empty workspace; invalid/future files are errors,
    /// never quietly replaced by an empty one.
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let (data, observed) = read(&path)?;
        Ok(Self {
            path: Some(path),
            data,
            observed,
            undo: Vec::new(),
        })
    }

    pub fn data(&self) -> &WorkspaceData {
        &self.data
    }

    pub fn meta(&self, task: &TaskRef) -> TaskMeta {
        self.data
            .tasks
            .get(&task_key(task))
            .cloned()
            .unwrap_or_default()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// All changes on every selected task form one atomic operation and one
    /// undo item. A failed validation/write changes neither memory nor disk.
    pub fn apply(
        &mut self,
        tasks: &[TaskRef],
        changes: &[Change],
        at: Millis,
    ) -> Result<bool, String> {
        let keys: BTreeSet<String> = tasks.iter().map(task_key).collect();
        if keys.is_empty() || changes.is_empty() {
            return Ok(false);
        }
        let mut next = self.data.clone();
        let mut undo = Vec::new();
        let mut changed = false;
        for key in keys {
            let before = next.tasks.get(&key).cloned().unwrap_or_default();
            let mut after = before.clone();
            for change in changes {
                apply_change(&mut after, change, at)?;
            }
            if before != after {
                changed = true;
                next.tasks.insert(key.clone(), after.clone());
                if !same_properties(&before, &after) {
                    undo.push(UndoTask { key, before, after });
                }
            }
        }
        if !changed {
            return Ok(false);
        }
        validate(&next)?;
        self.commit(next)?;
        if !undo.is_empty() {
            self.undo.push(undo);
        }
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        Ok(true)
    }

    pub fn undo(&mut self, at: Millis) -> Result<bool, String> {
        let Some(undo) = self.undo.last() else {
            return Ok(false);
        };
        let mut next = self.data.clone();
        for item in undo {
            let Some(current) = next.tasks.get(&item.key) else {
                return Err("対象タスクの記録が見つからないため取り消せません".into());
            };
            if !same_properties(current, &item.after) {
                return Err(
                    "対象タスクはその後に編集されています。上書きせず、取り消しを中止しました"
                        .into(),
                );
            }
            let mut before = item.before.clone();
            // Keep the audit trail of the edit and its undo, while reverting
            // the properties/comments the operation changed.
            before.activity = current.activity.clone();
            before.seen = current.seen;
            push_activity(&mut before, at, "前の操作を取り消しました".into());
            next.tasks.insert(item.key.clone(), before);
        }
        validate(&next)?;
        self.commit(next)?;
        self.undo.pop();
        // The previous operation's after-image can differ only by our audit
        // entries after undoing the newest one; make sequential Undo possible.
        if let Some(previous) = self.undo.last_mut() {
            for item in previous {
                if let Some(current) = self.data.tasks.get(&item.key) {
                    let mut candidate = item.after.clone();
                    candidate.activity = current.activity.clone();
                    if &candidate == current {
                        item.after = current.clone();
                    }
                }
            }
        }
        Ok(true)
    }

    pub fn set_preferences(&mut self, preferences: Preferences) -> Result<bool, String> {
        if self.data.preferences == preferences {
            return Ok(false);
        }
        let mut next = self.data.clone();
        next.preferences = preferences;
        self.commit(next)?;
        Ok(true)
    }

    /// Assigns ranks to the visible sequence in one operation. Hidden tasks
    /// keep their rank, and one Undo restores every moved row together.
    pub fn reorder(&mut self, tasks: &[TaskRef], at: Millis) -> Result<bool, String> {
        let mut next = self.data.clone();
        let mut seen = BTreeSet::new();
        let mut undo = Vec::new();
        for (index, task) in tasks.iter().enumerate() {
            let key = task_key(task);
            if !seen.insert(key.clone()) {
                return Err("同じタスクを表示順に重複して指定できません".into());
            }
            let before = next.tasks.get(&key).cloned().unwrap_or_default();
            let mut after = before.clone();
            let rank = i64::try_from(index).map_err(|_| "タスク件数が上限を超えました")?;
            apply_change(&mut after, &Change::Rank(rank), at)?;
            if before != after {
                next.tasks.insert(key.clone(), after.clone());
                undo.push(UndoTask { key, before, after });
            }
        }
        if undo.is_empty() {
            return Ok(false);
        }
        self.commit(next)?;
        self.undo.push(undo);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        Ok(true)
    }

    pub fn save_view(&mut self, view: SavedView) -> Result<bool, String> {
        if view.id.trim().is_empty() || view.name.trim().is_empty() {
            return Err("ビューの ID と名前を入力してください".into());
        }
        let mut next = self.data.clone();
        if let Some(existing) = next.views.iter_mut().find(|v| v.id == view.id) {
            if *existing == view {
                return Ok(false);
            }
            *existing = view;
        } else {
            next.views.push(view);
        }
        self.commit(next)?;
        Ok(true)
    }

    pub fn remove_view(&mut self, id: &str) -> Result<bool, String> {
        let mut next = self.data.clone();
        next.views.retain(|view| view.id != id);
        if next.views == self.data.views {
            return Ok(false);
        }
        self.commit(next)?;
        Ok(true)
    }

    pub fn reload(&mut self) -> Result<bool, String> {
        let Some(path) = &self.path else {
            return Ok(false);
        };
        let (data, observed) = read(path)?;
        if self.observed == observed {
            return Ok(false);
        }
        self.data = data;
        self.observed = observed;
        Ok(true)
    }

    fn commit(&mut self, mut next: WorkspaceData) -> Result<(), String> {
        validate(&next)?;
        next.revision = self
            .data
            .revision
            .checked_add(1)
            .ok_or("ワークスペースの更新番号が上限に達しました")?;
        let bytes = serde_json::to_vec_pretty(&next).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("ワークスペースが 16 MiB を超えるため保存できません".into());
        }
        if let Some(path) = &self.path {
            let _lock = lock(path).map_err(|e| format!("ワークスペースをロックできません: {e}"))?;
            let (_, current) = read(path)?;
            if current != self.observed {
                return Err("別のウィンドウでワークスペースが更新されています。再取得してから操作してください".into());
            }
            fsx::write_replace(base(path)?, path, &bytes)
                .map_err(|e| format!("ワークスペースを保存できません: {e}"))?;
            self.observed = Some(bytes);
        }
        self.data = next;
        Ok(())
    }
}

/// TaskRef's existing serialized format is also what TaskKey stores in the
/// cross-widget link log. Repository is part of a GitHub key; path and digest
/// are both part of a code marker key.
pub fn task_key(task: &TaskRef) -> String {
    serde_json::to_string(task).expect("TaskRef contains only strings and integers")
}

pub fn effective_workflow(task: &Task, last: Option<&RunView>, meta: &TaskMeta) -> Workflow {
    meta.workflow.unwrap_or_else(|| match column(task, last) {
        Column::Todo => Workflow::Todo,
        Column::Running => Workflow::InProgress,
        Column::Review => Workflow::InReview,
        Column::Done => Workflow::Done,
    })
}

pub fn needs_attention(last: Option<&RunView>) -> bool {
    last.is_some_and(|run| {
        matches!(
            run.liveness,
            Liveness::Alive {
                agent: Some(AgentState::Permission | AgentState::Input | AgentState::Error)
            }
        ) || (matches!(run.liveness, Liveness::Gone) && matches!(run.result, ResultFact::Missing))
    })
}

/// A searchable description, separate from the person's workflow.
pub fn agent_status(last: Option<&RunView>) -> &'static str {
    match last.map(|run| &run.liveness) {
        None => "未実行",
        Some(Liveness::Starting) => "起動中",
        Some(Liveness::Alive {
            agent: Some(AgentState::Permission),
        }) => "許可待ち",
        Some(Liveness::Alive {
            agent: Some(AgentState::Input),
        }) => "入力待ち",
        Some(Liveness::Alive {
            agent: Some(AgentState::Error),
        }) => "失敗",
        Some(Liveness::Alive {
            agent: Some(AgentState::Finished),
        }) => "実行終了",
        Some(Liveness::Alive { .. }) => "実行中",
        Some(Liveness::Gone) => "ペイン終了",
    }
}

type TaskSortItem<'a> = (&'a Task, Option<&'a RunView>, &'a TaskMeta);

pub fn compare_tasks(sort: Sort, a: TaskSortItem<'_>, b: TaskSortItem<'_>) -> Ordering {
    let updated = |(task, last, meta): TaskSortItem<'_>| {
        task.updated
            .max(last.map_or(0, |run| run.run.started))
            .max(meta.activity.last().map_or(0, |event| event.at))
    };
    b.2.pinned
        .cmp(&a.2.pinned)
        .then_with(|| match sort {
            Sort::Activity => updated(b).cmp(&updated(a)),
            Sort::Priority => {
                a.2.priority
                    .rank()
                    .cmp(&b.2.priority.rank())
                    .then_with(|| updated(b).cmp(&updated(a)))
            }
            Sort::Title => a.0.title.to_lowercase().cmp(&b.0.title.to_lowercase()),
            Sort::Manual => a.2.rank.cmp(&b.2.rank),
        })
        .then_with(|| a.0.r#ref.cmp(&b.0.r#ref))
}

/// AND terms, quoted phrases and -negation. Qualifiers are deliberately
/// small; an unknown qualifier is literal text, not an ignored condition.
pub fn matches_query(query: &str, task: &Task, last: Option<&RunView>, meta: &TaskMeta) -> bool {
    let contains = |haystack: &str, needle: &str| haystack.to_lowercase().contains(needle);
    let workflow = effective_workflow(task, last, meta);
    let id = match &task.r#ref {
        TaskRef::Local(id) => id.clone(),
        TaskRef::GitHub { repo, number } => format!("{repo}#{number}"),
        TaskRef::Code { path, key } => format!("{path} {key}"),
    };
    let source = match task.r#ref.source() {
        Source::Local => "local ローカル",
        Source::GitHub => "github issue",
        Source::Code => "code todo コード",
    };
    let plain = format!(
        "{} {} {} {} {} {} {} {} {} {} {} {} {}",
        task.title,
        id,
        task.body.as_deref().unwrap_or_default(),
        task.labels.join(" "),
        meta.assignee,
        meta.project,
        meta.cycle,
        meta.due,
        meta.comments
            .iter()
            .map(|c| c.body.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        workflow.label(),
        workflow.word(),
        last.and_then(|run| run.report.as_ref())
            .map_or("", |report| report.text.as_str()),
        agent_status(last),
    )
    .to_lowercase();
    tokens(query).into_iter().all(|raw| {
        let (negative, token) = raw
            .strip_prefix('-')
            .map_or((false, raw.as_str()), |t| (true, t));
        if token.is_empty() {
            return true;
        }
        let (field, value) = token.split_once(':').unwrap_or(("", token));
        let matched = match field {
            "state" | "status" => {
                contains(workflow.word(), value) || contains(workflow.label(), value)
            }
            "priority" => {
                contains(meta.priority.word(), value) || contains(meta.priority.label(), value)
            }
            "label" => task.labels.iter().any(|label| contains(label, value)),
            "agent" => last.is_some_and(|run| contains(run.run.agent.command(), value)),
            "project" => contains(&meta.project, value),
            "cycle" => contains(&meta.cycle, value),
            "assignee" => contains(&meta.assignee, value),
            "source" => contains(source, value),
            "due" => contains(&meta.due, value),
            "pinned" => boolean(value, meta.pinned),
            "blocked" => boolean(value, !meta.blocked_by.is_empty()),
            "attention" => boolean(value, needs_attention(last)),
            "is" => match value {
                "pinned" => meta.pinned,
                "blocked" => !meta.blocked_by.is_empty(),
                "attention" => needs_attention(last),
                "active" => matches!(
                    workflow,
                    Workflow::Todo | Workflow::InProgress | Workflow::InReview
                ),
                "done" => workflow.is_finished(),
                _ => false,
            },
            "" => plain.contains(value),
            _ => plain.contains(token),
        };
        matched != negative
    })
}

fn boolean(value: &str, actual: bool) -> bool {
    match value {
        "true" | "yes" | "1" => actual,
        "false" | "no" | "0" => !actual,
        _ => false,
    }
}

fn tokens(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for c in query.chars() {
        if c == '"' {
            quoted = !quoted;
        } else if c.is_whitespace() && !quoted {
            if !word.is_empty() {
                tokens.push(std::mem::take(&mut word).to_lowercase());
            }
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        tokens.push(word.to_lowercase());
    }
    tokens
}

fn apply_change(meta: &mut TaskMeta, change: &Change, at: Millis) -> Result<(), String> {
    let before = meta.clone();
    let description = match change {
        Change::Workflow(value) => {
            meta.workflow = *value;
            format!(
                "状態を「{}」に変更",
                value.map_or("実行状況に追従", Workflow::label)
            )
        }
        Change::Priority(value) => {
            meta.priority = *value;
            format!("優先度を「{}」に変更", value.label())
        }
        Change::Assignee(value) => {
            meta.assignee = name(value)?;
            format!("担当者を「{}」に変更", display_empty(&meta.assignee))
        }
        Change::Project(value) => {
            meta.project = name(value)?;
            format!("プロジェクトを「{}」に変更", display_empty(&meta.project))
        }
        Change::Cycle(value) => {
            meta.cycle = name(value)?;
            format!("サイクルを「{}」に変更", display_empty(&meta.cycle))
        }
        Change::Due(value) => {
            let value = value.trim();
            if !valid_date(value) {
                return Err("期限は YYYY-MM-DD 形式の実在する日付で入力してください".into());
            }
            meta.due = value.to_owned();
            format!("期限を「{}」に変更", display_empty(&meta.due))
        }
        Change::Estimate(value) => {
            if *value > 999 {
                return Err("見積もりは 0〜999 で入力してください".into());
            }
            meta.estimate = *value;
            format!("見積もりを {value} に変更")
        }
        Change::Pinned(value) => {
            meta.pinned = *value;
            if *value {
                "ピン留めしました"
            } else {
                "ピン留めを解除しました"
            }
            .into()
        }
        Change::Parent(value) => {
            meta.parent = value.clone();
            "親タスクを変更".into()
        }
        Change::BlockedBy(values) => {
            meta.blocked_by = values
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            "依存タスクを変更".into()
        }
        Change::Comment(body) => {
            let body = body.trim();
            if body.is_empty() || body.len() > 64 * 1024 {
                return Err("コメントは空でない 64 KiB 以内の文章にしてください".into());
            }
            let id = format!("comment-{at}-{}", meta.comments.len() + 1);
            meta.comments.push(Comment {
                id,
                body: body.to_owned(),
                at,
            });
            "コメントを追加".into()
        }
        Change::Rank(value) => {
            meta.rank = *value;
            "表示順を変更".into()
        }
        Change::Seen(value) => {
            meta.seen = *value;
            return Ok(());
        }
    };
    if *meta != before {
        push_activity(meta, at, description);
    }
    Ok(())
}

fn display_empty(value: &str) -> &str {
    if value.is_empty() { "未設定" } else { value }
}

fn same_properties(a: &TaskMeta, b: &TaskMeta) -> bool {
    let mut a = a.clone();
    a.activity = b.activity.clone();
    a.seen = b.seen;
    &a == b
}

fn name(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.chars().any(char::is_control) || value.len() > 500 {
        return Err("プロパティは改行を含まない 500 バイト以内の文字列にしてください".into());
    }
    Ok(value.to_owned())
}

fn push_activity(meta: &mut TaskMeta, at: Millis, text: String) {
    meta.activity.push(Activity { at, text });
    if meta.activity.len() > HISTORY_LIMIT {
        meta.activity.drain(..meta.activity.len() - HISTORY_LIMIT);
    }
}

pub fn valid_date(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
    {
        return false;
    }
    let year = value[..4].parse::<u32>().unwrap_or(0);
    let month = value[5..7].parse::<u32>().unwrap_or(0);
    let day = value[8..].parse::<u32>().unwrap_or(0);
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    year > 0 && day > 0 && day <= days
}

fn validate(data: &WorkspaceData) -> Result<(), String> {
    if data.version != FORMAT_VERSION {
        return Err(format!(
            "ワークスペース形式 {} には対応していません",
            data.version
        ));
    }
    for (key, meta) in &data.tasks {
        serde_json::from_str::<TaskRef>(key).map_err(|_| "タスクの識別子が不正です")?;
        if !valid_date(&meta.due) || meta.estimate > 999 {
            return Err("期限または見積もりが不正です".into());
        }
        for value in [&meta.assignee, &meta.project, &meta.cycle] {
            name(value)?;
        }
        for other in meta.parent.iter().chain(meta.blocked_by.iter()) {
            serde_json::from_str::<TaskRef>(other).map_err(|_| "関連タスクの識別子が不正です")?;
            if other == key {
                return Err("自分自身を親タスクや依存タスクに指定できません".into());
            }
        }
    }
    if cyclic(data, true) {
        return Err("親子関係が循環するため変更できません".into());
    }
    if cyclic(data, false) {
        return Err("依存関係が循環するため変更できません".into());
    }
    Ok(())
}

/// Kahn's algorithm avoids a recursion limit on long imported task chains.
fn cyclic(data: &WorkspaceData, parents: bool) -> bool {
    let mut incoming: BTreeMap<&str, usize> = BTreeMap::new();
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (key, meta) in &data.tasks {
        incoming.entry(key).or_default();
        let related: Vec<&str> = if parents {
            meta.parent.iter().map(String::as_str).collect()
        } else {
            meta.blocked_by.iter().map(String::as_str).collect()
        };
        for target in related {
            edges.entry(key).or_default().push(target);
            *incoming.entry(target).or_default() += 1;
        }
    }
    let mut ready: Vec<&str> = incoming
        .iter()
        .filter_map(|(key, count)| (*count == 0).then_some(*key))
        .collect();
    let mut visited = 0;
    while let Some(key) = ready.pop() {
        visited += 1;
        if let Some(targets) = edges.get(key) {
            for target in targets {
                if let Some(count) = incoming.get_mut(target) {
                    *count -= 1;
                    if *count == 0 {
                        ready.push(target);
                    }
                }
            }
        }
    }
    visited != incoming.len()
}

/// The configured tasks root is trusted; its per-repository key directory
/// and files are walked by fsx without following links.
fn base(path: &Path) -> Result<&Path, String> {
    path.parent()
        .and_then(Path::parent)
        .filter(|base| !base.as_os_str().is_empty())
        .ok_or_else(|| "ワークスペースにはリポジトリ別の保存先が必要です".into())
}

fn read(path: &Path) -> Result<(WorkspaceData, Option<Vec<u8>>), String> {
    let base = base(path)?;
    let file = match fsx::open_read(base, path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((WorkspaceData::default(), None));
        }
        Err(error) => return Err(format!("ワークスペースを読めません: {error}")),
    };
    let (bytes, over) = fsx::read_capped(file, MAX_BYTES).map_err(|e| e.to_string())?;
    if over {
        return Err("ワークスペースが 16 MiB を超えています".into());
    }
    let data = serde_json::from_slice::<WorkspaceData>(&bytes)
        .map_err(|e| format!("ワークスペースを読めません。元のファイルを保持します: {e}"))?;
    validate(&data)?;
    Ok((data, Some(bytes)))
}

fn lock(path: &Path) -> io::Result<File> {
    let base = base(path).map_err(io::Error::other)?;
    fs::create_dir_all(base)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("保存先がありません"))?;
    let dir = fsx::open_dir(base, parent, true)?;
    let name = CString::new("workspace.json.lock").expect("constant has no NUL");
    // SAFETY: dir owns its descriptor; name is a live NUL-terminated string.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new descriptor and file is its only owner.
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("ロックは通常のファイルではありません"));
    }
    // A UI edit must not wait indefinitely for another process.
    // SAFETY: file is an owned, live descriptor.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::State;
    use crate::testkit::{TempDir, local, run_view};

    fn local_ref(id: &str) -> TaskRef {
        TaskRef::Local(id.into())
    }

    #[test]
    fn existing_tasks_keep_derived_workflow_until_explicitly_changed() {
        let mut task = local("t-1", "Issue");
        let meta = TaskMeta::default();
        assert_eq!(effective_workflow(&task, None, &meta), Workflow::Todo);
        task.state = State::Closed;
        assert_eq!(effective_workflow(&task, None, &meta), Workflow::Done);
        let meta = TaskMeta {
            workflow: Some(Workflow::InReview),
            ..meta
        };
        assert_eq!(effective_workflow(&task, None, &meta), Workflow::InReview);
        assert_eq!(
            task.state,
            State::Closed,
            "local planning never mutates the source"
        );
    }

    #[test]
    fn human_workflow_and_agent_attention_are_independent() {
        let task = local("t-1", "Waiting for a person");
        let run = run_view(
            &task.r#ref,
            Liveness::Alive {
                agent: Some(AgentState::Permission),
            },
            ResultFact::Missing,
        );
        let meta = TaskMeta {
            workflow: Some(Workflow::InReview),
            ..TaskMeta::default()
        };
        assert_eq!(
            effective_workflow(&task, Some(&run), &meta),
            Workflow::InReview
        );
        assert!(needs_attention(Some(&run)));
        assert_eq!(agent_status(Some(&run)), "許可待ち");
        assert!(matches_query(
            "state:in_review attention:true",
            &task,
            Some(&run),
            &meta
        ));
    }

    #[test]
    fn task_keys_keep_sources_repositories_and_marker_paths_separate() {
        let refs = [
            TaskRef::Local("1".into()),
            TaskRef::GitHub {
                repo: "a/one".into(),
                number: 1,
            },
            TaskRef::GitHub {
                repo: "b/two".into(),
                number: 1,
            },
            TaskRef::Code {
                path: "one.rs".into(),
                key: "same".into(),
            },
            TaskRef::Code {
                path: "two.rs".into(),
                key: "same".into(),
            },
        ];
        let keys = refs.iter().map(task_key).collect::<BTreeSet<_>>();
        assert_eq!(keys.len(), refs.len());
        for r in refs {
            assert_eq!(serde_json::from_str::<TaskRef>(&task_key(&r)).unwrap(), r);
        }
    }

    #[test]
    fn bulk_edit_is_atomic_and_undo_keeps_unrelated_tasks() {
        let (a, b, c) = (local_ref("a"), local_ref("b"), local_ref("c"));
        let mut ws = Workspace::memory();
        ws.apply(
            std::slice::from_ref(&c),
            &[Change::Project("other".into())],
            1,
        )
        .unwrap();
        ws.apply(
            &[a.clone(), b.clone()],
            &[Change::Priority(Priority::High), Change::Pinned(true)],
            2,
        )
        .unwrap();
        assert_eq!(ws.meta(&a).priority, Priority::High);
        assert!(ws.meta(&b).pinned);
        ws.undo(3).unwrap();
        assert_eq!(ws.meta(&a).priority, Priority::None);
        assert!(!ws.meta(&b).pinned);
        assert_eq!(ws.meta(&c).project, "other");
        ws.undo(4).unwrap();
        assert!(ws.meta(&c).project.is_empty());
        assert!(!ws.can_undo());
    }

    #[test]
    fn sequential_undo_of_the_same_task_keeps_an_audit_trail() {
        let a = local_ref("a");
        let mut ws = Workspace::memory();
        ws.apply(
            std::slice::from_ref(&a),
            &[Change::Priority(Priority::High)],
            1,
        )
        .unwrap();
        ws.apply(
            std::slice::from_ref(&a),
            &[Change::Project("render".into())],
            2,
        )
        .unwrap();
        ws.undo(3).unwrap();
        ws.undo(4).unwrap();
        let meta = ws.meta(&a);
        assert_eq!(meta.priority, Priority::None);
        assert!(meta.project.is_empty());
        assert_eq!(meta.activity.len(), 4);
    }

    #[test]
    fn dependencies_and_parents_refuse_self_links_and_cycles() {
        let (a, b) = (local_ref("a"), local_ref("b"));
        let mut ws = Workspace::memory();
        assert!(
            ws.apply(
                std::slice::from_ref(&a),
                &[Change::Parent(Some(task_key(&a)))],
                1
            )
            .is_err()
        );
        ws.apply(
            std::slice::from_ref(&a),
            &[Change::Parent(Some(task_key(&b)))],
            1,
        )
        .unwrap();
        assert!(
            ws.apply(
                std::slice::from_ref(&b),
                &[Change::Parent(Some(task_key(&a)))],
                2
            )
            .is_err()
        );
        ws.apply(
            std::slice::from_ref(&a),
            &[Change::BlockedBy(vec![task_key(&b)])],
            3,
        )
        .unwrap();
        assert!(
            ws.apply(
                std::slice::from_ref(&b),
                &[Change::BlockedBy(vec![task_key(&a)])],
                4
            )
            .is_err()
        );
        assert!(ws.meta(&b).blocked_by.is_empty());
        assert!(ws.meta(&b).parent.is_none());
    }

    #[test]
    fn one_bad_change_does_not_partly_apply_bulk_edit() {
        let a = local_ref("a");
        let mut ws = Workspace::memory();
        assert!(
            ws.apply(
                std::slice::from_ref(&a),
                &[
                    Change::Priority(Priority::Urgent),
                    Change::Due("2026-02-29".into())
                ],
                1
            )
            .is_err()
        );
        assert_eq!(ws.meta(&a), TaskMeta::default());
        assert!(!ws.can_undo());
    }

    #[test]
    fn marking_seen_is_not_an_undo_step_and_is_not_reverted() {
        let a = local_ref("a");
        let mut ws = Workspace::memory();
        ws.apply(
            std::slice::from_ref(&a),
            &[Change::Priority(Priority::High)],
            1,
        )
        .unwrap();
        ws.apply(std::slice::from_ref(&a), &[Change::Seen(20)], 20)
            .unwrap();
        ws.undo(30).unwrap();
        assert_eq!(ws.meta(&a).priority, Priority::None);
        assert_eq!(ws.meta(&a).seen, 20);
        assert!(!ws.can_undo());
    }

    #[test]
    fn one_reorder_and_undo_preserve_the_other_task_properties() {
        let (a, b, c) = (local_ref("a"), local_ref("b"), local_ref("c"));
        let mut ws = Workspace::memory();
        ws.apply(
            std::slice::from_ref(&b),
            &[Change::Priority(Priority::High)],
            1,
        )
        .unwrap();
        ws.reorder(&[c.clone(), a.clone(), b.clone()], 2).unwrap();
        assert_eq!(ws.meta(&c).rank, 0);
        assert_eq!(ws.meta(&a).rank, 1);
        assert_eq!(ws.meta(&b).rank, 2);
        ws.undo(3).unwrap();
        assert_eq!(ws.meta(&a).rank, 0);
        assert_eq!(ws.meta(&b).rank, 0);
        assert_eq!(ws.meta(&b).priority, Priority::High);
    }

    #[test]
    fn validates_real_dates_and_unicode_without_panicking() {
        for date in ["", "2024-02-29", "2026-11-26", "2000-02-29"] {
            assert!(valid_date(date), "{date}");
        }
        for date in [
            "2026-02-29",
            "1900-02-29",
            "2026-04-31",
            "2026-00-01",
            "0000-01-01",
            "日付-01-01",
            "2026-1-1",
        ] {
            assert!(!valid_date(date), "{date}");
        }
    }

    #[test]
    fn search_covers_body_comments_qualifiers_phrases_and_negation() {
        let mut task = local("t-10", "Reconnect IPC");
        task.body = Some("Back off when the socket disappears".into());
        task.labels = vec!["bug".into()];
        let meta = TaskMeta {
            project: "Terminal Core".into(),
            priority: Priority::High,
            comments: vec![Comment {
                body: "Reproduce after sleep".into(),
                ..Comment::default()
            }],
            ..TaskMeta::default()
        };
        assert!(matches_query(
            "socket priority:high -label:feature",
            &task,
            None,
            &meta
        ));
        assert!(matches_query(
            "project:\"terminal core\" sleep",
            &task,
            None,
            &meta
        ));
        assert!(!matches_query("socket priority:urgent", &task, None, &meta));
        assert!(!matches_query("-label:bug", &task, None, &meta));
        assert!(!matches_query("unknown:ignored", &task, None, &meta));
    }

    #[test]
    fn missing_file_and_empty_legacy_fields_load_without_migration() {
        let temp = TempDir::new("workspace-legacy");
        let path = temp.path().join("repo").join(FILE_NAME);
        let mut ws = Workspace::open(path.clone()).unwrap();
        assert_eq!(ws.data(), &WorkspaceData::default());
        ws.apply(&[local_ref("a")], &[Change::Priority(Priority::High)], 1)
            .unwrap();
        assert_eq!(
            Workspace::open(path)
                .unwrap()
                .meta(&local_ref("a"))
                .priority,
            Priority::High
        );
        assert_eq!(
            serde_json::from_str::<WorkspaceData>("{}").unwrap(),
            WorkspaceData::default()
        );
    }

    #[test]
    fn stale_window_cannot_overwrite_and_reload_retains_other_tasks() {
        let temp = TempDir::new("workspace-conflict");
        let path = temp.path().join("repo").join(FILE_NAME);
        let mut first = Workspace::open(path.clone()).unwrap();
        let mut second = Workspace::open(path.clone()).unwrap();
        let (a, b) = (local_ref("a"), local_ref("b"));
        first
            .apply(
                std::slice::from_ref(&a),
                &[Change::Priority(Priority::High)],
                1,
            )
            .unwrap();
        assert!(
            second
                .apply(std::slice::from_ref(&b), &[Change::Pinned(true)], 2)
                .unwrap_err()
                .contains("別のウィンドウ")
        );
        assert!(!second.meta(&b).pinned);
        second.reload().unwrap();
        second
            .apply(std::slice::from_ref(&b), &[Change::Pinned(true)], 2)
            .unwrap();
        first.reload().unwrap();
        first.undo(3).unwrap();
        let final_state = Workspace::open(path).unwrap();
        assert_eq!(final_state.meta(&a).priority, Priority::None);
        assert!(final_state.meta(&b).pinned);
    }

    #[test]
    fn corrupt_or_newer_file_is_preserved_even_after_open() {
        let temp = TempDir::new("workspace-corrupt");
        let path = temp.path().join("repo").join(FILE_NAME);
        fs::create_dir(path.parent().unwrap()).unwrap();
        fs::write(&path, b"{broken").unwrap();
        assert!(Workspace::open(path.clone()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{broken");
        fs::write(&path, b"{\"version\":2}").unwrap();
        assert!(Workspace::open(path.clone()).is_err());
        fs::remove_file(&path).unwrap();
        let mut ws = Workspace::open(path.clone()).unwrap();
        fs::write(&path, b"{broken").unwrap();
        assert!(
            ws.apply(&[local_ref("a")], &[Change::Pinned(true)], 1)
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"{broken");
    }

    #[test]
    fn a_hand_edit_with_the_same_revision_is_still_a_conflict() {
        let temp = TempDir::new("workspace-hand-edit");
        let path = temp.path().join("repo").join(FILE_NAME);
        let mut ws = Workspace::open(path.clone()).unwrap();
        ws.apply(&[local_ref("a")], &[Change::Pinned(true)], 1)
            .unwrap();
        let mut edited = ws.data().clone();
        edited.preferences.query = "manual edit".into();
        let bytes = serde_json::to_vec(&edited).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(
            ws.apply(&[local_ref("b")], &[Change::Pinned(true)], 2)
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn undo_refuses_a_task_changed_in_another_window() {
        let temp = TempDir::new("workspace-undo-conflict");
        let path = temp.path().join("repo").join(FILE_NAME);
        let a = local_ref("a");
        let mut first = Workspace::open(path.clone()).unwrap();
        first
            .apply(
                std::slice::from_ref(&a),
                &[Change::Priority(Priority::High)],
                1,
            )
            .unwrap();
        let mut second = Workspace::open(path).unwrap();
        second
            .apply(
                std::slice::from_ref(&a),
                &[Change::Priority(Priority::Urgent)],
                2,
            )
            .unwrap();
        first.reload().unwrap();
        assert!(first.undo(3).unwrap_err().contains("その後に編集"));
        assert_eq!(first.meta(&a).priority, Priority::Urgent);
    }

    #[test]
    fn file_and_lock_symlinks_are_refused_without_touching_the_target() {
        use std::os::unix::fs::symlink;
        let temp = TempDir::new("workspace-symlink");
        let target = temp.path().join("target");
        fs::write(&target, b"keep").unwrap();
        let dir = temp.path().join("repo");
        fs::create_dir(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        symlink(&target, &path).unwrap();
        assert!(Workspace::open(path.clone()).is_err());
        fs::remove_file(&path).unwrap();
        let mut ws = Workspace::open(path).unwrap();
        symlink(&target, dir.join("workspace.json.lock")).unwrap();
        assert!(
            ws.apply(&[local_ref("a")], &[Change::Pinned(true)], 1)
                .is_err()
        );
        assert_eq!(fs::read(target).unwrap(), b"keep");
    }

    #[test]
    fn saved_views_preferences_comments_and_relations_round_trip() {
        let temp = TempDir::new("workspace-roundtrip");
        let path = temp.path().join("repo").join(FILE_NAME);
        let mut ws = Workspace::open(path.clone()).unwrap();
        ws.save_view(SavedView {
            id: "bugs".into(),
            name: "不具合".into(),
            query: "label:bug".into(),
            section: "active".into(),
            layout: "list".into(),
            sort: Sort::Priority,
            source: Some("github".into()),
            label: Some("bug".into()),
        })
        .unwrap();
        ws.set_preferences(Preferences {
            query: "project:Noa".into(),
            ..Preferences::default()
        })
        .unwrap();
        ws.apply(
            &[local_ref("a")],
            &[
                Change::Comment("再現手順を確認".into()),
                Change::Due("2026-12-01".into()),
            ],
            1,
        )
        .unwrap();
        let reopened = Workspace::open(path).unwrap();
        assert_eq!(reopened.data(), ws.data());
        assert_eq!(
            reopened.meta(&local_ref("a")).comments[0].body,
            "再現手順を確認"
        );
    }
}
