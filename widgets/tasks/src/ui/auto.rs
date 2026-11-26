//! 「自動で割り当て」 in the 実行 form (`docs/specs/auto-assign.md`, U6): an
//! installed agent CLI reads the task and picks the one agent that runs it,
//! with a reason. The pick only selects the form's エージェント segment; the
//! person still presses 実行, which records the pick on the run's
//! `RunStarted` (`auto`).
//!
//! Tasks has no model or effort (spec Q3): every candidate has empty
//! lists, so the planner may only choose the agent.
//!
//! The planner reads the task as it is when a planner is chosen, not when
//! the form opened: an issue's body loaded since is in the request.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::TryRecvError;
use std::time::{SystemTime, UNIX_EPOCH};

use noa_gpui_core::widgets::{
    Button, ButtonClicked, Container, ControlSize, Label, MenuChosen, MenuItem, PullDownButton,
};
use noa_gpui_core::{Message, Role, UiTree, WidgetId};

use tasks::auto_assign::{
    self, Assignment, AutoRecord, Candidate, Job, Pick, PlanError, Planner, Planning, Request, Slot,
};
use tasks::model::types::{AgentKind, Millis, Task, TaskRef};

use super::{View, style};

/// The 実行 form's one slot.
pub(super) const WORKER: &str = "worker";
/// The form's caption for that slot, used in the assignment block.
const WORKER_LABEL: &str = "エージェント";
/// The form's start button.
const START_LABEL: &str = "実行";
const WIDGET: &str = "tasks";
const RUN: &str = "one agent does one task in a work tree";
const ROLE: &str = "the one agent that does the task in a work tree and reports";
const THREAD: &str = "tasks-assign";
const CHANGED: &str = "割り当て中にエージェントの選択が変わったため反映しませんでした。もう一度「自動で割り当て」を押してください";
/// The note when the form's task left the view (the run says the same).
const GONE: &str = "そのタスクが見つかりません";

/// Called by the planner's thread once its result is on the channel.
pub(crate) type Wake = Box<dyn Fn() + Send>;
/// Starts a planner: [`auto_assign::spawn`], the demo's canned answer
/// (`Feed::planner`), or a test's fake. A result already on the channel
/// when it returns is shown at once.
pub(crate) type PlanFn = dyn Fn(Job, Wake) -> Planning;

/// What the screen lends auto-assign: how a planner starts and the clock
/// an assignment's arrival is stamped with. Tests replace both.
pub(super) struct Hooks {
    pub plan: Box<PlanFn>,
    pub clock: Box<dyn Fn() -> Millis>,
}

impl Hooks {
    /// The real planner thread and the system clock.
    pub fn system() -> Hooks {
        Hooks {
            plan: Box::new(|job, wake| auto_assign::spawn(job, THREAD, wake)),
            clock: Box::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as Millis)
            }),
        }
    }
}

/// What a planner run needs from the screen when it starts.
pub(super) struct Env<'a> {
    pub hooks: &'a Hooks,
    /// Wakes the screen (the live feed's waker), so `woken` drains the result.
    pub wake: Option<Arc<dyn Fn() + Send + Sync>>,
    /// The screen's view as it is now: the task the planner reads, and the
    /// repository, its working directory.
    pub view: &'a View,
}

/// The 実行 form's slots: one worker.
pub(super) fn slots() -> Vec<Slot> {
    vec![Slot {
        id: WORKER.to_owned(),
        role: ROLE.to_owned(),
    }]
}

/// The form's agents as candidates: no model or effort may be named.
pub(super) fn candidates(agents: &[AgentKind]) -> Vec<Candidate> {
    agents
        .iter()
        .map(|kind| Candidate {
            agent: kind.command().to_owned(),
            models: Vec::new(),
            efforts: Vec::new(),
        })
        .collect()
}

