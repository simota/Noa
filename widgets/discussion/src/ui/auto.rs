//! Planning fills the editable start form. Each run owns a fresh result
//! channel, so dropping a cancelled run also discards its generation's reply.
//! The last fill applied to the form is kept apart from the run in flight:
//! a re-run, its cancel, failure or refusal leaves that fill's block and its
//! record, as the rows still hold it.

use super::{Discussion, Source, caption, note, start};
use crate::session::runner::{Clock, SystemClock, login_shell};
use discussion::auto_assign::{
    self, Assignment, AutoRecord, Candidate, Job, Pick, PlanError, Planner, Planning, Request, Slot,
};
use discussion::model::types::{AgentChoice, AgentKind, AgentSetup, Millis};
use noa_gpui_core::widgets::{
    Button, ButtonClicked, ControlSize, Label, MenuChosen, MenuItem, PullDownButton,
};
use noa_gpui_core::{Message, UiTree, WidgetId};
use noa_gpui_text::TextStore;
use std::path::PathBuf;
use std::sync::{Arc, atomic::Ordering, mpsc::TryRecvError};

type Wake = Arc<dyn Fn() + Send + Sync>;
type Spawn = dyn Fn(Job, Wake) -> Planning;

struct Running {
    planner: Planner,
    planning: Planning,
    moderator: AgentChoice,
    panel: Vec<AgentChoice>,
    /// Each panel row's host at planning time: a row removed and re-added
    /// with the same values is still a change.
    rows: Vec<WidgetId>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.planning.cancel.store(true, Ordering::Release);
    }
}

enum State {
    Idle,
    Running(Running),
    Failed(String),
}

/// The fill the rows hold, with each panel row's host at fill time: a row
/// keeps its host until it is removed, so removing one row leaves the
/// others matched to their own picks.
struct Filled {
    assignment: Assignment,
    at: Millis,
    rows: Vec<WidgetId>,
}

impl Filled {
    fn unchanged(&self, pick: &Pick, parts: &start::Parts, form: &start::Form) -> bool {
        let Some(index) = panel_index(pick) else {
            return form.moderator == choice(pick);
        };
        let Some(host) = self.rows.get(index) else {
            return false;
        };
        parts
            .panel
            .iter()
            .zip(&form.panel)
            .any(|(row, current)| row.host == *host && *current == choice(pick))
    }
}

impl State {
    fn failure(err: PlanError, planner: Planner) -> Self {
        match err {
            PlanError::Cancelled => Self::Idle,
            err => Self::Failed(format!(
                "自動割り当てに失敗しました: {}。手動で選ぶか、別のエージェントで決めてください",
                err.message(&planner.label())
            )),
        }
    }
}

/// The header's controls live as long as the start view and are shown,
/// hidden and enabled in place: an open menu keeps its widget, so its
/// choice arrives, and focus moves between the pull-down and 中止.
pub(super) struct Auto {
    pub(super) spawn: Box<Spawn>,
    menu: WidgetId,
    status: WidgetId,
    cancel: WidgetId,
    /// The pull-down's items, in menu order.
    planners: Vec<Planner>,
    state: State,
    filled: Option<Filled>,
}

fn planner_menu(planners: &[Planner]) -> PullDownButton {
    let items = planners
        .iter()
        .map(|p| MenuItem::new(format!("{} で決める", p.agent)))
        .collect();
    PullDownButton::new("自動で割り当て", items).control_size(ControlSize::Small)
}

impl Auto {
    pub(super) fn new(tree: &mut UiTree, host: WidgetId) -> Self {
        let menu = tree.insert(Some(host), planner_menu(&[]));
        let status = caption(tree, host, "");
        let cancel = tree.insert(
            Some(host),
            Button::new("中止").control_size(ControlSize::Small),
        );
        tree.set_hidden(status, true);
        tree.set_hidden(cancel, true);
        Self {
            spawn: Box::new(|job, wake| {
                auto_assign::spawn(job, "discussion-assign", move || wake())
            }),
            menu,
            status,
            cancel,
            planners: Vec::new(),
            state: State::Idle,
            filled: None,
        }
    }

