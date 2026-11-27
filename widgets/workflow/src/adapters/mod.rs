//! Live node adapters and their durable side-effect receipts. A persisted
//! dispatch intent is never treated as permission to spawn a second worker.

pub mod fs;
pub mod git;
mod github;
pub mod ipc;
mod process;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{Artifact, ArtifactKind, Node, NodeKind, Run, TargetSnapshot};

#[derive(Clone, Debug)]
pub enum Observation {
    Running {
        external_ref: Option<String>,
        message: String,
    },
    Waiting {
        external_ref: Option<String>,
        message: String,
    },
    Complete {
        outputs: Vec<Artifact>,
        target: Option<TargetSnapshot>,
    },
    Failed {
        message: String,
        retryable: bool,
    },
    Uncertain {
        external_ref: Option<String>,
        message: String,
    },
    Canceled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    SpawnIntent,
    Running,
    Finishing,
    PrIntent,
    Complete,
    Failed,
    CancelIntent,
    Canceled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Job {
    schema_version: u32,
    run_id: String,
    node: Node,
    attempt: u32,
    request_id: String,
    objective: String,
    target: TargetSnapshot,
    source: PathBuf,
    repository_common: PathBuf,
    worktree: PathBuf,
    report: PathBuf,
    agent: String,
    label: String,
    phase: Phase,
    pane_id: Option<u64>,
    board_id: Option<u64>,
    server_instance: Option<String>,
    prepared_pr: Option<github::PreparedPr>,
    outputs: Vec<Artifact>,
    result_target: Option<TargetSnapshot>,
    report_digest: Option<String>,
    failure: Option<String>,
    worker_stopped: bool,
    awaiting_publication: bool,
    finish_blocked: Option<String>,
    prepare_publication: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportFile {
    /// Relative to the request directory, never to the repository root.
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportCheck {
    pub criterion: String,
    pub command: Vec<String>,
    pub exit_code: i32,
    pub log: String,
}

/// This is an adapter's evidence receipt, not an agent-state notification.
/// Every field binding a receipt to an attempt is required.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentReport {
    pub schema_version: u32,
    pub run_id: String,
    pub node_id: String,
    pub attempt: u32,
    pub request_id: String,
    pub target_digest: String,
    pub passed: bool,
    pub summary: String,
    pub files: Vec<ReportFile>,
    pub checks: Vec<ReportCheck>,
}

pub struct Adapters {
    repository: PathBuf,
    root: PathBuf,
    connection: Option<ipc::Ipc>,
    board: Option<(String, u64)>,
    jobs: BTreeMap<(String, String, u32), Job>,
    pub connection_message: Option<String>,
    reconnect_after: Option<std::time::Instant>,
    reconnect_attempts: u8,
}

impl Adapters {
    pub fn new(repository: PathBuf, root: PathBuf) -> Result<Self, String> {
        fs::plain_dirs(&root)?;
        Ok(Self {
            repository,
            root,
            connection: None,
            board: None,
            jobs: BTreeMap::new(),
            connection_message: None,
            reconnect_after: None,
            reconnect_attempts: 0,
        })
    }

    pub fn connected(&self) -> bool {
        self.connection.is_some()
    }

    pub fn connect(&mut self) -> Result<(), String> {
        if self.connection.is_none() {
            if self
                .reconnect_after
                .is_some_and(|deadline| std::time::Instant::now() < deadline)
            {
                return Err(self
                    .connection_message
                    .clone()
                    .unwrap_or_else(|| "Noaへの再接続を待っています".into()));
            }
            match ipc::Ipc::connect() {
                Ok(ipc) => {
                    self.connection = Some(ipc);
                    self.connection_message = None;
                    self.reconnect_after = None;
                    self.reconnect_attempts = 0;
                }
                Err(error) => {
                    self.connection_message = Some(error.clone());
                    self.defer_reconnect();
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    pub fn reconnect(&mut self) -> Result<(), String> {
        self.reconnect_after = None;
        self.connect()
    }

    fn defer_reconnect(&mut self) {
        self.reconnect_attempts = self.reconnect_attempts.saturating_add(1);
        let seconds = (1u64 << self.reconnect_attempts.min(5)).min(30);
        self.reconnect_after =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(seconds));
    }

    fn lost(&mut self, error: &impl std::fmt::Display) {
        self.connection_message = Some(error.to_string());
        self.connection = None;
        self.board = None;
        self.defer_reconnect();
    }

    fn panels(&mut self) -> Result<Vec<noa_ipc::Panel>, String> {
        self.connect()?;
        match self.connection.as_mut().expect("connected").panels() {
            Ok(panels) => Ok(panels),
            Err(error) => {
                self.lost(&error);
                Err(error.to_string())
            }
        }
    }

    fn close_pane(&mut self, pane: u64) -> Result<(), String> {
        match self
            .connection
            .as_mut()
            .ok_or("Noaへの接続がありません")?
            .close(pane)
        {
            Ok(()) => Ok(()),
            Err(error) => {
                self.lost(&error);
                Err(error.to_string())
            }
        }
    }

    fn directory(&self, run: &str, node: &str, attempt: u32) -> Result<PathBuf, String> {
        if !fs::valid_id(run) || !fs::valid_id(node) || attempt == 0 {
            return Err("実行・工程・試行の識別子が不正です".into());
        }
        Ok(self
            .root
            .join(run)
            .join("jobs")
            .join(format!("{node}-{attempt}")))
    }

    fn save(&mut self, job: &Job) -> Result<(), String> {
        let file = self
            .directory(&job.run_id, &job.node.id, job.attempt)?
            .join("receipt.json");
        fs::write_json(&file, job)?;
        self.jobs.insert(
            (job.run_id.clone(), job.node.id.clone(), job.attempt),
            job.clone(),
        );
        Ok(())
    }

    fn load(&mut self, run: &str, node: &str, attempt: u32) -> Result<Option<Job>, String> {
        let key = (run.into(), node.into(), attempt);
        if let Some(job) = self.jobs.get(&key) {
            return Ok(Some(job.clone()));
        }
        let path = self.directory(run, node, attempt)?.join("receipt.json");
        let job: Option<Job> = fs::read_json(&path)?;
        if let Some(job) = &job {
            if job.schema_version != 1
                || job.run_id != run
                || job.node.id != node
                || job.attempt != attempt
            {
                return Err("実行receiptの識別子または形式が保存場所と一致しません".into());
            }
            let run_root = self.root.join(run);
            if !job.worktree.starts_with(&run_root) || !job.report.starts_with(&job.worktree) {
                return Err("保存済みworktreeまたは結果ファイルの場所が実行領域外です".into());
            }
            self.jobs.insert(key, job.clone());
        }
        Ok(job)
    }

    pub fn current_target(&self, run: &Run) -> Result<TargetSnapshot, String> {
        let writer = self.root.join(&run.id).join("implementation");
        let path = if writer.exists() {
            &writer
        } else {
            &run.repository
        };
        git::snapshot(path, &run.target.base_sha)
    }

    /// Adopting partially written files is a new, explicit attempt. It is
    /// allowed only after both the receipt and live pane inventory confirm
    /// that the failed writer cannot still change that checkout.
    pub fn retry_target(&mut self, run: &Run, node_id: &str) -> Result<TargetSnapshot, String> {
        let state = run.nodes.get(node_id).ok_or("工程がありません")?;
        if state.state != crate::model::NodeState::Failed
            || run
                .definition
                .node(node_id)
                .is_none_or(|node| node.kind != NodeKind::Implement)
        {
            return Err("確定した実装の失敗だけ対象を引き継いで再試行できます".into());
        }
        let job = self.load(&run.id, node_id, state.attempt)?;
        let writer = self.root.join(&run.id).join("implementation");
        if let Some(job) = job {
            if !matches!(job.phase, Phase::Failed | Phase::Prepared | Phase::Canceled) {
                return Err("前の実装処理の終了が確認できません。先に照合してください".into());
            }
            if writer.exists() {
                let panels = self.panels()?;
                let writer = std::fs::canonicalize(&writer).map_err(|e| e.to_string())?;
                if panels.iter().any(|panel| {
                    panel.exit.is_none()
                        && std::fs::canonicalize(&panel.cwd).ok().as_ref() == Some(&writer)
                }) {
                    return Err(
                        "実装worktreeで生存中のペインがあります。停止を確認してください".into(),
                    );
                }
                if git::common_dir(&writer)? != git::common_dir(&run.repository)? {
                    return Err("実装worktreeのリポジトリが変わっています".into());
                }
            }
        }
        self.current_target(run)
    }

    fn source(&self, run: &Run) -> PathBuf {
        let writer = self.root.join(&run.id).join("implementation");
        if writer.exists() {
            writer
        } else {
            run.repository.clone()
        }
    }

    pub fn execute(
        &mut self,
        run: &Run,
        node: Node,
        attempt: u32,
        request_id: String,
        inputs: Vec<Artifact>,
        target: TargetSnapshot,
    ) -> Observation {
        let node_id = node.id.clone();
        let is_pr = node.kind == NodeKind::PullRequest;
        match self.execute_inner(run, node, attempt, request_id, inputs, target) {
            Ok(observation) => observation,
            Err(message) => {
                // A failed receipt write after spawn is not a failed
                // spawn. The durable intent must be reconciled first.
                match self.load(&run.id, &node_id, attempt) {
                    Ok(Some(job))
                        if matches!(
                            job.phase,
                            Phase::SpawnIntent
                                | Phase::Running
                                | Phase::Finishing
                                | Phase::PrIntent
                                | Phase::CancelIntent
                        ) =>
                    {
                        Observation::Uncertain {
                            external_ref: job.pane_id.map(|id| format!("pane:{id}")),
                            message,
                        }
                    }
                    Err(_) => Observation::Uncertain {
                        external_ref: None,
                        message,
                    },
                    _ => Observation::Failed {
                        message,
                        retryable: is_pr,
                    },
                }
            }
        }
    }

    fn execute_inner(
        &mut self,
        run: &Run,
        node: Node,
        attempt: u32,
        request_id: String,
        inputs: Vec<Artifact>,
        target: TargetSnapshot,
    ) -> Result<Observation, String> {
        if let Some(job) = self.load(&run.id, &node.id, attempt)? {
            if job.request_id != request_id || job.target != target || job.node != node {
                return Err("既存の実行receiptと操作内容が一致しません".into());
            }
            return self.observe_inner(job, true);
        }
        let directory = self.directory(&run.id, &node.id, attempt)?;
        fs::plain_dirs(&directory)?;
        let source = self.source(run);
        if git::common_dir(&source)? != git::common_dir(&run.repository)? {
            return Err("実行元のworktreeが開始時のリポジトリと一致しません".into());
        }
        git::same_snapshot(&source, &target)?;
        if node.kind == NodeKind::Trigger {
            let mut artifact =
                Artifact::new(ArtifactKind::Task, target.digest(), run.objective.clone());
            artifact.id = format!("{request_id}:task");
            artifact.data = serde_json::json!({"objective":run.objective});
            return Ok(Observation::Complete {
                outputs: vec![artifact],
                target: None,
            });
        }
        if node.kind == NodeKind::Approval {
            return Err("承認工程は実行エンジンが管理します".into());
        }
        let run_root = self.root.join(&run.id);
        let worktree = if matches!(node.kind, NodeKind::Implement | NodeKind::PullRequest) {
            run_root.join("implementation")
        } else {
            run_root.join(format!("{}-{attempt}", node.id))
        };
        let report = worktree
            .join(".noa")
            .join("workflow")
            .join(format!("{}-{attempt}", node.id))
            .join("result.json");
        let label = format!("wf:{}:{}:{attempt}", run.id, node.id);
        if label.len() > 128 {
            return Err("実行のラベルが長すぎます".into());
        }
        let mut job = Job {
            schema_version: 1,
            run_id: run.id.clone(),
            node: node.clone(),
            attempt,
            request_id,
            objective: run.objective.clone(),
            target: target.clone(),
            source,
            repository_common: git::common_dir(&run.repository)?,
            worktree,
            report,
            agent: "codex".into(),
            label,
            phase: Phase::Prepared,
            pane_id: None,
            board_id: None,
            server_instance: None,
            prepared_pr: None,
            outputs: Vec::new(),
            result_target: None,
            report_digest: None,
            failure: None,
            worker_stopped: false,
            awaiting_publication: false,
            finish_blocked: None,
            prepare_publication: run
                .definition
                .nodes
                .iter()
                .any(|node| node.kind == NodeKind::PullRequest),
        };
        self.save(&job)?;
        if node.kind == NodeKind::PullRequest {
            if !inputs
                .iter()
                .any(|a| a.kind == ArtifactKind::Approval && a.target_digest == target.digest())
            {
                return Err("対象版に対する承認成果物がありません".into());
            }
            let publication = inputs
                .iter()
                .find(|a| a.kind == ArtifactKind::Approval)
                .and_then(|a| a.data.get("publication"))
                .ok_or("承認済みのPR公開内容がありません")?;
            let prepared: github::PreparedPr = serde_json::from_value(publication.clone())
                .map_err(|_| "承認済みのPR公開内容が不正です")?;
            if prepared.target != target {
                return Err("承認したPRの対象版と現在の対象が一致しません".into());
            }
            job.prepared_pr = Some(prepared.clone());
            job.phase = Phase::PrIntent;
            self.save(&job)?;
            return match github::publish(&job.worktree, &directory, &prepared) {
                Ok(found) => self.pr_found(job, found),
                Err(message) => Ok(Observation::Uncertain {
                    external_ref: None,
                    message,
                }),
            };
        }
        if job.worktree != job.source {
            let branch = format!("noa-workflow/{}", run.id);
            git::prepare(
                &job.source,
                &job.worktree,
                (node.kind == NodeKind::Implement).then_some(branch.as_str()),
                &target,
            )?;
        } else {
            git::same_snapshot(&job.worktree, &target)?;
        }
        if node.kind == NodeKind::Implement {
            // A completed worker is closed before this method returns its
            // Change. Any remaining writer blocks another attempt.
            self.no_previous_writer(&job)?;
        }
        job.agent = if job.node.agent.provider == "auto" {
            // Compatibility for a run saved before explicit assignments existed.
            ipc::detect_agent()?
        } else {
            job.node.agent.provider.clone()
        };
        self.save(&job)?;
        let request_dir = job.report.parent().ok_or("結果ディレクトリがありません")?;
        private_results(&job.worktree)?;
        fs::plain_dirs(request_dir)?;
        let request = request_dir.join("request.md");
        let local_inputs = copy_inputs(request_dir, &inputs)?;
        fs::atomic_write(&request, brief(run, &job, &local_inputs).as_bytes())?;
        self.connect()?;
        let instance = self.connection.as_ref().expect("connected").instance();
        let board = match self.board.clone() {
            Some((known, pane)) if known == instance => pane,
            _ => match self
                .connection
                .as_mut()
                .expect("connected")
                .board(&self.repository)
            {
                Ok(pane) => {
                    self.board = Some((instance.clone(), pane));
                    pane
                }
                Err(error) => {
                    self.lost(&error);
                    return Ok(Observation::Uncertain {
                        external_ref: None,
                        message: error.to_string(),
                    });
                }
            },
        };
        job.board_id = Some(board);
        job.server_instance = Some(instance);
        job.phase = Phase::SpawnIntent;
        self.save(&job)?;
        let spec = ipc::AgentSpec {
            choice: crate::model::AgentChoice {
                provider: job.agent.clone(),
                ..job.node.agent.clone()
            },
            directory: job.worktree.clone(),
            label: job.label.clone(),
            prompt: format!(
                "Read the workflow request at {}. Follow its scope and write the required structured result to {}. Never publish, push, or create a pull request; the workflow owns those operations.",
                request.display(),
                job.report.display()
            ),
            result: job.report.clone(),
        };
        match self
            .connection
            .as_mut()
            .expect("connected")
            .spawn(board, &spec)
        {
            Ok(pane) => {
                job.phase = Phase::Running;
                job.pane_id = Some(pane);
                self.save(&job)?;
                Ok(Observation::Running {
                    external_ref: Some(format!("pane:{pane}")),
                    message: "エージェントを開始しました".into(),
                })
            }
            Err(ipc::Error::Refused(message)) => {
                job.phase = Phase::Failed;
                job.failure = Some(message.clone());
                self.save(&job)?;
                Ok(Observation::Failed {
                    message,
                    retryable: true,
                })
            }
            Err(error) => {
                self.lost(&error);
                Ok(Observation::Uncertain {
                    external_ref: None,
                    message: error.to_string(),
                })
            }
        }
    }

    fn no_previous_writer(&mut self, job: &Job) -> Result<(), String> {
        let panels = self.panels()?;
        let worktree = std::fs::canonicalize(&job.worktree).map_err(|e| e.to_string())?;
        for panel in panels {
            if std::fs::canonicalize(&panel.cwd).ok().as_ref() == Some(&worktree)
                && panel.exit.is_none()
            {
                return Err("同じ実装worktreeで生存中のペインがあります。停止を確認してから再試行してください".into());
            }
        }
        Ok(())
    }

    pub fn observe(&mut self, run: &str, node: &str, attempt: u32, reconcile: bool) -> Observation {
        let result = self.load(run, node, attempt).and_then(|job| match job {
            Some(job) => self.observe_inner(job, reconcile),
            None => Ok(Observation::Uncertain {
                external_ref: None,
                message: "工程の実行receiptがありません。自動で再起動しません".into(),
            }),
        });
        match result {
            Ok(observation) => observation,
            Err(message) => match self.load(run, node, attempt) {
                Ok(Some(mut job)) if matches!(job.phase, Phase::Running | Phase::SpawnIntent) => {
                    job.failure = Some(message.clone());
                    job.phase = Phase::Finishing;
                    self.save(&job)
                        .and_then(|()| self.finish_job(job))
                        .unwrap_or_else(|why| Observation::Uncertain {
                            external_ref: None,
                            message: why,
                        })
                }
                _ => Observation::Uncertain {
                    external_ref: None,
                    message,
                },
            },
        }
    }

    fn observe_inner(&mut self, mut job: Job, reconcile: bool) -> Result<Observation, String> {
        let external_ref = job.pane_id.map(|id| format!("pane:{id}"));
        if job.phase == Phase::Complete {
            validate_saved_evidence(&job.outputs)?;
            return Ok(Observation::Complete {
                outputs: job.outputs,
                target: job.result_target,
            });
        }
        if job.phase == Phase::Finishing {
            if !reconcile && let Some(message) = &job.finish_blocked {
                return Ok(Observation::Uncertain {
                    external_ref,
                    message: message.clone(),
                });
            }
            if job.awaiting_publication && !reconcile {
                return Ok(Observation::Uncertain {
                    external_ref,
                    message: "変更は保存済みです。PRの公開内容を準備するため結果を照合してください"
                        .into(),
                });
            }
            return self.finish_job(job);
        }
        if job.phase == Phase::Canceled {
            return Ok(Observation::Canceled);
        }
        if job.phase == Phase::Failed {
            return Ok(Observation::Failed {
                message: job.failure.unwrap_or_else(|| "工程が失敗しました".into()),
                retryable: false,
            });
        }
        if job.phase == Phase::PrIntent {
            if !reconcile {
                return Ok(Observation::Uncertain {
                    external_ref,
                    message: "PRの結果を照合してください".into(),
                });
            }
            let prepared = job
                .prepared_pr
                .as_ref()
                .ok_or("PRの公開内容が保存されていません")?;
            return match github::reconcile(&job.worktree, prepared) {
                Ok(found) => self.pr_found(job, found),
                Err(message) => Ok(Observation::Uncertain {
                    external_ref,
                    message,
                }),
            };
        }
        if job.phase == Phase::Prepared {
            return Ok(Observation::Uncertain {
                external_ref,
                message: "起動準備中に中断しました。実行を再作成せず、保存状態を確認してください"
                    .into(),
            });
        }
        if job.phase == Phase::CancelIntent {
            return self.cancel_job(job);
        }
        if let Err(message) = self.connect() {
            return Ok(Observation::Uncertain {
                external_ref,
                message,
            });
        }
        let panels = match self.connection.as_mut().expect("connected").panels() {
            Ok(panels) => panels,
            Err(error) => {
                self.lost(&error);
                return Ok(Observation::Uncertain {
                    external_ref,
                    message: error.to_string(),
                });
            }
        };
        let pane = ipc::matched(&panels, &job.label, &job.worktree)?;
        let Some(pane) = pane else {
            return Ok(Observation::Uncertain {
                external_ref,
                message: "実行ペインが見つかりません。再起動せず結果を確認してください".into(),
            });
        };
        if job.pane_id != Some(pane.pane_id.0) || job.phase == Phase::SpawnIntent {
            job.pane_id = Some(pane.pane_id.0);
            job.server_instance = Some(self.connection.as_ref().expect("connected").instance());
            job.phase = Phase::Running;
            self.save(&job)?;
        }
        let agent = pane.agent.as_ref();
        let state = agent.and_then(|agent| agent.state.as_deref());
        if matches!(state, Some("permission" | "input")) {
            let message = agent
                .and_then(|agent| agent.detail.clone())
                .unwrap_or_else(|| "エージェントからの入力要求があります".into());
            return Ok(Observation::Waiting {
                external_ref: Some(format!("pane:{}", pane.pane_id.0)),
                message,
            });
        }
        if state == Some("error")
            || pane
                .exit
                .as_ref()
                .is_some_and(|exit| exit.code.is_some_and(|c| c != 0))
        {
            job.phase = Phase::Finishing;
            job.failure = Some("エージェントが失敗を報告しました".into());
            self.save(&job)?;
            return self.finish_job(job);
        }
        let complete = state == Some("finished")
            || pane.exit.as_ref().is_some_and(|exit| exit.code == Some(0));
        let Some(bytes) = fs::read(&job.report, fs::MAX_REPORT_BYTES)? else {
            return Ok(if complete {
                Observation::Waiting {
                    external_ref,
                    message:
                        "エージェントは終了しましたが、構造化された成果物をまだ受領していません"
                            .into(),
                }
            } else {
                Observation::Running {
                    external_ref,
                    message: "エージェントが作業中です".into(),
                }
            });
        };
        if !complete {
            return Ok(Observation::Running {
                external_ref,
                message: "成果物を受領しました。エージェントの終了を確認中です".into(),
            });
        }
        let digest = fs::hash(&bytes);
        if job.report_digest.as_ref() != Some(&digest) {
            job.report_digest = Some(digest);
            self.save(&job)?;
            return Ok(Observation::Running {
                external_ref,
                message: "成果物の書き込み完了と対象版を確認中です".into(),
            });
        }
        let report: AgentReport =
            serde_json::from_slice(&bytes).map_err(|e| format!("成果物の形式が不正です: {e}"))?;
        validate_identity(&job, &report)?;
        if git::common_dir(&job.worktree)? != job.repository_common {
            return Err("成果物のworktreeが実行開始時と異なるリポジトリを指しています".into());
        }
        if job.node.kind == NodeKind::Implement
            && git::branch(&job.worktree)? != format!("noa-workflow/{}", job.run_id)
        {
            return Err(
                "実装worktreeのブランチがWorkflowの作成したブランチから変わっています".into(),
            );
        }
        let target = git::snapshot(&job.worktree, &job.target.base_sha)?;
        if job.node.kind != NodeKind::Implement && target != job.target {
            return Err(
                "読取・検証工程で対象ファイルが変更されました。結果を合格として扱いません".into(),
            );
        }
        let artifacts = collect_artifacts(&job, &report, &target)?;
        job.outputs = artifacts;
        job.result_target = (job.node.kind == NodeKind::Implement).then_some(target);
        if !report.passed {
            job.failure = Some(report.summary);
        }
        // Persist the accepted receipt before stopping the worker so a
        // crash between close and acknowledgement can finish on reopen.
        job.phase = Phase::Finishing;
        self.save(&job)?;
        self.finish_job(job)
    }

    fn finish_job(&mut self, mut job: Job) -> Result<Observation, String> {
        if !job.worker_stopped {
            let panels = self.panels()?;
            if let Some(pane) = ipc::matched(&panels, &job.label, &job.worktree)? {
                match self
                    .connection
                    .as_mut()
                    .expect("connected")
                    .close(pane.pane_id.0)
                {
                    Ok(()) => {}
                    Err(error) => {
                        self.lost(&error);
                        return Ok(Observation::Uncertain { external_ref: Some(format!("pane:{}", pane.pane_id.0)), message: "成果物は保存しましたがペインの停止を確認できません。結果を照合してください".into() });
                    }
                }
            }
            job.worker_stopped = true;
            self.save(&job)?;
        }
        // The worker has stopped before this final target/evidence check;
        // a last write between its finished signal and close cannot sneak
        // into a successful Change or quality verdict.
        if job.failure.is_none() {
            if git::common_dir(&job.worktree)? != job.repository_common {
                return Err("保存済み成果物のリポジトリ識別情報が変わっています".into());
            }
            if job.node.kind == NodeKind::Implement
                && git::branch(&job.worktree)? != format!("noa-workflow/{}", job.run_id)
            {
                return Err("実装成果のブランチが受領時から変わっています".into());
            }
            validate_saved_evidence(&job.outputs)?;
            git::same_snapshot(
                &job.worktree,
                job.result_target.as_ref().unwrap_or(&job.target),
            )?;
        }
        if let Some(message) = job.failure.clone() {
            job.phase = Phase::Failed;
            self.save(&job)?;
            Ok(Observation::Failed {
                message,
                retryable: false,
            })
        } else {
            if job.node.kind == NodeKind::Implement {
                let directory = self.directory(&job.run_id, &job.node.id, job.attempt)?;
                if let Err(message) = capture_change_diff(&mut job, &directory) {
                    let message = format!(
                        "実装成果は保存済みですが、変更差分を表示できません。成功として進めず、結果の照合を待っています: {message}"
                    );
                    job.finish_blocked = Some(message.clone());
                    self.save(&job)?;
                    return Ok(Observation::Uncertain {
                        external_ref: None,
                        message,
                    });
                }
                job.finish_blocked = None;
                self.save(&job)?;
            }
            if job.node.kind == NodeKind::Implement
                && job.prepare_publication
                && job
                    .outputs
                    .first()
                    .is_some_and(|a| a.data.get("publication").is_none())
            {
                let target = job
                    .result_target
                    .as_ref()
                    .ok_or("実装成果の変更版がありません")?;
                let operation_id = format!("{}-publication-{}", job.run_id, &target.digest()[..16]);
                match github::prepare(&job.worktree, &operation_id, &job.objective, target) {
                    Ok(prepared) => {
                        job.outputs[0].data["publication"] =
                            serde_json::to_value(prepared).map_err(|e| e.to_string())?;
                        job.awaiting_publication = false;
                    }
                    Err(message) => {
                        job.awaiting_publication = true;
                        self.save(&job)?;
                        return Ok(Observation::Uncertain {
                            external_ref: None,
                            message: format!(
                                "実装成果は保存し、作業ペインを停止しました。公開内容の準備を再確認してください: {message}"
                            ),
                        });
                    }
                }
            }
            job.phase = Phase::Complete;
            self.save(&job)?;
            Ok(Observation::Complete {
                outputs: job.outputs,
                target: job.result_target,
            })
        }
    }

    pub fn validate_evidence(&self, run: &Run) -> Result<(), String> {
        for node in run
            .nodes
            .values()
            .filter(|node| node.state == crate::model::NodeState::Succeeded)
        {
            validate_saved_evidence(&node.outputs)?;
        }
        Ok(())
    }

    fn pr_found(&mut self, mut job: Job, found: github::Found) -> Result<Observation, String> {
        match found {
            github::Found::Absent => Ok(Observation::Uncertain {
                external_ref: None,
                message: "同じ操作のPRはまだ確認できません。重複作成を避けるため自動再送しません"
                    .into(),
            }),
            github::Found::Present { url, number } => {
                let mut artifact = Artifact::new(
                    ArtifactKind::PullRequest,
                    job.target.digest(),
                    format!("Draft PR #{number}"),
                );
                artifact.id = format!("{}:pr", job.request_id);
                artifact.data =
                    serde_json::json!({"url":url,"number":number,"request_id":job.request_id});
                job.outputs = vec![artifact];
                job.phase = Phase::Complete;
                self.save(&job)?;
                Ok(Observation::Complete {
                    outputs: job.outputs,
                    target: None,
                })
            }
        }
    }

    pub fn cancel(&mut self, run: &str, node: &str, attempt: u32) -> Observation {
        let result = self.load(run, node, attempt).and_then(|job| match job {
            Some(job) => self.cancel_job(job),
            None => Ok(Observation::Canceled),
        });
        result.unwrap_or_else(|message| Observation::Uncertain {
            external_ref: None,
            message,
        })
    }

    fn cancel_job(&mut self, mut job: Job) -> Result<Observation, String> {
        if job.phase == Phase::PrIntent {
            return Ok(Observation::Uncertain {
                external_ref: None,
                message:
                    "公開済みかもしれないPR操作は取り消し扱いにできません。先に照合してください"
                        .into(),
            });
        }
        if matches!(job.phase, Phase::Prepared | Phase::Canceled)
            || (job.phase == Phase::Failed && job.pane_id.is_none())
        {
            job.phase = Phase::Canceled;
            self.save(&job)?;
            return Ok(Observation::Canceled);
        }
        job.phase = Phase::CancelIntent;
        self.save(&job)?;
        let panels = self.panels()?;
        if let Some(pane) = ipc::matched(&panels, &job.label, &job.worktree)? {
            // Persist the observation before closing: a lost close response
            // can then be reconciled by the worker's absence after restart.
            job.pane_id = Some(pane.pane_id.0);
            job.server_instance = Some(self.connection.as_ref().expect("connected").instance());
            self.save(&job)?;
            self.close_pane(pane.pane_id.0)?;
        } else if job.pane_id.is_none() {
            // A timed-out spawn may still be queued on Noa's UI thread.
            // Until its worker has been seen, absence is not cancellation.
            return Ok(Observation::Uncertain {
                external_ref: None,
                message: "起動要求の結果が未確認のため、停止完了を確認できません。結果を照合してください"
                    .into(),
            });
        }
        job.phase = Phase::Canceled;
        self.save(&job)?;
        Ok(Observation::Canceled)
    }

    pub fn focus(&mut self, run: &str, node: &str, attempt: u32) -> Result<(), String> {
        let job = self
            .load(run, node, attempt)?
            .ok_or("工程の実行記録がありません")?;
        let panels = self.panels()?;
        let pane =
            ipc::matched(&panels, &job.label, &job.worktree)?.ok_or("実行ペインがありません")?;
        self.connection
            .as_mut()
            .expect("connected")
            .focus(pane.pane_id.0)
            .map_err(|e| e.to_string())
    }

    pub fn answer(
        &mut self,
        run: &str,
        node: &str,
        attempt: u32,
        answer: &str,
    ) -> Result<(), String> {
        let job = self
            .load(run, node, attempt)?
            .ok_or("工程の実行記録がありません")?;
        let panels = self.panels()?;
        let pane =
            ipc::matched(&panels, &job.label, &job.worktree)?.ok_or("実行ペインがありません")?;
        self.connection
            .as_mut()
            .expect("connected")
            .answer(pane, answer)
            .map_err(|e| e.to_string())
    }
}

fn validate_identity(job: &Job, report: &AgentReport) -> Result<(), String> {
    if report.schema_version != 1
        || report.run_id != job.run_id
        || report.node_id != job.node.id
        || report.attempt != job.attempt
        || report.request_id != job.request_id
        || report.target_digest != job.target.digest()
    {
        return Err("成果物が現在の実行・工程・試行・対象版に一致しません。以前の成功結果は再利用できません".into());
    }
    if report.summary.trim().is_empty()
        || report.summary.len() > 32 * 1024
        || report.files.is_empty()
        || report.files.len() > 32
        || report.checks.len() > 64
    {
        return Err("成果物の要約・証跡の件数が不正です".into());
    }
    if job.node.kind == NodeKind::Verify && report.checks.is_empty() {
        return Err("検証結果には実行したチェックとログが必要です".into());
    }
    if report.passed && report.checks.iter().any(|check| check.exit_code != 0) {
        return Err("失敗したチェックを含む成果物を合格にはできません".into());
    }
    Ok(())
}

fn capture_change_diff(job: &mut Job, directory: &Path) -> Result<(), String> {
    let target = job
        .result_target
        .as_ref()
        .ok_or("実装成果の対象版がありません")?;
    let artifact = job.outputs.first_mut().ok_or("実装成果がありません")?;
    if artifact.data["evidence"].as_array().is_some_and(|files| {
        files
            .iter()
            .any(|file| file["source"] == "captured_git_diff")
    }) {
        return Ok(());
    }
    let bytes = git::committed_diff(&job.worktree, target)?;
    // Kept with the durable attempt receipt, outside the agent's result
    // directory, so this cannot overwrite one of its submitted files.
    let path = directory.join("changes.patch");
    fs::atomic_write(&path, &bytes)?;
    let item = serde_json::json!({
        "path": path,
        "sha256": fs::hash(&bytes),
        "source": "captured_git_diff",
        "scope": "committed_changes",
        "base_sha": target.base_sha,
        "head_sha": target.head_sha,
        "bytes": bytes.len()
    });
    if artifact.data["evidence"].is_null() {
        artifact.data["evidence"] = serde_json::json!([]);
    }
    artifact.data["evidence"]
        .as_array_mut()
        .ok_or("実装成果の証跡一覧が不正です")?
        .push(item);
    Ok(())
}

fn evidence_file(base: &Path, relative: &str) -> Result<(PathBuf, String), String> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("証跡ファイルは結果ディレクトリ内の相対パスで指定してください".into());
    }
    let path = base.join(relative);
    let bytes = fs::read(&path, 8 * 1024 * 1024)?.ok_or("証跡ファイルがありません")?;
    if bytes.is_empty() {
        return Err("空の証跡ファイルは受領できません".into());
    }
    Ok((path, fs::hash(&bytes)))
}

fn private_results(worktree: &Path) -> Result<(), String> {
    let root = worktree.join(".noa/workflow");
    fs::plain_dirs(&root)?;
    let ignore = root.join(".gitignore");
    if !ignore.exists() {
        fs::atomic_write(&ignore, b"*\n")?;
    }
    git::command(
        worktree,
        &["check-ignore", "--quiet", "--", ".noa/workflow/probe"],
    )
    .map_err(|_| {
        "成果物ディレクトリがGitに無視されていません。.noa/workflowの設定を確認してください"
            .to_owned()
    })?;
    Ok(())
}

fn copy_inputs(directory: &Path, inputs: &[Artifact]) -> Result<Vec<Artifact>, String> {
    validate_saved_evidence(inputs)?;
    let mut copied = inputs.to_vec();
    for (index, input) in copied.iter_mut().enumerate() {
        let local = directory.join("inputs").join(index.to_string());
        if let Some(path) = &input.path {
            let bytes =
                fs::read(path, fs::MAX_REPORT_BYTES)?.ok_or("入力成果物が見つかりません")?;
            let output = local.join("result.json");
            fs::atomic_write(&output, &bytes)?;
            input.path = Some(output);
        }
        if let Some(files) = input
            .data
            .get_mut("evidence")
            .and_then(serde_json::Value::as_array_mut)
        {
            for (file_index, file) in files.iter_mut().enumerate() {
                let original =
                    PathBuf::from(file["path"].as_str().ok_or("入力証跡のパスが不正です")?);
                let bytes =
                    fs::read(&original, 8 * 1024 * 1024)?.ok_or("入力証跡が見つかりません")?;
                let output = local.join(format!("evidence-{file_index}.txt"));
                fs::atomic_write(&output, &bytes)?;
                file["path"] = serde_json::json!(output);
            }
        }
    }
    Ok(copied)
}

fn validate_saved_evidence(artifacts: &[Artifact]) -> Result<(), String> {
    for artifact in artifacts {
        if let (Some(path), Some(want)) = (&artifact.path, artifact.data["report_sha256"].as_str())
        {
            let bytes =
                fs::read(path, fs::MAX_REPORT_BYTES)?.ok_or("保存済みの報告書がありません")?;
            if fs::hash(&bytes) != want {
                return Err("保存済み報告書の内容が受領時から変わりました".into());
            }
        }
        if let Some(evidence) = artifact.data["evidence"].as_array() {
            for file in evidence {
                let path = PathBuf::from(
                    file["path"]
                        .as_str()
                        .ok_or("保存済み証跡のパスが不正です")?,
                );
                let want = file["sha256"]
                    .as_str()
                    .ok_or("保存済み証跡の識別子がありません")?;
                let bytes = fs::read(&path, 8 * 1024 * 1024)?.ok_or("保存済み証跡がありません")?;
                if fs::hash(&bytes) != want {
                    return Err(
                        "証跡の内容が受領時から変わりました。以前の承認を使用しません".into(),
                    );
                }
            }
        }
    }
    Ok(())
}

fn collect_artifacts(
    job: &Job,
    report: &AgentReport,
    target: &TargetSnapshot,
) -> Result<Vec<Artifact>, String> {
    let base = job.report.parent().ok_or("結果の保存先がありません")?;
    let mut evidence = Vec::new();
    for file in &report.files {
        let (path, digest) = evidence_file(base, &file.path)?;
        evidence.push(serde_json::json!({"path":path,"sha256":digest}));
    }
    for check in &report.checks {
        if check.criterion.trim().is_empty() || check.command.is_empty() || check.command.len() > 64
        {
            return Err("検証チェックの基準・実行引数が不正です".into());
        }
        let (path, digest) = evidence_file(base, &check.log)?;
        evidence.push(serde_json::json!({"path":path,"sha256":digest,"criterion":check.criterion,"argv":check.command,"exit_code":check.exit_code,"source":"agent_report"}));
    }
    let mut artifact = Artifact::new(
        job.node.kind.output_kind(),
        target.digest(),
        report.summary.clone(),
    );
    artifact.id = format!("{}:result", job.request_id);
    artifact.path = Some(job.report.clone());
    artifact.data = serde_json::json!({"passed":report.passed,"worktree":job.worktree,"evidence":evidence,"request_id":job.request_id,"source":"structured_agent_report","report_sha256":job.report_digest});
    Ok(vec![artifact])
}

fn brief(run: &Run, job: &Job, inputs: &[Artifact]) -> String {
    let policy = match job.node.kind {
        NodeKind::Plan => {
            "Create a concrete plan and acceptance criteria. Do not modify project files. Write your plan as plan.md beside result.json."
        }
        NodeKind::Implement => {
            "Implement the requested change in this isolated worktree. Read all AGENTS.md instructions. Commit the completed change on the current noa-workflow branch, with checks and a clear report. Never push or create a PR. Write change.md beside result.json."
        }
        NodeKind::Review => {
            "Review the exact supplied target independently. Do not modify or commit project files. Record concrete findings and locations in review.md beside result.json. passed may be true only when no blocking findings remain."
        }
        NodeKind::Verify => {
            "Verify the exact supplied target against its acceptance criteria. Run the relevant project checks; do not modify or commit project files. Save actual command logs beside result.json and describe each criterion and exit code. passed may be true only when every required criterion was checked and passed; missing verification is not success."
        }
        _ => "Follow the requested node's scope.",
    };
    let sample = AgentReport {
        schema_version: 1,
        run_id: job.run_id.clone(),
        node_id: job.node.id.clone(),
        attempt: job.attempt,
        request_id: job.request_id.clone(),
        target_digest: job.target.digest(),
        passed: false,
        summary: "Describe the observed result; set passed only when supported by the files."
            .into(),
        files: vec![ReportFile {
            path: "report.md".into(),
        }],
        checks: Vec::new(),
    };
    format!(
        "# Noa Workflow request\n\n## Objective\n{}\n\n## Scope\n{policy}\n\n{}\n\nAll file changes must remain in this worktree. Do not publish, push, merge, delete a branch/worktree, or use an external issue write API. Agent permission prompts remain under the person's control.\n\n## Exact input target\n```json\n{}\n```\n\n## Inputs\n```json\n{}\n```\n\n## Previous repair feedback\n{}\n\n## Required result\nWrite a complete UTF-8 JSON object atomically to {}. Use this schema exactly; preserve all identity fields. files and checks use paths relative to the result file's directory, with no symlinks. For Verify, checks must include at least one actual executed command with criterion, command (argv array), exit_code and log (path). The runner validates the receipt and target; an agent finished notification alone does not complete the node.\n```json\n{}\n```\n",
        run.objective,
        job.node.instruction,
        serde_json::to_string_pretty(&job.target).unwrap_or_default(),
        serde_json::to_string_pretty(inputs).unwrap_or_default(),
        serde_json::to_string_pretty(&run.repair_context).unwrap_or_default(),
        job.report.display(),
        serde_json::to_string_pretty(&sample).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct CancellationBackend {
        panels: Mutex<Vec<noa_ipc::Panel>>,
        closed: Mutex<Vec<u64>>,
        uncertain_close: AtomicBool,
    }

    impl noa_ipc::IpcBackend for CancellationBackend {
        fn list_panels(&self) -> Arc<Vec<noa_ipc::Panel>> {
            Arc::new(self.panels.lock().unwrap().clone())
        }
        fn get_text(
            &self,
            _: u64,
            _: noa_ipc::TextSource,
            _: usize,
        ) -> Result<noa_ipc::TextResult, noa_ipc::IpcError> {
            Err(noa_ipc::IpcError::UnknownPane)
        }
        fn get_grid(&self, _: u64, _: u64, _: u64) -> Result<noa_ipc::GridResult, noa_ipc::IpcError> {
            Err(noa_ipc::IpcError::UnknownPane)
        }
        fn send_text(
            &self,
            _: u64,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<(), noa_ipc::IpcError> {
            panic!("cancellation must not send input")
        }
        fn focus_pane(&self, _: u64) -> Result<(), noa_ipc::IpcError> {
            Err(noa_ipc::IpcError::UnknownPane)
        }
        fn new_tab(&self, _: Option<u64>) -> Result<u64, noa_ipc::IpcError> {
            panic!("cancellation must not spawn a tab")
        }
        fn split(&self, _: u64, _: noa_ipc::SplitDirection) -> Result<u64, noa_ipc::IpcError> {
            panic!("cancellation must not spawn a split")
        }
        fn close_pane(&self, pane: u64) -> Result<(), noa_ipc::IpcError> {
            self.panels.lock().unwrap().retain(|p| p.pane_id.0 != pane);
            self.closed.lock().unwrap().push(pane);
            if self.uncertain_close.swap(false, Ordering::SeqCst) {
                return Err(noa_ipc::IpcError::OutcomeUnknown("close timed out".into()));
            }
            Ok(())
        }
    }

    fn cancellation_server() -> (Arc<CancellationBackend>, noa_ipc::ServerHandle) {
        let backend = Arc::new(CancellationBackend::default());
        let server = noa_ipc::Server::start(
            noa_ipc::ServerConfig {
                port: 0,
                bind_addr: noa_ipc::ServerConfig::DEFAULT_BIND_ADDR,
                token: "workflow-cancel-test".into(),
                allowed_scopes: noa_ipc::ScopeSet::from_strings(["read", "control"]),
                hello_deadline: noa_ipc::ServerConfig::DEFAULT_HELLO_DEADLINE,
                handshake_timeout: noa_ipc::ServerConfig::DEFAULT_HANDSHAKE_TIMEOUT,
            },
            backend.clone(),
            noa_ipc::Broadcaster::new(),
        )
        .unwrap();
        (backend, server)
    }

    fn cancellation_connection(server: &noa_ipc::ServerHandle) -> ipc::Ipc {
        ipc::Ipc::from_client(
            noa_ipc::Client::connect(
                &format!("ws://127.0.0.1:{}/", server.port()),
                "workflow-cancel-test",
                noa_ipc::ScopeSet::from_strings(["read", "control"]),
            )
            .unwrap(),
        )
    }

    fn worker_panel(job: &Job) -> noa_ipc::Panel {
        serde_json::from_value(serde_json::json!({
            "windowGroupId": "1", "windowId": "1", "paneId": "7",
            "name": "worker", "cwd": job.worktree, "label": job.label,
            "busy": false, "attention": false, "preview": []
        }))
        .unwrap()
    }

    fn job(root: &Path) -> Job {
        let worktree = root.join("run").join("verify-1");
        Job {
            schema_version: 1,
            run_id: "run".into(),
            node: Node::new("verify", NodeKind::Verify, 0.0, 0.0),
            attempt: 1,
            request_id: "run-verify-1".into(),
            objective: "test".into(),
            target: TargetSnapshot {
                base_sha: "a".repeat(40),
                head_sha: "b".repeat(40),
                dirty_digest: fs::hash(&[]),
            },
            source: root.join("source"),
            repository_common: root.join("source/.git"),
            report: worktree.join(".noa/workflow/verify-1/result.json"),
            worktree,
            agent: "codex".into(),
            label: "wf:run:verify:1".into(),
            phase: Phase::Prepared,
            pane_id: None,
            board_id: None,
            server_instance: None,
            prepared_pr: None,
            outputs: vec![],
            result_target: None,
            report_digest: None,
            failure: None,
            worker_stopped: false,
            awaiting_publication: false,
            finish_blocked: None,
            prepare_publication: false,
        }
    }

    #[test]
    fn evidence_paths_do_not_reach_outside_the_attempt() {
        for path in ["../old/result", "/tmp/result", "a/../../b", ""] {
            assert!(evidence_file(Path::new("/nonexistent"), path).is_err());
        }
    }

    #[test]
    fn unknown_report_fields_do_not_silently_change_the_contract() {
        let raw = r#"{"schema_version":1,"run_id":"run","node_id":"verify","attempt":1,"request_id":"r","target_digest":"a","passed":true,"summary":"ok","files":[],"checks":[],"skip_validation":true}"#;
        assert!(serde_json::from_str::<AgentReport>(raw).is_err());
    }

    #[test]
    fn stale_reports_and_unchecked_success_are_refused() {
        let job = job(Path::new("/tmp/workflow"));
        let mut report = AgentReport {
            schema_version: 1,
            run_id: job.run_id.clone(),
            node_id: job.node.id.clone(),
            attempt: 1,
            request_id: job.request_id.clone(),
            target_digest: job.target.digest(),
            passed: true,
            summary: "Verified".into(),
            files: vec![ReportFile {
                path: "report.md".into(),
            }],
            checks: vec![ReportCheck {
                criterion: "tests".into(),
                command: vec!["cargo".into(), "test".into()],
                exit_code: 0,
                log: "tests.log".into(),
            }],
        };
        assert!(validate_identity(&job, &report).is_ok());
        report.attempt = 2;
        assert!(validate_identity(&job, &report).is_err());
        report.attempt = 1;
        report.target_digest = "another revision".into();
        assert!(validate_identity(&job, &report).is_err());
        report.target_digest = job.target.digest();
        report.checks[0].exit_code = 1;
        assert!(validate_identity(&job, &report).is_err());
        report.checks.clear();
        assert!(validate_identity(&job, &report).is_err());
    }

    #[test]
    fn an_interrupted_preparation_is_observed_without_starting_an_agent() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let mut adapters = Adapters::new(root.join("repository"), root.join("runs")).unwrap();
        let job = job(&adapters.root);
        adapters.save(&job).unwrap();
        drop(adapters);
        let mut reopened = Adapters::new(root.join("repository"), root.join("runs")).unwrap();
        let observed = reopened.observe("run", "verify", 1, true);
        assert!(matches!(observed, Observation::Uncertain { .. }));
        assert!(!reopened.connected());
        assert_eq!(
            reopened.load("run", "verify", 1).unwrap().unwrap().phase,
            Phase::Prepared
        );
    }

    #[test]
    fn cancellation_waits_for_an_unconfirmed_spawn_across_restarts() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let (backend, server) = cancellation_server();
        let mut adapters = Adapters::new(root.join("repository"), root.join("runs")).unwrap();
        adapters.connection = Some(cancellation_connection(&server));
        let mut job = job(&adapters.root);
        job.phase = Phase::SpawnIntent;
        fs::plain_dirs(&job.worktree).unwrap();
        adapters.save(&job).unwrap();

        for _ in 0..2 {
            assert!(matches!(
                adapters.cancel("run", "verify", 1),
                Observation::Uncertain { .. }
            ));
            let saved = adapters.load("run", "verify", 1).unwrap().unwrap();
            assert_eq!(saved.phase, Phase::CancelIntent);
            assert_eq!(saved.pane_id, None);
        }
        drop(adapters);

        let mut reopened = Adapters::new(root.join("repository"), root.join("runs")).unwrap();
        reopened.connection = Some(cancellation_connection(&server));
        assert!(matches!(
            reopened.observe("run", "verify", 1, true),
            Observation::Uncertain { .. }
        ));

        // Noa finally executes the spawn that timed out before cancellation.
        backend.panels.lock().unwrap().push(worker_panel(&job));
        assert!(matches!(
            reopened.observe("run", "verify", 1, true),
            Observation::Canceled
        ));
        let saved = reopened.load("run", "verify", 1).unwrap().unwrap();
        assert_eq!(saved.phase, Phase::Canceled);
        assert_eq!(saved.pane_id, Some(7));
        assert!(backend.panels.lock().unwrap().is_empty());
        assert_eq!(*backend.closed.lock().unwrap(), vec![7]);
        assert!(matches!(
            reopened.observe("run", "verify", 1, true),
            Observation::Canceled
        ));
        assert_eq!(*backend.closed.lock().unwrap(), vec![7]);
    }

    #[test]
    fn cancellation_records_a_late_worker_before_an_uncertain_close() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let (backend, server) = cancellation_server();
        let mut adapters = Adapters::new(root.join("repository"), root.join("runs")).unwrap();
        adapters.connection = Some(cancellation_connection(&server));
        let mut job = job(&adapters.root);
        job.phase = Phase::CancelIntent;
        fs::plain_dirs(&job.worktree).unwrap();
        adapters.save(&job).unwrap();
        backend.panels.lock().unwrap().push(worker_panel(&job));
        backend.uncertain_close.store(true, Ordering::SeqCst);

        assert!(matches!(
            adapters.observe("run", "verify", 1, true),
            Observation::Uncertain { .. }
        ));
        let saved = adapters.load("run", "verify", 1).unwrap().unwrap();
        assert_eq!(saved.phase, Phase::CancelIntent);
        assert_eq!(saved.pane_id, Some(7));
        assert!(backend.panels.lock().unwrap().is_empty());
        drop(adapters);

        let mut reopened = Adapters::new(root.join("repository"), root.join("runs")).unwrap();
        reopened.connection = Some(cancellation_connection(&server));
        assert!(matches!(
            reopened.observe("run", "verify", 1, true),
            Observation::Canceled
        ));
        assert_eq!(
            reopened.load("run", "verify", 1).unwrap().unwrap().phase,
            Phase::Canceled
        );
        assert_eq!(*backend.closed.lock().unwrap(), vec![7]);
    }

    #[test]
    fn a_definitely_refused_spawn_can_be_canceled_without_reconnecting() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let mut adapters = Adapters::new(root.join("repository"), root.join("runs")).unwrap();
        let mut job = job(&adapters.root);
        job.phase = Phase::Failed;
        job.failure = Some("worker limit reached".into());
        adapters.save(&job).unwrap();
        assert!(matches!(
            adapters.cancel("run", "verify", 1),
            Observation::Canceled
        ));
        assert!(!adapters.connected());
        assert_eq!(
            adapters.load("run", "verify", 1).unwrap().unwrap().phase,
            Phase::Canceled
        );
    }

    #[test]
    fn evidence_changes_after_receipt_are_detected_before_approval() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let path = root.join("check.log");
        std::fs::write(&path, "passed\n").unwrap();
        let mut artifact = Artifact::new(ArtifactKind::Evidence, "target", "checked");
        artifact.data =
            serde_json::json!({"evidence":[{"path":path,"sha256":fs::hash(b"passed\n")} ]});
        assert!(validate_saved_evidence(&[artifact.clone()]).is_ok());
        std::fs::write(&path, "failed\n").unwrap();
        assert!(validate_saved_evidence(&[artifact]).is_err());
    }

