//! 「自動で割り当て」 in the start view (`docs/specs/auto-assign.md` L1-2,
//! L2-2): an installed agent CLI reads the verification's request,
//! criteria, spec and target and the judge and verifier rows, and picks the
//! agent, model and effort of every row, each with a reason. The result only
//! fills the rows (D1, D2); 検証を開始 stays the one way to start.
//!
//! The planning itself is the shared `verify::auto_assign`; this module is
//! the verify widget's side of it: what the planner is told, how a pick maps
//! back to a row, and the control and the assignment block in the start
//! view.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command as OsCommand, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc::TryRecvError;

use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, MenuItem, PullDownButton};
use noa_gpui_core::{Role, UiTree, WidgetId};
use noa_gpui_text::TextStore;

use verify::auto_assign::{
    self, Assignment, AutoRecord, Candidate, Job, Pick, PlanError, Planner, Planning, Request, Slot,
};
use verify::model::types::{
    AgentChoice, AgentKind, AgentSetup, Head, Millis, StrategyChoice, Target, TargetKind,
    VerifierChoice,
};

use super::start::{Detection, Form};
use super::{Action, caption, clear_children};
use crate::installed::Installed;

/// The widget's name and run description in the planner's prompt (L1-8).
const WIDGET: &str = "verify";
const RUN: &str =
    "a multi-agent verification of a change against acceptance criteria, with evidence";
const JUDGE_SLOT: &str = "judge";
const JUDGE_ROLE: &str = "judge: writes the acceptance criteria, hashes the evidence and gives \
                          the per-criterion and overall verdict";
/// How much of the spec file and of the diff summary the content holds
/// (L1-3); the whole content is capped again by `auto_assign::prompt`.
const SPEC_MAX: usize = 16 * 1024;
const STAT_MAX: usize = 8 * 1024;
/// A custom strategy's text in its role line, in characters.
const CUSTOM_MAX: usize = 300;
const THREAD: &str = "verify-assign";
/// The start button's label, which the assignment block names.
const START_LABEL: &str = "検証を開始";
const CHANGED: &str = "割り当て中にエージェントの選択が変わったため反映しませんでした。\
                       もう一度「自動で割り当て」を押してください";

/// Starts a planner: `auto_assign::spawn` in the app; tests put a fake in
/// its place and answer on its channel.
pub(super) type Plan = Box<dyn Fn(Job, Box<dyn Fn() + Send>) -> Planning>;

pub(super) fn system_plan() -> Plan {
    Box::new(|job, wake| auto_assign::spawn(job, THREAD, wake))
}

/// The agent rows a planner was started on, to tell whether they changed
/// before its answer came.
type Rows = (AgentChoice, Vec<VerifierChoice>);

fn rows(form: &Form) -> Rows {
    (form.judge.clone(), form.panel.clone())
}

pub(super) enum AutoState {
    Idle,
    /// A planner is deciding; `snapshot` is the rows it was started on.
    /// The answer is read only from `planning`, which goes with this
    /// state: an answer of a planner cancelled or replaced is never read.
    Running {
        planner: Planner,
        planning: Planning,
        snapshot: Rows,
    },
    /// The rows were filled from `assignment`, which arrived at `at`.
    Filled {
        assignment: Assignment,
        at: Millis,
    },
    /// The whole line the note shows (a failure, or a refused fill).
    Failed(String),
}

/// One line of the note the auto-assign part writes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    Heading(String),
    Text(String),
}

/// What the control shows: the planner deciding, or the pull-down with
/// these planners, enabled or not.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Control {
    Running(String),
    Menu(Vec<&'static str>, bool),
}

pub(super) struct Auto {
    pub(super) state: AutoState,
    plan: Plan,
    /// The pull-down while it is shown, and the planner of each item.
    menu: Option<(WidgetId, Vec<Planner>)>,
    cancel: Option<WidgetId>,
    /// The container this part writes into the start view's note.
    note: Option<WidgetId>,
    shown_control: Option<Control>,
    shown_note: Option<Vec<Line>>,
}

impl Auto {
    pub(super) fn new(plan: Plan) -> Self {
        Self {
            state: AutoState::Idle,
            plan,
            menu: None,
            cancel: None,
            note: None,
            shown_control: None,
            shown_note: None,
        }
    }

    #[cfg(test)]
    pub(super) fn set_plan(&mut self, plan: Plan) {
        self.plan = plan;
    }

