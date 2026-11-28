//! Deterministic scheduling. Callers persist `run()` before carrying out any
//! returned effect; an agent finishing a turn is never a completion input.

use std::fmt;
use std::path::PathBuf;

use crate::model::*;

const MAX_EVENTS: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineError(pub String);

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EngineError {}

#[derive(Debug, Clone)]
pub enum Input {
    Start {
        now: Millis,
    },
    Tick {
        now: Millis,
    },
    Completed {
        node_id: String,
        attempt: u32,
        outputs: Vec<Artifact>,
        target: Option<TargetSnapshot>,
        now: Millis,
    },
    Failed {
        node_id: String,
        attempt: u32,
        message: String,
        retryable: bool,
        outputs: Vec<Artifact>,
        now: Millis,
    },
    Uncertain {
        node_id: String,
        attempt: u32,
        message: String,
        external_ref: Option<String>,
        now: Millis,
    },
    ObservedRunning {
        node_id: String,
        attempt: u32,
        external_ref: Option<String>,
        message: String,
        now: Millis,
    },
    Waiting {
        node_id: String,
        attempt: u32,
        message: String,
        external_ref: Option<String>,
        input_sequence: Option<u64>,
        now: Millis,
    },
    Reconcile {
        node_id: String,
        now: Millis,
    },
    Approve {
        node_id: String,
        digest: String,
        current_target: TargetSnapshot,
        now: Millis,
    },
    Pause {
        now: Millis,
    },
    Resume {
        now: Millis,
    },
    Retry {
        node_id: String,
        now: Millis,
    },
    RetryAtTarget {
        node_id: String,
        current_target: TargetSnapshot,
        now: Millis,
    },
    Repair {
        now: Millis,
    },
    Cancel {
        now: Millis,
    },
    Canceled {
        node_id: String,
        attempt: u32,
        now: Millis,
    },
    CostObserved {
        cost_microusd: u64,
        now: Millis,
    },
}

