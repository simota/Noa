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

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::TryRecvError;

use noa_gpui_core::widgets::{Button, Container, ControlSize, Label, MenuItem, PullDownButton};
use noa_gpui_core::{Role, UiTree, WidgetId};
use noa_gpui_text::TextStore;

use verify::auto_assign::{
    self, Assignment, AutoRecord, Candidate, Job, Pick, PlanError, Planner, Planning, Request, Slot,
};
use verify::model::types::{
    AgentChoice, AgentKind, AgentSetup, Head, Millis, StrategyChoice, TargetKind, VerifierChoice,
};

use super::start::{Detection, Form, Parts};
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

/// The agent rows a planner was started on, each verifier row with its
/// identity (`ids`, [`Parts::row_ids`]), to tell whether they changed
/// before its answer came: a row removed and another added in its place
/// is a change, whatever they hold.
type Rows = (AgentChoice, Vec<(WidgetId, VerifierChoice)>);

fn rows(form: &Form, ids: &[WidgetId]) -> Rows {
    (
        form.judge.clone(),
        ids.iter()
            .copied()
            .zip(form.panel.iter().cloned())
            .collect(),
    )
}

/// The current attempt. The last fill put in the form is kept apart
/// ([`Auto::filled`]): an attempt after it that fails or is cancelled
/// leaves the form as that fill made it, so its record stays.
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
    /// The whole line the note shows (a failure, or a refused fill).
    Failed(String),
}

/// The last fill put in the form: `assignment`, which arrived at `at`, and
/// the verifier rows it went into.
struct Filled {
    assignment: Assignment,
    at: Millis,
    /// Slot `verifier-i`'s row (entry i-1) by its identity, with the
    /// strategy it had: a pick is matched to its row, not to the row at
    /// its place, which another takes once a row above goes.
    rows: Vec<(WidgetId, StrategyChoice)>,
}

impl Filled {
    /// Whether `pick`'s row is still in the form (`form`, its verifier rows
    /// identified by `ids`) as the fill left it: the same agent, model and
    /// effort and, for a verifier, the same strategy, which the planner
    /// was told and the reason rests on.
    fn unchanged(&self, pick: &Pick, form: &Form, ids: &[WidgetId]) -> bool {
        let Some(choice) = choice_of(pick) else {
            return false;
        };
        if pick.slot == JUDGE_SLOT {
            return form.judge == choice;
        }
        let Some((id, strategy)) = verifier_index(&pick.slot).and_then(|i| self.rows.get(i)) else {
            return false;
        };
        ids.iter()
            .zip(&form.panel)
            .any(|(row, now)| row == id && now.agent == choice && now.strategy == *strategy)
    }
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
    filled: Option<Filled>,
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
            filled: None,
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

    /// Starts `planner` on `form`'s rows (its verifier rows identified by
    /// `ids`), the menus offering `kinds`; a planner still deciding is
    /// cancelled first. The last fill stays until this one replaces it.
    pub(super) fn begin(
        &mut self,
        planner: Planner,
        form: &Form,
        ids: &[WidgetId],
        kinds: &[AgentKind],
        wake: Box<dyn Fn() + Send>,
    ) {
        self.cancel();
        let planning = (self.plan)(job(planner, form, kinds), wake);
        self.state = AutoState::Running {
            planner,
            planning,
            snapshot: rows(form, ids),
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
        self.filled = None;
    }

    /// Takes the planner's answer once it has come: the form to put in the
    /// start view when it is a valid assignment and the rows are still the
    /// ones the planner was started on (`current`, its verifier rows
    /// identified by `ids`). A failure keeps the last fill.
    pub(super) fn poll(&mut self, current: &Form, ids: &[WidgetId], now: Millis) -> Option<Form> {
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
            Ok(_) if rows(current, ids) != *snapshot => {
                (AutoState::Failed(CHANGED.to_owned()), None)
            }
            Ok(assignment) => {
                let mut form = current.clone();
                fill(&mut form, &assignment);
                self.filled = Some(Filled {
                    assignment,
                    at: now,
                    rows: ids
                        .iter()
                        .copied()
                        .zip(form.panel.iter().map(|row| row.strategy.clone()))
                        .collect(),
                });
                (AutoState::Idle, Some(form))
            }
            Err(PlanError::Cancelled) => (AutoState::Idle, None),
            Err(why) => (AutoState::Failed(failure(&why.message(&label))), None),
        };
        self.state = state;
        filled
    }