    /// The control shown: 中止 while planning, else the pull-down.
    fn control(&self) -> WidgetId {
        if matches!(self.state, State::Running(_)) {
            self.cancel
        } else {
            self.menu
        }
    }

    pub(super) fn reset(&mut self) {
        self.state = State::Idle;
        self.filled = None;
    }

    pub(super) fn record(&self, parts: &start::Parts, form: &start::Form) -> Option<AutoRecord> {
        let filled = self.filled.as_ref()?;
        AutoRecord::of(&filled.assignment, filled.at, |pick| {
            filled.unchanged(pick, parts, form)
        })
    }
}

fn choice(pick: &Pick) -> AgentChoice {
    let kind = AgentKind::ALL
        .into_iter()
        .find(|k| k.command() == pick.agent)
        .expect("validated installed candidate");
    AgentChoice {
        kind,
        setup: AgentSetup {
            model: pick.model.clone(),
            effort: pick.effort.clone(),
        },
    }
}

fn hosts(parts: &start::Parts) -> Vec<WidgetId> {
    parts.panel.iter().map(|row| row.host).collect()
}

/// The panel position a pick was made for; `None` for the moderator.
fn panel_index(pick: &Pick) -> Option<usize> {
    pick.slot
        .strip_prefix("panelist-")?
        .parse::<usize>()
        .ok()?
        .checked_sub(1)
}

fn job(planner: Planner, form: &start::Form, cwd: PathBuf, available: Vec<AgentKind>) -> Job {
    let topic = form.topic.clone();
    let workdir = form.workdir.clone();
    let panel_len = form.panel.len();
    Job {
        planner,
        shell: login_shell(),
        cwd,
        request: Box::new(move || {
            let mut slots = vec![Slot {
                id: "moderator".into(),
                role: "moderator: steers the discussion and writes the minutes".into(),
            }];
            slots.extend((1..=panel_len).map(|i| Slot {
                id: format!("panelist-{i}"),
                role: "panelist".into(),
            }));
            let strings = |values: &[&str]| values.iter().map(|s| (*s).to_owned()).collect();
            Request {
                widget: "discussion".into(),
                run: "a moderated multi-agent discussion of a topic".into(),
                content: format!("議題: {topic}\n作業ディレクトリ: {workdir}"),
                slots,
                candidates: available
                    .into_iter()
                    .map(|kind| Candidate {
                        agent: kind.command().into(),
                        models: strings(kind.models()),
                        efforts: strings(kind.efforts()),
                    })
                    .collect(),
            }
        }),
        check: Box::new(|pick| {
            let choice = choice(pick);
            choice.setup.check(choice.kind)
        }),
    }
}

impl Discussion {
    fn planners(&self) -> Vec<Planner> {
        let installed: Vec<_> = self
            .installed
            .iter()
            .flat_map(|i| i.available())
            .map(AgentKind::command)
            .collect();
        auto_assign::planners(&installed)
    }

    pub(super) fn render_auto(&mut self, text: &mut TextStore) {
        start::rebuild_note(
            &mut self.tree,
            &self.start,
            &self.link,
            self.installed.as_ref(),
            text,
        );
        let planners = self.planners();
        let ready = self.agents_ready();
        let live = matches!(self.source, Source::Live(_));
        let (tree, auto, parts) = (&mut self.tree, &mut self.auto, &self.start);
        let had_focus = tree
            .focused()
            .is_some_and(|id| id == auto.menu || id == auto.cancel);
        let running = matches!(auto.state, State::Running(_));
        tree.set_hidden(auto.status, !running);
        tree.set_hidden(auto.cancel, !running);
        tree.set_hidden(auto.menu, running);
        let form = start::read(tree, parts);
        let area = parts.note;
        if let State::Running(run) = &auto.state {
            let status = format!("{} が割り当てを決めています…", run.planner.agent);
            tree.update::<Label, _>(auto.status, |label, cx| label.set_text(cx, status));
        } else {
            // A pull-down's items are fixed when it is made; only the
            // check for installed agents changes them.
            if planners != auto.planners {
                tree.remove(auto.menu, text);
                auto.menu = tree.insert(Some(parts.auto_controls), planner_menu(&planners));
                auto.planners = planners;
            }
            let unavailable = if !ready {
                None // Detection's note is already visible.
            } else if auto.planners.is_empty() {
                Some(PlanError::NoPlanner.message(""))
            } else {
                form.checked_workdir().err()
            };
            tree.set_enabled(
                auto.menu,
                live && !auto.planners.is_empty() && unavailable.is_none(),
            );
            if let Some(why) = unavailable {
                note(tree, area, why);
            }
        }
        if had_focus {
            tree.set_focus(Some(auto.control()));
        }
        if let State::Failed(why) = &auto.state {
            note(tree, area, why);
        }
        if let Some(filled) = &auto.filled {
            note(
                tree,
                area,
                format!(
                    "自動割り当て（{} が決定）— 内容を確認して「ディスカッションを開始」を押してください",
                    filled.assignment.planner
                ),
            );
            for pick in &filled.assignment.picks {
                let slot = panel_index(pick)
                    .map_or_else(|| "司会".to_owned(), |i| format!("パネリスト {}", i + 1));
                let edited = if filled.unchanged(pick, parts, &form) {
                    ""
                } else {
                    "（手動で変更）"
                };
                note(
                    tree,
                    area,
                    format!("{slot}: {} — {}{edited}", choice(pick).label(), pick.reason),
                );
            }
        }
    }

