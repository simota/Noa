//! The start form's 「自動で割り当て」 (`docs/specs/auto-assign.md`): an
//! installed agent CLI reads the request and the agent rows (judges, then
//! contestants) and picks each row's agent, model and effort with a reason.
//! The result only fills the rows; 「コンペティションを開始」 still starts.
//!
//! Each planning run owns its receiver: a run that is replaced, cancelled
//! or left behind is dropped with it (and told to stop), so a stale result
//! never reaches the form.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::TryRecvError;

use noa_gpui_core::widgets::{Button, ControlSize, Label, MenuItem, PullDownButton};
use noa_gpui_core::{Role, UiTree, WidgetId};
use noa_gpui_text::TextStore;

use competition::auto_assign::{
    self, Assignment, AutoRecord, Candidate, Job, Pick, PlanError, Planner, Planning, Request, Slot,
};
use competition::model::types::{AgentChoice, AgentKind, AgentSetup, Millis};

use super::start::{self, Detection, Form, Parts};
use super::{Competition, Source, clear_children, note, system_now};
use crate::installed::Installed;

pub(super) type Wake = Arc<dyn Fn() + Send + Sync>;
/// Starts a planner: `auto_assign::spawn`, or a fake in tests.
pub(super) type Spawn = dyn Fn(Job, Wake) -> Planning;

const RUN_DESCRIPTION: &str = "agents compete blind on the same task and judges score them";
const START_LABEL: &str = "コンペティションを開始";
const CHANGED_MEANWHILE: &str = "割り当て中にエージェントの選択が変わったため反映しませんでした。\
     もう一度「自動で割り当て」を押してください";
/// The start's own message for an empty request: with nothing to read,
/// there is nothing to assign by.
const NO_REQUEST: &str = "依頼内容を入力してください";

/// Who an agent row is, whatever rows around it are added or removed: the
/// lead judge's row is always there; the others are their row's widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKey {
    Lead,
    CoJudge(WidgetId),
    Contestant(WidgetId),
}

/// The agent rows as they stand, in slot order (judges, lead first, then
/// contestants): each row's slot id, identity and choice.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    slot: String,
    key: RowKey,
    choice: AgentChoice,
}

fn rows(form: &Form, parts: &Parts) -> Vec<Row> {
    let lead = Row {
        slot: "judge-1".into(),
        key: RowKey::Lead,
        choice: form.judge.clone(),
    };
    let co_judges =
        parts
            .co_judges
            .iter()
            .zip(&form.co_judges)
            .enumerate()
            .map(|(i, (row, choice))| Row {
                slot: format!("judge-{}", i + 2),
                key: RowKey::CoJudge(row.host),
                choice: choice.clone(),
            });
    let contestants = parts
        .panel
        .iter()
        .zip(&form.panel)
        .enumerate()
        .map(|(i, (row, choice))| Row {
            slot: format!("contestant-{}", i + 1),
            key: RowKey::Contestant(row.host),
            choice: choice.clone(),
        });
    std::iter::once(lead)
        .chain(co_judges)
        .chain(contestants)
        .collect()
}

pub(super) struct Auto {
    pub(super) spawn: Box<Spawn>,
    /// The pull-down, the caption while planning and 中止: made once and
    /// shown or hidden in place, so focus and an open menu survive a
    /// rebuild. The pull-down is made again only when its items change.
    pub(super) menu: WidgetId,
    status: WidgetId,
    pub(super) cancel: WidgetId,
    host: WidgetId,
    planners: Vec<Planner>,
    state: State,
    /// The last assignment applied to the form. A later run, its failure or
    /// its cancel leaves it: a start then records it.
    filled: Option<Filled>,
}

enum State {
    Idle,
    Running(Running),
    /// The note to show: a planner's failure, or rows changed while it
    /// ran. Why the form keeps it from running is not kept here: each
    /// render works that out afresh.
    Failed(String),
}

struct Running {
    planner: Planner,
    planning: Planning,
    /// The agent rows when planning began: a result for other rows is not
    /// applied.
    rows: Vec<Row>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.planning.cancel.store(true, Ordering::Release);
    }
}

struct Filled {
    assignment: Assignment,
    at: Millis,
    /// Which row each slot of the assignment filled.
    rows: Vec<(String, RowKey)>,
}

impl Filled {
    /// Whether `pick`'s row is still there and still shows the pick.
    fn unchanged(&self, pick: &Pick, now: &[Row]) -> bool {
        let Some((_, key)) = self.rows.iter().find(|(slot, _)| *slot == pick.slot) else {
            return false;
        };
        now.iter()
            .find(|row| row.key == *key)
            .is_some_and(|row| choice(pick).as_ref() == Some(&row.choice))
    }
}