    /// What a start now records: the last fill's picks still as filled,
    /// and the slots changed since (`form`, its verifier rows identified by
    /// `ids`). A planner still deciding is cancelled: the start goes ahead
    /// with the rows as they are.
    pub(super) fn record(&mut self, form: &Form, ids: &[WidgetId]) -> Option<AutoRecord> {
        self.cancel();
        let filled = self.filled.as_ref()?;
        AutoRecord::of(&filled.assignment, filled.at, |pick| {
            filled.unchanged(pick, form, ids)
        })
    }

    /// Draws the control into the agents' header of the start view
    /// (`parts`) and the assignment block (or why the control is off) into
    /// its note, each only when it changed or its widgets went with a
    /// rebuild of the note.
    pub(super) fn show(
        &mut self,
        tree: &mut UiTree,
        parts: &Parts,
        form: &Form,
        detection: &Detection,
        text: &mut TextStore,
    ) {
        let (host, note) = (parts.auto_host, parts.note());
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
        let lines = self.note_lines(form, &parts.row_ids(), detection, &planners, blocker);
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
        ids: &[WidgetId],
        detection: &Detection,
        planners: &[Planner],
        blocker: Option<String>,
    ) -> Vec<Line> {
        let mut lines = Vec::new();
        if let AutoState::Failed(why) = &self.state {
            lines.push(Line::Text(why.clone()));
        }
        if let Some(filled) = &self.filled {
            lines.push(Line::Heading(format!(
                "自動割り当て（{} が決定）— 内容を確認して「{START_LABEL}」を押してください",
                filled.assignment.planner
            )));
            for pick in &filled.assignment.picks {
                let choice = choice_of(pick).map_or_else(|| pick.agent.clone(), |c| c.label());
                let edited = if filled.unchanged(pick, form, ids) {
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
        match verify::engine::diff_stat(&target) {
            Some(stat) => out.push_str(&auto_assign::cap(&stat, STAT_MAX)),
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

/// The first [`SPEC_MAX`] bytes of the spec file `asked`, taken as the
/// start takes it (`capture::capture_spec`): relative to the top of the
/// repository, or absolute inside it, a leading `./` (or any `.` part)
/// dropped. It is opened through `fsx::open_read`, so a symbolic link
/// anywhere on the way (the file, or a folder above it) is refused and
/// nothing outside the repository is sent.
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
    let mut rel = PathBuf::new();
    for part in relative.components() {
        match part {
            Component::Normal(name) => rel.push(name),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if rel.as_os_str().is_empty() {
        return None;
    }
    let file = verify::fsx::open_read(&root, &root.join(rel)).ok()?;
    let (bytes, _) = verify::fsx::read_capped(file, verify::capture::MAX_SPEC_BYTES).ok()?;
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

/// The verifier row (from 0) that slot `verifier-i` names in the request.
fn verifier_index(slot: &str) -> Option<usize> {
    slot.strip_prefix("verifier-")?
        .parse::<usize>()
        .ok()?
        .checked_sub(1)
}

/// Puts each pick into its row of `form`: the agent, model and effort only
/// (the strategies and the row count stay, D2). The rows are those the
/// planner was asked about, in its order (the answer is put in only then).
pub(super) fn fill(form: &mut Form, assignment: &Assignment) {
    for pick in &assignment.picks {
        let Some(choice) = choice_of(pick) else {
            continue;
        };
        if pick.slot == JUDGE_SLOT {
            form.judge = choice;
        } else if let Some(row) = verifier_index(&pick.slot).and_then(|i| form.panel.get_mut(i)) {
            row.agent = choice;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command as OsCommand, Stdio};
    use verify::model::types::{BuiltinStrategy, Target};

    /// Git in `dir`, none of the person's hooks or signing settings in the
    /// way; `false` when it fails (or git is not there).
    fn git(dir: &Path, args: &[&str]) -> bool {
        OsCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// A new repository in a fresh temporary folder `name`, holding
    /// `files` in its one commit; `None` (the test skips) without git.
    fn repo_with(name: &str, files: &[(&str, &str)]) -> Option<PathBuf> {
        let repo =
            std::env::temp_dir().join(format!("noa-verify-auto-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        for (path, body) in files {
            let path = repo.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let made = git(&repo, &["init", "-q"])
            && git(&repo, &["add", "-A"])
            && git(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
        if !made {
            eprintln!("skipped: git is not available");
            return None;
        }
        Some(repo)
    }

    /// The form of a diff target in `dir`: its work tree against the last
    /// commit.
    fn startable(dir: &Path) -> Form {
        Form {
            kind: TargetKind::Diff,
            repo: dir.display().to_string(),
            base: "HEAD".into(),
            ..Form::default()
        }
    }

    /// A diff target: the work tree of `repo` against its last commit.
    fn worktree_target(repo: PathBuf, paths: &[&str]) -> Target {
        Target {
            kind: TargetKind::Diff,
            repo,
            base: Some("HEAD".into()),
            head: Head::Worktree,
            paths: paths.iter().map(|p| (*p).to_owned()).collect(),
            spec: None,
        }
    }

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
    fn a_fill_sets_agents_only_and_each_pick_is_matched_to_its_own_row() {
        let mut form = Form::default();
        let strategies: Vec<StrategyChoice> =
            form.panel.iter().map(|r| r.strategy.clone()).collect();
        let assignment = Assignment {
            planner: "claude (sonnet · medium)".into(),
            picks: vec![
                pick("judge", "codex", Some("gpt-6.2-sol"), Some("high")),
                pick("verifier-1", "agy", None, None),
                pick("verifier-2", "agy", None, None),
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
        let mut tree = UiTree::new();
        let root = tree.insert(None, Container::column());
        let mut ids: Vec<WidgetId> = (0..3)
            .map(|_| tree.insert(Some(root), Container::row()))
            .collect();
        let filled = Filled {
            rows: ids.iter().copied().zip(strategies).collect(),
            assignment,
            at: 1,
        };
        let picks = &filled.assignment.picks;
        assert!(picks.iter().all(|p| filled.unchanged(p, &form, &ids)));
        assert!(!filled.unchanged(&pick("verifier-9", "claude", None, None), &form, &ids));
        // A changed effort or strategy is a change of that row only.
        let mut edited = form.clone();
        edited.panel[2].agent.setup.effort = Some("max".into());
        edited.panel[1].strategy = StrategyChoice::Builtin(BuiltinStrategy::Regression);
        let still: Vec<bool> = picks
            .iter()
            .map(|p| filled.unchanged(p, &edited, &ids))
            .collect();
        assert_eq!(still, [true, true, false, false]);
        // Verifier 1's row goes: verifier 2's row, as alike as it was, moves
        // up into its place, and neither takes the other's pick.
        form.panel.remove(0);
        ids.remove(0);
        let still: Vec<bool> = picks
            .iter()
            .map(|p| filled.unchanged(p, &form, &ids))
            .collect();
        assert_eq!(still, [true, false, true, true]);
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
        let Some(repo) = repo_with("spec", &[]) else {
            return;
        };
        std::fs::create_dir_all(repo.join("docs")).unwrap();
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

    #[test]
    fn a_spec_reached_through_a_linked_folder_is_not_sent() {
        let Some(repo) = repo_with("spec-linked", &[]) else {
            return;
        };
        // `docs` in the repository is a link to a folder outside it.
        let outside = repo.with_extension("outside");
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("spec.md"), "not the repository's").unwrap();
        std::os::unix::fs::symlink(&outside, repo.join("docs")).unwrap();
        assert!(spec_text(&repo, "docs/spec.md").is_none());
        assert!(spec_text(&repo, "./docs/spec.md").is_none());
        let absolute = repo.join("docs/spec.md").display().to_string();
        assert!(spec_text(&repo, &absolute).is_none());
        let form = Form {
            spec: "docs/spec.md".into(),
            ..startable(&repo)
        };
        let text = content(&form);
        assert!(!text.contains("not the repository's"), "{text}");
        assert!(
            text.contains("Spec file docs/spec.md:\n(not readable)"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(repo);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn a_spec_path_may_start_with_dot_slash_as_the_start_takes_it() {
        let Some(repo) = repo_with("spec-dot", &[("docs/spec.md", "# 仕様\n")]) else {
            return;
        };
        assert_eq!(
            spec_text(&repo, "./docs/spec.md").as_deref(),
            Some("# 仕様\n")
        );
        assert_eq!(
            spec_text(&repo, "docs/./spec.md").as_deref(),
            Some("# 仕様\n")
        );
        assert!(spec_text(&repo, "./").is_none());
        // Relative to the top, whichever folder of the work tree is given.
        assert_eq!(
            spec_text(&repo.join("docs"), "./docs/spec.md").as_deref(),
            Some("# 仕様\n")
        );
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn the_diff_summary_runs_no_filter_driver_of_the_settings() {
        let files = [
            ("a.txt", "one\n"),
            (".gitattributes", "a.txt filter=probe\n"),
        ];
        let Some(repo) = repo_with("stat-filter", &files) else {
            return;
        };
        // A clean filter that leaves a mark when git runs it on a.txt.
        let marker = repo.with_extension("ran");
        let _ = std::fs::remove_file(&marker);
        let clean = format!("touch '{}'; cat", marker.display());
        assert!(git(&repo, &["config", "filter.probe.clean", &clean]));
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        let stat = verify::engine::diff_stat(&worktree_target(repo.clone(), &[])).unwrap();
        assert!(stat.contains("a.txt"), "{stat}");
        assert!(stat.contains("1 file changed, 1 insertion(+)"), "{stat}");
        assert!(!marker.exists(), "the summary ran the clean filter");
        // The probe is live: git as the person runs it does run the filter.
        let plain = OsCommand::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["diff", "--stat", "HEAD"])
            .output()
            .unwrap();
        assert!(plain.status.success());
        assert!(marker.exists(), "the probe filter never ran");
        let _ = std::fs::remove_dir_all(repo);
        let _ = std::fs::remove_file(marker);
    }

    #[test]
    fn the_diff_summary_reads_the_filter_from_the_top_of_the_repository() {
        let files = [("sub/a.txt", "one\n"), ("b.txt", "b\n")];
        let Some(repo) = repo_with("stat-sub", &files) else {
            return;
        };
        std::fs::write(repo.join("sub/a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(repo.join("b.txt"), "b\nc\n").unwrap();
        // The form names a folder below the top; the filter is the top's.
        let target = worktree_target(repo.join("sub"), &["sub/a.txt"]);
        let stat = verify::engine::diff_stat(&target).unwrap();
        assert!(stat.contains("sub/a.txt"), "{stat}");
        assert!(!stat.contains("b.txt"), "{stat}");
        assert!(stat.contains("1 file changed, 1 insertion(+)"), "{stat}");
        let form = Form {
            filter: "sub/a.txt".into(),
            ..startable(&repo.join("sub"))
        };
        let text = content(&form);
        assert!(
            text.contains("Diff summary (git diff --stat):\n sub/a.txt"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(repo);
    }
}