    pub(super) fn handle_auto(&mut self, message: &Message, text: &mut TextStore) -> bool {
        if self.auto.control() != message.source {
            return false;
        }
        if let Some(MenuChosen(index)) = message.downcast_ref() {
            if let Some(planner) = self.auto.planners.get(*index).copied() {
                self.begin_planning(planner, text);
            }
        } else if message.downcast_ref::<ButtonClicked>().is_some() {
            self.cancel_planning(text);
        }
        true
    }

    fn begin_planning(&mut self, planner: Planner, text: &mut TextStore) {
        let Source::Live(channel) = &self.source else {
            return;
        };
        let form = start::read(&self.tree, &self.start);
        let Ok(workdir) = form.checked_workdir() else {
            self.render_auto(text);
            return;
        };
        let Some(cwd) = workdir.or_else(|| std::env::var_os("HOME").map(PathBuf::from)) else {
            self.auto.state = State::failure(
                PlanError::Spawn("ホームディレクトリが分かりません".into()),
                planner,
            );
            self.render_auto(text);
            return;
        };
        let planning = (self.auto.spawn)(
            job(planner, &form, cwd, self.start.available.clone()),
            channel.wake.clone(),
        );
        self.auto.state = State::Running(Running {
            planner,
            planning,
            moderator: form.moderator,
            panel: form.panel,
            rows: hosts(&self.start),
        });
        self.render_auto(text);
        self.poll_auto(text); // A thread-start failure is delivered synchronously.
    }

    pub(super) fn cancel_planning(&mut self, text: &mut TextStore) {
        if matches!(self.auto.state, State::Running(_)) {
            self.auto.state = State::Idle;
            self.render_auto(text);
        }
    }

    pub(super) fn poll_auto(&mut self, text: &mut TextStore) {
        let State::Running(run) = &self.auto.state else {
            return;
        };
        let result = match run.planning.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Err(PlanError::Spawn("割り当てスレッドが終了しました".into()))
            }
        };
        self.auto.state = match result {
            Ok(assignment) => {
                let form = start::read(&self.tree, &self.start);
                if form.moderator != run.moderator
                    || form.panel != run.panel
                    || hosts(&self.start) != run.rows
                {
                    State::Failed("割り当て中にエージェントの選択が変わったため反映しませんでした。もう一度「自動で割り当て」を押してください".into())
                } else {
                    let mut choices = assignment.picks.iter().map(choice);
                    let moderator = choices.next().expect("validated moderator slot");
                    start::fill_agents(
                        &mut self.tree,
                        &mut self.start,
                        &moderator,
                        &choices.collect::<Vec<_>>(),
                        text,
                    );
                    self.auto.filled = Some(Filled {
                        assignment,
                        at: SystemClock.now(),
                        rows: hosts(&self.start),
                    });
                    State::Idle
                }
            }
            Err(err) => State::failure(err, run.planner),
        };
        self.rebuild(text); // A fill replaced the panel's remove buttons.
    }
}
