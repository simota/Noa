//! The review start form's adapter to the shared assignment planner.

use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::TryRecvError;

use noa_gpui_core::WidgetId;
use noa_gpui_core::widgets::{Button, ControlSize, MenuItem, PullDownButton};
use noa_gpui_text::TextStore;
use review::auto_assign::{
    self, Assignment, AutoRecord, Candidate, Job, Pick, PlanError, Planner, Planning, Request, Slot,
};
use review::model::types::{
    AgentChoice, AgentKind, AgentSetup, Head, LensChoice, Millis, ReviewerChoice, Target,
    TargetKind, custom_label,
};

use super::start::{self, Form};
use super::{Review, Source, caption, clear_children, note, system_now};

type Wake = Arc<dyn Fn() + Send + Sync>;
type Spawn = dyn Fn(Job, Wake) -> Planning;

pub(super) struct Auto {
    pub(super) spawn: Box<Spawn>,
    pub(super) control: Option<WidgetId>,
    planners: Vec<Planner>,
    state: State,
    filled: Option<Filled>,
}

enum State {
    Idle,
    Running(Running),
    Failed(String),
}

struct Filled {
    assignment: Assignment,
    at: Millis,
    lenses: Vec<LensChoice>,
}

struct Running {
    planner: Planner,
    planning: Planning,
    lead: AgentChoice,
    panel: Vec<ReviewerChoice>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.planning.cancel.store(true, Ordering::Release);
    }
}

impl Default for Auto {
    fn default() -> Self {
        Self {
            spawn: Box::new(|job, wake| auto_assign::spawn(job, "review-assign", move || wake())),
            control: None,
            planners: Vec::new(),
            state: State::Idle,
            filled: None,
        }
    }
}

fn choice(pick: &Pick) -> AgentChoice {
    let kind = AgentKind::ALL
        .into_iter()
        .find(|k| k.command() == pick.agent)
        .expect("shared validation admits only the request's candidates");
    AgentChoice {
        kind,
        setup: AgentSetup {
            model: pick.model.clone(),
            effort: pick.effort.clone(),
        },
    }
}

fn reviewer_index(pick: &Pick) -> Option<usize> {
    pick.slot
        .strip_prefix("reviewer-")?
        .parse::<usize>()
        .ok()?
        .checked_sub(1)
}

fn unchanged(form: &Form, lenses: &[LensChoice], pick: &Pick) -> bool {
    if pick.slot == "lead" {
        return form.lead == choice(pick);
    }
    reviewer_index(pick).is_some_and(|i| {
        form.panel
            .get(i)
            .is_some_and(|row| row.agent == choice(pick) && lenses.get(i) == Some(&row.lens))
    })
}

impl Auto {
    pub(super) fn reset(&mut self) {
        self.state = State::Idle;
        self.filled = None;
    }

    pub(super) fn record(&self, form: &Form) -> Option<AutoRecord> {
        if matches!(self.state, State::Running(_)) {
            return None;
        }
        let filled = self.filled.as_ref()?;
        AutoRecord::of(&filled.assignment, filled.at, |pick| {
            unchanged(form, &filled.lenses, pick)
        })
    }

    pub(super) fn cancel_running(&mut self) {
        if matches!(self.state, State::Running(_)) {
            self.state = State::Idle;
        }
    }
}