impl Input {
    fn now(&self) -> Millis {
        match self {
            Self::Start { now }
            | Self::Tick { now }
            | Self::Completed { now, .. }
            | Self::Failed { now, .. }
            | Self::Uncertain { now, .. }
            | Self::ObservedRunning { now, .. }
            | Self::Waiting { now, .. }
            | Self::Reconcile { now, .. }
            | Self::Approve { now, .. }
            | Self::Pause { now }
            | Self::Resume { now }
            | Self::Retry { now, .. }
            | Self::RetryAtTarget { now, .. }
            | Self::Repair { now }
            | Self::Cancel { now }
            | Self::Canceled { now, .. }
            | Self::CostObserved { now, .. } => *now,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Execute {
        node: Box<Node>,
        attempt: u32,
        request_id: String,
        inputs: Vec<Artifact>,
        target: TargetSnapshot,
    },
    Reconcile {
        node_id: String,
        attempt: u32,
        request_id: String,
        external_ref: Option<String>,
    },
    Cancel {
        node_id: String,
        attempt: u32,
        request_id: String,
    },
}

#[derive(Debug, Clone)]
pub struct Engine {
    run: Run,
}

struct NodeObservation {
    state: NodeState,
    message: String,
    external_ref: Option<String>,
    input_sequence: Option<u64>,
}

impl Engine {
    pub fn new(
        id: String,
        definition: WorkflowDefinition,
        repository: PathBuf,
        objective: String,
        target: TargetSnapshot,
        now: Millis,
    ) -> Result<Self, EngineError> {
        if !valid_id(&id)
            || !repository.is_absolute()
            || objective.trim().is_empty()
            || objective.len() > 65_536
            || !target.is_valid()
        {
            return Err(EngineError(
                "実行ID・対象・目的・変更の識別情報を確認してください".into(),
            ));
        }
        let errors = definition.validate();
        if !errors.is_empty() {
            return Err(EngineError(
                errors
                    .into_iter()
                    .map(|e| e.message)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ));
        }
        let nodes = definition
            .nodes
            .iter()
            .map(|n| (n.id.clone(), NodeRun::pending(&n.id)))
            .collect();
        let definition_digest = definition.digest();
        let mut engine = Self {
            run: Run {
                schema_version: SCHEMA_VERSION,
                id,
                definition,
                definition_digest,
                repository,
                objective,
                origin_task: None,
                target,
                state: NodeState::Pending,
                nodes,
                repair_attempts: 0,
                repair_context: Vec::new(),
                started_at: now,
                updated_at: now,
                seq: 0,
                events: Vec::new(),
                pause_requested: false,
                cancel_requested: false,
                cost_microusd: None,
                limit_reason: None,
            },
        };
        engine.record(
            now,
            None,
            "created",
            "実行時のワークフローと対象を固定しました",
        );
        Ok(engine)
    }

    /// Restoring never resends an operation. Live adapters must reconcile any
    /// active attempt against its persisted request id before continuing it.
    pub fn restore(run: Run) -> Result<Self, EngineError> {
        validate_run(&run)?;
        Ok(Self { run })
    }

    pub fn run(&self) -> &Run {
        &self.run
    }

    pub fn into_run(self) -> Run {
        self.run
    }

    /// Rejected input leaves both the run and its event sequence unchanged.
    pub fn handle(&mut self, input: Input) -> Result<Vec<Effect>, EngineError> {
        if self.run.events.len() >= MAX_EVENTS {
            return Err(EngineError(
                "実行履歴の上限に達しました。新しい実行を作成してください".into(),
            ));
        }
        let now = input.now();
        let mut next = self.clone();
        let mut effects = next.apply(input)?;
        next.advance(now, &mut effects)?;
        validate_run(&next.run)?;
        *self = next;
        Ok(effects)
    }

    fn record(&mut self, now: Millis, node_id: Option<&str>, kind: &str, message: &str) {
        self.run.seq += 1;
        self.run.updated_at = self.run.updated_at.max(now);
        self.run.events.push(RunEvent {
            seq: self.run.seq,
            at: self.run.updated_at,
            node_id: node_id.map(str::to_owned),
            kind: kind.into(),
            message: message.into(),
        });
    }

    fn node_definition(&self, id: &str) -> Result<&Node, EngineError> {
        self.run
            .definition
            .node(id)
            .ok_or_else(|| EngineError("工程が見つかりません".into()))
    }

    fn active_attempt(&self, id: &str, attempt: u32) -> Result<&NodeRun, EngineError> {
        let node = self
            .run
            .nodes
            .get(id)
            .ok_or_else(|| EngineError("工程が見つかりません".into()))?;
        if node.attempt != attempt || !node.state.active() {
            return Err(EngineError(
                "以前の試行または終了済み工程からの結果です".into(),
            ));
        }
        Ok(node)
    }

    fn inputs(&self, id: &str) -> Vec<Artifact> {
        let mut inputs = Vec::new();
        for edge in self.run.definition.edges.iter().filter(|e| e.to == id) {
            if let Some(source) = self.run.nodes.get(&edge.from) {
                inputs.extend(
                    source
                        .outputs
                        .iter()
                        .filter(|a| a.kind == edge.artifact)
                        .cloned(),
                );
            }
        }
        inputs
    }

    pub fn approval_digest(&self, id: &str) -> Result<String, EngineError> {
        if self.node_definition(id)?.kind != NodeKind::Approval {
            return Err(EngineError("承認工程ではありません".into()));
        }
        let evidence: Vec<_> = self.inputs(id).iter().map(Artifact::digest).collect();
        Ok(digest_bytes(
            &serde_json::to_vec(&(
                &self.run.id,
                &self.run.definition_digest,
                id,
                &self.run.target,
                evidence,
            ))
            .expect("approval identity contains JSON values only"),
        ))
    }

    fn publication(&self, id: &str) -> Result<serde_json::Value, EngineError> {
        if !self
            .run
            .definition
            .nodes
            .iter()
            .any(|node| node.kind == NodeKind::PullRequest)
        {
            return Ok(serde_json::Value::Null);
        }
        let publication = self
            .inputs(id)
            .iter()
            .find(|a| a.kind == ArtifactKind::Change)
            .and_then(|a| a.data.get("publication"))
            .cloned()
            .ok_or_else(|| {
                EngineError("PRの公開先・ブランチ・本文がまだ確定していません".into())
            })?;
        for field in [
            "repository",
            "push_url",
            "branch",
            "base",
            "title",
            "body",
            "marker",
        ] {
            if publication
                .get(field)
                .and_then(serde_json::Value::as_str)
                .is_none_or(|s| s.trim().is_empty())
            {
                return Err(EngineError(format!("PRの公開内容に {field} がありません")));
            }
        }
        let target: TargetSnapshot =
            serde_json::from_value(publication.get("target").cloned().unwrap_or_default())
                .map_err(|_| EngineError("PRの公開対象を確認できません".into()))?;
        if target != self.run.target {
            return Err(EngineError("PRの公開対象と検証対象が一致しません".into()));
        }
        Ok(publication)
    }

    fn apply(&mut self, input: Input) -> Result<Vec<Effect>, EngineError> {
        let now = input.now();
        let mut effects = Vec::new();
        match input {
            Input::Start { .. } => {
                if self.run.state != NodeState::Pending {
                    return Err(EngineError("この実行はすでに開始しています".into()));
                }
                self.run.state = NodeState::Running;
                self.record(now, None, "started", "ワークフローを開始しました");
            }
            Input::Tick { .. } => {}
            Input::Completed {
                node_id,
                attempt,
                outputs,
                target,
                ..
            } => {
                if self.run.nodes.get(&node_id).is_some_and(|n| {
                    n.attempt == attempt && n.state == NodeState::Succeeded && n.outputs == outputs
                }) && target.as_ref().is_none_or(|t| t == &self.run.target)
                {
                    return Ok(effects);
                }
                self.active_attempt(&node_id, attempt)?;
                let definition = self.node_definition(&node_id)?.clone();
                if definition.kind == NodeKind::Approval {
                    return Err(EngineError("承認工程は人の承認入力でのみ完了します".into()));
                }
                let next_target = target.unwrap_or_else(|| self.run.target.clone());
                if !next_target.is_valid()
                    || next_target.base_sha != self.run.target.base_sha
                    || (definition.kind != NodeKind::Implement && next_target != self.run.target)
                {
                    return Err(EngineError("この工程は検証対象を変更できません".into()));
                }
                let digest = next_target.digest();
                if outputs.is_empty()
                    || outputs.len() > 64
                    || outputs.iter().any(|a| {
                        a.id.is_empty()
                            || a.id.len() > 256
                            || a.kind != definition.kind.output_kind()
                            || a.target_digest != digest
                            || a.summary.len() > 65_536
                            || (a.kind == ArtifactKind::Evidence
                                && a.data.get("passed").and_then(serde_json::Value::as_bool)
                                    != Some(true))
                    })
                {
                    return Err(EngineError(
                        "成果物の型・検証対象・合格の証跡が一致しません".into(),
                    ));
                }
                if next_target != self.run.target {
                    self.invalidate_descendants(&node_id, now)?;
                    self.run.target = next_target;
                    self.record(
                        now,
                        Some(&node_id),
                        "target_changed",
                        "新しい変更対象を固定し、以前の検証と承認を失効しました",
                    );
                }
                let node = self.run.nodes.get_mut(&node_id).expect("validated node");
                node.state = NodeState::Succeeded;
                node.finished_at = Some(now);
                node.message = outputs[0].summary.clone();
                node.outputs = outputs;
                node.retry_at = None;
                node.input_sequence = None;
                self.record(
                    now,
                    Some(&node_id),
                    "succeeded",
                    "成果物と対象を確認しました",
                );
            }
            Input::Failed {
                node_id,
                attempt,
                message,
                retryable,
                outputs,
                ..
            } => {
                self.active_attempt(&node_id, attempt)?;
                let definition = self.node_definition(&node_id)?.clone();
                validate_failure_outputs(definition.kind, &self.run.target, &outputs)?;
                let node = self.run.nodes.get_mut(&node_id).expect("validated node");
                node.state = NodeState::Failed;
                node.finished_at = Some(now);
                node.message = message.clone();
                node.retryable = retryable;
                node.outputs = outputs;
                node.input_sequence = None;
                self.record(now, Some(&node_id), "failed", &message);
                if retryable
                    && definition.kind != NodeKind::PullRequest
                    && attempt <= u32::from(definition.max_retries)
                    && !self.run.cancel_requested
                    && !self.run.pause_requested
                {
                    self.reset_node(&node_id);
                    let node = self.run.nodes.get_mut(&node_id).expect("validated node");
                    node.retry_at =
                        Some(now.saturating_add(1_000_u64.saturating_mul(1_u64 << attempt.min(5))));
                    node.message = "一時的な問題を再試行します".into();
                    self.record(
                        now,
                        Some(&node_id),
                        "retry_scheduled",
                        "上限内で再試行を予約しました",
                    );
                }
            }
            Input::Uncertain {
                node_id,
                attempt,
                message,
                external_ref,
                ..
            } => {
                effects.extend(self.observe(
                    &node_id,
                    attempt,
                    NodeObservation {
                        state: NodeState::Uncertain,
                        message,
                        external_ref,
                        input_sequence: None,
                    },
                    now,
                )?);
            }
            Input::Waiting {
                node_id,
                attempt,
                message,
                external_ref,
                input_sequence,
                ..
            } => {
                effects.extend(self.observe(
                    &node_id,
                    attempt,
                    NodeObservation {
                        state: NodeState::Waiting,
                        message,
                        external_ref,
                        input_sequence,
                    },
                    now,
                )?);
            }
            Input::ObservedRunning {
                node_id,
                attempt,
                message,
                external_ref,
                ..
            } => {
                effects.extend(self.observe(
                    &node_id,
                    attempt,
                    NodeObservation {
                        state: NodeState::Running,
                        message,
                        external_ref,
                        input_sequence: None,
                    },
                    now,
                )?);
            }
            Input::Reconcile { node_id, .. } => {
                let node = self
                    .run
                    .nodes
                    .get(&node_id)
                    .ok_or_else(|| EngineError("工程が見つかりません".into()))?;
                if !node.state.active()
                    || self.node_definition(&node_id)?.kind == NodeKind::Approval
                {
                    return Err(EngineError("結果を照合する実行中の工程がありません".into()));
                }
                effects.push(Effect::Reconcile {
                    node_id: node_id.clone(),
                    attempt: node.attempt,
                    request_id: node.request_id.clone(),
                    external_ref: node.external_ref.clone(),
                });
                let node = self.run.nodes.get_mut(&node_id).expect("validated node");
                node.state = NodeState::Uncertain;
                node.input_sequence = None;
                node.message = "保存済みの要求と外部の状態を照合しています".into();
                self.record(
                    now,
                    Some(&node_id),
                    "reconciling",
                    "以前の操作を再送せず結果を確認します",
                );
            }
            Input::Approve {
                node_id,
                digest,
                current_target,
                ..
            } => {
                let definition = self.node_definition(&node_id)?;
                let node = self
                    .run
                    .nodes
                    .get(&node_id)
                    .expect("definition has runtime state");
                if definition.kind != NodeKind::Approval
                    || node.state != NodeState::Waiting
                    || self.run.cancel_requested
                    || self.run.pause_requested
                {
                    return Err(EngineError("現在承認できる工程ではありません".into()));
                }
                if current_target != self.run.target {
                    let node = self.run.nodes.get_mut(&node_id).expect("validated node");
                    node.state = NodeState::Failed;
                    node.approval_digest = None;
                    node.message =
                        "検証後に対象が変わりました。新しい対象で再検証してください".into();
                    self.record(
                        now,
                        Some(&node_id),
                        "approval_invalidated",
                        "対象の変化を検出したため承認しませんでした",
                    );
                } else {
                    let publication = self.publication(&node_id)?;
                    let expected = self.approval_digest(&node_id)?;
                    if digest != expected
                        || node.approval_digest.as_deref() != Some(expected.as_str())
                    {
                        return Err(EngineError(
                            "表示後に承認内容が変わりました。最新の内容を確認してください".into(),
                        ));
                    }
                    let mut artifact = Artifact::new(
                        ArtifactKind::Approval,
                        self.run.target.digest(),
                        "変更と検証結果を承認しました",
                    );
                    artifact.data = serde_json::json!({
                        "approval_digest": expected,
                        "approved_at": now,
                        "publication": publication,
                    });
                    let node = self.run.nodes.get_mut(&node_id).expect("validated node");
                    node.outputs = vec![artifact];
                    node.state = NodeState::Succeeded;
                    node.finished_at = Some(now);
                    node.message = "対象に一致する承認を記録しました".into();
                    self.record(
                        now,
                        Some(&node_id),
                        "approved",
                        "現在の対象と証跡に対する承認を記録しました",
                    );
                }
            }
            Input::Pause { .. } => {
                if matches!(self.run.state, NodeState::Succeeded | NodeState::Canceled) {
                    return Err(EngineError("終了した実行は一時停止できません".into()));
                }
                self.run.pause_requested = true;
                self.record(
                    now,
                    None,
                    "paused",
                    "新しい工程の開始を停止しました。実行中の処理は継続します",
                );
            }
            Input::Resume { .. } => {
                if self.run.cancel_requested
                    || matches!(self.run.state, NodeState::Succeeded | NodeState::Canceled)
                {
                    return Err(EngineError(
                        "終了または中断要求済みの実行は再開できません".into(),
                    ));
                }
                if now.saturating_sub(self.run.started_at)
                    >= self.run.definition.limits.max_duration_ms
                    || self
                        .run
                        .definition
                        .limits
                        .max_cost_microusd
                        .zip(self.run.cost_microusd)
                        .is_some_and(|(limit, cost)| cost >= limit)
                {
                    return Err(EngineError(
                        "実行の上限に達しています。成果を確認し、新しい実行を作成してください"
                            .into(),
                    ));
                }
                self.run.pause_requested = false;
                self.run.limit_reason = None;
                self.record(now, None, "resumed", "保存した工程から再開します");
                for node in self
                    .run
                    .nodes
                    .values()
                    .filter(|n| n.state == NodeState::Uncertain)
                {
                    effects.push(Effect::Reconcile {
                        node_id: node.node_id.clone(),
                        attempt: node.attempt,
                        request_id: node.request_id.clone(),
                        external_ref: node.external_ref.clone(),
                    });
                }
            }
            Input::Retry { node_id, .. } => {
                self.retry(&node_id, None, now)?;
            }
            Input::RetryAtTarget {
                node_id,
                current_target,
                ..
            } => {
                self.retry(&node_id, Some(current_target), now)?;
            }
            Input::Repair { .. } => self.repair(now)?,
            Input::Cancel { .. } => {
                if self.run.state == NodeState::Succeeded {
                    return Err(EngineError("終了済みの操作は中断で取り消せません".into()));
                }
                if !self.run.cancel_requested {
                    self.run.cancel_requested = true;
                    self.record(
                        now,
                        None,
                        "cancel_requested",
                        "実行中の処理へ中断を要求しました",
                    );
                }
                self.cancel_workers(now, &mut effects);
            }
            Input::Canceled {
                node_id, attempt, ..
            } => {
                self.active_attempt(&node_id, attempt)?;
                let node = self.run.nodes.get_mut(&node_id).expect("validated node");
                if !node.cancel_requested {
                    return Err(EngineError("この工程には中断を要求していません".into()));
                }
                node.state = NodeState::Canceled;
                node.input_sequence = None;
                node.finished_at = Some(now);
                node.message = "処理の停止を確認しました".into();
                self.record(
                    now,
                    Some(&node_id),
                    "canceled",
                    "実処理の停止を確認しました",
                );
            }
            Input::CostObserved { cost_microusd, .. } => {
                if self.run.cost_microusd == Some(cost_microusd) {
                    return Ok(effects);
                }
                if self
                    .run
                    .cost_microusd
                    .is_some_and(|cost| cost > cost_microusd)
                {
                    return Err(EngineError("累積費用を以前の値より小さくできません".into()));
                }
                self.run.cost_microusd = Some(cost_microusd);
                if !self.run.pause_requested {
                    self.run.limit_reason = None;
                }
                self.record(
                    now,
                    None,
                    "cost_observed",
                    "取得できた累積費用を更新しました",
                );
            }
        }
        Ok(effects)
    }

    fn observe(
        &mut self,
        id: &str,
        attempt: u32,
        observation: NodeObservation,
        now: Millis,
    ) -> Result<Vec<Effect>, EngineError> {
        let NodeObservation {
            state,
            message,
            external_ref,
            input_sequence,
        } = observation;
        self.active_attempt(id, attempt)?;
        if self.node_definition(id)?.kind == NodeKind::Approval {
            return Err(EngineError(
                "承認工程の状態をエージェントから変更できません".into(),
            ));
        }
        let node = self.run.nodes.get_mut(id).expect("validated node");
        let external_ref = external_ref.or_else(|| node.external_ref.clone());
        if node.state == state
            && node.message == message
            && node.external_ref == external_ref
            && node.input_sequence == input_sequence
        {
            return Ok(Vec::new());
        }
        node.state = state;
        node.message = message.clone();
        node.external_ref = external_ref;
        node.input_sequence = input_sequence;
        self.record(now, Some(id), "observed", &message);
        Ok(Vec::new())
    }

    fn ensure_no_workers(&self) -> Result<(), EngineError> {
        if self.run.nodes.values().any(|n| {
            n.state.active()
                && self
                    .run
                    .definition
                    .node(&n.node_id)
                    .is_some_and(|d| d.kind != NodeKind::Approval)
        }) {
            return Err(EngineError(
                "実行中・結果不明の工程が残っています。終了または照合を待ってください".into(),
            ));
        }
        Ok(())
    }

    fn retry(
        &mut self,
        id: &str,
        current_target: Option<TargetSnapshot>,
        now: Millis,
    ) -> Result<(), EngineError> {
        self.ensure_no_workers()?;
        let node = self
            .run
            .nodes
            .get(id)
            .ok_or_else(|| EngineError("工程が見つかりません".into()))?;
        let kind = self.node_definition(id)?.kind;
        if node.state != NodeState::Failed || self.run.cancel_requested {
            return Err(EngineError(
                "失敗が確定した工程だけ再試行できます。不明な結果は先に照合してください".into(),
            ));
        }
        if kind == NodeKind::PullRequest && !node.retryable {
            return Err(EngineError(
                "PR操作の未実行を確認してから再試行してください".into(),
            ));
        }
        if let Some(target) = &current_target
            && (kind != NodeKind::Implement
                || !target.is_valid()
                || target.base_sha != self.run.target.base_sha)
        {
            return Err(EngineError(
                "途中の変更を引き継げるのは同じベースの実装工程だけです".into(),
            ));
        }
        self.invalidate_descendants(id, now)?;
        self.reset_node(id);
        if let Some(target) = current_target {
            self.run.target = target;
            self.record(
                now,
                Some(id),
                "retry_target",
                "停止した実装の途中変更を確認し、次の試行へ引き継ぎました",
            );
        }
        self.record(
            now,
            Some(id),
            "retry",
            "失敗した工程と影響する工程を再実行します",
        );
        Ok(())
    }

    fn reset_node(&mut self, id: &str) {
        let target_digest = self.run.target.digest();
        let node = self
            .run
            .nodes
            .get_mut(id)
            .expect("definition has runtime state");
        if node.attempt > 0 {
            node.history.push(AttemptRecord {
                attempt: node.attempt,
                state: node.state,
                message: node.message.clone(),
                outputs: node.outputs.clone(),
                target_digest: node
                    .outputs
                    .first()
                    .map_or(target_digest, |artifact| artifact.target_digest.clone()),
            });
        }
        node.state = NodeState::Pending;
        node.started_at = None;
        node.finished_at = None;
        node.message.clear();
        node.outputs.clear();
        node.external_ref = None;
        node.approval_digest = None;
        node.retry_at = None;
        node.retryable = false;
        node.cancel_requested = false;
        node.timeout_reconciled_at = None;
        node.input_sequence = None;
    }

    fn invalidate_descendants(&mut self, id: &str, now: Millis) -> Result<(), EngineError> {
        let descendants = self.run.definition.descendants(id);
        if descendants.iter().any(|id| {
            self.run.nodes.get(id).is_some_and(|n| n.state.active())
                && self
                    .run
                    .definition
                    .node(id)
                    .is_some_and(|n| n.kind != NodeKind::Approval)
        }) {
            return Err(EngineError("変更に依存する工程がまだ動作しています".into()));
        }
        for descendant in descendants {
            if self.run.nodes[&descendant].state != NodeState::Pending {
                self.reset_node(&descendant);
                self.record(
                    now,
                    Some(&descendant),
                    "invalidated",
                    "入力が変わるため以前の成果と承認を失効しました",
                );
            }
        }
        Ok(())
    }

    fn repair(&mut self, now: Millis) -> Result<(), EngineError> {
        self.ensure_no_workers()?;
        if self.run.cancel_requested
            || self.run.repair_attempts >= self.run.definition.limits.max_repair_attempts
        {
            return Err(EngineError(
                "自動修正の上限に達しました。成果と未解決事項を確認してください".into(),
            ));
        }
        let failed: Vec<_> = self
            .run
            .definition
            .nodes
            .iter()
            .filter(|d| {
                matches!(d.kind, NodeKind::Review | NodeKind::Verify)
                    && self.run.nodes[&d.id].state == NodeState::Failed
            })
            .map(|d| d.id.clone())
            .collect();
        if failed.is_empty() {
            return Err(EngineError(
                "修正対象のレビューまたは検証の失敗がありません".into(),
            ));
        }
        self.run.repair_context = failed
            .iter()
            .flat_map(|id| {
                let node = &self.run.nodes[id];
                if !node.outputs.is_empty() {
                    return node.outputs.clone();
                }
                let mut artifact = Artifact::new(
                    ArtifactKind::Evidence,
                    self.run.target.digest(),
                    &node.message,
                );
                artifact.data =
                    serde_json::json!({"passed": false, "node_id": id, "attempt": node.attempt});
                vec![artifact]
            })
            .collect();
        let implementation = self
            .run
            .definition
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Implement)
            .expect("validated single writer")
            .id
            .clone();
        self.invalidate_descendants(&implementation, now)?;
        self.reset_node(&implementation);
        self.run.repair_attempts += 1;
        self.record(
            now,
            Some(&implementation),
            "repair",
            "失敗の根拠を引き継いで修正し、新しい対象を再検証します",
        );
        Ok(())
    }

    fn cancel_workers(&mut self, now: Millis, effects: &mut Vec<Effect>) {
        let ids: Vec<_> = self.run.nodes.keys().cloned().collect();
        for id in ids {
            let kind = self
                .run
                .definition
                .node(&id)
                .expect("validated definition")
                .kind;
            let node = self.run.nodes.get_mut(&id).expect("validated node");
            if node.state == NodeState::Pending
                || (kind == NodeKind::Approval && node.state == NodeState::Waiting)
            {
                node.state = NodeState::Canceled;
                node.finished_at = Some(now);
                node.message = "開始前に中断しました".into();
            } else if node.state.active() && !node.cancel_requested {
                node.cancel_requested = true;
                effects.push(Effect::Cancel {
                    node_id: id,
                    attempt: node.attempt,
                    request_id: node.request_id.clone(),
                });
            }
        }
    }

    fn advance(&mut self, now: Millis, effects: &mut Vec<Effect>) -> Result<(), EngineError> {
        if self.run.state == NodeState::Pending || self.run.state == NodeState::Succeeded {
            return Ok(());
        }
        if self.run.cancel_requested {
            self.run.state = if self
                .run
                .nodes
                .values()
                .any(|n| n.state == NodeState::Uncertain)
            {
                NodeState::Uncertain
            } else if self.run.nodes.values().any(|n| n.state.active()) {
                NodeState::Running
            } else {
                NodeState::Canceled
            };
            return Ok(());
        }
        let limits = self.run.definition.limits.clone();
        let reached = if now.saturating_sub(self.run.started_at) >= limits.max_duration_ms {
            Some("実行時間の上限に達しました".to_owned())
        } else if limits
            .max_cost_microusd
            .zip(self.run.cost_microusd)
            .is_some_and(|(limit, cost)| cost >= limit)
        {
            Some("取得した累積費用が停止基準に達しました".to_owned())
        } else {
            None
        };
        if let Some(reason) = reached
            && self.run.limit_reason.as_ref() != Some(&reason)
        {
            self.run.limit_reason = Some(reason.clone());
            self.run.pause_requested = true;
            self.record(now, None, "limit_reached", &reason);
            self.cancel_workers(now, effects);
        }
        if self.run.pause_requested {
            self.run.state = NodeState::Paused;
            return Ok(());
        }
        if limits.max_cost_microusd.is_some()
            && self.run.cost_microusd.is_none()
            && !limits.allow_unknown_cost
        {
            let reason = "費用を取得できないため、金額の制限がある実行を開始できません";
            if self.run.limit_reason.as_deref() != Some(reason) {
                self.run.limit_reason = Some(reason.into());
                self.record(now, None, "cost_unknown", reason);
            }
            self.run.state = NodeState::Waiting;
            return Ok(());
        }
        let timed_out: Vec<_> = self
            .run
            .definition
            .nodes
            .iter()
            .filter(|d| {
                let n = &self.run.nodes[&d.id];
                d.kind != NodeKind::Approval
                    && n.state == NodeState::Running
                    && !n.cancel_requested
                    && n.timeout_reconciled_at.is_none()
                    && n.started_at
                        .is_some_and(|start| now.saturating_sub(start) >= d.timeout_ms)
            })
            .map(|d| d.id.clone())
            .collect();
        for id in timed_out {
            let node = self.run.nodes.get_mut(&id).expect("validated node");
            node.state = NodeState::Uncertain;
            node.input_sequence = None;
            node.timeout_reconciled_at = Some(now);
            node.message = "応答の期限を過ぎました。処理結果を照合してください".into();
            effects.push(Effect::Reconcile {
                node_id: id.clone(),
                attempt: node.attempt,
                request_id: node.request_id.clone(),
                external_ref: node.external_ref.clone(),
            });
            self.record(
                now,
                Some(&id),
                "timeout",
                "期限超過を成功や未実行と推測せず、結果を照合します",
            );
        }
        let failed: Vec<_> = self
            .run
            .definition
            .nodes
            .iter()
            .filter(|d| self.run.nodes[&d.id].state == NodeState::Failed)
            .map(|d| d.kind)
            .collect();
        if !failed.is_empty()
            && failed
                .iter()
                .all(|k| matches!(k, NodeKind::Review | NodeKind::Verify))
            && self.ensure_no_workers().is_ok()
            && self.run.repair_attempts < limits.max_repair_attempts
        {
            self.repair(now)?;
        }
        if self
            .run
            .nodes
            .values()
            .any(|n| n.state == NodeState::Uncertain)
        {
            self.run.state = NodeState::Uncertain;
            return Ok(());
        }
        if self
            .run
            .nodes
            .values()
            .any(|n| n.state == NodeState::Failed)
        {
            self.run.state = NodeState::Failed;
            return Ok(());
        }
        let mut active = self.run.nodes.values().filter(|n| n.state.active()).count();
        let definitions = self.run.definition.nodes.clone();
        for definition in definitions {
            let id = &definition.id;
            if self.run.nodes[id].state != NodeState::Pending
                || self.run.nodes[id].retry_at.is_some_and(|retry| now < retry)
                // Every planning stage reads the initial snapshot. Finish all
                // of them before the single writer can replace the run target,
                // including plans that are not its direct input.
                || (definition.kind == NodeKind::Implement
                    && self.run.definition.nodes.iter().any(|node| {
                        node.kind == NodeKind::Plan
                            && self.run.nodes[&node.id].state != NodeState::Succeeded
                    }))
                || !self
                    .run
                    .definition
                    .edges
                    .iter()
                    .filter(|e| &e.to == id)
                    .all(|e| self.run.nodes[&e.from].state == NodeState::Succeeded)
            {
                continue;
            }
            if active >= usize::from(limits.max_parallel_nodes) {
                break;
            }
            let mut inputs = self.inputs(id);
            if definition.kind == NodeKind::Implement {
                inputs.extend(self.run.repair_context.clone());
            }
            if definition.kind == NodeKind::Approval {
                if inputs
                    .iter()
                    .any(|a| a.target_digest != self.run.target.digest())
                {
                    return Err(EngineError(
                        "検証対象が一致しないため承認へ進めません".into(),
                    ));
                }
                let approval_digest = self.approval_digest(id)?;
                let node = self.run.nodes.get_mut(id).expect("validated node");
                node.attempt += 1;
                node.request_id = format!("{}-{}-{}", self.run.id, id, node.attempt);
                node.state = NodeState::Waiting;
                node.started_at = Some(now);
                node.approval_digest = Some(approval_digest);
                node.message = "差分と検証結果を確認し、PR作成を承認してください".into();
                self.record(
                    now,
                    Some(id),
                    "approval_waiting",
                    "次の外部操作への承認を待っています",
                );
            } else {
                let node = self.run.nodes.get_mut(id).expect("validated node");
                node.attempt += 1;
                node.request_id = format!("{}-{}-{}", self.run.id, id, node.attempt);
                node.state = NodeState::Running;
                node.started_at = Some(now);
                node.retry_at = None;
                node.message = "実行を開始しています".into();
                effects.push(Effect::Execute {
                    node: Box::new(definition.clone()),
                    attempt: node.attempt,
                    request_id: node.request_id.clone(),
                    inputs,
                    target: self.run.target.clone(),
                });
                self.record(now, Some(id), "scheduled", "要求を記録してから実行します");
            }
            active += 1;
        }
        self.run.state = if self
            .run
            .nodes
            .values()
            .all(|n| n.state == NodeState::Succeeded)
        {
            NodeState::Succeeded
        } else if self
            .run
            .nodes
            .values()
            .any(|n| n.state == NodeState::Running)
        {
            NodeState::Running
        } else if self
            .run
            .nodes
            .values()
            .any(|n| n.state == NodeState::Waiting)
        {
            NodeState::Waiting
        } else if self
            .run
            .nodes
            .values()
            .any(|n| n.state == NodeState::Canceled)
        {
            NodeState::Canceled
        } else {
            NodeState::Running
        };
        Ok(())
    }
}

fn validate_failure_outputs(
    kind: NodeKind,
    target: &TargetSnapshot,
    outputs: &[Artifact],
) -> Result<(), EngineError> {
    let digest = target.digest();
    if outputs.len() > 64
        || outputs.iter().any(|artifact| {
            artifact.id.is_empty()
                || artifact.id.len() > 256
                || artifact.kind != kind.output_kind()
                || artifact.summary.len() > 65_536
                || artifact
                    .data
                    .get("passed")
                    .and_then(serde_json::Value::as_bool)
                    != Some(false)
                || if kind == NodeKind::Implement {
                    // A failed writer can leave partial changes. Preserve its
                    // report without adopting that target or approving it.
                    artifact.target_digest.len() != 64
                        || !artifact
                            .target_digest
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit())
                        || outputs
                            .first()
                            .is_some_and(|first| first.target_digest != artifact.target_digest)
                } else {
                    artifact.target_digest != digest
                }
        })
    {
        return Err(EngineError(
            "失敗した成果物の型・検証対象・不合格の記録が一致しません".into(),
        ));
    }
    Ok(())
}