/// The form's kind and setup of `pick`; `None` for an agent the widget does
/// not have (validation admits only the request's candidates).
fn choice(pick: &Pick) -> Option<AgentChoice> {
    let kind = AgentKind::ALL
        .into_iter()
        .find(|k| k.command() == pick.agent)?;
    Some(AgentChoice {
        kind,
        setup: AgentSetup {
            model: pick.model.clone(),
            effort: pick.effort.clone(),
        },
    })
}

/// 「審判 1」, 「参加者 2」: the slot as the form names its rows.
fn slot_label(slot: &str) -> String {
    if let Some(n) = slot.strip_prefix("judge-") {
        format!("審判 {n}")
    } else if let Some(n) = slot.strip_prefix("contestant-") {
        format!("参加者 {n}")
    } else {
        slot.to_owned()
    }
}

fn planner_menu(planners: &[Planner]) -> PullDownButton {
    let items = planners
        .iter()
        .map(|p| MenuItem::new(format!("{} で決める", p.agent)))
        .collect();
    PullDownButton::new("自動で割り当て", items).control_size(ControlSize::Small)
}

/// The kinds a planner may be: the installed ones, all of them when the
/// check could not tell, none while it runs.
fn installed_kinds(detection: &Detection) -> Vec<AgentKind> {
    match detection {
        Detection::Pending => Vec::new(),
        Detection::Done(Installed::Found(kinds)) => kinds.clone(),
        Detection::Done(Installed::Unknown(_)) => AgentKind::ALL.to_vec(),
    }
}

impl Auto {
    pub(super) fn new(tree: &mut UiTree, host: WidgetId) -> Self {
        let menu = tree.insert(Some(host), planner_menu(&[]));
        let status = tree.insert(
            Some(host),
            Label::new("")
                .font_size(crate::appearance::font(11.0))
                .color(Role::Muted),
        );
        let cancel = tree.insert(
            Some(host),
            Button::new("中止").control_size(ControlSize::Small),
        );
        tree.set_enabled(menu, false);
        tree.set_hidden(status, true);
        tree.set_hidden(cancel, true);
        Self {
            spawn: Box::new(|job, wake| {
                auto_assign::spawn(job, "competition-assign", move || wake())
            }),
            menu,
            status,
            cancel,
            host,
            planners: Vec::new(),
            state: State::Idle,
            filled: None,
        }
    }

    pub(super) fn running(&self) -> bool {
        matches!(self.state, State::Running(_))
    }

    /// The pull-down's items, by planner agent.
    #[cfg(test)]
    pub(super) fn planner_agents(&self) -> Vec<&'static str> {
        self.planners.iter().map(|p| p.agent).collect()
    }

    /// Stops a planner still running (its result, if any, is dropped).
    pub(super) fn cancel_running(&mut self) {
        if self.running() {
            self.state = State::Idle;
        }
    }

    /// The form was replaced (a rematch, a clean form): nothing of an
    /// earlier assignment applies to it.
    pub(super) fn forget(&mut self) {
        self.cancel_running();
        self.filled = None;
        if matches!(self.state, State::Failed(_)) {
            self.state = State::Idle;
        }
    }

    /// What a start records: the applied assignment's picks whose rows are
    /// still as filled, and the slots changed by hand since.
    pub(super) fn record(&self, form: &Form, parts: &Parts) -> Option<AutoRecord> {
        let filled = self.filled.as_ref()?;
        let now = rows(form, parts);
        AutoRecord::of(&filled.assignment, filled.at, |pick| {
            filled.unchanged(pick, &now)
        })
    }
}