/// What the run works on: the task's title, source, labels and body, the
/// material `runs::brief` gives the agent.
pub(super) fn content_of(task: &Task) -> String {
    let mut out = format!("Title: {}\n", task.title);
    let source = match &task.r#ref {
        TaskRef::Local(id) => format!("ローカル (local task {id})"),
        TaskRef::GitHub { repo, number } => format!("#{number} (GitHub issue of {repo})"),
        TaskRef::Code { path, .. } => format!("コード (a TODO / FIXME marker in {path})"),
    };
    out.push_str(&format!("Source: {source}\n"));
    if !task.labels.is_empty() {
        out.push_str(&format!("Labels: {}\n", task.labels.join(", ")));
    }
    out.push('\n');
    match task.body.as_deref().map(str::trim_end) {
        Some(body) if !body.is_empty() => out.push_str(body),
        Some(_) => out.push_str("(No description beyond the title.)"),
        None => out.push_str("(The issue's text is not loaded.)"),
    }
    out.push('\n');
    out
}

/// The task `r` names as the run's brief will have it, from `view` as it
/// is now: an issue with its body once loaded, as `LiveFeed::task` makes
/// it for 実行. An issue whose detail is not loaded goes without its body,
/// as it does to the run.
pub(super) fn current_task(view: &View, r: &TaskRef) -> Option<Task> {
    let mut task = super::all_tasks(view)
        .into_iter()
        .find(|t| &t.r#ref == r)?
        .clone();
    if let TaskRef::GitHub { number, .. } = r
        && let Some(Ok(detail)) = view.github.details.get(number)
    {
        task.body = Some(detail.body.clone());
    }
    Some(task)
}

/// The planner's request for `task` on `agents`.
pub(super) fn request(task: &Task, agents: &[AgentKind]) -> Request {
    Request {
        widget: WIDGET.to_owned(),
        run: RUN.to_owned(),
        slots: slots(),
        candidates: candidates(agents),
        content: content_of(task),
    }
}

/// The kind a pick names, among the form's `agents`.
pub(super) fn kind_of(pick: &Pick, agents: &[AgentKind]) -> Option<AgentKind> {
    agents.iter().copied().find(|k| k.command() == pick.agent)
}

/// Whether the form's agent is still the one `pick` filled in.
pub(super) fn unchanged(current: Option<AgentKind>, pick: &Pick) -> bool {
    current.is_some_and(|k| k.command() == pick.agent)
}

/// The note line of a failed planner run.
pub(super) fn failure(err: &PlanError, planner: &Planner) -> String {
    format!(
        "自動割り当てに失敗しました: {}。手動で選ぶか、別のエージェントで決めてください",
        err.message(&planner.label())
    )
}

/// Where auto-assign is.
pub(super) enum AutoState {
    Idle,
    /// A planner is deciding; `snapshot` is the form's agent when it started.
    Running {
        planner: Planner,
        planning: Planning,
        snapshot: Option<AgentKind>,
    },
    /// The assignment filled the form; `at` is when it arrived.
    Filled {
        assignment: Assignment,
        at: Millis,
    },
    /// Why nothing was filled: the note says it.
    Failed(String),
}

/// The widgets [`Auto::insert`] made.
#[derive(Debug, Clone, Copy)]
struct Ids {
    menu: WidgetId,
    caption: WidgetId,
    stop: WidgetId,
    note: WidgetId,
}

/// Auto-assign's part of one 実行 form.
pub(super) struct Auto {
    /// The form's task: read from the view when a planner starts.
    task: TaskRef,
    agents: Vec<AgentKind>,
    planners: Vec<Planner>,
    state: AutoState,
    ids: Option<Ids>,
}

impl std::fmt::Debug for Auto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = match &self.state {
            AutoState::Idle => "Idle",
            AutoState::Running { .. } => "Running",
            AutoState::Filled { .. } => "Filled",
            AutoState::Failed(_) => "Failed",
        };
        f.debug_struct("Auto")
            .field("planners", &self.planners)
            .field("state", &state)
            .finish()
    }
}

impl Auto {
    /// For the 実行 form on `task` offering `agents`; the planners are the
    /// installed ones of claude and codex.
    pub fn new(task: &TaskRef, agents: &[AgentKind]) -> Auto {
        let installed: Vec<&str> = agents.iter().map(|k| k.command()).collect();
        Auto {
            task: task.clone(),
            agents: agents.to_vec(),
            planners: auto_assign::planners(&installed),
            state: AutoState::Idle,
            ids: None,
        }
    }

