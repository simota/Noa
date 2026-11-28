use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;
pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_ATTACHMENT_BYTES: usize = 128 * 1024;
pub const MAX_ATTACHMENTS: usize = 8;
pub const MAX_CONTEXT_BYTES: usize = 512 * 1024;
pub const MAX_MESSAGES: usize = 800;
pub const MAX_RUNS: usize = 200;
pub const MAX_MEMORIES: usize = 200;
pub const MAX_RECEIPTS: usize = 16_384;
pub const MAX_STATE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Scope {
    #[default]
    Global,
    Project {
        root: String,
    },
}

impl Scope {
    pub fn label(&self) -> String {
        match self {
            Self::Global => "個人".into(),
            Self::Project { root } => std::path::Path::new(root)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(root)
                .to_owned(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Self::Project { root } = self
            && (root.len() > 4096 || !std::path::Path::new(root).is_absolute())
        {
            return Err("プロジェクトには絶対パスを指定してください".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    #[default]
    User,
    Assistant,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "あなた",
            Self::Assistant => "エンバー",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub id: String,
    pub scope: Scope,
    pub role: Role,
    pub content: String,
    pub run_id: String,
    pub created_at: u64,
    /// A deletion invalidates older automatic context, including late replies.
    pub context_epoch: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEntry {
    pub id: String,
    pub scope: Scope,
    pub text: String,
    pub revision: u64,
    pub created_at: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    #[default]
    Conversation,
    Research,
    Document,
}

impl RunKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Conversation => "会話",
            Self::Research => "資料の調査",
            Self::Document => "文書の作成",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    #[default]
    Queued,
    Running,
    Completed,
    Failed,
    CancelRequested,
    Canceled,
    Uncertain,
}

impl RunStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "開始待ち",
            Self::Running => "進行中",
            Self::Completed => "完了",
            Self::Failed => "失敗",
            Self::CancelRequested => "停止を確認中",
            Self::Canceled => "停止済み",
            Self::Uncertain => "結果を確認できません",
        }
    }

    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Canceled | Self::Uncertain
        )
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    pub path: String,
    pub text: String,
    pub digest: String,
    pub bytes: u64,
    pub captured_at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSnapshot {
    pub scope: Scope,
    pub captured_at: u64,
    pub context_epoch: u64,
    pub attachments: Vec<Attachment>,
    pub memory_ids: Vec<String>,
    pub input: Option<String>,
    pub input_digest: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    /// A relative path inside Ember's private store, never a model-supplied path.
    pub path: String,
    pub title: String,
    pub digest: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub id: String,
    pub kind: RunKind,
    pub scope: Scope,
    pub objective: String,
    pub status: RunStatus,
    pub answer: Option<String>,
    pub artifact: Option<Artifact>,
    pub error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub archived: bool,
    pub model: String,
    pub context: ContextSnapshot,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub notified_at: Option<u64>,
    pub notification_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub model: String,
    pub notifications: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: "gpt-5-mini".into(),
            notifications: false,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if self.model.is_empty()
            || self.model.len() > 120
            || !self
                .model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        {
            return Err("モデル名を確認してください".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub request_id: String,
    pub digest: String,
    pub accepted_revision: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub schema_version: u32,
    pub revision: u64,
    pub settings: Settings,
    pub messages: Vec<Message>,
    pub memories: Vec<MemoryEntry>,
    pub runs: Vec<Run>,
    pub receipts: Vec<Receipt>,
    pub context_epoch: u64,
    pub next_id: u64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            settings: Settings::default(),
            messages: Vec::new(),
            memories: Vec::new(),
            runs: Vec::new(),
            receipts: Vec::new(),
            context_epoch: 0,
            next_id: 1,
        }
    }
}

impl State {
    pub fn id(&mut self, prefix: &str) -> Result<String, String> {
        let n = self.next_id;
        self.next_id = n.checked_add(1).ok_or("識別子の上限に達しました")?;
        Ok(format!("{prefix}-{n}"))
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err("未対応の保存形式です".into());
        }
        self.settings.validate()?;
        if self.messages.len() > MAX_MESSAGES
            || self.runs.len() > MAX_RUNS
            || self.memories.len() > MAX_MEMORIES
            || self.receipts.len() > MAX_RECEIPTS
        {
            return Err("保存件数の上限です。履歴を整理してください".into());
        }
        let mut ids = std::collections::HashSet::new();
        for run in &self.runs {
            if !valid_id(&run.id) || !ids.insert(run.id.as_str()) {
                return Err("依頼の識別子が不正です".into());
            }
            run.scope.validate()?;
            if run.context.scope != run.scope
                || run.objective.len() > MAX_TEXT_BYTES
                || run
                    .answer
                    .as_ref()
                    .is_some_and(|s| s.len() > MAX_CONTEXT_BYTES)
                || run.context.attachments.len() > MAX_ATTACHMENTS
                || run
                    .context
                    .input
                    .as_ref()
                    .is_some_and(|s| s.len() > MAX_CONTEXT_BYTES)
            {
                return Err("依頼の保存内容が不正です".into());
            }
            if run.status == RunStatus::Completed
                && (run.answer.as_ref().is_none_or(|s| s.trim().is_empty())
                    || (run.kind != RunKind::Conversation && run.artifact.is_none()))
            {
                return Err("完了した依頼に回答・成果物がありません".into());
            }
            if matches!(run.status, RunStatus::Running | RunStatus::CancelRequested)
                && run.context.input.is_none()
            {
                return Err("実行中の依頼に確定した文脈がありません".into());
            }
            for a in &run.context.attachments {
                if a.text.len() > MAX_ATTACHMENT_BYTES
                    || crate::store::digest(a.text.as_bytes()) != a.digest
                {
                    return Err("資料の内容とハッシュが一致しません".into());
                }
            }
            if let Some(input) = &run.context.input
                && run.context.input_digest.as_deref()
                    != Some(crate::store::digest(input.as_bytes()).as_str())
            {
                return Err("依頼の文脈ハッシュが一致しません".into());
            }
            if let Some(a) = &run.artifact
                && a.path != format!("artifacts/{}.md", run.id)
            {
                return Err("成果物の保存先が不正です".into());
            }
        }
        let mut message_ids = std::collections::HashSet::new();
        for message in &self.messages {
            message.scope.validate()?;
            if !valid_id(&message.id)
                || !message_ids.insert(&message.id)
                || message.content.len() > MAX_CONTEXT_BYTES
            {
                return Err("会話の保存内容が不正です".into());
            }
            if !self
                .runs
                .iter()
                .any(|r| r.id == message.run_id && r.scope == message.scope)
            {
                return Err("会話と依頼の参照範囲が一致しません".into());
            }
        }
        let mut memory_ids = std::collections::HashSet::new();
        for memory in &self.memories {
            memory.scope.validate()?;
            if !valid_id(&memory.id)
                || !memory_ids.insert(&memory.id)
                || memory.text.trim().is_empty()
                || memory.text.len() > 4096
            {
                return Err("記憶の保存内容が不正です".into());
            }
        }
        let mut requests = std::collections::HashSet::new();
        for receipt in &self.receipts {
            if !valid_id(&receipt.request_id)
                || !requests.insert(&receipt.request_id)
                || receipt.accepted_revision > self.revision
            {
                return Err("操作の記録が不正です".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    pub state: State,
    pub backend_available: bool,
    pub notice: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Send {
        text: String,
        scope: Scope,
        attachments: Vec<String>,
    },
    StartRun {
        kind: RunKind,
        objective: String,
        scope: Scope,
        attachments: Vec<String>,
    },
    CancelRun {
        run_id: String,
    },
    SaveMemory {
        id: Option<String>,
        expected_revision: Option<u64>,
        scope: Scope,
        text: String,
    },
    DeleteMemory {
        id: String,
        expected_revision: u64,
    },
    UpdateSettings {
        settings: Settings,
    },
    ArchiveRun {
        run_id: String,
        archived: bool,
    },
    DeleteRun {
        run_id: String,
    },
    ClearConversation {
        scope: Scope,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    View {
        protocol_version: u32,
    },
    RefreshBackend {
        protocol_version: u32,
    },
    Command {
        protocol_version: u32,
        request_id: String,
        intent: Command,
    },
    Artifact {
        protocol_version: u32,
        run_id: String,
    },
    PrepareHandoff {
        protocol_version: u32,
        run_id: String,
    },
    Export {
        protocol_version: u32,
    },
}

impl Request {
    pub fn version(&self) -> u32 {
        match self {
            Self::View { protocol_version }
            | Self::RefreshBackend { protocol_version }
            | Self::Command {
                protocol_version, ..
            }
            | Self::Artifact {
                protocol_version, ..
            }
            | Self::PrepareHandoff {
                protocol_version, ..
            }
            | Self::Export { protocol_version } => *protocol_version,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub protocol_version: u32,
    pub accepted_revision: Option<u64>,
    pub view: Option<View>,
    pub text: Option<String>,
    pub error: Option<String>,
    pub outcome_unknown: bool,
}

pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 120
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis().min(u64::MAX as u128) as u64)
}
