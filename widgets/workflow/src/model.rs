//! Typed workflow graphs and immutable inputs to a run. No I/O or GUI types.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_NODES: usize = 64;
pub const MAX_EDGES: usize = 256;
pub type Millis = u64;

pub fn digest_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Explicit launch settings. `auto` is resolved once when a run is created;
/// retries read the frozen run definition rather than detecting another agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentChoice {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl Default for AgentChoice {
    fn default() -> Self {
        Self {
            provider: "auto".into(),
            model: None,
            effort: None,
        }
    }
}

impl AgentChoice {
    pub const PROVIDERS: [&'static str; 4] = ["auto", "codex", "claude", "agy"];

    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn efforts(provider: &str) -> &'static [&'static str] {
        match provider {
            "codex" => &["low", "medium", "high", "xhigh", "max", "ultra"],
            "claude" | "agy" => &["low", "medium", "high", "xhigh", "max"],
            _ => &[],
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !Self::PROVIDERS.contains(&self.provider.as_str()) {
            return Err("エージェントは auto / codex / claude / agy から選んでください".into());
        }
        if self.provider == "auto" && (self.model.is_some() || self.effort.is_some()) {
            return Err("モデルやeffortを指定する場合はエージェントを選んでください".into());
        }
        if let Some(model) = &self.model
            && (model.is_empty()
                || model.len() > 200
                || model.starts_with(['-', '='])
                || !model
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._:/@+=-".contains(&c)))
        {
            return Err(
                "モデル名には英数字と . _ : / @ + = - を使用してください（先頭の - と = は不可）"
                    .into(),
            );
        }
        if let Some(effort) = &self.effort
            && !Self::efforts(&self.provider).contains(&effort.as_str())
        {
            return Err(format!(
                "{} は effort {effort:?} に対応していません",
                self.provider
            ));
        }
        Ok(())
    }

    pub fn label(&self) -> String {
        let mut parts = vec![self.provider.as_str()];
        parts.extend(self.model.as_deref());
        parts.extend(self.effort.as_deref());
        parts.join(" · ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPreset {
    pub name: String,
    pub assignments: BTreeMap<String, AgentChoice>,
}

impl AgentPreset {
    pub fn from_definition(name: String, definition: &WorkflowDefinition) -> Self {
        Self {
            name,
            assignments: definition
                .nodes
                .iter()
                .filter(|n| n.kind.uses_agent())
                .map(|n| (n.id.clone(), n.agent.clone()))
                .collect(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty()
            || self.name.len() > 240
            || self.name.chars().any(char::is_control)
        {
            return Err("担当構成の名前を1〜240バイトで入力してください".into());
        }
        if self.assignments.is_empty()
            || self.assignments.len() > MAX_NODES
            || self.assignments.keys().any(|id| !valid_id(id))
        {
            return Err("担当構成は計画・実装・レビュー・検証の設定です".into());
        }
        for choice in self.assignments.values() {
            choice.validate()?;
        }
        Ok(())
    }

    pub fn apply(&self, definition: &mut WorkflowDefinition) -> Result<(), String> {
        self.validate()?;
        if self.assignments.keys().any(|id| {
            definition
                .node(id)
                .is_none_or(|node| !node.kind.uses_agent())
        }) {
            return Err("担当構成の工程IDが現在のフローと一致しません".into());
        }
        for node in &mut definition.nodes {
            if let Some(choice) = self.assignments.get(&node.id) {
                node.agent = choice.clone();
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Task,
    Plan,
    Change,
    Evidence,
    Approval,
    PullRequest,
}

impl ArtifactKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Task => "タスク",
            Self::Plan => "計画",
            Self::Change => "変更",
            Self::Evidence => "証跡",
            Self::Approval => "承認",
            Self::PullRequest => "PR",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Trigger,
    Plan,
    Implement,
    Review,
    Verify,
    Approval,
    PullRequest,
}

impl NodeKind {
    pub fn uses_agent(self) -> bool {
        matches!(
            self,
            Self::Plan | Self::Implement | Self::Review | Self::Verify
        )
    }
    pub const ALL: [Self; 7] = [
        Self::Trigger,
        Self::Plan,
        Self::Implement,
        Self::Review,
        Self::Verify,
        Self::Approval,
        Self::PullRequest,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Trigger => "タスク",
            Self::Plan => "計画",
            Self::Implement => "実装",
            Self::Review => "レビュー",
            Self::Verify => "検証",
            Self::Approval => "承認",
            Self::PullRequest => "PR作成",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Trigger => "目的と受け入れ条件を確定する",
            Self::Plan => "影響範囲と検証方法を計画する",
            Self::Implement => "隔離した作業領域で変更を作る",
            Self::Review => "変更を読み、問題と根拠を記録する",
            Self::Verify => "受け入れ条件を証跡で検証する",
            Self::Approval => "対象と検証結果を確認して承認する",
            Self::PullRequest => "承認した変更のPRを作成する",
        }
    }

    pub fn output_kind(self) -> ArtifactKind {
        match self {
            Self::Trigger => ArtifactKind::Task,
            Self::Plan => ArtifactKind::Plan,
            Self::Implement => ArtifactKind::Change,
            Self::Review | Self::Verify => ArtifactKind::Evidence,
            Self::Approval => ArtifactKind::Approval,
            Self::PullRequest => ArtifactKind::PullRequest,
        }
    }

    pub fn accepts(self, artifact: ArtifactKind) -> bool {
        match self {
            Self::Trigger => false,
            Self::Plan => artifact == ArtifactKind::Task,
            Self::Implement => artifact == ArtifactKind::Plan,
            Self::Review | Self::Verify => artifact == ArtifactKind::Change,
            Self::Approval => {
                matches!(artifact, ArtifactKind::Change | ArtifactKind::Evidence)
            }
            Self::PullRequest => artifact == ArtifactKind::Approval,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub title: String,
    pub position: Point,
    pub instruction: String,
    pub timeout_ms: Millis,
    pub max_retries: u8,
    /// Defaults are omitted so pre-assignment definitions retain their digest.
    #[serde(default, skip_serializing_if = "AgentChoice::is_default")]
    pub agent: AgentChoice,
}

impl Node {
    pub fn new(id: impl Into<String>, kind: NodeKind, x: f32, y: f32) -> Self {
        Self {
            id: id.into(),
            kind,
            title: kind.label().to_owned(),
            position: Point { x, y },
            instruction: String::new(),
            timeout_ms: 30 * 60_000,
            max_retries: 2,
            agent: AgentChoice::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub artifact: ArtifactKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_repair_attempts: u8,
    pub max_duration_ms: Millis,
    pub max_parallel_nodes: u8,
    pub max_cost_microusd: Option<u64>,
    pub allow_unknown_cost: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_repair_attempts: 2,
            max_duration_ms: 90 * 60_000,
            max_parallel_nodes: 2,
            max_cost_microusd: None,
            allow_unknown_cost: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinition {
    pub schema_version: u32,
    pub revision: u64,
    pub name: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub limits: Limits,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    pub node_id: Option<String>,
    pub code: String,
    pub message: String,
}

fn issue(node: Option<&str>, code: &str, message: impl Into<String>) -> ValidationIssue {
    ValidationIssue {
        node_id: node.map(str::to_owned),
        code: code.to_owned(),
        message: message.into(),
    }
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Default for WorkflowDefinition {
    fn default() -> Self {
        Self::standard()
    }
}

impl WorkflowDefinition {
    pub fn standard() -> Self {
        let nodes = vec![
            Node::new("task", NodeKind::Trigger, 32.0, 48.0),
            Node::new("plan", NodeKind::Plan, 284.0, 48.0),
            Node::new("implement", NodeKind::Implement, 536.0, 48.0),
            Node::new("review", NodeKind::Review, 284.0, 260.0),
            Node::new("verify", NodeKind::Verify, 536.0, 260.0),
            Node::new("approval", NodeKind::Approval, 284.0, 472.0),
            Node::new("pr", NodeKind::PullRequest, 536.0, 472.0),
        ];
        let edges = [
            ("task", "plan", ArtifactKind::Task),
            ("plan", "implement", ArtifactKind::Plan),
            ("implement", "review", ArtifactKind::Change),
            ("implement", "verify", ArtifactKind::Change),
            ("implement", "approval", ArtifactKind::Change),
            ("review", "approval", ArtifactKind::Evidence),
            ("verify", "approval", ArtifactKind::Evidence),
            ("approval", "pr", ArtifactKind::Approval),
        ]
        .into_iter()
        .map(|(from, to, artifact)| Edge {
            from: from.to_owned(),
            to: to.to_owned(),
            artifact,
        })
        .collect();
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 1,
            name: "開発サイクル".to_owned(),
            nodes,
            edges,
            limits: Limits::default(),
        }
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn digest(&self) -> String {
        digest_bytes(&serde_json::to_vec(self).expect("definition contains JSON values only"))
    }

    pub fn descendants(&self, id: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let mut queue = VecDeque::from([id.to_owned()]);
        while let Some(current) = queue.pop_front() {
            for edge in self.edges.iter().filter(|e| e.from == current) {
                if found.insert(edge.to.clone()) {
                    queue.push_back(edge.to.clone());
                }
            }
        }
        found
    }

    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut errors = Vec::new();
        if self.schema_version != SCHEMA_VERSION {
            errors.push(issue(None, "schema", "未対応のワークフロー形式です"));
        }
        if self.name.trim().is_empty() || self.name.len() > 1024 {
            errors.push(issue(
                None,
                "name",
                "ワークフロー名を入力してください（最大1024バイト）",
            ));
        }
        if self.nodes.is_empty() || self.nodes.len() > MAX_NODES || self.edges.len() > MAX_EDGES {
            errors.push(issue(
                None,
                "size",
                "工程は1〜64件、接続は256件以内にしてください",
            ));
            return errors;
        }
        if self.limits.max_parallel_nodes == 0
            || self.limits.max_parallel_nodes > 8
            || self.limits.max_repair_attempts > 10
            || self.limits.max_duration_ms == 0
            || self.limits.max_duration_ms > 24 * 60 * 60_000
        {
            errors.push(issue(
                None,
                "limits",
                "並列数・修正回数・実行時間の上限を確認してください",
            ));
        }
        let mut by_id = BTreeMap::new();
        for node in &self.nodes {
            if let Err(message) = node.agent.validate() {
                errors.push(issue(Some(&node.id), "agent", message));
            }
            if !node.kind.uses_agent() && !node.agent.is_default() {
                errors.push(issue(
                    Some(&node.id),
                    "agent",
                    "この工程はエージェントを使用しません",
                ));
            }
            if !valid_id(&node.id) || by_id.insert(node.id.as_str(), node).is_some() {
                errors.push(issue(
                    Some(&node.id),
                    "node_id",
                    "工程IDが無効または重複しています",
                ));
            }
            if node.title.trim().is_empty()
                || node.title.len() > 1024
                || node.instruction.len() > 65_536
                || !node.position.x.is_finite()
                || !node.position.y.is_finite()
                || node.position.x.abs() > 1_000_000.0
                || node.position.y.abs() > 1_000_000.0
                || node.timeout_ms == 0
                || node.timeout_ms > 24 * 60 * 60_000
                || node.max_retries > 5
            {
                errors.push(issue(
                    Some(&node.id),
                    "node_config",
                    "工程名・位置・指示・上限が無効です",
                ));
            }
        }
        for kind in [NodeKind::Trigger, NodeKind::Implement] {
            if self.nodes.iter().filter(|n| n.kind == kind).count() != 1 {
                errors.push(issue(
                    None,
                    "single_node",
                    format!("{}工程は1つにしてください", kind.label()),
                ));
            }
        }
        let mut seen = BTreeSet::new();
        let mut indegree: BTreeMap<&str, usize> = by_id.keys().map(|id| (*id, 0)).collect();
        for edge in &self.edges {
            let (Some(from), Some(to)) =
                (by_id.get(edge.from.as_str()), by_id.get(edge.to.as_str()))
            else {
                errors.push(issue(
                    None,
                    "missing_node",
                    "接続先または接続元の工程がありません",
                ));
                continue;
            };
            if !seen.insert((&edge.from, &edge.to, edge.artifact)) {
                errors.push(issue(
                    Some(&edge.to),
                    "duplicate_edge",
                    "同じ接続が重複しています",
                ));
            }
            if from.kind.output_kind() != edge.artifact || !to.kind.accepts(edge.artifact) {
                errors.push(issue(
                    Some(&edge.to),
                    "port_type",
                    "受け渡す成果物の型が一致しません",
                ));
            }
            if let Some(degree) = indegree.get_mut(edge.to.as_str()) {
                *degree += 1;
            }
        }
        let mut ready: VecDeque<&str> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut visited = 0;
        while let Some(id) = ready.pop_front() {
            visited += 1;
            for edge in self.edges.iter().filter(|e| e.from == id) {
                if let Some(degree) = indegree.get_mut(edge.to.as_str()) {
                    *degree = degree.saturating_sub(1);
                    if *degree == 0 {
                        ready.push_back(edge.to.as_str());
                    }
                }
            }
        }
        if visited != by_id.len() {
            errors.push(issue(
                None,
                "cycle",
                "循環接続は使えません。修正回数の設定を使用してください",
            ));
        }
        for node in &self.nodes {
            let incoming: Vec<_> = self.edges.iter().filter(|e| e.to == node.id).collect();
            let required = match node.kind {
                NodeKind::Trigger => None,
                NodeKind::Plan => Some(ArtifactKind::Task),
                NodeKind::Implement => Some(ArtifactKind::Plan),
                NodeKind::Review | NodeKind::Verify | NodeKind::Approval => {
                    Some(ArtifactKind::Change)
                }
                NodeKind::PullRequest => Some(ArtifactKind::Approval),
            };
            if let Some(kind) = required
                && incoming.iter().filter(|e| e.artifact == kind).count() != 1
            {
                errors.push(issue(
                    Some(&node.id),
                    "missing_input",
                    format!("{}の入力を1つ接続してください", kind.label()),
                ));
            }
            if node.kind == NodeKind::Approval {
                for required_kind in [NodeKind::Review, NodeKind::Verify] {
                    if !incoming.iter().any(|e| {
                        e.artifact == ArtifactKind::Evidence
                            && by_id
                                .get(e.from.as_str())
                                .is_some_and(|n| n.kind == required_kind)
                    }) {
                        errors.push(issue(
                            Some(&node.id),
                            "missing_evidence",
                            format!("承認には{}の証跡が必要です", required_kind.label()),
                        ));
                    }
                }
            }
        }
        if let Some(trigger) = self.nodes.iter().find(|n| n.kind == NodeKind::Trigger) {
            let reachable = self.descendants(&trigger.id);
            for node in self
                .nodes
                .iter()
                .filter(|n| n.id != trigger.id && !reachable.contains(&n.id))
            {
                errors.push(issue(
                    Some(&node.id),
                    "unreachable",
                    "開始工程から接続されていません",
                ));
            }
        }
        errors
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetSnapshot {
    pub base_sha: String,
    pub head_sha: String,
    pub dirty_digest: String,
}

impl TargetSnapshot {
    pub fn digest(&self) -> String {
        digest_bytes(&serde_json::to_vec(self).expect("target contains strings only"))
    }

    pub fn is_valid(&self) -> bool {
        !self.base_sha.trim().is_empty()
            && !self.head_sha.trim().is_empty()
            && !self.dirty_digest.trim().is_empty()
            && self.base_sha.len() <= 128
            && self.head_sha.len() <= 128
            && self.dirty_digest.len() <= 128
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub id: String,
    pub kind: ArtifactKind,
    pub target_digest: String,
    pub summary: String,
    pub path: Option<PathBuf>,
    pub data: serde_json::Value,
}

impl Artifact {
    pub fn new(
        kind: ArtifactKind,
        target_digest: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        let target_digest = target_digest.into();
        let summary = summary.into();
        let id = digest_bytes(
            &serde_json::to_vec(&(kind, &target_digest, &summary)).expect("artifact key is JSON"),
        );
        Self {
            id,
            kind,
            target_digest,
            summary,
            path: None,
            data: serde_json::Value::Null,
        }
    }

    pub fn digest(&self) -> String {
        digest_bytes(&serde_json::to_vec(self).expect("artifact contains JSON values only"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    Pending,
    Running,
    Waiting,
    Uncertain,
    Succeeded,
    Failed,
    Paused,
    Canceled,
}

impl NodeState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "未開始",
            Self::Running => "実行中",
            Self::Waiting => "判断待ち",
            Self::Uncertain => "結果確認中",
            Self::Succeeded => "完了",
            Self::Failed => "問題あり",
            Self::Paused => "一時停止",
            Self::Canceled => "中断",
        }
    }

    pub fn active(self) -> bool {
        matches!(self, Self::Running | Self::Waiting | Self::Uncertain)
    }
}

pub type RunState = NodeState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRecord {
    pub attempt: u32,
    pub state: NodeState,
    pub message: String,
    pub outputs: Vec<Artifact>,
    pub target_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeRun {
    pub node_id: String,
    pub state: NodeState,
    pub attempt: u32,
    pub request_id: String,
    pub started_at: Option<Millis>,
    pub finished_at: Option<Millis>,
    pub message: String,
    pub outputs: Vec<Artifact>,
    pub external_ref: Option<String>,
    pub approval_digest: Option<String>,
    pub history: Vec<AttemptRecord>,
    pub retry_at: Option<Millis>,
    pub retryable: bool,
    pub cancel_requested: bool,
    /// An expired attempt is automatically reconciled once. A running reply
    /// must not immediately reconcile the same expired deadline again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_reconciled_at: Option<Millis>,
    /// The agent's sequence identifies the exact input request being answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_sequence: Option<u64>,
}

impl NodeRun {
    pub fn pending(id: &str) -> Self {
        Self {
            node_id: id.to_owned(),
            state: NodeState::Pending,
            attempt: 0,
            request_id: String::new(),
            started_at: None,
            finished_at: None,
            message: String::new(),
            outputs: Vec::new(),
            external_ref: None,
            approval_digest: None,
            history: Vec::new(),
            retry_at: None,
            retryable: false,
            cancel_requested: false,
            timeout_reconciled_at: None,
            input_sequence: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvent {
    pub seq: u64,
    pub at: Millis,
    pub node_id: Option<String>,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub schema_version: u32,
    pub id: String,
    pub definition: WorkflowDefinition,
    pub definition_digest: String,
    pub repository: PathBuf,
    pub objective: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_task: Option<crate::tasks::TaskOrigin>,
    pub target: TargetSnapshot,
    pub state: RunState,
    pub nodes: BTreeMap<String, NodeRun>,
    pub repair_attempts: u8,
    pub repair_context: Vec<Artifact>,
    pub started_at: Millis,
    pub updated_at: Millis,
    pub seq: u64,
    pub events: Vec<RunEvent>,
    pub pause_requested: bool,
    pub cancel_requested: bool,
    pub cost_microusd: Option<u64>,
    pub limit_reason: Option<String>,
}

impl Run {
    pub fn node(&self, id: &str) -> Option<&NodeRun> {
        self.nodes.get(id)
    }

    pub fn demo() -> Self {
        crate::engine::demo_waiting(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_assignments_preserve_old_serialization_and_run_digests() {
        let definition = WorkflowDefinition::standard();
        let encoded = serde_json::to_string(&definition).unwrap();
        assert!(!encoded.contains("\"agent\""));
        let restored: WorkflowDefinition = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored.digest(), definition.digest());
        assert!(restored.nodes.iter().all(|n| n.agent.is_default()));
        let run = Run::demo();
        let encoded = serde_json::to_string(&run).unwrap();
        assert!(!encoded.contains("origin_task"));
        assert!(!encoded.contains("timeout_reconciled_at"));
        assert!(!encoded.contains("input_sequence"));
        crate::engine::Engine::restore(serde_json::from_str(&encoded).unwrap()).unwrap();
    }

    #[test]
    fn a_preset_preserves_distinct_assignments_for_repeated_stage_kinds() {
        let mut source = WorkflowDefinition::standard();
        let mut extra = Node::new("second-review", NodeKind::Review, 30.0, 30.0);
        extra.agent.provider = "claude".into();
        source.nodes.push(extra);
        source
            .nodes
            .iter_mut()
            .find(|n| n.id == "review")
            .unwrap()
            .agent
            .provider = "codex".into();
        let preset = AgentPreset::from_definition("Independent review".into(), &source);
        let restored: AgentPreset =
            serde_json::from_slice(&serde_json::to_vec(&preset).unwrap()).unwrap();
        let mut target = source.clone();
        for node in &mut target.nodes {
            node.agent = AgentChoice::default();
        }
        let positions: Vec<_> = target.nodes.iter().map(|n| n.position).collect();
        restored.apply(&mut target).unwrap();
        assert_eq!(target.node("review").unwrap().agent.provider, "codex");
        assert_eq!(
            target.node("second-review").unwrap().agent.provider,
            "claude"
        );
        assert_eq!(
            positions,
            target.nodes.iter().map(|n| n.position).collect::<Vec<_>>()
        );
        let mut other = WorkflowDefinition::standard();
        let unchanged = other.clone();
        assert!(restored.apply(&mut other).is_err());
        assert_eq!(other, unchanged);
    }

    #[test]
    fn standard_graph_requires_both_independent_verdicts() {
        let mut definition = WorkflowDefinition::standard();
        assert!(definition.validate().is_empty());
        definition
            .edges
            .retain(|e| !(e.from == "verify" && e.to == "approval"));
        assert!(
            definition
                .validate()
                .iter()
                .any(|i| i.code == "missing_evidence")
        );
    }

    #[test]
    fn mismatched_ports_and_cycle_are_rejected() {
        let mut definition = WorkflowDefinition::standard();
        definition.edges.push(Edge {
            from: "pr".into(),
            to: "plan".into(),
            artifact: ArtifactKind::PullRequest,
        });
        let errors = definition.validate();
        assert!(errors.iter().any(|i| i.code == "port_type"));
        assert!(errors.iter().any(|i| i.code == "cycle"));
    }

    #[test]
    fn orphan_and_duplicate_writer_cannot_start() {
        let mut definition = WorkflowDefinition::standard();
        definition
            .nodes
            .push(Node::new("another", NodeKind::Implement, 0.0, 0.0));
        let errors = definition.validate();
        assert!(errors.iter().any(|i| i.code == "single_node"));
        assert!(errors.iter().any(|i| i.code == "unreachable"));
    }

    #[test]
    fn target_identity_includes_dirty_bytes() {
        let first = TargetSnapshot {
            base_sha: "base".into(),
            head_sha: "head".into(),
            dirty_digest: "clean".into(),
        };
        let mut second = first.clone();
        second.dirty_digest = "dirty".into();
        assert_ne!(first.digest(), second.digest());
    }

    #[test]
    fn definitions_round_trip_with_editable_positions() {
        let mut definition = WorkflowDefinition::standard();
        let undo = definition.clone();
        definition.nodes[0].position.x += 80.0;
        assert_ne!(definition.digest(), undo.digest());
        let json = serde_json::to_string(&definition).unwrap();
        assert_eq!(definition, serde_json::from_str(&json).unwrap());
    }
}
