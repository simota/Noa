//! Planning fills the editable start form. Each run owns a fresh result
//! channel, so dropping a cancelled run also discards its generation's reply.

use super::{Discussion, Source, caption, clear_children, note, start};
use crate::session::runner::{Clock, SystemClock, login_shell};
use discussion::auto_assign::{
    self, Assignment, AutoRecord, Candidate, Job, Pick, PlanError, Planner, Planning, Request, Slot,
};
use discussion::model::types::{AgentChoice, AgentKind, AgentSetup, Millis};
use noa_gpui_core::widgets::{
    Button, ButtonClicked, ControlSize, MenuChosen, MenuItem, PullDownButton,
};
use noa_gpui_core::{Message, WidgetId};
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
}

impl Drop for Running {
    fn drop(&mut self) {
        self.planning.cancel.store(true, Ordering::Release);
    }
}

enum State {
    Idle,
    Running(Running),
    Filled(Assignment, Millis),
    Failed(String),
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

pub(super) struct Auto {
    state: State,
    control: Option<WidgetId>,
    pub(super) spawn: Box<Spawn>,
}

impl Default for Auto {
    fn default() -> Self {
        Self {
            state: State::Idle,
            control: None,
            spawn: Box::new(|job, wake| {
                auto_assign::spawn(job, "discussion-assign", move || wake())
            }),
        }
    }
}

impl Auto {
    pub(super) fn reset(&mut self) {
        self.state = State::Idle;
    }

    pub(super) fn record(&self, form: &start::Form) -> Option<AutoRecord> {
        match &self.state {
            State::Filled(assignment, at) => {
                AutoRecord::of(assignment, *at, |pick| unchanged(form, pick))
            }
            _ => None,
        }
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

fn unchanged(form: &start::Form, pick: &Pick) -> bool {
    let current = match pick.slot.strip_prefix("panelist-") {
        Some(index) => form
            .panel
            .get(index.parse::<usize>().expect("validated panel slot") - 1),
        None => Some(&form.moderator),
    };
    current == Some(&choice(pick))
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
        let host = self.start.auto_controls;
        clear_children(&mut self.tree, host, text);
        if let State::Running(run) = &self.auto.state {
            caption(
                &mut self.tree,
                host,
                format!("{} が割り当てを決めています…", run.planner.agent),
            );
            self.auto.control = Some(self.tree.insert(
                Some(host),
                Button::new("中止").control_size(ControlSize::Small),
            ));
            return;
        }
        let planners = self.planners();
        let items = planners
            .iter()
            .map(|p| MenuItem::new(format!("{} で決める", p.agent)))
            .collect();
        let menu = self.tree.insert(
            Some(host),
            PullDownButton::new("自動で割り当て", items).control_size(ControlSize::Small),
        );
        self.auto.control = Some(menu);
        let form = start::read(&self.tree, &self.start);
        let unavailable = if !self.agents_ready() {
            None // Detection's note is already visible.
        } else if planners.is_empty() {
            Some(PlanError::NoPlanner.message(""))
        } else {
            form.checked_workdir().err()
        };
        self.tree.set_enabled(
            menu,
            matches!(self.source, Source::Live(_)) && !planners.is_empty() && unavailable.is_none(),
        );
        let area = self.start.note;
        if let Some(why) = unavailable {
            note(&mut self.tree, area, why);
        }
        match &self.auto.state {
            State::Filled(assignment, _) => {
                note(
                    &mut self.tree,
                    area,
                    format!(
                        "自動割り当て（{} が決定）— 内容を確認して「ディスカッションを開始」を押してください",
                        assignment.planner
                    ),
                );
                for (index, pick) in assignment.picks.iter().enumerate() {
                    let slot = if index == 0 {
                        "司会".into()
                    } else {
                        format!("パネリスト {index}")
                    };
                    let edited = if unchanged(&form, pick) {
                        ""
                    } else {
                        "（手動で変更）"
                    };
                    note(
                        &mut self.tree,
                        area,
                        format!("{slot}: {} — {}{edited}", choice(pick).label(), pick.reason),
                    );
                }
            }
            State::Failed(why) => {
                note(&mut self.tree, area, why);
            }
            _ => {}
        }
    }

    pub(super) fn handle_auto(&mut self, message: &Message, text: &mut TextStore) -> bool {
        if self.auto.control != Some(message.source) {
            return false;
        }
        if let Some(MenuChosen(index)) = message.downcast_ref() {
            if let Some(planner) = self.planners().get(*index).copied() {
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
        });
        self.render_auto(text);
        self.poll_auto(text); // A thread-start failure is delivered synchronously.
    }

    pub(super) fn cancel_planning(&mut self, text: &mut TextStore) {
        if matches!(self.auto.state, State::Running(_)) {
            self.auto.reset();
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
                if form.moderator != run.moderator || form.panel != run.panel {
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
                    State::Filled(assignment, SystemClock.now())
                }
            }
            Err(err) => State::failure(err, run.planner),
        };
        self.rebuild(text); // A fill replaced the panel's remove buttons.
    }
}