    #[cfg(test)]
    pub fn state(&self) -> &AutoState {
        &self.state
    }

    /// The pull-down's planners, in its order.
    #[cfg(test)]
    pub fn planners(&self) -> &[Planner] {
        &self.planners
    }

    /// Builds the control and the note under `column` (below the エージェント
    /// segment).
    pub fn insert(&mut self, tree: &mut UiTree, column: WidgetId) {
        let row = tree.insert(Some(column), Container::new(style::row(8.0)));
        let items = self
            .planners
            .iter()
            .map(|p| MenuItem::new(format!("{} で決める", p.agent)))
            .collect();
        let menu = tree.insert(
            Some(row),
            PullDownButton::new("自動で割り当て", items).control_size(ControlSize::Small),
        );
        let caption = tree.insert(
            Some(row),
            Label::new("").font_size(12.0).color(Role::Muted).wrap(true),
        );
        let stop = tree.insert(
            Some(row),
            Button::new("中止").control_size(ControlSize::Small),
        );
        let note = tree.insert(
            Some(column),
            Label::new("").font_size(12.0).wrap(true).selectable(true),
        );
        self.ids = Some(Ids {
            menu,
            caption,
            stop,
            note,
        });
        self.render(tree, None);
    }

    /// Takes in `message` when it is auto-assign's (the pull-down, 中止, or
    /// a change of the agent segment `segment`, whose choice is `current`);
    /// whether it was.
    pub fn handle(
        &mut self,
        message: &Message,
        tree: &mut UiTree,
        segment: WidgetId,
        current: Option<AgentKind>,
        env: &Env,
    ) -> bool {
        let Some(ids) = self.ids else {
            return false;
        };
        let taken = if message.source == ids.menu
            && let Some(MenuChosen(i)) = message.downcast_ref::<MenuChosen>()
        {
            if let Some(planner) = self.planners.get(*i).copied() {
                self.start(planner, current, env);
            }
            true
        } else if message.source == ids.stop && message.downcast_ref::<ButtonClicked>().is_some() {
            self.cancel();
            true
        } else if message.source == segment {
            // A change of the agent only marks the block (手動で変更).
            false
        } else {
            return false;
        };
        self.render(tree, current);
        taken
    }

    fn start(&mut self, planner: Planner, snapshot: Option<AgentKind>, env: &Env) {
        self.cancel();
        let Some(task) = current_task(env.view, &self.task) else {
            self.state = AutoState::Failed(GONE.to_owned());
            return;
        };
        let agents = self.agents.clone();
        let repo = &env.view.repo;
        let cwd = if repo.is_dir() {
            repo.to_owned()
        } else {
            std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
        };
        let job = Job {
            planner,
            shell: tasks::shell::login_shell(),
            cwd,
            request: Box::new(move || request(&task, &agents)),
            // Tasks has no setup to check (spec Q3): `validate` already
            // keeps the agent to the form's and the model and effort null.
            check: Box::new(|_| Ok(())),
        };
        let wake = env.wake.clone();
        let planning = (env.hooks.plan)(
            job,
            Box::new(move || {
                if let Some(wake) = &wake {
                    wake();
                }
            }),
        );
        self.state = AutoState::Running {
            planner,
            planning,
            snapshot,
        };
    }

    /// Stops a running planner (its process group is killed); the note
    /// clears.
    pub fn cancel(&mut self) {
        if let AutoState::Running { planning, .. } = &self.state {
            planning.cancel.store(true, Ordering::Relaxed);
            self.state = AutoState::Idle;
        }
    }