    /// The pull-down, while it is shown.
    pub(super) fn menu(&self) -> Option<WidgetId> {
        self.menu.as_ref().map(|(id, _)| *id)
    }

    /// The planner of the pull-down's item `index`.
    pub(super) fn chosen(&self, index: usize) -> Option<Planner> {
        self.menu.as_ref()?.1.get(index).copied()
    }

    /// Starts `planner` on `form`'s rows, the menus offering `kinds`; a
    /// planner still deciding is cancelled first.
    pub(super) fn begin(
        &mut self,
        planner: Planner,
        form: &Form,
        kinds: &[AgentKind],
        wake: Box<dyn Fn() + Send>,
    ) {
        self.cancel();
        let planning = (self.plan)(job(planner, form, kinds), wake);
        self.state = AutoState::Running {
            planner,
            planning,
            snapshot: rows(form),
        };
    }

    /// Stops a planner still deciding (中止, or a start meanwhile).
    pub(super) fn cancel(&mut self) {
        if let AutoState::Running { planning, .. } = &self.state {
            planning.cancel.store(true, Ordering::Release);
            self.state = AutoState::Idle;
        }
    }

    /// Forgets the last assignment: the form was filled from elsewhere.
    pub(super) fn reset(&mut self) {
        self.cancel();
        self.state = AutoState::Idle;
    }

    /// Takes the planner's answer once it has come: the form to put in the
    /// start view when it is a valid assignment and the rows are still the
    /// ones the planner was started on (`current`).
    pub(super) fn poll(&mut self, current: &Form, now: Millis) -> Option<Form> {
        let AutoState::Running {
            planner,
            planning,
            snapshot,
        } = &self.state
        else {
            return None;
        };
        let label = planner.label();
        let answer = match planning.rx.try_recv() {
            Ok(answer) => answer,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(PlanError::Spawn(
                "割り当てのスレッドが結果を返さずに終了しました".to_owned(),
            )),
        };
        let (state, filled) = match answer {
            Ok(_) if rows(current) != *snapshot => (AutoState::Failed(CHANGED.to_owned()), None),
            Ok(assignment) => {
                let mut form = current.clone();
                fill(&mut form, &assignment);
                (
                    AutoState::Filled {
                        assignment,
                        at: now,
                    },
                    Some(form),
                )
            }
            Err(PlanError::Cancelled) => (AutoState::Idle, None),
            Err(why) => (AutoState::Failed(failure(&why.message(&label))), None),
        };
        self.state = state;
        filled
    }

    /// What a start now records: the filled picks still as filled, and the
    /// slots changed since. A planner still deciding is cancelled: the
    /// start goes ahead with the rows as they are.
    pub(super) fn record(&mut self, form: &Form) -> Option<AutoRecord> {
        self.cancel();
        match &self.state {
            AutoState::Filled { assignment, at } => {
                AutoRecord::of(assignment, *at, |pick| unchanged(form, pick))
            }
            _ => None,
        }
    }

    /// Draws the control into `host` and the assignment block (or why the
    /// control is off) into `note`, each only when it changed or its
    /// widgets went with a rebuild of the note.
    pub(super) fn show(
        &mut self,
        tree: &mut UiTree,
        host: WidgetId,
        note: WidgetId,
        form: &Form,
        detection: &Detection,
        text: &mut TextStore,
    ) {
        let planners = planners(detection);
        let blocker = form.target().err();
        let control = match &self.state {
            AutoState::Running { planner, .. } => Control::Running(planner.label()),
            _ => Control::Menu(
                planners.iter().map(|p| p.agent).collect(),
                !planners.is_empty() && blocker.is_none(),
            ),
        };
        let control_gone = self
            .menu()
            .or(self.cancel)
            .is_none_or(|id| !tree.contains(id));
        if self.shown_control.as_ref() != Some(&control) || control_gone {
            clear_children(tree, host, text);
            self.menu = None;
            self.cancel = None;
            match &control {
                Control::Running(label) => {
                    caption(tree, host, format!("{label} が割り当てを決めています…"));
                    let id = tree.insert(
                        Some(host),
                        Button::new("中止").control_size(ControlSize::Small),
                    );
                    self.cancel = Some(id);
                }
                Control::Menu(_, enabled) => {
                    let items = planners
                        .iter()
                        .map(|p| MenuItem::new(format!("{} で決める", p.agent)))
                        .collect();
                    let id = tree.insert(
                        Some(host),
                        PullDownButton::new("自動で割り当て", items)
                            .control_size(ControlSize::Small),
                    );
                    tree.set_enabled(id, *enabled);
                    self.menu = Some((id, planners.clone()));
                }
            }
            self.shown_control = Some(control);
        }
        let lines = self.note_lines(form, detection, &planners, blocker);
        let note_gone = self.note.is_none_or(|id| !tree.contains(id));
        if self.shown_note.as_ref() == Some(&lines) && !note_gone {
            return;
        }
        let host = match self.note.filter(|id| tree.contains(*id)) {
            Some(id) => {
                clear_children(tree, id, text);
                id
            }
            None => {
                let id = tree.insert(Some(note), Container::column().gap(3.0));
                self.note = Some(id);
                id
            }
        };
        for line in &lines {
            let label = match line {
                Line::Heading(text) => Label::new(text.clone()).font_size(12.0).weight(600),
                Line::Text(text) => Label::new(text.clone()).font_size(11.5).color(Role::Muted),
            };
            tree.insert(Some(host), label.wrap(true));
        }
        self.shown_note = Some(lines);
    }