impl Competition {
    /// Brings the control and the auto note up to date: the pull-down or
    /// the running caption with 中止, why it cannot run, the failure, and
    /// the assignment block with each row's reason.
    pub(super) fn render_auto(&mut self, text: &mut TextStore) {
        let focused = self.tree.focused();
        let on_control = focused == Some(self.auto.menu) || focused == Some(self.auto.cancel);
        let running = self.auto.running();
        let planners = auto_assign::planners(
            &installed_kinds(&self.detection)
                .iter()
                .map(|k| k.command())
                .collect::<Vec<_>>(),
        );
        // A pull-down's items are fixed once made; they change only when
        // the installed check reports.
        if planners != self.auto.planners {
            let index = self
                .tree
                .children(self.auto.host)
                .iter()
                .position(|id| *id == self.auto.menu)
                .unwrap_or(0);
            self.tree.remove(self.auto.menu, text);
            self.auto.menu = self
                .tree
                .insert(Some(self.auto.host), planner_menu(&planners));
            self.tree.move_child(self.auto.menu, index);
            self.auto.planners = planners;
        }
        let Competition { tree, auto, .. } = self;
        tree.set_hidden(auto.menu, running);
        tree.set_hidden(auto.status, !running);
        tree.set_hidden(auto.cancel, !running);
        if let State::Running(run) = &auto.state {
            let caption = format!("{} が割り当てを決めています…", run.planner.agent);
            tree.update::<Label, _>(auto.status, |l, cx| l.set_text(cx, caption));
        }
        if on_control {
            tree.set_focus(Some(if running { auto.cancel } else { auto.menu }));
        }

        let parts = &self.start;
        let form = start::read(&self.tree, parts);
        clear_children(&mut self.tree, parts.auto_note, text);
        let kinds = installed_kinds(&self.detection);
        let blocker = if kinds.is_empty() {
            // Pending, or nothing installed: the note above says so.
            None
        } else if self.auto.planners.is_empty() {
            Some(PlanError::NoPlanner.message(""))
        } else if form.request.is_empty() {
            Some(NO_REQUEST.to_owned())
        } else {
            // The start's own check of the folder, with its message.
            start::workdir(&form.workdir).err()
        };
        // A form the planner cannot read supersedes the last run's
        // failure: the note says what to fix, and once it is fixed no
        // stale failure is left.
        if blocker.is_some() && matches!(self.auto.state, State::Failed(_)) {
            self.auto.state = State::Idle;
        }
        let usable = matches!(self.source, Source::Live(_))
            && self.session.is_none()
            && !kinds.is_empty()
            && !self.auto.planners.is_empty()
            && blocker.is_none();
        self.tree.set_enabled(self.auto.menu, usable);
        if !running && let Some(why) = blocker {
            note(&mut self.tree, parts.auto_note, why);
        }
        if let State::Failed(why) = &self.auto.state {
            note(&mut self.tree, parts.auto_note, why.clone());
        }
        if let Some(filled) = &self.auto.filled {
            let now = rows(&form, parts);
            note(
                &mut self.tree,
                parts.auto_note,
                format!(
                    "自動割り当て（{} が決定）— 内容を確認して「{START_LABEL}」を押してください",
                    filled.assignment.planner
                ),
            );
            for pick in &filled.assignment.picks {
                let label = choice(pick).map_or_else(|| pick.agent.clone(), |c| c.label());
                let edited = if filled.unchanged(pick, &now) {
                    ""
                } else {
                    "（手動で変更）"
                };
                note(
                    &mut self.tree,
                    parts.auto_note,
                    format!(
                        "{}: {label} — {}{edited}",
                        slot_label(&pick.slot),
                        pick.reason
                    ),
                );
            }
        }
    }

    /// Starts planning with the `index`th planner of the pull-down.
    pub(super) fn begin_auto(&mut self, index: usize, text: &mut TextStore) {
        let Source::Live(channel) = &self.source else {
            return;
        };
        if self.session.is_some() || self.auto.running() {
            return;
        }
        let Some(&planner) = self.auto.planners.get(index) else {
            return;
        };
        let wake = Arc::clone(&channel.wake);
        let form = start::read(&self.tree, &self.start);
        // The start's own check of the folder, so planning runs where the
        // competition would. The pull-down is off while the form cannot be
        // read, but the folder may have gone since the last edit: the
        // render then says so and turns the pull-down off.
        let workdir = match start::workdir(&form.workdir) {
            Ok(workdir) if !form.request.is_empty() => workdir,
            _ => return self.render_auto(text),
        };
        let rows = rows(&form, &self.start);
        let slots = rows.iter().map(|row| slot(&row.slot)).collect();
        let candidates = candidates(self.start.kinds());
        let content = content(&form.request, workdir.as_deref());
        let cwd = planner_cwd(workdir, std::env::var_os("HOME"));
        let job = Job {
            planner,
            shell: crate::session::runner::login_shell(),
            cwd,
            request: Box::new(move || Request {
                widget: "competition".into(),
                run: RUN_DESCRIPTION.into(),
                slots,
                candidates,
                content,
            }),
            check: Box::new(|pick| match choice(pick) {
                Some(choice) => choice.setup.check(choice.kind),
                None => Err(format!("{} はこのウィジェットにありません", pick.agent)),
            }),
        };
        let planning = (self.auto.spawn)(job, wake);
        self.auto.state = State::Running(Running {
            planner,
            planning,
            rows,
        });
        self.render_auto(text);
        // Whoever chose a planner may stop it next.
        self.tree.set_focus(Some(self.auto.cancel));
    }

    /// Stops planning (中止).
    pub(super) fn cancel_auto(&mut self, text: &mut TextStore) {
        if !self.auto.running() {
            return;
        }
        self.auto.cancel_running();
        self.render_auto(text);
        self.focus_menu_after_planning();
    }