    /// Takes a running planner's result, if it came: a valid assignment
    /// for an unchanged form is the kind to select (and the state is
    /// `Filled`, stamped `now`); anything else is the note's.
    pub fn poll(
        &mut self,
        tree: &mut UiTree,
        current: Option<AgentKind>,
        now: Millis,
    ) -> Option<AgentKind> {
        let AutoState::Running {
            planner,
            planning,
            snapshot,
        } = &self.state
        else {
            return None;
        };
        let (planner, snapshot) = (*planner, *snapshot);
        let result = match planning.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            // The thread ended without an answer (it panicked).
            Err(TryRecvError::Disconnected) => Err(PlanError::Spawn(
                "割り当てのスレッドが止まりました".to_owned(),
            )),
        };
        let mut fill = None;
        self.state = match result {
            Err(PlanError::Cancelled) => AutoState::Idle,
            Err(err) => AutoState::Failed(failure(&err, &planner)),
            Ok(_) if current != snapshot => AutoState::Failed(CHANGED.to_owned()),
            Ok(assignment) => {
                let kind = assignment
                    .picks
                    .iter()
                    .find(|p| p.slot == WORKER)
                    .and_then(|p| kind_of(p, &self.agents));
                match kind {
                    Some(kind) => {
                        fill = Some(kind);
                        AutoState::Filled {
                            assignment,
                            at: now,
                        }
                    }
                    // `validate` checked the slot and the agent: not reached.
                    None => AutoState::Failed(failure(
                        &PlanError::Invalid(format!("{WORKER} の割り当てがありません")),
                        &planner,
                    )),
                }
            }
        };
        self.render(tree, fill.or(current));
        fill
    }

    /// The record 実行 carries: the fill's pick while the form's agent is
    /// still that pick, the slot as overridden otherwise; `None` without a
    /// fill or when every pick was changed.
    pub fn record(&self, current: Option<AgentKind>) -> Option<AutoRecord> {
        let AutoState::Filled { assignment, at } = &self.state else {
            return None;
        };
        AutoRecord::of(assignment, *at, |pick| unchanged(current, pick))
    }

    /// The note: the assignment block, a failure, or nothing.
    pub fn note(&self, current: Option<AgentKind>) -> String {
        match &self.state {
            AutoState::Idle | AutoState::Running { .. } if self.planners.is_empty() => {
                PlanError::NoPlanner.message("")
            }
            AutoState::Idle | AutoState::Running { .. } => String::new(),
            AutoState::Failed(why) => why.clone(),
            AutoState::Filled { assignment, .. } => {
                let mut out = format!(
                    "自動割り当て（{} が決定）— 内容を確認して「{START_LABEL}」を押してください",
                    assignment.planner
                );
                for pick in &assignment.picks {
                    let edited = if unchanged(current, pick) {
                        ""
                    } else {
                        "（手動で変更）"
                    };
                    out.push_str(&format!(
                        "\n{WORKER_LABEL}: {} — {}{edited}",
                        pick.agent, pick.reason
                    ));
                }
                out
            }
        }
    }

    fn render(&self, tree: &mut UiTree, current: Option<AgentKind>) {
        let Some(ids) = self.ids else {
            return;
        };
        let running = match &self.state {
            AutoState::Running { planner, .. } => Some(planner.label()),
            _ => None,
        };
        tree.set_hidden(ids.menu, running.is_some());
        tree.set_enabled(ids.menu, !self.planners.is_empty());
        tree.set_hidden(ids.caption, running.is_none());
        tree.set_hidden(ids.stop, running.is_none());
        let caption = running
            .map(|p| format!("{p} が割り当てを決めています…"))
            .unwrap_or_default();
        tree.update::<Label, _>(ids.caption, |l, cx| l.set_text(cx, caption));
        let note = self.note(current);
        tree.set_hidden(ids.note, note.is_empty());
        tree.update::<Label, _>(ids.note, |l, cx| l.set_text(cx, note));
    }
}

impl Drop for Auto {
    /// A closed form (キャンセル, Escape, 実行, another form) stops its
    /// planner: 実行 while planning starts the manual pick.
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tasks::model::types::State;

    fn task(r: TaskRef, body: Option<&str>) -> Task {
        Task {
            r#ref: r,
            title: "フォントを直す".into(),
            body: body.map(str::to_owned),
            state: State::Open,
            labels: vec!["bug".into(), "ui".into()],
            updated: 1,
            url: None,
            closed: None,
            problem: None,
        }
    }