pub fn validate_run(run: &Run) -> Result<(), EngineError> {
    if let Some(origin) = &run.origin_task {
        origin
            .validate(&run.repository, &run.objective)
            .map_err(EngineError)?;
    }
    if run.schema_version != SCHEMA_VERSION
        || !valid_id(&run.id)
        || !run.repository.is_absolute()
        || run.definition_digest != run.definition.digest()
        || !run.definition.validate().is_empty()
        || !run.target.is_valid()
        || run.nodes.len() != run.definition.nodes.len()
        || run.events.len() > MAX_EVENTS
        || run.events.last().map_or(0, |e| e.seq) != run.seq
        || run.repair_attempts > run.definition.limits.max_repair_attempts
    {
        return Err(EngineError(
            "保存された実行の形式・定義・履歴が一致しません".into(),
        ));
    }
    if run
        .events
        .iter()
        .enumerate()
        .any(|(index, event)| event.seq != index as u64 + 1)
    {
        return Err(EngineError("保存された実行履歴に欠落があります".into()));
    }
    if run.state == NodeState::Succeeded
        && !run
            .nodes
            .values()
            .all(|node| node.state == NodeState::Succeeded)
    {
        return Err(EngineError(
            "実行全体の完了と工程の状態が一致しません".into(),
        ));
    }
    let target_digest = run.target.digest();
    for definition in &run.definition.nodes {
        let Some(node) = run.nodes.get(&definition.id) else {
            return Err(EngineError("工程の実行状態が欠落しています".into()));
        };
        if node.node_id != definition.id
            || (node.state.active() && node.attempt == 0)
            || (node.input_sequence.is_some()
                && (node.state != NodeState::Waiting || !definition.kind.uses_agent()))
            || (node.attempt > 0
                && node.request_id != format!("{}-{}-{}", run.id, node.node_id, node.attempt))
        {
            return Err(EngineError("工程の要求ID・試行が一致しません".into()));
        }
        if node.state == NodeState::Succeeded
            && (node.outputs.is_empty()
                || node
                    .outputs
                    .iter()
                    .any(|a| a.kind != definition.kind.output_kind()))
        {
            return Err(EngineError("完了した工程の成果物がありません".into()));
        }
        if node.state == NodeState::Failed {
            validate_failure_outputs(definition.kind, &run.target, &node.outputs)?;
        }
        if (node.state.active() || node.state == NodeState::Succeeded)
            && run
                .definition
                .edges
                .iter()
                .filter(|edge| edge.to == node.node_id)
                .any(|edge| {
                    run.nodes
                        .get(&edge.from)
                        .is_none_or(|source| source.state != NodeState::Succeeded)
                })
        {
            return Err(EngineError(
                "開始条件を満たしていない工程が実行されています".into(),
            ));
        }
        if node.state == NodeState::Succeeded
            && matches!(
                definition.kind,
                NodeKind::Implement
                    | NodeKind::Review
                    | NodeKind::Verify
                    | NodeKind::Approval
                    | NodeKind::PullRequest
            )
            && node.outputs.iter().any(|artifact| {
                artifact.target_digest != target_digest
                    || (artifact.kind == ArtifactKind::Evidence
                        && artifact
                            .data
                            .get("passed")
                            .and_then(serde_json::Value::as_bool)
                            != Some(true))
            })
        {
            return Err(EngineError(
                "保存済み成果物の対象・合格証跡が一致しません".into(),
            ));
        }
        if definition.kind == NodeKind::Approval
            && matches!(node.state, NodeState::Waiting | NodeState::Succeeded)
        {
            let engine = Engine { run: run.clone() };
            let expected = engine.approval_digest(&node.node_id)?;
            if node.approval_digest.as_deref() != Some(expected.as_str()) {
                return Err(EngineError(
                    "承認内容と保存された対象・証跡が一致しません".into(),
                ));
            }
            if node.state == NodeState::Succeeded {
                let publication = engine.publication(&node.node_id)?;
                if node.outputs.iter().any(|artifact| {
                    artifact
                        .data
                        .get("approval_digest")
                        .and_then(serde_json::Value::as_str)
                        != Some(expected.as_str())
                        || artifact.data.get("publication") != Some(&publication)
                }) {
                    return Err(EngineError("承認した公開操作の内容が一致しません".into()));
                }
            }
        }
    }
    Ok(())
}