    fn stopped_implementation(root: &Path) -> (Adapters, Job) {
        let repository = root.join("repository");
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
        let target = git::snapshot(&repository, &git::head(&repository).unwrap()).unwrap();
        let mut adapters = Adapters::new(repository.clone(), root.join("runs")).unwrap();
        let mut job = job(&adapters.root);
        job.node = Node::new("implement", NodeKind::Implement, 0.0, 0.0);
        job.request_id = "run-implement-1".into();
        job.label = "wf:run:implement:1".into();
        job.target = target.clone();
        job.source = repository.clone();
        job.repository_common = git::common_dir(&repository).unwrap();
        job.worktree = adapters.root.join("run/implementation");
        job.report = job.worktree.join(".noa/workflow/implement-1/result.json");
        git::prepare(
            &repository,
            &job.worktree,
            Some("noa-workflow/run"),
            &target,
        )
        .unwrap();
        std::fs::write(job.worktree.join("README.md"), "implemented\n").unwrap();
        git::command(&job.worktree, &["add", "README.md"]).unwrap();
        git::command(&job.worktree, &["commit", "-qm", "implemented"]).unwrap();
        let result_target = git::snapshot(&job.worktree, &target.base_sha).unwrap();
        private_results(&job.worktree).unwrap();
        fs::atomic_write(&job.report, b"{\"accepted\":true}").unwrap();
        let mut artifact =
            Artifact::new(ArtifactKind::Change, result_target.digest(), "implemented");
        artifact.path = Some(job.report.clone());
        artifact.data = serde_json::json!({
            "report_sha256": fs::hash(b"{\"accepted\":true}"),
            "passed": true
        });
        job.outputs = vec![artifact];
        job.result_target = Some(result_target);
        job.phase = Phase::Finishing;
        job.worker_stopped = true;
        adapters.save(&job).unwrap();
        (adapters, job)
    }