    /// L1-3 (Tasks): one slot, `worker`, with its role line; candidates are
    /// the form's agents with empty model and effort lists, so the prompt
    /// says they must be null.
    #[test]
    fn the_request_has_one_worker_slot_and_kind_only_candidates() {
        let t = task(TaskRef::Local("t-1".into()), Some("手順\n"));
        let req = request(&t, &[AgentKind::Claude, AgentKind::Agy]);
        assert_eq!(req.widget, "tasks");
        assert_eq!(req.run, "one agent does one task in a work tree");
        assert_eq!(
            req.slots,
            [Slot {
                id: "worker".into(),
                role: "the one agent that does the task in a work tree and reports".into(),
            }]
        );
        assert_eq!(
            req.candidates,
            [
                Candidate {
                    agent: "claude".into(),
                    models: vec![],
                    efforts: vec![],
                },
                Candidate {
                    agent: "agy".into(),
                    models: vec![],
                    efforts: vec![],
                },
            ]
        );
        let prompt = auto_assign::prompt(&req);
        assert!(
            prompt.contains("- agy: model and effort must be null (the CLI's default)"),
            "{prompt}"
        );
        assert!(prompt.contains("- worker: the one agent"), "{prompt}");
    }

    /// L1-3 (Tasks): the content is the task's title, source, labels and
    /// body; an issue whose body is not loaded says so.
    #[test]
    fn the_content_is_the_tasks_title_source_labels_and_body() {
        let local = content_of(&task(TaskRef::Local("t-1".into()), Some("手順\n")));
        assert_eq!(
            local,
            "Title: フォントを直す\nSource: ローカル (local task t-1)\nLabels: bug, ui\n\n手順\n"
        );
        let issue = content_of(&task(
            TaskRef::GitHub {
                repo: "o/n".into(),
                number: 12,
            },
            None,
        ));
        assert!(
            issue.contains("Source: #12 (GitHub issue of o/n)"),
            "{issue}"
        );
        assert!(
            issue.ends_with("(The issue's text is not loaded.)\n"),
            "{issue}"
        );
        let marker = content_of(&task(
            TaskRef::Code {
                path: "src/a.rs".into(),
                key: "k".into(),
            },
            Some(""),
        ));
        assert!(marker.contains("Source: コード (a TODO / FIXME marker in src/a.rs)"));
        assert!(marker.contains("(No description beyond the title.)"));
    }

    /// L1-4: agy is never a planner, though it stays a candidate.
    #[test]
    fn the_planners_are_the_installed_claude_and_codex() {
        let t = task(TaskRef::Local("t-1".into()), None);
        let all = Auto::new(&t.r#ref, &AgentKind::ALL);
        let agents: Vec<&str> = all.planners.iter().map(|p| p.agent).collect();
        assert_eq!(agents, ["claude", "codex"]);
        let agy = Auto::new(&t.r#ref, &[AgentKind::Agy]);
        assert!(agy.planners.is_empty());
        assert_eq!(
            agy.note(Some(AgentKind::Agy)),
            "自動割り当てに使えるエージェントがインストールされていません"
        );
    }

    /// L1-6 through the shared core: a Tasks answer naming a model is
    /// refused, one naming the form's agent with nulls is valid.
    #[test]
    fn validation_refuses_a_model_and_accepts_the_kind_alone() {
        let t = task(TaskRef::Local("t-1".into()), None);
        let req = request(&t, &[AgentKind::Codex]);
        let planner = auto_assign::PLANNERS[0];
        let check = |_: &Pick| Ok(());
        let with_model = auto_assign::parse(
            r#"{"assignments":[{"slot":"worker","agent":"codex","model":"o3","effort":null,"reason":"r"}]}"#,
        )
        .unwrap();
        assert_eq!(
            auto_assign::validate(with_model, &req, &planner, &check),
            Err(PlanError::Invalid("codex のモデルは指定できません".into()))
        );
        let kind_only = auto_assign::parse(
            r#"{"assignments":[{"slot":"worker","agent":"codex","model":null,"effort":null,"reason":"小さな修正"}]}"#,
        )
        .unwrap();
        let a = auto_assign::validate(kind_only, &req, &planner, &check).unwrap();
        assert_eq!(
            kind_of(&a.picks[0], &[AgentKind::Codex]),
            Some(AgentKind::Codex)
        );
        assert!(unchanged(Some(AgentKind::Codex), &a.picks[0]));
        assert!(!unchanged(Some(AgentKind::Claude), &a.picks[0]));
    }
}