    /// The 中止 button's action, while it is shown.
    pub(super) fn actions(&self, actions: &mut Vec<(WidgetId, Action)>) {
        if let Some(id) = self.cancel {
            actions.push((id, Action::AutoCancel));
        }
    }

    fn note_lines(
        &self,
        form: &Form,
        detection: &Detection,
        planners: &[Planner],
        blocker: Option<String>,
    ) -> Vec<Line> {
        let mut lines = Vec::new();
        match &self.state {
            AutoState::Filled { assignment, .. } => {
                lines.push(Line::Heading(format!(
                    "自動割り当て（{} が決定）— 内容を確認して「{START_LABEL}」を押してください",
                    assignment.planner
                )));
                for pick in &assignment.picks {
                    let choice = choice_of(pick).map_or_else(|| pick.agent.clone(), |c| c.label());
                    let edited = if unchanged(form, pick) {
                        ""
                    } else {
                        "（手動で変更）"
                    };
                    lines.push(Line::Text(format!(
                        "{}: {choice} — {}{edited}",
                        slot_label(&pick.slot),
                        pick.reason
                    )));
                }
            }
            AutoState::Failed(why) => lines.push(Line::Text(why.clone())),
            AutoState::Idle | AutoState::Running { .. } => {}
        }
        let running = matches!(self.state, AutoState::Running { .. });
        match detection {
            // The check's own note already says why nothing can start.
            Detection::Pending => {}
            Detection::Done(Installed::Found(kinds)) if kinds.is_empty() => {}
            _ if planners.is_empty() => {
                lines.push(Line::Text(PlanError::NoPlanner.message("")));
            }
            _ => {
                if let Some(why) = blocker.filter(|_| !running) {
                    lines.push(Line::Text(format!("自動で割り当てるには: {why}")));
                }
            }
        }
        lines
    }
}

/// 「自動割り当てに失敗しました: {cause}。…」 (L1-2 item 5).
fn failure(cause: &str) -> String {
    format!("自動割り当てに失敗しました: {cause}。手動で選ぶか、別のエージェントで決めてください")
}

/// The pull-down's planners: the installed ones among claude and codex
/// (agy never plans, Q9), all of them when the check could not tell, none
/// while it runs.
pub(super) fn planners(detection: &Detection) -> Vec<Planner> {
    let installed: Vec<&str> = match detection {
        Detection::Pending => Vec::new(),
        Detection::Done(Installed::Found(kinds)) => kinds.iter().map(|k| k.command()).collect(),
        Detection::Done(Installed::Unknown(_)) => {
            AgentKind::ALL.iter().map(|k| k.command()).collect()
        }
    };
    auto_assign::planners(&installed)
}

/// The judge and verifier labels of the form: 判定役, 検証者 i.
pub(super) fn slot_label(slot: &str) -> String {
    match slot.strip_prefix("verifier-") {
        Some(n) => format!("検証者 {n}"),
        None if slot == JUDGE_SLOT => "判定役".to_owned(),
        None => slot.to_owned(),
    }
}