impl Review {
    pub(super) fn render_auto(&mut self, text: &mut TextStore) {
        let auto = &mut self.auto;
        let tree = &mut self.tree;
        let parts = &self.start;
        clear_children(tree, parts.auto_host, text);
        clear_children(tree, parts.auto_note, text);
        let host = Some(parts.auto_host);
        if let State::Running(run) = &auto.state {
            caption(
                tree,
                parts.auto_host,
                format!("{} が割り当てを決めています…", run.planner.agent),
            );
            auto.control =
                Some(tree.insert(host, Button::new("中止").control_size(ControlSize::Small)));
            return;
        }
        let form = start::read(tree, parts);
        let kinds = self.detection.fix_kinds().unwrap_or_default();
        auto.planners =
            auto_assign::planners(&kinds.iter().map(|k| k.command()).collect::<Vec<_>>());
        let menu = tree.insert(
            host,
            PullDownButton::new(
                "自動で割り当て",
                auto.planners
                    .iter()
                    .map(|p| MenuItem::new(format!("{} で決める", p.agent)))
                    .collect(),
            )
            .control_size(ControlSize::Small),
        );
        auto.control = Some(menu);
        let blocker = if kinds.is_empty() {
            None // The installed-agents note already explains this.
        } else if auto.planners.is_empty() {
            Some(PlanError::NoPlanner.message(""))
        } else {
            plan_target(&form).err()
        };
        tree.set_enabled(
            menu,
            matches!(self.source, Source::Live(_))
                && !self.starting
                && !auto.planners.is_empty()
                && blocker.is_none(),
        );
        if let Some(why) = blocker {
            note(tree, parts.auto_note, why);
        }
        if let State::Failed(why) = &auto.state {
            note(tree, parts.auto_note, why);
        }
        if let Some(Filled {
            assignment, lenses, ..
        }) = &auto.filled
        {
            note(
                tree,
                parts.auto_note,
                format!(
                    "自動割り当て（{} が決定）— 内容を確認して「レビューを開始」を押してください",
                    assignment.planner
                ),
            );
            for pick in &assignment.picks {
                let label = reviewer_index(pick)
                    .map_or_else(|| "リード".to_owned(), |i| format!("レビュアー {}", i + 1));
                let edited = if unchanged(&form, lenses, pick) {
                    ""
                } else {
                    "（手動で変更）"
                };
                note(
                    tree,
                    parts.auto_note,
                    format!(
                        "{label}: {} — {}{edited}",
                        choice(pick).label(),
                        pick.reason
                    ),
                );
            }
        }
    }

    pub(super) fn begin_auto(&mut self, index: usize, text: &mut TextStore) {
        let Source::Live(channel) = &self.source else {
            return;
        };
        if self.starting || self.session.is_some() || matches!(self.auto.state, State::Running(_)) {
            return;
        }
        let Some(&planner) = self.auto.planners.get(index) else {
            return;
        };
        let form = start::read(&self.tree, &self.start);
        let Ok(target) = plan_target(&form) else {
            return;
        };
        let cwd = match target.kind {
            TargetKind::Git | TargetKind::Files => target.path.clone(),
            TargetKind::Document => target.workdir.clone(),
        }
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_default();
        let kinds = self.start.kinds().to_vec();
        let lead = form.lead;
        let panel = form.panel;
        let roles = panel.clone();
        let job = Job {
            planner,
            cwd,
            shell: crate::session::runner::login_shell(),
            request: Box::new(move || Request {
                widget: "review".into(),
                run: RUN_DESCRIPTION.into(),
                slots: slots(&roles),
                candidates: candidates(&kinds),
                content: content(&form.request, &target),
            }),
            check: Box::new(|p| {
                let agent = choice(p);
                agent.setup.check(agent.kind)
            }),
        };
        // Each generation owns a fresh receiver. Dropping it also cancels the old job.
        let planning = (self.auto.spawn)(job, channel.wake.clone());
        self.auto.state = State::Running(Running {
            planner,
            planning,
            lead,
            panel,
        });
        self.render_auto(text);
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
                let mut form = start::read(&self.tree, &self.start);
                if form.lead != run.lead || form.panel != run.panel {
                    State::Failed("割り当て中にエージェントの選択が変わったため反映しませんでした。もう一度「自動で割り当て」を押してください".into())
                } else {
                    // The shared validator puts the picks in the requested slot order.
                    form.lead = choice(&assignment.picks[0]);
                    for (row, pick) in form.panel.iter_mut().zip(&assignment.picks[1..]) {
                        row.agent = choice(pick);
                    }
                    start::fill_agents(&mut self.tree, &mut self.start, &form, text);
                    let lenses = form.panel.into_iter().map(|row| row.lens).collect();
                    self.auto.filled = Some(Filled {
                        assignment,
                        at: system_now(),
                        lenses,
                    });
                    State::Idle
                }
            }
            Err(PlanError::Cancelled) => State::Idle,
            Err(err) => State::Failed(format!(
                "自動割り当てに失敗しました: {}。手動で選ぶか、別のエージェントで決めてください",
                err.message(&run.planner.label())
            )),
        };
        self.rebuild(text);
    }
}

