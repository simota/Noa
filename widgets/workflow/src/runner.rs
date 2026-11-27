//! UI-independent live workflow controller. The model is committed before
//! adapter effects are dispatched; reopening observes receipts instead of
//! replaying a previous spawn or publication.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::adapters::{Adapters, Observation, fs, git};
use crate::engine::{Effect, Engine, Input};
use crate::model::{AgentPreset, NodeKind, NodeState, Run, SCHEMA_VERSION, WorkflowDefinition};
use crate::store::{Store, StoredWorkflow};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Connection {
    Connecting,
    Connected,
    Disconnected(String),
}

#[derive(Clone, Debug)]
pub struct View {
    pub repository: PathBuf,
    pub definition: WorkflowDefinition,
    pub version: u64,
    pub run: Option<Run>,
    pub connection: Connection,
    pub notice: Option<String>,
    /// False while another process owns this repository's runner lease.
    pub can_control: bool,
    /// A Tasks handoff observes saved runs until an explicit Start or Resume.
    pub deferred: bool,
    pub presets: Vec<AgentPreset>,
}

#[derive(Clone, Debug)]
pub enum Intent {
    Save {
        definition: WorkflowDefinition,
        expected_version: u64,
    },
    Start {
        objective: String,
        origin_task: Option<crate::tasks::TaskOrigin>,
    },
    SavePreset(AgentPreset),
    Pause,
    Resume,
    Cancel,
    Approve {
        node_id: String,
        digest: String,
    },
    Retry {
        node_id: String,
    },
    Repair,
    Reconcile {
        node_id: String,
    },
    Focus {
        node_id: String,
    },
    Answer {
        node_id: String,
        answer: String,
    },
    Refresh,
}

struct Shared {
    commands: mpsc::Sender<WorkerCommand>,
    views: Arc<Mutex<Option<View>>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

enum WorkerCommand {
    Act(Intent),
    Detach,
}

/// Clones keep the runner alive. After the last clone is dropped the
/// scheduler stops, retaining agent panes and durable receipts for reopen.
#[derive(Clone)]
pub struct Channel(Arc<Shared>);

impl Channel {
    pub fn open(
        repository: PathBuf,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let repository = git::repository(&repository)?;
        let root = data_root(&repository)?;
        Self::open_in(repository, root, wake)
    }

    /// Fill a Tasks handoff without recovering or scheduling an older run.
    pub fn open_deferred(
        repository: PathBuf,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let repository = git::repository(&repository)?;
        let root = data_root(&repository)?;
        Self::open_in_mode(repository, root, true, wake)
    }

    pub fn open_in(
        repository: PathBuf,
        root: PathBuf,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        Self::open_in_mode(repository, root, false, wake)
    }