/// One slot per agent row, in form order: `judge`, `verifier-1` … (L1-3).
pub(super) fn slots_of(form: &Form) -> Vec<Slot> {
    std::iter::once(Slot {
        id: JUDGE_SLOT.to_owned(),
        role: JUDGE_ROLE.to_owned(),
    })
    .chain(form.panel.iter().enumerate().map(|(i, row)| {
        let strategy = match &row.strategy {
            StrategyChoice::Builtin(s) => s.label().to_owned(),
            StrategyChoice::Custom(text) => text.chars().take(CUSTOM_MAX).collect(),
        };
        Slot {
            id: format!("verifier-{}", i + 1),
            role: format!("verifier using the strategy '{strategy}'"),
        }
    }))
    .collect()
}

/// Each kind the menus offer, with the models and efforts they suggest.
pub(super) fn candidates(kinds: &[AgentKind]) -> Vec<Candidate> {
    kinds
        .iter()
        .map(|kind| Candidate {
            agent: kind.command().to_owned(),
            models: kind.models().iter().map(|m| (*m).to_owned()).collect(),
            efforts: kind.efforts().iter().map(|e| (*e).to_owned()).collect(),
        })
        .collect()
}

/// What the verification works on (L1-3), built on the planner's thread:
/// it reads the spec file and runs `git diff --stat`.
pub(super) fn content_of(form: &Form) -> impl FnOnce() -> String + Send + 'static {
    let form = form.clone();
    move || content(&form)
}

fn content(form: &Form) -> String {
    let mut out = String::new();
    let request = form.request.trim();
    out.push_str("Request (依頼):\n");
    out.push_str(if request.is_empty() {
        "(none)"
    } else {
        request
    });
    out.push_str("\n\nAcceptance criteria the person wrote:\n");
    let criteria = form.user_criteria();
    if criteria.is_empty() {
        out.push_str("(none: the judge drafts them from the request and the spec)\n");
    }
    for line in &criteria {
        out.push_str(&format!("- {line}\n"));
    }
    let target = match form.target() {
        Ok(target) => target,
        Err(why) => {
            out.push_str(&format!("\nTarget: (not readable: {why})\n"));
            return out;
        }
    };
    out.push_str(&format!(
        "\nTarget: {}\nRepository: {}\n",
        match target.kind {
            TargetKind::Diff => "a change (diff)",
            TargetKind::State => "the tree as it is",
        },
        target.repo.display()
    ));
    if target.kind == TargetKind::Diff {
        out.push_str(&format!(
            "Base: {}\n",
            target.base.as_deref().unwrap_or("(the default branch)")
        ));
    }
    out.push_str(&format!(
        "Head: {}\n",
        match &target.head {
            Head::Worktree => "the work tree, uncommitted changes included",
            Head::Ref(head) => head,
        }
    ));
    if !target.paths.is_empty() {
        out.push_str(&format!("Paths: {}\n", target.paths.join(", ")));
    }
    if target.kind == TargetKind::Diff {
        out.push_str("\nDiff summary (git diff --stat):\n");
        match diff_stat(&target) {
            Some(stat) => out.push_str(&stat),
            None => out.push_str("(diff summary unavailable)\n"),
        }
    }
    if let Some(asked) = &target.spec {
        out.push_str(&format!("\nSpec file {asked}:\n"));
        match spec_text(&target.repo, asked) {
            Some(spec) => out.push_str(&spec),
            None => out.push_str("(not readable)\n"),
        }
    }
    out
}

/// A ref the person typed that git would not read as an option.
fn plain_ref(name: &str) -> Option<&str> {
    (!name.is_empty() && !name.starts_with('-')).then_some(name)
}

/// `git diff --stat` of the target, cut to [`STAT_MAX`]: base…head, or the
/// work tree against the merge base. External diff drivers and textconv
/// filters are off, as the repository's config may name commands.
fn diff_stat(target: &Target) -> Option<String> {
    let base = match &target.base {
        Some(base) => base.clone(),
        None => verify::capture::default_base(&target.repo)?,
    };
    let base = plain_ref(&base)?;
    let mut cmd = OsCommand::new("git");
    cmd.arg("-C").arg(&target.repo).args([
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--stat",
    ]);
    match &target.head {
        Head::Worktree => cmd.args(["--merge-base", base]),
        Head::Ref(head) => cmd.arg(format!("{base}...{}", plain_ref(head)?)),
    };
    cmd.arg("--").args(&target.paths);
    let out = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| auto_assign::cap(&String::from_utf8_lossy(&out.stdout), STAT_MAX))
}