/// Offline fixture only: the same transitions as a live run, without effects
/// being sent to an adapter. The native binary opts into these with --demo.
pub fn demo_waiting(now: Millis) -> Run {
    demo(now, false)
}

pub fn demo_failed(now: Millis) -> Run {
    demo(now, true)
}

fn demo_publication(target: &TargetSnapshot) -> serde_json::Value {
    serde_json::json!({
        "repository": "simota/Noa",
        "push_url": "https://github.com/simota/Noa.git",
        "branch": "workflow/demo-keyboard",
        "base": "main",
        "title": "Improve workflow keyboard navigation (demo)",
        "body": "Example only: preserve focus and add keyboard navigation.",
        "marker": "<!-- noa-workflow:demo -->",
        "target": target,
    })
}

fn demo(now: Millis, fail: bool) -> Run {
    let mut definition = WorkflowDefinition::standard();
    if fail {
        definition.limits.max_repair_attempts = 0;
    }
    let target = TargetSnapshot {
        base_sha: "9d4fa4a414d804c483b8afe30ae0c85161659f18".into(),
        head_sha: "4b7c2e1d0a9f8365b2d8e6a1c0f497352e6b8a90".into(),
        dirty_digest: digest_bytes(b"example-only"),
    };
    let mut engine = Engine::new(
        "demo-workflow".into(),
        definition,
        PathBuf::from("/example/Noa"),
        "ワークフローの操作をキーボードから使えるようにする（デモ）".into(),
        target,
        now,
    )
    .expect("valid demo definition");
    let mut queue: std::collections::VecDeque<_> = engine
        .handle(Input::Start { now })
        .expect("demo start")
        .into();
    while let Some(effect) = queue.pop_front() {
        let Effect::Execute {
            node,
            attempt,
            target,
            ..
        } = effect
        else {
            continue;
        };
        let input = if fail && node.kind == NodeKind::Verify {
            Input::Failed { node_id: node.id, attempt, message: "必須条件：狭い画面でフォーカスが維持されません。入力を残して戻れるように修正してください（デモ）".into(), retryable: false, outputs: Vec::new(), now }
        } else {
            let summary = match node.kind {
                NodeKind::Trigger => "目的と受け入れ条件を保存しました（デモ）",
                NodeKind::Plan => "影響範囲と検証計画をまとめました（デモ）",
                NodeKind::Implement => "キーボード操作とフォーカス保持を変更しました（デモ）",
                NodeKind::Review => "必須の指摘を解消しました（デモ）",
                NodeKind::Verify => "受け入れ条件を検証しました（デモ）",
                _ => "工程が完了しました（デモ）",
            };
            let mut artifact = Artifact::new(node.kind.output_kind(), target.digest(), summary);
            if artifact.kind == ArtifactKind::Evidence {
                artifact.data = serde_json::json!({"passed": true, "demo": true});
            }
            if artifact.kind == ArtifactKind::Change {
                artifact.data = serde_json::json!({"publication": demo_publication(&target)});
            }
            Input::Completed {
                node_id: node.id,
                attempt,
                outputs: vec![artifact],
                target: None,
                now,
            }
        };
        queue.extend(engine.handle(input).expect("valid demo transition"));
    }
    engine.into_run()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Engine {
        Engine::new(
            "run-1".into(),
            WorkflowDefinition::standard(),
            PathBuf::from("/example/repo"),
            "受け入れ条件を満たす変更を作成する".into(),
            TargetSnapshot {
                base_sha: "base".into(),
                head_sha: "head".into(),
                dirty_digest: "clean".into(),
            },
            0,
        )
        .unwrap()
    }

    fn completion(engine: &Engine, id: &str, now: Millis) -> Input {
        let definition = engine.run.definition.node(id).unwrap();
        let mut artifact = Artifact::new(
            definition.kind.output_kind(),
            engine.run.target.digest(),
            format!("{id} completed"),
        );
        if artifact.kind == ArtifactKind::Evidence {
            artifact.data = serde_json::json!({"passed": true});
        }
        if artifact.kind == ArtifactKind::Change {
            artifact.data =
                serde_json::json!({"publication": demo_publication(&engine.run.target)});
        }
        Input::Completed {
            node_id: id.into(),
            attempt: engine.run.nodes[id].attempt,
            outputs: vec![artifact],
            target: None,
            now,
        }
    }

    fn to_review(engine: &mut Engine) {
        engine.handle(Input::Start { now: 1 }).unwrap();
        for id in ["task", "plan", "implement"] {
            let input = completion(engine, id, 2);
            engine.handle(input).unwrap();
        }
        assert_eq!(engine.run.nodes["review"].state, NodeState::Running);
        assert_eq!(engine.run.nodes["verify"].state, NodeState::Running);
    }

    #[test]
    fn both_reports_and_matching_human_approval_are_required() {
        let mut engine = engine();
        to_review(&mut engine);
        let input = completion(&engine, "review", 3);
        engine.handle(input).unwrap();
        assert_eq!(engine.run.nodes["approval"].state, NodeState::Pending);
        let input = completion(&engine, "verify", 4);
        engine.handle(input).unwrap();
        assert_eq!(engine.run.nodes["approval"].state, NodeState::Waiting);
        assert_eq!(engine.run.nodes["pr"].state, NodeState::Pending);
        let stale = engine.handle(Input::Approve {
            node_id: "approval".into(),
            digest: "stale".into(),
            current_target: engine.run.target.clone(),
            now: 5,
        });
        assert!(stale.is_err());
        let digest = engine.approval_digest("approval").unwrap();
        let effects = engine
            .handle(Input::Approve {
                node_id: "approval".into(),
                digest,
                current_target: engine.run.target.clone(),
                now: 6,
            })
            .unwrap();
        assert!(effects.iter().any(
            |e| matches!(e, Effect::Execute { node, .. } if node.kind == NodeKind::PullRequest)
        ));
    }

    #[test]
    fn changing_dirty_bytes_invalidates_the_gate_without_publishing() {
        let mut engine = Engine::restore(demo_waiting(0)).unwrap();
        let mut current = engine.run.target.clone();
        current.dirty_digest = "changed-after-verification".into();
        let digest = engine.approval_digest("approval").unwrap();
        let effects = engine
            .handle(Input::Approve {
                node_id: "approval".into(),
                digest,
                current_target: current,
                now: 1,
            })
            .unwrap();
        assert!(effects.is_empty());
        assert_eq!(engine.run.nodes["approval"].state, NodeState::Failed);
        assert!(engine.run.nodes["approval"].approval_digest.is_none());
        assert_eq!(engine.run.nodes["pr"].state, NodeState::Pending);
    }

    #[test]
    fn duplicate_completion_is_idempotent_and_stale_attempt_is_rejected() {
        let mut engine = engine();
        engine.handle(Input::Start { now: 1 }).unwrap();
        let completed = completion(&engine, "task", 2);
        engine.handle(completed.clone()).unwrap();
        let previous = engine.run.clone();
        assert!(engine.handle(completed).unwrap().is_empty());
        assert_eq!(engine.run, previous);
        let stale = completion(&engine, "plan", 3);
        engine
            .handle(Input::Failed {
                node_id: "plan".into(),
                attempt: 1,
                message: "temporary disconnect".into(),
                retryable: true,
                outputs: Vec::new(),
                now: 3,
            })
            .unwrap();
        engine.handle(Input::Tick { now: 3000 }).unwrap();
        assert_eq!(engine.run.nodes["plan"].attempt, 2);
        let before = engine.run.clone();
        assert!(engine.handle(stale).is_err());
        assert_eq!(engine.run, before);
    }

    #[test]
    fn an_unknown_result_is_reconciled_not_replayed() {
        let mut engine = engine();
        engine.handle(Input::Start { now: 1 }).unwrap();
        engine
            .handle(Input::Uncertain {
                node_id: "task".into(),
                attempt: 1,
                message: "connection lost after request".into(),
                external_ref: Some("pane-1".into()),
                now: 2,
            })
            .unwrap();
        assert!(engine.handle(Input::Tick { now: 3 }).unwrap().is_empty());
        assert!(
            engine
                .handle(Input::Retry {
                    node_id: "task".into(),
                    now: 4
                })
                .is_err()
        );
        let effects = engine
            .handle(Input::Reconcile {
                node_id: "task".into(),
                now: 5,
            })
            .unwrap();
        assert!(
            matches!(effects.as_slice(), [Effect::Reconcile { request_id, .. }] if request_id == "run-1-task-1")
        );
    }

    #[test]
    fn repeated_observation_preserves_known_external_identity_without_new_events() {
        let mut engine = engine();
        engine.handle(Input::Start { now: 1 }).unwrap();
        engine
            .handle(Input::Uncertain {
                node_id: "task".into(),
                attempt: 1,
                message: "connection unavailable".into(),
                external_ref: Some("pane-1".into()),
                now: 2,
            })
            .unwrap();
        let before = engine.run.clone();
        engine
            .handle(Input::Uncertain {
                node_id: "task".into(),
                attempt: 1,
                message: "connection unavailable".into(),
                external_ref: None,
                now: 3,
            })
            .unwrap();
        assert_eq!(engine.run, before);
    }

    #[test]
    fn cancellation_waits_for_the_actual_worker_to_stop() {
        let mut engine = engine();
        engine.handle(Input::Start { now: 1 }).unwrap();
        let effects = engine.handle(Input::Cancel { now: 2 }).unwrap();
        assert!(
            matches!(effects.as_slice(), [Effect::Cancel { node_id, .. }] if node_id == "task")
        );
        assert_eq!(engine.run.nodes["task"].state, NodeState::Running);
        assert_ne!(engine.run.state, NodeState::Canceled);
        engine
            .handle(Input::Canceled {
                node_id: "task".into(),
                attempt: 1,
                now: 3,
            })
            .unwrap();
        assert_eq!(engine.run.state, NodeState::Canceled);
        assert!(engine.handle(Input::Resume { now: 4 }).is_err());
    }

    #[test]
    fn repair_is_bounded_and_carries_the_failure_to_the_writer() {
        let mut engine = engine();
        engine.run.definition.limits.max_repair_attempts = 1;
        engine.run.definition_digest = engine.run.definition.digest();
        to_review(&mut engine);
        engine
            .handle(Input::Failed {
                node_id: "review".into(),
                attempt: 1,
                message: "missing boundary case".into(),
                retryable: false,
                outputs: Vec::new(),
                now: 3,
            })
            .unwrap();
        let input = completion(&engine, "verify", 4);
        let effects = engine.handle(input).unwrap();
        assert_eq!(engine.run.repair_attempts, 1);
        assert_eq!(engine.run.nodes["implement"].attempt, 2);
        assert!(effects.iter().any(|effect| matches!(effect, Effect::Execute { node, inputs, .. } if node.kind == NodeKind::Implement && inputs.iter().any(|a| a.summary == "missing boundary case"))));
        assert_eq!(engine.run.nodes["review"].state, NodeState::Pending);
        assert_eq!(engine.run.nodes["verify"].history.len(), 1);
        let input = completion(&engine, "implement", 5);
        engine.handle(input).unwrap();
        engine
            .handle(Input::Failed {
                node_id: "review".into(),
                attempt: 2,
                message: "still failing".into(),
                retryable: false,
                outputs: Vec::new(),
                now: 6,
            })
            .unwrap();
        let input = completion(&engine, "verify", 7);
        engine.handle(input).unwrap();
        assert_eq!(engine.run.state, NodeState::Failed);
        assert_eq!(engine.run.repair_attempts, 1);
        assert!(engine.handle(Input::Repair { now: 8 }).is_err());
    }

    #[test]
    fn failed_reports_keep_their_evidence_in_history_and_repair_inputs() {
        let mut engine = engine();
        to_review(&mut engine);
        let mut findings = Artifact::new(
            ArtifactKind::Evidence,
            engine.run.target.digest(),
            "See the report for concrete blocking findings",
        );
        findings.path = Some(PathBuf::from("/results/review/result.json"));
        findings.data = serde_json::json!({
            "passed": false,
            "report_sha256": "report-hash",
            "evidence": [{"path": "/results/review/review.md", "sha256": "findings-hash"}],
        });
        engine
            .handle(Input::Failed {
                node_id: "review".into(),
                attempt: 1,
                message: findings.summary.clone(),
                retryable: false,
                outputs: vec![findings.clone()],
                now: 3,
            })
            .unwrap();
        assert_eq!(engine.run.nodes["review"].outputs, vec![findings.clone()]);
        assert_eq!(engine.run.nodes["approval"].state, NodeState::Pending);
        Engine::restore(engine.run.clone()).unwrap();
        let effects = engine.handle(completion(&engine, "verify", 4)).unwrap();
        assert_eq!(engine.run.repair_context, vec![findings.clone()]);
        assert_eq!(
            engine.run.nodes["review"].history.last().unwrap().outputs,
            vec![findings.clone()]
        );
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::Execute { node, inputs, .. }
                if node.kind == NodeKind::Implement && inputs.contains(&findings)
        )));
        assert!(engine.run.nodes["review"].outputs.is_empty());
        assert_eq!(engine.run.nodes["approval"].state, NodeState::Pending);
    }

    #[test]
    fn failed_evidence_must_be_nonpassing_and_for_the_current_target() {
        let mut engine = engine();
        engine.run.definition.limits.max_repair_attempts = 0;
        engine.run.definition_digest = engine.run.definition.digest();
        to_review(&mut engine);
        let mut findings = Artifact::new(
            ArtifactKind::Evidence,
            engine.run.target.digest(),
            "blocking findings",
        );
        findings.data = serde_json::json!({"passed": false});
        for invalid in [
            Artifact {
                target_digest: "another-target".into(),
                ..findings.clone()
            },
            Artifact {
                kind: ArtifactKind::Plan,
                ..findings.clone()
            },
            Artifact {
                data: serde_json::json!({"passed": true}),
                ..findings.clone()
            },
            Artifact {
                data: serde_json::Value::Null,
                ..findings.clone()
            },
        ] {
            let before = engine.run.clone();
            assert!(
                engine
                    .handle(Input::Failed {
                        node_id: "review".into(),
                        attempt: 1,
                        message: "rejected report".into(),
                        retryable: false,
                        outputs: vec![invalid],
                        now: 3,
                    })
                    .is_err()
            );
            assert_eq!(engine.run, before);
        }
        engine
            .handle(Input::Failed {
                node_id: "review".into(),
                attempt: 1,
                message: findings.summary.clone(),
                retryable: false,
                outputs: vec![findings],
                now: 4,
            })
            .unwrap();
        engine.handle(completion(&engine, "verify", 5)).unwrap();
        assert_eq!(engine.run.state, NodeState::Failed);
        assert_eq!(engine.run.nodes["approval"].state, NodeState::Pending);
        assert_eq!(engine.run.nodes["pr"].state, NodeState::Pending);
        let mut corrupt = engine.run.clone();
        corrupt.nodes.get_mut("review").unwrap().outputs[0].data["passed"] = true.into();
        assert!(Engine::restore(corrupt).is_err());
    }

    #[test]
    fn a_failed_writers_report_does_not_adopt_its_partial_target() {
        let mut engine = engine();
        engine.handle(Input::Start { now: 1 }).unwrap();
        for id in ["task", "plan"] {
            engine.handle(completion(&engine, id, 2)).unwrap();
        }
        let original = engine.run.target.clone();
        let mut partial = original.clone();
        partial.dirty_digest = "partial-changes".into();
        let mut report = Artifact::new(ArtifactKind::Change, partial.digest(), "incomplete work");
        report.data = serde_json::json!({"passed": false});
        let inconsistent = Artifact {
            target_digest: original.digest(),
            ..report.clone()
        };
        assert!(
            engine
                .handle(Input::Failed {
                    node_id: "implement".into(),
                    attempt: 1,
                    message: report.summary.clone(),
                    retryable: false,
                    outputs: vec![report.clone(), inconsistent],
                    now: 3,
                })
                .is_err()
        );
        engine
            .handle(Input::Failed {
                node_id: "implement".into(),
                attempt: 1,
                message: report.summary.clone(),
                retryable: false,
                outputs: vec![report.clone()],
                now: 3,
            })
            .unwrap();
        assert_eq!(engine.run.target, original);
        assert_eq!(engine.run.nodes["implement"].outputs, vec![report.clone()]);
        assert_eq!(engine.run.nodes["review"].state, NodeState::Pending);
        Engine::restore(engine.run.clone()).unwrap();
        engine
            .handle(Input::RetryAtTarget {
                node_id: "implement".into(),
                current_target: partial.clone(),
                now: 4,
            })
            .unwrap();
        let history = engine.run.nodes["implement"].history.last().unwrap();
        assert_eq!(history.target_digest, partial.digest());
        assert_eq!(history.outputs, vec![report]);
    }

    #[test]
    fn unknown_cost_is_not_assumed_to_be_zero() {
        let mut engine = engine();
        engine.run.definition.limits.max_cost_microusd = Some(100);
        engine.run.definition.limits.allow_unknown_cost = false;
        engine.run.definition_digest = engine.run.definition.digest();
        assert!(engine.handle(Input::Start { now: 1 }).unwrap().is_empty());
        assert_eq!(engine.run.state, NodeState::Waiting);
        assert_eq!(engine.run.cost_microusd, None);
        let effects = engine
            .handle(Input::CostObserved {
                cost_microusd: 0,
                now: 2,
            })
            .unwrap();
        assert!(matches!(effects.as_slice(), [Effect::Execute { .. }]));
        let effects = engine
            .handle(Input::CostObserved {
                cost_microusd: 100,
                now: 3,
            })
            .unwrap();
        assert!(matches!(effects.as_slice(), [Effect::Cancel { .. }]));
        assert_eq!(engine.run.state, NodeState::Paused);
    }

    #[test]
    fn pause_keeps_finished_work_and_resume_schedules_the_next_node() {
        let mut engine = engine();
        engine.handle(Input::Start { now: 1 }).unwrap();
        engine.handle(Input::Pause { now: 2 }).unwrap();
        let input = completion(&engine, "task", 3);
        assert!(engine.handle(input).unwrap().is_empty());
        assert_eq!(engine.run.nodes["task"].state, NodeState::Succeeded);
        assert_eq!(engine.run.nodes["plan"].state, NodeState::Pending);
        let effects = engine.handle(Input::Resume { now: 4 }).unwrap();
        assert!(matches!(effects.as_slice(), [Effect::Execute { node, .. }] if node.id == "plan"));
    }

    #[test]
    fn reports_require_explicit_pass_and_the_exact_target() {
        let mut engine = engine();
        to_review(&mut engine);
        let mut evidence = Artifact::new(
            ArtifactKind::Evidence,
            engine.run.target.digest(),
            "looks good",
        );
        let rejected = Input::Completed {
            node_id: "review".into(),
            attempt: 1,
            outputs: vec![evidence.clone()],
            target: None,
            now: 3,
        };
        let original = engine.run.clone();
        assert!(engine.handle(rejected).is_err());
        assert_eq!(engine.run, original);
        evidence.data = serde_json::json!({"passed": true});
        evidence.target_digest = "another-target".into();
        assert!(
            engine
                .handle(Input::Completed {
                    node_id: "review".into(),
                    attempt: 1,
                    outputs: vec![evidence],
                    target: None,
                    now: 4
                })
                .is_err()
        );
    }

    #[test]
    fn restored_run_rejects_changed_definition_and_preserves_identity() {
        let run = demo_waiting(0);
        assert_eq!(Engine::restore(run.clone()).unwrap().run(), &run);
        let mut changed = run;
        changed.definition.nodes[0].instruction = "different instructions".into();
        assert!(Engine::restore(changed).is_err());
    }

    #[test]
    fn fixtures_follow_the_real_state_machine() {
        let waiting = demo_waiting(100);
        assert_eq!(waiting.state, NodeState::Waiting);
        assert_eq!(waiting.nodes["pr"].state, NodeState::Pending);
        validate_run(&waiting).unwrap();
        let failed = demo_failed(100);
        assert_eq!(failed.state, NodeState::Failed);
        validate_run(&failed).unwrap();
    }

    #[test]
    fn publication_details_are_part_of_the_approval_identity() {
        let run = demo_waiting(0);
        let mut changed = run.clone();
        changed.nodes.get_mut("implement").unwrap().outputs[0].data["publication"]["base"] =
            "another-base".into();
        assert!(Engine::restore(changed).is_err());
        let mut engine = Engine::restore(run).unwrap();
        let digest = engine.approval_digest("approval").unwrap();
        engine
            .handle(Input::Approve {
                node_id: "approval".into(),
                digest,
                current_target: engine.run.target.clone(),
                now: 1,
            })
            .unwrap();
        let publication = engine.run.nodes["approval"].outputs[0].data["publication"].clone();
        assert_eq!(
            publication,
            engine.run.nodes["implement"].outputs[0].data["publication"]
        );
        let mut corrupted = engine.into_run();
        corrupted.nodes.get_mut("approval").unwrap().outputs[0].data["publication"]["push_url"] =
            "https://github.com/another/repo.git".into();
        assert!(Engine::restore(corrupted).is_err());
    }

    #[test]
    fn restoring_never_accepts_old_or_nonpassing_evidence() {
        let mut run = demo_waiting(0);
        run.nodes.get_mut("verify").unwrap().outputs[0].data["passed"] = false.into();
        assert!(Engine::restore(run).is_err());
        let mut run = demo_waiting(0);
        run.nodes.get_mut("review").unwrap().outputs[0].target_digest = "old-target".into();
        assert!(Engine::restore(run).is_err());
    }

    #[test]
    fn failed_writer_can_carry_partial_changes_into_a_new_attempt() {
        let mut engine = engine();
        engine.handle(Input::Start { now: 1 }).unwrap();
        for id in ["task", "plan"] {
            let input = completion(&engine, id, 2);
            engine.handle(input).unwrap();
        }
        let mut target = engine.run.target.clone();
        target.dirty_digest = "partial-work".into();
        assert!(
            engine
                .handle(Input::RetryAtTarget {
                    node_id: "implement".into(),
                    current_target: target.clone(),
                    now: 3
                })
                .is_err()
        );
        engine
            .handle(Input::Failed {
                node_id: "implement".into(),
                attempt: 1,
                message: "writer exited after partial changes".into(),
                retryable: false,
                outputs: Vec::new(),
                now: 4,
            })
            .unwrap();
        let effects = engine
            .handle(Input::RetryAtTarget {
                node_id: "implement".into(),
                current_target: target.clone(),
                now: 5,
            })
            .unwrap();
        assert_eq!(engine.run.target, target);
        assert!(
            matches!(effects.as_slice(), [Effect::Execute { node, attempt: 2, target: observed, .. }] if node.kind == NodeKind::Implement && observed == &target)
        );
        assert_eq!(engine.run.nodes["implement"].history.len(), 1);
    }

    #[test]
    fn exhausted_run_limits_cannot_be_bypassed_with_resume() {
        let mut engine = engine();
        engine.handle(Input::Start { now: 1 }).unwrap();
        let deadline = engine.run.definition.limits.max_duration_ms;
        let effects = engine.handle(Input::Tick { now: deadline }).unwrap();
        assert!(matches!(effects.as_slice(), [Effect::Cancel { .. }]));
        let previous = engine.run.clone();
        assert!(engine.handle(Input::Resume { now: deadline + 1 }).is_err());
        assert_eq!(engine.run, previous);
    }

    #[test]
    fn a_running_timeout_reply_does_not_reconcile_the_same_deadline_again() {
        let mut engine = engine();
        engine.handle(Input::Start { now: 1 }).unwrap();
        let timeout = engine.run.definition.node("task").unwrap().timeout_ms + 1;
        let effects = engine.handle(Input::Tick { now: timeout }).unwrap();
        assert!(matches!(effects.as_slice(), [Effect::Reconcile { .. }]));
        assert_eq!(
            engine.run.nodes["task"].timeout_reconciled_at,
            Some(timeout)
        );

        // A live adapter may immediately confirm the existing worker. Its
        // original start time remains useful without becoming another timeout.
        let effects = engine
            .handle(Input::ObservedRunning {
                node_id: "task".into(),
                attempt: 1,
                external_ref: Some("pane:1".into()),
                message: "still running".into(),
                now: timeout,
            })
            .unwrap();
        assert!(effects.is_empty());
        assert_eq!(engine.run.nodes["task"].started_at, Some(1));
        let persisted = serde_json::to_vec(engine.run()).unwrap();
        let mut engine = Engine::restore(serde_json::from_slice(&persisted).unwrap()).unwrap();
        let before = engine.run.clone();
        for elapsed in 1..=10 {
            assert!(
                engine
                    .handle(Input::Tick {
                        now: timeout + elapsed,
                    })
                    .unwrap()
                    .is_empty()
            );
        }
        assert_eq!(engine.run, before);
        assert!(matches!(
            engine
                .handle(Input::Cancel { now: timeout + 11 })
                .unwrap()
                .as_slice(),
            [Effect::Cancel { .. }]
        ));
    }

    #[test]
    fn a_new_attempt_gets_its_own_timeout_after_an_earlier_attempt_expired() {
        let mut engine = engine();
        engine.handle(Input::Start { now: 1 }).unwrap();
        let duration = engine.run.definition.node("task").unwrap().timeout_ms;
        engine.handle(Input::Tick { now: duration + 1 }).unwrap();
        engine
            .handle(Input::Failed {
                node_id: "task".into(),
                attempt: 1,
                message: "temporary failure after reconciliation".into(),
                retryable: true,
                outputs: Vec::new(),
                now: duration + 2,
            })
            .unwrap();
        assert_eq!(engine.run.nodes["task"].timeout_reconciled_at, None);
        let restarted = duration + 2_002;
        let effects = engine.handle(Input::Tick { now: restarted }).unwrap();
        assert!(matches!(
            effects.as_slice(),
            [Effect::Execute { attempt: 2, .. }]
        ));
        assert!(
            engine
                .handle(Input::Tick {
                    now: restarted + duration - 1,
                })
                .unwrap()
                .is_empty()
        );
        let effects = engine
            .handle(Input::Tick {
                now: restarted + duration,
            })
            .unwrap();
        assert!(matches!(
            effects.as_slice(),
            [Effect::Reconcile { attempt: 2, .. }]
        ));
    }

    #[test]
    fn implementation_waits_for_all_plans_before_changing_the_target() {
        for parallel in [1, 2] {
            let mut engine = engine();
            // A plan appended after Implement in editor order is valid even
            // when it is not the implementation's direct Plan input.
            engine
                .run
                .definition
                .nodes
                .push(Node::new("second-plan", NodeKind::Plan, 0.0, 0.0));
            engine.run.definition.edges.push(Edge {
                from: "task".into(),
                to: "second-plan".into(),
                artifact: ArtifactKind::Task,
            });
            engine.run.definition.limits.max_parallel_nodes = parallel;
            engine.run.definition_digest = engine.run.definition.digest();
            engine
                .run
                .nodes
                .insert("second-plan".into(), NodeRun::pending("second-plan"));
            validate_run(engine.run()).unwrap();
            let initial_digest = engine.run.target.digest();
            engine.handle(Input::Start { now: 1 }).unwrap();
            engine.handle(completion(&engine, "task", 2)).unwrap();
            engine.handle(completion(&engine, "plan", 3)).unwrap();
            assert_eq!(engine.run.nodes["implement"].state, NodeState::Pending);
            assert_eq!(engine.run.nodes["second-plan"].state, NodeState::Running);
            let effects = engine
                .handle(completion(&engine, "second-plan", 4))
                .unwrap();
            assert!(matches!(
                effects.as_slice(),
                [Effect::Execute { node, .. }] if node.kind == NodeKind::Implement
            ));
            let mut changed = engine.run.target.clone();
            changed.head_sha = "implemented-head".into();
            let mut input = completion(&engine, "implement", 5);
            if let Input::Completed {
                outputs, target, ..
            } = &mut input
            {
                outputs[0].target_digest = changed.digest();
                outputs[0].data = serde_json::json!({"publication": demo_publication(&changed)});
                *target = Some(changed.clone());
            }
            engine.handle(input).unwrap();
            assert_eq!(engine.run.target, changed);
            assert_eq!(engine.run.nodes["second-plan"].state, NodeState::Succeeded);
            assert_eq!(
                engine.run.nodes["second-plan"].outputs[0].target_digest,
                initial_digest
            );
        }
    }

    #[test]
    fn local_only_approval_does_not_require_a_github_publication() {
        let mut engine = engine();
        engine
            .run
            .definition
            .nodes
            .retain(|node| node.kind != NodeKind::PullRequest);
        engine.run.definition.edges.retain(|edge| edge.to != "pr");
        engine.run.definition_digest = engine.run.definition.digest();
        engine.run.nodes.remove("pr");
        engine.handle(Input::Start { now: 1 }).unwrap();
        for id in ["task", "plan", "implement", "review", "verify"] {
            let mut input = completion(&engine, id, 2);
            if let Input::Completed { outputs, .. } = &mut input
                && outputs[0].kind == ArtifactKind::Change
            {
                outputs[0].data = serde_json::Value::Null;
            }
            engine.handle(input).unwrap();
        }
        let digest = engine.approval_digest("approval").unwrap();
        let effects = engine
            .handle(Input::Approve {
                node_id: "approval".into(),
                digest,
                current_target: engine.run.target.clone(),
                now: 3,
            })
            .unwrap();
        assert!(effects.is_empty());
        assert_eq!(engine.run.state, NodeState::Succeeded);
        assert!(engine.run.nodes["approval"].outputs[0].data["publication"].is_null());
    }
}