    #[test]
    fn local_completion_needs_no_github_and_rechecks_the_stopped_writers_target() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let (mut adapters, job) = stopped_implementation(&root);
        // There is intentionally no origin or running Noa service.
        let Observation::Complete { outputs, .. } = adapters.observe("run", "implement", 1, true)
        else {
            panic!("stopped local implementation should complete without a remote");
        };
        let diff = &outputs[0].data["evidence"][0];
        assert_eq!(diff["source"], "captured_git_diff");
        let patch = std::fs::read_to_string(diff["path"].as_str().unwrap()).unwrap();
        assert!(patch.contains("+implemented"));
        assert_eq!(diff["sha256"], fs::hash(patch.as_bytes()));
        assert!(!adapters.connected());

        let mut changed = job;
        changed.phase = Phase::Finishing;
        adapters.save(&changed).unwrap();
        std::fs::write(changed.worktree.join("README.md"), "late change\n").unwrap();
        assert!(matches!(
            adapters.observe("run", "implement", 1, true),
            Observation::Uncertain { .. }
        ));
    }

    #[test]
    fn interrupted_publication_preparation_keeps_the_accepted_change_without_respawning() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let (mut adapters, mut job) = stopped_implementation(&root);
        job.prepare_publication = true;
        adapters.save(&job).unwrap();
        // Missing origin fails locally, before any GitHub request.
        assert!(matches!(
            adapters.observe("run", "implement", 1, true),
            Observation::Uncertain { .. }
        ));
        drop(adapters);
        let mut reopened = Adapters::new(root.join("repository"), root.join("runs")).unwrap();
        assert!(matches!(
            reopened.observe("run", "implement", 1, false),
            Observation::Uncertain { .. }
        ));
        let saved = reopened.load("run", "implement", 1).unwrap().unwrap();
        assert!(saved.worker_stopped && saved.awaiting_publication);
        assert_eq!(saved.phase, Phase::Finishing);
        assert_eq!(saved.outputs[0].summary, job.outputs[0].summary);
        assert_eq!(saved.outputs[0].path, job.outputs[0].path);
        assert_eq!(
            saved.outputs[0].data["evidence"][0]["source"],
            "captured_git_diff"
        );
        assert!(validate_saved_evidence(&saved.outputs).is_ok());
        assert_eq!(saved.request_id, job.request_id);
        assert!(!reopened.connected());
    }
}