/// The first [`SPEC_MAX`] bytes of the spec file `asked` (relative to the
/// repository, or absolute inside it), when it is a regular file there: a
/// link is not followed, so nothing outside the repository is sent.
fn spec_text(repo: &Path, asked: &str) -> Option<String> {
    let root = verify::capture::repo_root(repo).ok()?;
    let given = Path::new(asked.trim());
    let relative: PathBuf = if given.is_absolute() {
        // As the capture reads it: the folder resolved (macOS's /tmp is
        // /private/tmp), the file itself not.
        let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
        let full = canonical(given.parent()?).join(given.file_name()?);
        full.strip_prefix(canonical(&root)).ok()?.to_owned()
    } else {
        given.to_owned()
    };
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }
    let path = root.join(relative);
    if !std::fs::symlink_metadata(&path).ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .ok()?
        .take(verify::capture::MAX_SPEC_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(auto_assign::cap(&String::from_utf8_lossy(&bytes), SPEC_MAX))
}

/// The planner run for `form`'s rows, the menus offering `kinds`.
fn job(planner: Planner, form: &Form, kinds: &[AgentKind]) -> Job {
    let slots = slots_of(form);
    let candidates = candidates(kinds);
    let content = content_of(form);
    let cwd = form
        .target()
        .map(|t| t.repo)
        .ok()
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"));
    Job {
        planner,
        shell: crate::session::runner::login_shell(),
        cwd,
        request: Box::new(move || Request {
            widget: WIDGET.to_owned(),
            run: RUN.to_owned(),
            slots,
            candidates,
            content: content(),
        }),
        check: Box::new(check),
    }
}

/// The widget's own check of a pick: what a start checks of a row
/// (`AgentSetup::check`).
pub(super) fn check(pick: &Pick) -> Result<(), String> {
    let kind = kind_of(&pick.agent)
        .ok_or_else(|| format!("{} は検証で使えるエージェントではありません", pick.agent))?;
    setup_of(pick).check(kind)
}

fn kind_of(agent: &str) -> Option<AgentKind> {
    AgentKind::ALL.into_iter().find(|k| k.command() == agent)
}

fn setup_of(pick: &Pick) -> AgentSetup {
    AgentSetup {
        model: pick.model.clone(),
        effort: pick.effort.clone(),
    }
}

/// The row choice a pick makes.
fn choice_of(pick: &Pick) -> Option<AgentChoice> {
    Some(AgentChoice {
        kind: kind_of(&pick.agent)?,
        setup: setup_of(pick),
    })
}

/// The row of `form` that `slot` names.
fn row_of<'a>(form: &'a Form, slot: &str) -> Option<&'a AgentChoice> {
    if slot == JUDGE_SLOT {
        return Some(&form.judge);
    }
    let n: usize = slot.strip_prefix("verifier-")?.parse().ok()?;
    form.panel.get(n.checked_sub(1)?).map(|row| &row.agent)
}

/// Puts each pick into its row of `form`: the agent, model and effort only
/// (the strategies and the row count stay, D2).
pub(super) fn fill(form: &mut Form, assignment: &Assignment) {
    for pick in &assignment.picks {
        let Some(choice) = choice_of(pick) else {
            continue;
        };
        if pick.slot == JUDGE_SLOT {
            form.judge = choice;
        } else if let Some(row) = pick
            .slot
            .strip_prefix("verifier-")
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| form.panel.get_mut(i))
        {
            row.agent = choice;
        }
    }
}