const RUN_DESCRIPTION: &str = "a multi-agent code review: reviewers review alone, then \
    cross-examine each other's findings; the lead writes the verdict";

fn candidates(kinds: &[AgentKind]) -> Vec<Candidate> {
    kinds
        .iter()
        .map(|kind| Candidate {
            agent: kind.command().into(),
            models: kind.models().iter().map(|s| (*s).to_owned()).collect(),
            efforts: kind.efforts().iter().map(|s| (*s).to_owned()).collect(),
        })
        .collect()
}

fn slots(panel: &[ReviewerChoice]) -> Vec<Slot> {
    let lead = Slot {
        id: "lead".into(),
        role: "lead: consolidates every reviewer's findings over the rounds and writes the final verdict".into(),
    };
    std::iter::once(lead)
        .chain(panel.iter().enumerate().map(|(i, row)| {
            let (label, checklist) = match &row.lens {
                LensChoice::Builtin(id) => (id.label().to_owned(), id.checklist().to_owned()),
                LensChoice::Custom(s) => (custom_label(s), s.chars().take(300).collect()),
            };
            Slot {
                id: format!("reviewer-{}", i + 1),
                role: format!("reviewer from the lens '{label}': {checklist}"),
            }
        }))
        .collect()
}

fn plan_target(form: &Form) -> Result<Target, String> {
    let target = form.target()?;
    if target.kind == TargetKind::Document
        && let Some(path) = &target.path
    {
        let metadata = path
            .metadata()
            .map_err(|err| format!("{} を読めません: {err}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("{} は通常のファイルではありません", path.display()));
        }
    }
    Ok(target)
}

fn content(request: &str, target: &Target) -> String {
    let path = target.path.as_deref().unwrap_or(Path::new(""));
    let paths = target.paths.join("\n");
    let details = match target.kind {
        TargetKind::Files => format!("Files\nroot: {}\npaths:\n{paths}", path.display()),
        TargetKind::Document => {
            let document = match &target.text {
                Some(text) => text.clone(),
                None => {
                    let mut buf = Vec::new();
                    match File::options()
                        .read(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(path)
                        .and_then(|f| f.take(16 * 1024).read_to_end(&mut buf))
                    {
                        Ok(_) => String::from_utf8_lossy(&buf).into_owned(),
                        Err(err) => format!("(document unavailable: {err})"),
                    }
                }
            };
            format!("Document\n{document}")
        }
        TargetKind::Git => {
            let base = target
                .base
                .clone()
                .or_else(|| review::capture::default_base(path));
            let head = match &target.head {
                Head::Worktree => "worktree",
                Head::Ref(s) => s,
            };
            let summary = base
                .as_deref()
                .and_then(|b| diff_stat(path, b, &target.head, &target.paths))
                .unwrap_or_else(|| "(diff summary unavailable)".into());
            format!(
                "Git\nrepo: {}\nbase: {}\nhead: {head}\npaths:\n{paths}\n{summary}",
                path.display(),
                base.as_deref().unwrap_or("(default)")
            )
        }
    };
    format!("依頼・重点: {request}\n{details}")
}

fn diff_stat(repo: &Path, base: &str, head: &Head, paths: &[String]) -> Option<String> {
    let range = match head {
        Head::Ref(head) => format!("{base}...{head}"),
        Head::Worktree => {
            let output = Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["merge-base", "--", base, "HEAD"])
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            String::from_utf8(output.stdout).ok()?.trim().to_owned()
        }
    };
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--stat",
            "--end-of-options",
            &range,
            "--",
        ])
        .args(paths)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let mut buf = Vec::new();
    let prefix = (&mut out).take(8192).read_to_end(&mut buf);
    let rest = io::copy(&mut out, &mut io::sink());
    let status = child.wait().ok()?;
    prefix.ok()?;
    let skipped = rest.ok()?;
    if !status.success() {
        return None;
    }
    let mut summary = String::from_utf8_lossy(&buf).into_owned();
    if skipped > 0 {
        summary.push_str(&format!("\n[… {skipped} more bytes not shown]"));
    }
    Some(summary)
}