    fn open_in_mode(
        repository: PathBuf,
        root: PathBuf,
        deferred: bool,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let repository = git::repository(&repository)?;
        fs::plain_dirs(&root)?;
        let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
        let lease = fs::Lease::try_acquire(&root.join("runner.lock"))?;
        let store = Store::open(root.clone()).map_err(|e| e.to_string())?;
        let loaded = if lease.is_some() {
            store.load()
        } else {
            store.observe()
        }
        .map_err(|e| e.to_string())?;
        let stored = match loaded {
            Some(stored) => stored,
            None if lease.is_some() => store
                .save(0, &WorkflowDefinition::standard(), None)
                .map_err(|e| e.to_string())?,
            // Another owner may be between taking the runner lease and its
            // first snapshot. This placeholder is never written by observers.
            None => StoredWorkflow {
                schema_version: SCHEMA_VERSION,
                version: 0,
                definition: WorkflowDefinition::standard(),
                run: None,
            },
        };
        let engine = restore_stored(&stored, &repository)?;
        let presets = read_presets(&root)?;
        let adapters = Adapters::new(repository.clone(), root.join("runs"))?;
        let (commands, receive) = mpsc::channel();
        let views = Arc::new(Mutex::new(None));
        let channel = Self(Arc::new(Shared {
            commands,
            views: Arc::clone(&views),
            thread: Mutex::new(None),
        }));
        let mut worker = Worker {
            repository,
            root,
            store,
            stored,
            engine,
            adapters,
            views,
            wake: Arc::new(wake),
            notice: lease.is_none().then(|| OBSERVER_NOTICE.to_owned()),
            persistence_failed: false,
            lease,
            presets,
            deferred,
        };
        worker.publish();
        let thread = std::thread::Builder::new()
            .name("workflow-runner".into())
            .spawn(move || {
                if worker.lease.is_some() {
                    worker.recover();
                }
                let mut detached = false;
                loop {
                    match receive.recv_timeout(Duration::from_secs(1)) {
                        Ok(WorkerCommand::Act(intent)) => {
                            if let Err(error) = worker.intent(intent) {
                                worker.notice = Some(error);
                            }
                            worker.publish();
                        }
                        Ok(WorkerCommand::Detach) => detached = true,
                        Err(mpsc::RecvTimeoutError::Timeout) => worker.poll_active(),
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                    if detached && !worker.has_autonomous_work() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("実行スレッドを開始できません: {e}"))?;
        *channel
            .0
            .thread
            .lock()
            .map_err(|_| "実行スレッドの状態を保存できません")? = Some(thread);
        Ok(channel)
    }

    pub fn act(&self, intent: Intent) -> Result<(), String> {
        self.0
            .commands
            .send(WorkerCommand::Act(intent))
            .map_err(|_| "実行サービスとの接続が終了しました".into())
    }

    pub fn poll(&self) -> Option<View> {
        self.0.views.lock().ok()?.take()
    }

    /// Called only after the native window closes. The marker is queued
    /// after all its actions, so a last Start/Approve cannot be lost to a
    /// stale UI snapshot. Automatic work continues until human input or
    /// recovery is required; this is not a system daemon.
    pub fn finish_autonomous(self) -> Result<(), String> {
        let _ = self.0.commands.send(WorkerCommand::Detach);
        let thread = self
            .0
            .thread
            .lock()
            .map_err(|_| "実行スレッドの状態を読めません")?
            .take();
        if let Some(thread) = thread {
            thread.join().map_err(|_| {
                "実行スレッドが予期せず終了しました。保存済みの状態から再接続してください"
                    .to_owned()
            })?;
        }
        Ok(())
    }
}

struct Worker {
    repository: PathBuf,
    root: PathBuf,
    store: Store,
    stored: StoredWorkflow,
    engine: Option<Engine>,
    adapters: Adapters,
    views: Arc<Mutex<Option<View>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    notice: Option<String>,
    persistence_failed: bool,
    lease: Option<fs::Lease>,
    presets: Vec<AgentPreset>,
    deferred: bool,
}

fn data_root(repository: &std::path::Path) -> Result<PathBuf, String> {
    let state = noa_config::session_state_path().ok_or("Noaの保存先が見つかりません")?;
    let data = state
        .parent()
        .ok_or("Noaの保存先が不正です")?
        .join("workflows");
    Ok(data.join(fs::hash(repository.as_os_str().as_encoded_bytes())))
}

fn read_presets(root: &std::path::Path) -> Result<Vec<AgentPreset>, String> {
    let presets: Vec<AgentPreset> =
        fs::read_json(&root.join("agent-presets.json"))?.unwrap_or_default();
    if presets.len() > 32 {
        return Err("担当構成は32件以内にしてください".into());
    }
    for preset in &presets {
        preset.validate()?;
    }
    Ok(presets)
}

/// Opens a frozen run for inspection, without starting a runner or connecting
/// to Noa. References exported to Tasks always point to this durable snapshot.
pub fn read_run(repository: &std::path::Path, id: &str) -> Result<Run, String> {
    if !crate::model::valid_id(id) {
        return Err("実行IDが不正です".into());
    }
    let repository = git::repository(repository)?;
    let bytes = fs::read(
        &data_root(&repository)?
            .join("runs")
            .join(id)
            .join("run.json"),
        32 * 1024 * 1024,
    )?
    .ok_or("指定されたWorkflow実行が見つかりません")?;
    let run: Run = serde_json::from_slice(&bytes).map_err(|e| format!("実行を読めません: {e}"))?;
    if run.repository != repository || run.id != id {
        return Err("実行とリポジトリが一致しません".into());
    }
    crate::engine::validate_run(&run).map_err(|e| e.to_string())?;
    Ok(run)
}

const OBSERVER_NOTICE: &str =
    "別の実行サービスが処理中です。現在は閲覧のみで、処理の区切りで操作を引き継ぎます";

fn restore_stored(
    stored: &StoredWorkflow,
    repository: &std::path::Path,
) -> Result<Option<Engine>, String> {
    if stored
        .run
        .as_ref()
        .is_some_and(|run| run.repository != repository)
    {
        return Err("保存された実行のリポジトリが一致しません".into());
    }
    stored
        .run
        .clone()
        .map(Engine::restore)
        .transpose()
        .map_err(|e| e.to_string())
}

impl Worker {
    fn has_autonomous_work(&self) -> bool {
        if self.deferred || self.persistence_failed || self.lease.is_none() {
            return false;
        }
        let Some(run) = self.engine.as_ref().map(Engine::run) else {
            return false;
        };
        if run
            .nodes
            .values()
            .any(|node| node.state == NodeState::Running)
        {
            return true;
        }
        !run.pause_requested
            && !run.cancel_requested
            && run.limit_reason.is_none()
            && run.nodes.values().any(|node| {
                matches!(node.state, NodeState::Pending | NodeState::Failed)
                    && node.retry_at.is_some()
            })
    }
    fn publish(&self) {
        let connection = if self.lease.is_none() {
            Connection::Disconnected("別の実行サービスを監視しています".into())
        } else if self.adapters.connected() {
            Connection::Connected
        } else {
            Connection::Disconnected(
                self.adapters
                    .connection_message
                    .clone()
                    .unwrap_or_else(|| "実行時にNoaへ接続します".into()),
            )
        };
        let view = View {
            repository: self.repository.clone(),
            definition: self.stored.definition.clone(),
            version: self.stored.version,
            run: self.engine.as_ref().map(|e| e.run().clone()),
            connection,
            notice: self.notice.clone(),
            can_control: self.lease.is_some() && !self.persistence_failed,
            deferred: self.deferred,
            presets: self.presets.clone(),
        };
        if let Ok(mut latest) = self.views.lock() {
            *latest = Some(view);
        }
        (self.wake)();
    }

    fn save_engine(&mut self, engine: Engine) -> Result<(), String> {
        match self.store.save(
            self.stored.version,
            &self.stored.definition,
            Some(engine.run()),
        ) {
            Ok(stored) => {
                self.stored = stored;
                self.engine = Some(engine);
                // Derived references follow the committed state. Their failure
                // does not turn a persisted dispatch into an ambiguous retry.
                if let Some(run) = self.engine.as_ref().map(Engine::run) {
                    let projection =
                        fs::write_json(&self.root.join("runs").join(&run.id).join("run.json"), run)
                            .and_then(|()| crate::tasks::publish(run));
                    if let Err(error) = projection {
                        self.notice = Some(format!(
                            "実行は保存済みですが関連成果を更新できません: {error}"
                        ));
                    }
                }
                Ok(())
            }
            Err(error) => {
                self.persistence_failed = true;
                Err(format!(
                    "状態を保存できないため新しい操作を停止しました: {error}"
                ))
            }
        }
    }

    fn apply(&mut self, input: Input) -> Result<Vec<Effect>, String> {
        if self.persistence_failed {
            return Err(
                "保存に失敗しています。保存先を復旧してウィンドウを開き直してください".into(),
            );
        }
        let mut engine = self
            .engine
            .clone()
            .ok_or("実行中のワークフローがありません")?;
        let old = engine.run().clone();
        let effects = engine.handle(input).map_err(|e| e.to_string())?;
        if *engine.run() != old || !effects.is_empty() {
            self.save_engine(engine)?;
        }
        Ok(effects)
    }

    fn accept(
        &mut self,
        node_id: String,
        attempt: u32,
        observation: Observation,
    ) -> Result<Vec<Effect>, String> {
        let now = now();
        let input = match observation {
            Observation::Running {
                external_ref,
                message,
            } => Input::ObservedRunning {
                node_id,
                attempt,
                external_ref,
                message,
                now,
            },
            Observation::Waiting {
                external_ref,
                message,
            } => Input::Waiting {
                node_id,
                attempt,
                external_ref,
                message,
                now,
            },
            Observation::Complete { outputs, target } => Input::Completed {
                node_id,
                attempt,
                outputs,
                target,
                now,
            },
            Observation::Failed { message, retryable } => Input::Failed {
                node_id,
                attempt,
                message,
                retryable,
                now,
            },
            Observation::Uncertain {
                external_ref,
                message,
            } => Input::Uncertain {
                node_id,
                attempt,
                external_ref,
                message,
                now,
            },
            Observation::Canceled => Input::Canceled {
                node_id,
                attempt,
                now,
            },
        };
        self.apply(input)
    }

    fn effects(&mut self, effects: Vec<Effect>) -> Result<(), String> {
        if self.deferred && !effects.is_empty() {
            return Err("Tasksから開いた保存済み実行は、明示的に再開するまで操作できません".into());
        }
        let mut queue: VecDeque<Effect> = effects.into();
        let mut count = 0;
        while let Some(effect) = queue.pop_front() {
            count += 1;
            if count > 256 {
                return Err("工程の即時遷移が上限に達しました".into());
            }
            if self.persistence_failed {
                return Err("保存が復旧するまで工程を実行できません".into());
            }
            let run = self
                .engine
                .as_ref()
                .ok_or("実行がありません")?
                .run()
                .clone();
            let (node, attempt, observation) = match effect {
                Effect::Execute {
                    node,
                    attempt,
                    request_id,
                    inputs,
                    target,
                } => {
                    let id = node.id.clone();
                    let observation = self
                        .adapters
                        .execute(&run, node, attempt, request_id, inputs, target);
                    (id, attempt, observation)
                }
                Effect::Reconcile {
                    node_id, attempt, ..
                } => {
                    let observation = self.adapters.observe(&run.id, &node_id, attempt, true);
                    (node_id, attempt, observation)
                }
                Effect::Cancel {
                    node_id, attempt, ..
                } => {
                    let observation = self.adapters.cancel(&run.id, &node_id, attempt);
                    (node_id, attempt, observation)
                }
            };
            queue.extend(self.accept(node, attempt, observation)?);
            self.publish();
        }
        Ok(())
    }

    fn recover(&mut self) {
        if self.deferred {
            self.publish();
            return;
        }
        let active: Vec<_> = self
            .engine
            .as_ref()
            .into_iter()
            .flat_map(|e| {
                e.run()
                    .nodes
                    .values()
                    .filter(|n| n.state.active())
                    .filter(|n| {
                        e.run()
                            .definition
                            .node(&n.node_id)
                            .is_some_and(|d| d.kind != NodeKind::Approval)
                    })
                    .map(|n| n.node_id.clone())
            })
            .collect();
        for node_id in active {
            let result = self
                .apply(Input::Reconcile {
                    node_id,
                    now: now(),
                })
                .and_then(|effects| self.effects(effects));
            if let Err(error) = result {
                self.notice = Some(error);
                break;
            }
        }
        self.publish();
    }

    fn poll_active(&mut self) {
        if self.lease.is_none() {
            if let Err(error) = self.poll_observer() {
                self.notice = Some(error);
                self.publish();
            }
            return;
        }
        if self.deferred {
            return;
        }
        if self.persistence_failed {
            return;
        }
        let Some(run) = self.engine.as_ref().map(|e| e.run().clone()) else {
            return;
        };
        let active: Vec<_> = run
            .nodes
            .values()
            .filter(|n| n.state.active())
            .filter(|n| {
                run.definition
                    .node(&n.node_id)
                    .is_some_and(|d| d.kind != NodeKind::Approval)
            })
            .map(|n| (n.node_id.clone(), n.attempt, n.state))
            .collect();
        for (node_id, attempt, state) in active {
            // PR reconciliation can contact a remote service; it is an
            // explicit action after an uncertain acknowledgement.
            if run
                .definition
                .node(&node_id)
                .is_some_and(|n| n.kind == NodeKind::PullRequest)
                && state == NodeState::Uncertain
            {
                continue;
            }
            let observation = self.adapters.observe(&run.id, &node_id, attempt, false);
            if let Err(error) = self
                .accept(node_id, attempt, observation)
                .and_then(|effects| self.effects(effects))
            {
                self.notice = Some(error);
                break;
            }
        }
        if !matches!(run.state, NodeState::Succeeded | NodeState::Canceled)
            && let Err(error) = self
                .apply(Input::Tick { now: now() })
                .and_then(|effects| self.effects(effects))
        {
            self.notice = Some(error);
        }
        self.publish();
    }

    fn intent(&mut self, intent: Intent) -> Result<(), String> {
        if self.lease.is_none() {
            return match intent {
                Intent::Refresh => self.poll_observer(),
                _ => Err(OBSERVER_NOTICE.to_owned()),
            };
        }
        self.notice = None;
        if self.persistence_failed {
            return Err("保存先を復旧してウィンドウを開き直してください".into());
        }
        if self.deferred
            && !matches!(
                intent,
                Intent::Save { .. }
                    | Intent::SavePreset(_)
                    | Intent::Start { .. }
                    | Intent::Resume
                    | Intent::Refresh
            )
        {
            return Err("保存済み実行への操作は「以前の実行を再開」後に行ってください".into());
        }
        let start_deferred = self.deferred && matches!(intent, Intent::Start { .. });
        let resume_deferred = self.deferred && matches!(intent, Intent::Resume);
        let now = now();
        let input = match intent {
            Intent::Save {
                definition,
                expected_version,
            } => {
                if expected_version != self.stored.version {
                    return Err("表示後に状態が更新されました。最新の内容で保存してください".into());
                }
                self.stored = self
                    .store
                    .save(
                        expected_version,
                        &definition,
                        self.engine.as_ref().map(Engine::run),
                    )
                    .map_err(|e| e.to_string())?;
                return Ok(());
            }
            Intent::SavePreset(preset) => {
                preset.validate()?;
                let mut next = self.presets.clone();
                if let Some(previous) = next.iter_mut().find(|p| p.name == preset.name) {
                    *previous = preset;
                } else {
                    next.push(preset);
                }
                if next.len() > 32 {
                    return Err("担当構成は32件まで保存できます".into());
                }
                fs::write_json(&self.root.join("agent-presets.json"), &next)?;
                self.presets = next;
                self.notice = Some("担当構成を保存しました".into());
                return Ok(());
            }
            Intent::Start {
                objective,
                origin_task,
            } => {
                if let Some(origin) = &origin_task {
                    origin.validate(&self.repository, &objective)?;
                }
                if self
                    .engine
                    .as_ref()
                    .is_some_and(|e| e.run().nodes.values().any(|n| n.state.active()))
                {
                    return Err("前の実行に実行中・判断待ち・結果不明の工程があります".into());
                }
                if let Some(previous) = &self.engine {
                    fs::write_json(
                        &self
                            .root
                            .join("runs")
                            .join(&previous.run().id)
                            .join("run.json"),
                        previous.run(),
                    )?;
                }
                let base = git::head(&self.repository)?;
                let target = git::snapshot(&self.repository, &base)?;
                let id = format!("run-{now}-{}", self.stored.version);
                let definition = crate::adapters::ipc::freeze_assignments(
                    self.stored.definition.clone(),
                    crate::adapters::ipc::detect_agent,
                )?;
                let engine = Engine::new(
                    id,
                    definition,
                    self.repository.clone(),
                    objective,
                    target,
                    now,
                )
                .map_err(|e| e.to_string())?;
                let mut run = engine.into_run();
                run.origin_task = origin_task;
                let engine = Engine::restore(run).map_err(|e| e.to_string())?;
                self.save_engine(engine)?;
                Input::Start { now }
            }
            Intent::Pause => Input::Pause { now },
            Intent::Resume => Input::Resume { now },
            Intent::Cancel => Input::Cancel { now },
            Intent::Approve { node_id, digest } => {
                let run = self.engine.as_ref().ok_or("実行がありません")?.run();
                self.adapters.validate_evidence(run)?;
                let current_target = self.adapters.current_target(run)?;
                Input::Approve {
                    node_id,
                    digest,
                    current_target,
                    now,
                }
            }
            Intent::Retry { node_id } => {
                let run = self.engine.as_ref().ok_or("実行がありません")?.run();
                if run
                    .definition
                    .node(&node_id)
                    .is_some_and(|node| node.kind == NodeKind::Implement)
                {
                    let current_target = self.adapters.retry_target(run, &node_id)?;
                    Input::RetryAtTarget {
                        node_id,
                        current_target,
                        now,
                    }
                } else {
                    Input::Retry { node_id, now }
                }
            }
            Intent::Repair => Input::Repair { now },
            Intent::Reconcile { node_id } => Input::Reconcile { node_id, now },
            Intent::Focus { node_id } => {
                let run = self.engine.as_ref().ok_or("実行がありません")?.run();
                let node = run.nodes.get(&node_id).ok_or("工程がありません")?;
                return self.adapters.focus(&run.id, &node_id, node.attempt);
            }
            Intent::Answer { node_id, answer } => {
                let run = self.engine.as_ref().ok_or("実行がありません")?.run();
                let node = run.nodes.get(&node_id).ok_or("工程がありません")?;
                return self
                    .adapters
                    .answer(&run.id, &node_id, node.attempt, &answer);
            }
            Intent::Refresh => {
                if self.deferred {
                    self.publish();
                    return Ok(());
                }
                self.adapters.reconnect()?;
                self.poll_active();
                return Ok(());
            }
        };
        let effects = self.apply(input)?;
        // A rejected Start/Resume leaves the handoff suspended. Persist the
        // explicit transition before permitting any effects or timer ticks.
        if start_deferred || resume_deferred {
            self.deferred = false;
        }
        self.effects(effects)?;
        if resume_deferred {
            self.recover();
        }
        Ok(())
    }

    /// Read-only snapshots use atomic replacement and checksum validation;
    /// they do not take the store lock and cannot make the owner lose a save.
    fn poll_observer(&mut self) -> Result<(), String> {
        if let Some(stored) = self.store.observe().map_err(|e| e.to_string())? {
            let engine = restore_stored(&stored, &self.repository)?;
            self.stored = stored;
            self.engine = engine;
        }
        if let Some(lease) = fs::Lease::try_acquire(&self.root.join("runner.lock"))? {
            // Ownership can change between the observed snapshot and flock.
            // Always reload under the lease before restoring or dispatching.
            let stored = match self.store.load().map_err(|e| e.to_string())? {
                Some(stored) => stored,
                None => self
                    .store
                    .save(0, &WorkflowDefinition::standard(), None)
                    .map_err(|e| e.to_string())?,
            };
            let engine = restore_stored(&stored, &self.repository)?;
            // The old owner may have saved presets after this observer opened.
            // Reload under the lease before its first save can replace the file.
            self.presets = read_presets(&self.root)?;
            self.stored = stored;
            self.engine = engine;
            self.lease = Some(lease);
            self.notice = Some("保存済みの状態から操作を引き継ぎました".into());
            self.recover();
        } else {
            self.presets = read_presets(&self.root)?;
            self.notice = Some(OBSERVER_NOTICE.to_owned());
            self.publish();
        }
        Ok(())
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(u64::MAX as u128) as u64
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(directory.path()).unwrap();
        let repository = path.join("repo");
        std::fs::create_dir(&repository).unwrap();
        git::command(&repository, &["init", "-q"]).unwrap();
        git::command(&repository, &["config", "user.name", "Workflow Test"]).unwrap();
        git::command(
            &repository,
            &["config", "user.email", "workflow@example.invalid"],
        )
        .unwrap();
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git::command(&repository, &["add", "README.md"]).unwrap();
        git::command(&repository, &["commit", "-qm", "fixture"]).unwrap();
        (directory, repository, path.join("state"))
    }

    fn saved_attempt(
        repository: &std::path::Path,
        root: &std::path::Path,
        retry: bool,
        expired: bool,
    ) -> StoredWorkflow {
        let started = now().saturating_sub(10_000);
        let mut definition = WorkflowDefinition::standard();
        if expired {
            definition.limits.max_duration_ms = 5_000;
        }
        let head = git::head(repository).unwrap();
        let mut engine = Engine::new(
            "run-before-handoff".into(),
            definition.clone(),
            repository.to_owned(),
            "The previous task".into(),
            git::snapshot(repository, &head).unwrap(),
            started,
        )
        .unwrap();
        engine.handle(Input::Start { now: started }).unwrap();
        if retry {
            engine
                .handle(Input::Failed {
                    node_id: "task".into(),
                    attempt: 1,
                    message: "temporary".into(),
                    retryable: true,
                    now: started,
                })
                .unwrap();
            assert!(
                engine.run().nodes["task"]
                    .retry_at
                    .is_some_and(|at| at < now())
            );
        }
        Store::open(root.to_owned())
            .unwrap()
            .save(0, &definition, Some(engine.run()))
            .unwrap()
    }

    #[test]
    fn deferred_handoffs_do_not_recover_tick_or_resume_after_a_rejected_start() {
        for retry in [false, true] {
            let (_directory, repository, root) = repository();
            let before = saved_attempt(&repository, &root, retry, true);
            let channel = Channel::open_in_mode(repository, root.clone(), true, || {}).unwrap();
            assert!(channel.poll().unwrap().deferred);
            // Exercise the timer as well as refresh, refused operations, and close.
            std::thread::sleep(Duration::from_millis(1_100));
            channel
                .act(Intent::Start {
                    objective: String::new(),
                    origin_task: None,
                })
                .unwrap();
            channel.act(Intent::Resume).unwrap(); // The saved duration limit has expired.
            channel
                .act(Intent::Retry {
                    node_id: "task".into(),
                })
                .unwrap();
            channel.act(Intent::Refresh).unwrap();
            channel.clone().finish_autonomous().unwrap();
            let view = channel.poll().unwrap();
            assert!(view.deferred);
            let after = Store::open(root).unwrap().load().unwrap().unwrap();
            assert_eq!(after.version, before.version);
            assert_eq!(
                after.run, before.run,
                "opening a task must not advance the old run"
            );
        }
    }

    #[test]
    fn a_deferred_observer_stays_suspended_after_taking_the_lease() {
        let (_directory, repository, root) = repository();
        let before = saved_attempt(&repository, &root, false, false);
        let lease = fs::Lease::try_acquire(&root.join("runner.lock"))
            .unwrap()
            .unwrap();
        let channel = Channel::open_in_mode(repository.clone(), root.clone(), true, || {}).unwrap();
        assert!(!channel.poll().unwrap().can_control);
        drop(lease);
        channel.act(Intent::Refresh).unwrap();
        channel.clone().finish_autonomous().unwrap();
        let view = channel.poll().unwrap();
        assert!(view.can_control && view.deferred);
        assert_eq!(
            Store::open(root.clone())
                .unwrap()
                .load()
                .unwrap()
                .unwrap()
                .run,
            before.run
        );
        drop(channel);

        // The ordinary Workflow entry point retains its existing recovery behavior.
        let ordinary = Channel::open_in(repository, root.clone(), || {}).unwrap();
        ordinary.finish_autonomous().unwrap();
        let after = Store::open(root).unwrap().load().unwrap().unwrap();
        assert!(after.version > before.version);
        assert_ne!(after.run, before.run);
    }

    #[test]
    fn only_a_successful_explicit_start_or_resume_releases_a_handoff() {
        let (_directory, repository, root) = self::repository();
        let channel = Channel::open_in_mode(repository, root.clone(), true, || {}).unwrap();
        let initial = channel.poll().unwrap();
        let mut definition = initial.definition;
        definition.limits.max_cost_microusd = Some(1);
        definition.limits.allow_unknown_cost = false;
        for node in &mut definition.nodes {
            if node.kind.uses_agent() {
                node.agent.provider = "codex".into();
            }
        }
        channel
            .act(Intent::Save {
                definition,
                expected_version: initial.version,
            })
            .unwrap();
        channel
            .act(Intent::Start {
                objective: "The explicitly started task".into(),
                origin_task: None,
            })
            .unwrap();
        channel.clone().finish_autonomous().unwrap();
        let view = channel.poll().unwrap();
        assert!(!view.deferred);
        assert_eq!(view.run.unwrap().state, NodeState::Waiting); // The real budget gate stops dispatch.

        let (_directory, repository, root) = self::repository();
        saved_attempt(&repository, &root, false, false);
        let channel = Channel::open_in_mode(repository, root, true, || {}).unwrap();
        channel.act(Intent::Resume).unwrap();
        channel.clone().finish_autonomous().unwrap();
        assert!(!channel.poll().unwrap().deferred);
    }

    #[test]
    fn detaching_drains_a_just_queued_start_before_deciding_to_exit() {
        let (_directory, repository, root) = repository();
        let channel = Channel::open_in(repository.clone(), root.clone(), || {}).unwrap();
        let version = channel.poll().unwrap().version;
        let mut definition = WorkflowDefinition::standard();
        // Cost is intentionally unknown: the model must record Start and
        // stop at its real budget gate without connecting to an agent.
        definition.limits.max_cost_microusd = Some(1_000_000);
        definition.limits.allow_unknown_cost = false;
        for node in &mut definition.nodes {
            if node.kind.uses_agent() {
                node.agent.provider = "codex".into();
            }
        }
        assert!(definition.validate().is_empty());
        channel
            .act(Intent::Save {
                definition,
                expected_version: version,
            })
            .unwrap();
        channel
            .act(Intent::Start {
                objective: "queued immediately before closing".into(),
                origin_task: None,
            })
            .unwrap();
        channel.finish_autonomous().unwrap();
        let persisted = Store::open(root).unwrap().load().unwrap().unwrap();
        let run = persisted.run.unwrap();
        assert_eq!(run.state, NodeState::Waiting);
        assert_eq!(run.objective, "queued immediately before closing");
        assert_eq!(run.nodes["task"].attempt, 0);
        assert!(run.events.iter().any(|event| event.kind == "started"));
    }

    #[test]
    fn another_window_observes_then_takes_control_without_replaying_its_actions() {
        let (_directory, repository, root) = repository();
        let owner = Channel::open_in(repository.clone(), root.clone(), || {}).unwrap();
        let initial = owner.poll().unwrap();
        assert!(initial.can_control);
        let (changed, receive) = mpsc::channel();
        let observer = Channel::open_in(repository, root.clone(), move || {
            let _ = changed.send(());
        })
        .unwrap();
        receive.recv_timeout(Duration::from_secs(2)).unwrap();
        let observed = observer.poll().unwrap();
        assert!(!observed.can_control);
        assert_eq!(observed.version, initial.version);
        let mut rejected = observed.definition;
        rejected.name = "must never be saved".into();
        observer
            .act(Intent::Save {
                definition: rejected,
                expected_version: observed.version,
            })
            .unwrap();
        receive.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!observer.poll().unwrap().can_control);
        let mut definition = initial.definition;
        definition.name = "saved by owner before detaching".into();
        let first_preset = AgentPreset::from_definition("Owner preset".into(), &definition);
        owner.act(Intent::SavePreset(first_preset.clone())).unwrap();
        owner
            .act(Intent::Save {
                definition: definition.clone(),
                expected_version: initial.version,
            })
            .unwrap();
        owner.finish_autonomous().unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let promoted = loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            receive.recv_timeout(remaining).unwrap();
            if let Some(view) = observer.poll()
                && view.can_control
            {
                break view;
            }
        };
        assert_eq!(promoted.definition.name, definition.name);
        assert_eq!(promoted.presets, vec![first_preset.clone()]);
        assert!(promoted.version > initial.version);
        let mut taken_over = promoted.definition;
        taken_over.name = "saved after ownership transfer".into();
        observer
            .act(Intent::Save {
                definition: taken_over.clone(),
                expected_version: promoted.version,
            })
            .unwrap();
        let second_preset = AgentPreset::from_definition("Observer preset".into(), &taken_over);
        observer
            .act(Intent::SavePreset(second_preset.clone()))
            .unwrap();
        observer.finish_autonomous().unwrap();
        assert_eq!(
            read_presets(&root).unwrap(),
            vec![first_preset, second_preset]
        );
        let persisted = Store::open(root).unwrap().load().unwrap().unwrap();
        assert_eq!(persisted.definition.name, taken_over.name);
    }
}