/// Whether `pick`'s row in `form` is still as the pick filled it.
pub(super) fn unchanged(form: &Form, pick: &Pick) -> bool {
    row_of(form, &pick.slot).is_some_and(|row| Some(row) == choice_of(pick).as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use verify::model::types::BuiltinStrategy;

    fn pick(slot: &str, agent: &str, model: Option<&str>, effort: Option<&str>) -> Pick {
        Pick {
            slot: slot.into(),
            agent: agent.into(),
            model: model.map(str::to_owned),
            effort: effort.map(str::to_owned),
            reason: "理由".into(),
        }
    }

    #[test]
    fn slots_are_the_judge_then_each_verifier_with_its_strategy() {
        let mut form = Form::default();
        form.panel[1].strategy = StrategyChoice::Custom("全ロケールで".repeat(100));
        let slots = slots_of(&form);
        let ids: Vec<&str> = slots.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["judge", "verifier-1", "verifier-2", "verifier-3"]);
        assert_eq!(slots[0].role, JUDGE_ROLE);
        assert_eq!(
            slots[1].role,
            format!(
                "verifier using the strategy '{}'",
                BuiltinStrategy::Conformance.label()
            )
        );
        let custom = slots[2]
            .role
            .trim_start_matches("verifier using the strategy '");
        assert_eq!(custom.trim_end_matches('\'').chars().count(), CUSTOM_MAX);
        assert_eq!(slot_label("judge"), "判定役");
        assert_eq!(slot_label("verifier-3"), "検証者 3");
    }

    #[test]
    fn candidates_carry_the_models_and_efforts_the_menus_suggest() {
        let candidates = candidates(&[AgentKind::Codex, AgentKind::Agy]);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].agent, "codex");
        assert_eq!(candidates[0].models, AgentKind::Codex.models());
        assert_eq!(candidates[1].efforts, AgentKind::Agy.efforts());
    }

    #[test]
    fn the_check_is_the_start_views_own() {
        assert!(check(&pick("judge", "claude", Some("opus"), Some("high"))).is_ok());
        let bad = check(&pick("judge", "codex", Some("-x"), None)).unwrap_err();
        assert!(bad.contains("codex のモデル名"), "{bad}");
        assert!(check(&pick("judge", "gemini", None, None)).is_err());
    }

    #[test]
    fn a_fill_sets_agents_only_and_unchanged_compares_each_row() {
        let mut form = Form::default();
        let strategies: Vec<StrategyChoice> =
            form.panel.iter().map(|r| r.strategy.clone()).collect();
        let assignment = Assignment {
            planner: "claude (sonnet · medium)".into(),
            picks: vec![
                pick("judge", "codex", Some("gpt-6.2-sol"), Some("high")),
                pick("verifier-1", "agy", None, None),
                pick("verifier-2", "claude", Some("opus"), None),
                pick("verifier-3", "codex", None, Some("low")),
            ],
        };
        fill(&mut form, &assignment);
        assert_eq!(form.judge.label(), "codex (gpt-6.2-sol · high)");
        assert_eq!(form.panel[0].agent.kind, AgentKind::Agy);
        assert_eq!(
            form.panel
                .iter()
                .map(|r| r.strategy.clone())
                .collect::<Vec<_>>(),
            strategies
        );
        assert!(assignment.picks.iter().all(|p| unchanged(&form, p)));
        form.panel[1].agent.setup.effort = Some("max".into());
        assert!(!unchanged(&form, &assignment.picks[2]));
        assert!(!unchanged(&form, &pick("verifier-9", "claude", None, None)));
    }

    #[test]
    fn the_content_holds_the_request_criteria_and_target_without_a_repository() {
        let form = Form {
            request: "新しいタブの cwd".into(),
            criteria: "a\n\nb\n".into(),
            ..Form::default()
        };
        let text = content(&form);
        assert!(text.contains("新しいタブの cwd"), "{text}");
        assert!(text.contains("- a\n- b\n"), "{text}");
        assert!(text.contains("リポジトリを指定"), "{text}");
    }

    #[test]
    fn the_spec_file_is_read_only_inside_the_repository() {
        let repo =
            std::env::temp_dir().join(format!("noa-verify-auto-spec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        let git = OsCommand::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .status();
        if !git.is_ok_and(|s| s.success()) {
            eprintln!("skipped: git is not available");
            return;
        }
        std::fs::write(repo.join("docs/spec.md"), "x".repeat(SPEC_MAX + 10)).unwrap();
        let spec = spec_text(&repo, "docs/spec.md").unwrap();
        assert!(spec.starts_with(&"x".repeat(SPEC_MAX)));
        assert!(spec.ends_with("[… 10 more bytes not shown]"), "{spec}");
        assert!(spec_text(&repo, "../outside.md").is_none());
        assert!(spec_text(&repo, "/etc/hosts").is_none());
        std::os::unix::fs::symlink("/etc/hosts", repo.join("docs/link.md")).unwrap();
        assert!(spec_text(&repo, "docs/link.md").is_none());
        let _ = std::fs::remove_dir_all(repo);
    }
}