    /// Takes a planner's result, once it has one: a valid assignment fills
    /// the rows it was asked for, unless they changed meanwhile.
    pub(super) fn poll_auto(&mut self, text: &mut TextStore) {
        let State::Running(run) = &self.auto.state else {
            return;
        };
        let result = match run.planning.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(PlanError::Spawn(
                "割り当てのスレッドが結果を返さずに終了しました".into(),
            )),
        };
        let label = run.planner.label();
        let State::Running(run) = std::mem::replace(&mut self.auto.state, State::Idle) else {
            unreachable!("matched above");
        };
        self.auto.state = match result {
            Ok(assignment) => {
                let form = start::read(&self.tree, &self.start);
                if rows(&form, &self.start) != run.rows {
                    State::Failed(CHANGED_MEANWHILE.into())
                } else {
                    self.apply(&run.rows, assignment, text);
                    State::Idle
                }
            }
            Err(PlanError::Cancelled) => State::Idle,
            Err(err) => State::Failed(format!(
                "自動割り当てに失敗しました: {}。手動で選ぶか、別のエージェントで決めてください",
                err.message(&label)
            )),
        };
        drop(run);
        self.render_auto(text);
        self.focus_menu_after_planning();
    }

    /// Puts `assignment` into the rows it was planned for (`rows`, which
    /// are the form's rows as they still are) and keeps it for the start.
    fn apply(&mut self, rows: &[Row], assignment: Assignment, text: &mut TextStore) {
        let picked = |row: &Row| {
            assignment
                .picks
                .iter()
                .find(|pick| pick.slot == row.slot)
                .and_then(choice)
                .unwrap_or_else(|| row.choice.clone())
        };
        let judges: Vec<AgentChoice> = rows
            .iter()
            .filter(|row| !matches!(row.key, RowKey::Contestant(_)))
            .map(picked)
            .collect();
        let panel: Vec<AgentChoice> = rows
            .iter()
            .filter(|row| matches!(row.key, RowKey::Contestant(_)))
            .map(picked)
            .collect();
        start::show_agents(&mut self.tree, &mut self.start, &judges, &panel, text);
        self.auto.filled = Some(Filled {
            rows: rows.iter().map(|row| (row.slot.clone(), row.key)).collect(),
            assignment,
            at: system_now(),
        });
    }

    /// Back on the pull-down once planning ended, unless the person has
    /// moved on to another control meanwhile (the form stays editable).
    fn focus_menu_after_planning(&mut self) {
        let focused = self.tree.focused();
        if focused.is_none() || focused == Some(self.auto.cancel) {
            self.tree.set_focus(Some(self.auto.menu));
        }
    }
}

/// Where the planner runs: the 参照ディレクトリ, else a non-empty `HOME`,
/// else `/` (an empty path would fail the spawn).
fn planner_cwd(workdir: Option<PathBuf>, home: Option<OsString>) -> PathBuf {
    workdir
        .or_else(|| home.filter(|h| !h.is_empty()).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn slot(id: &str) -> Slot {
    let role = if id == "judge-1" {
        "lead judge: scores every round, decides early finish and writes the final review"
    } else if id.starts_with("judge-") {
        "co-judge: scores every round"
    } else {
        "contestant: does the task blind, competing with the others"
    };
    Slot {
        id: id.to_owned(),
        role: role.to_owned(),
    }
}

fn candidates(kinds: &[AgentKind]) -> Vec<Candidate> {
    kinds
        .iter()
        .map(|kind| Candidate {
            agent: kind.command().into(),
            models: kind.models().iter().map(|m| (*m).to_owned()).collect(),
            efforts: kind.efforts().iter().map(|e| (*e).to_owned()).collect(),
        })
        .collect()
}

/// The competition's content for the planner: the 依頼内容 and the
/// 参照ディレクトリ (as the start resolves it).
fn content(request: &str, workdir: Option<&std::path::Path>) -> String {
    let workdir = workdir.map_or_else(
        || "(none: each contestant works in a folder of its own)".to_owned(),
        |w| w.display().to_string(),
    );
    format!("Request (依頼内容):\n{request}\n\nReference directory (参照ディレクトリ): {workdir}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_planner_runs_in_the_workdir_else_a_non_empty_home_else_root() {
        let home = || Some(OsString::from("/home/me"));
        assert_eq!(
            planner_cwd(Some("/work".into()), home()),
            PathBuf::from("/work")
        );
        assert_eq!(planner_cwd(None, home()), PathBuf::from("/home/me"));
        assert_eq!(planner_cwd(None, Some(OsString::new())), PathBuf::from("/"));
        assert_eq!(planner_cwd(None, None), PathBuf::from("/"));
    }
}
