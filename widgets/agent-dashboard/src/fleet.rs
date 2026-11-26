use std::path::PathBuf;

const CARD_TAIL_LINES: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Permission,
    Input,
    Finished,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Running,
    Quiet,
    Permission,
    Input,
    Ended,
}

impl Phase {
    pub const LEGEND: [Phase; 5] = [
        Phase::Running,
        Phase::Quiet,
        Phase::Permission,
        Phase::Input,
        Phase::Ended,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Phase::Running => "作業中",
            Phase::Quiet => "静止",
            Phase::Permission => "承認待ち",
            Phase::Input => "入力待ち",
            Phase::Ended => "終了後",
        }
    }

    pub fn waits_on_a_person(self) -> bool {
        matches!(self, Phase::Permission | Phase::Input)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Approval,
    Failure,
    Question,
    Notice,
    Quiet,
}

impl Need {
    pub fn symbol(self) -> &'static str {
        match self {
            Need::Approval => "!",
            Need::Failure => "×",
            Need::Question => "?",
            Need::Notice => "!",
            Need::Quiet => "…",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Need::Approval => "承認待ち",
            Need::Failure => "失敗",
            Need::Question => "質問",
            Need::Notice => "通知",
            Need::Quiet => "静止",
        }
    }

    /// The queue's order: what waits on a decision (an approval, then a
    /// question), then a failure to look at, then what only informs.
    pub fn rank(self) -> u8 {
        match self {
            Need::Approval => 0,
            Need::Question => 1,
            Need::Failure => 2,
            Need::Notice => 3,
            Need::Quiet => 4,
        }
    }

    pub fn tier(self) -> Tier {
        match self {
            Need::Approval | Need::Question => Tier::Decision,
            Need::Failure => Tier::Check,
            Need::Notice | Need::Quiet => Tier::Info,
        }
    }
}

/// The queue's sections. 参考情報 asks nobody for anything: it is left out
/// of the queue's count and folded away until asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Decision,
    Check,
    Info,
}

impl Tier {
    pub fn title(self) -> &'static str {
        match self {
            Tier::Decision => "判断待ち",
            Tier::Check => "要確認",
            Tier::Info => "参考情報",
        }
    }
}

/// What a row says a worker is doing (its 状態). No word for success
/// exists here: a finished worker, open or closed, has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "the worker list (dashboard-worker-list U3) shows it"
)]
pub enum Status {
    Approval,
    Input,
    Failed,
    Running,
    Ended,
}

#[allow(
    dead_code,
    reason = "the worker list (dashboard-worker-list U3) shows it"
)]
impl Status {
    pub fn title(self) -> &'static str {
        match self {
            Status::Approval => "承認待ち",
            Status::Input => "入力待ち",
            Status::Failed => "失敗",
            Status::Running => "実行中",
            Status::Ended => "終了",
        }
    }

    /// The list's order: an approval, an input, a failure, then what runs,
    /// then what has ended.
    pub fn rank(self) -> u8 {
        match self {
            Status::Approval => 0,
            Status::Input => 1,
            Status::Failed => 2,
            Status::Running => 3,
            Status::Ended => 4,
        }
    }
}

/// Which list a worker is in: 現在 and 終了 split every worker between
/// them, and 要対応 is the part of 現在 that waits on a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "the worker list (dashboard-worker-list U3) switches it"
)]
pub enum Scope {
    Current,
    Attention,
    Ended,
}

#[allow(
    dead_code,
    reason = "the worker list (dashboard-worker-list U3) switches it"
)]
impl Scope {
    pub const ALL: [Scope; 3] = [Scope::Current, Scope::Attention, Scope::Ended];

    pub fn title(self) -> &'static str {
        match self {
            Scope::Current => "現在",
            Scope::Attention => "要対応",
            Scope::Ended => "終了",
        }
    }

    /// Its name in the saved view.
    pub fn name(self) -> &'static str {
        match self {
            Scope::Current => "current",
            Scope::Attention => "attention",
            Scope::Ended => "ended",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Scope::ALL.into_iter().find(|scope| scope.name() == name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Worker {
    /// The pane id: what the dashboard keys a worker by.
    pub pane: u64,
    pub label: String,
    pub kind: String,
    /// Empty when unknown, like `task`, `detail` and `model`.
    pub branch: String,
    pub model: String,
    pub effort: String,
    pub orchestrator: String,
    /// The orchestrator's pane, when it is one: what groups and graph roots
    /// are keyed by, since two orchestrators can share a name.
    pub orchestrator_pane: Option<u64>,
    pub parent: Option<u64>,
    pub state: State,
    pub attention: bool,
    pub task: String,
    pub detail: String,
    pub since: String,
    pub wait_minutes: f64,
    pub quiet: bool,
    pub reports: u64,
    /// How long ago the worker started, when that is known.
    pub started_minutes: Option<f64>,
    /// How long a done worker worked, when both its start and its finish
    /// are known.
    pub took_minutes: Option<f64>,
    /// Whether the worker is known to have left a result file.
    pub has_result: bool,
    /// That file, when the live feed found it (`$NOA_RESULT`, from its
    /// spawner's journal): the drawer reads it when asked to.
    pub result: Option<PathBuf>,
    pub screen_tail: Vec<String>,
    pub lines_per_tick: Vec<f64>,
    pub freshness: f64,
    pub phases: Vec<(Phase, f64)>,
    /// For a worker whose pane is gone, rebuilt from its spawner's journal:
    /// the id its pane had. `pane` is then a key of its own, never a pane's.
    pub closed_pane: Option<u64>,
    /// Where it works: its pane's cwd, or for a closed worker the cwd its
    /// spawn was journaled with; empty when unknown. A path, kept as Noa
    /// gave it (never put on one line): 差分を見る opens the Repo Browser
    /// on the worktree holding it.
    pub cwd: String,
}

impl Worker {
    pub fn need(&self) -> Option<Need> {
        // Its pane is gone: there is nothing left to act on, a failure
        // included (it stays failed in the statistics).
        if self.is_closed() {
            return None;
        }
        match self.state {
            State::Permission => Some(Need::Approval),
            State::Error => Some(Need::Failure),
            State::Input => Some(Need::Question),
            State::Running if self.quiet => Some(Need::Quiet),
            State::Running | State::Finished if self.attention => Some(Need::Notice),
            State::Running | State::Finished => None,
        }
    }

    /// The one definition of 要対応: its pane is open and it waits on an
    /// approval or an input, or it failed. A notice is not, nor is a quiet
    /// worker: both still run or have ended on their own.
    pub fn needs_attention(&self) -> bool {
        !self.is_closed() && matches!(self.state, State::Permission | State::Input | State::Error)
    }

    #[allow(
        dead_code,
        reason = "the worker list (dashboard-worker-list U3) shows it"
    )]
    pub fn status(&self) -> Status {
        match self.state {
            State::Permission if !self.is_closed() => Status::Approval,
            State::Input if !self.is_closed() => Status::Input,
            State::Error => Status::Failed,
            State::Running if !self.is_closed() => Status::Running,
            _ => Status::Ended,
        }
    }

    #[allow(
        dead_code,
        reason = "the worker list (dashboard-worker-list U3) filters by it"
    )]
    pub fn in_scope(&self, scope: Scope) -> bool {
        let current = !self.is_closed() && self.state != State::Finished;
        match scope {
            Scope::Current => current,
            Scope::Attention => self.needs_attention(),
            Scope::Ended => !current,
        }
    }

    /// The last lines of the screen tail, as a queue card shows them (the
    /// drawer shows the whole tail).
    pub fn card_tail(&self) -> &[String] {
        let from = self.screen_tail.len().saturating_sub(CARD_TAIL_LINES);
        &self.screen_tail[from..]
    }

    /// Whether a bulk close may take the worker without anyone looking at
    /// it: it has ended (finished or failed). One running, or waiting on a
    /// person for an approval or an answer, never.
    pub fn can_close_unseen(&self) -> bool {
        !self.is_closed() && matches!(self.state, State::Finished | State::Error)
    }

    /// Whether the worker's pane is gone (`closed_pane`): nothing can be
    /// sent to it, opened or closed.
    pub fn is_closed(&self) -> bool {
        self.closed_pane.is_some()
    }

    /// One of the coding agents, which have a model; any other kind is a
    /// plain command.
    pub fn is_agent(&self) -> bool {
        matches!(self.kind.as_str(), "claude" | "codex" | "agy")
    }

    pub fn model_line(&self) -> String {
        match (self.model.as_str(), self.effort.as_str()) {
            ("", "") => "モデル既定".to_owned(),
            ("", effort) => format!("モデル既定 · {effort}"),
            (model, "") => model.to_owned(),
            (model, effort) => format!("{model} · {effort}"),
        }
    }

    pub fn minutes_where(&self, matches: impl Fn(Phase) -> bool) -> f64 {
        // Folded from +0: an empty `sum` of floats is -0.
        self.phases
            .iter()
            .filter(|(phase, _)| matches(*phase))
            .fold(0.0, |total, (_, minutes)| total + minutes.max(0.0))
    }

    pub fn total_minutes(&self) -> f64 {
        self.minutes_where(|_| true)
    }

    /// The worker with every text on one line ([`one_line`]): what the
    /// live feed builds from what Noa reports, so no text it shows can make
    /// the shaper panic.
    pub fn on_one_line(self) -> Worker {
        let line = |text: String| one_line(&text);
        Worker {
            label: line(self.label),
            kind: line(self.kind),
            branch: line(self.branch),
            model: line(self.model),
            effort: line(self.effort),
            orchestrator: line(self.orchestrator),
            task: line(self.task),
            detail: line(self.detail),
            since: line(self.since),
            screen_tail: self.screen_tail.into_iter().map(line).collect(),
            ..self
        }
    }

    /// The worker without the values that change as time passes: two workers
    /// equal under this draw the same tree, and differ only in the texts,
    /// meters and lanes the dashboard updates in place.
    fn structure(&self) -> Worker {
        Worker {
            since: String::new(),
            wait_minutes: 0.0,
            reports: 0,
            started_minutes: None,
            took_minutes: None,
            lines_per_tick: Vec::new(),
            freshness: 0.0,
            phases: Vec::new(),
            ..self.clone()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fleet {
    pub workers: Vec<Worker>,
    /// The roots of the relation graph, in the order it draws them.
    pub orchestrators: Vec<Orchestrator>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Orchestrator {
    pub name: String,
    /// Its pane, when the orchestrator is an open pane; the graph then marks
    /// the root with that pane's state, if it is a worker.
    pub pane: Option<u64>,
}

impl Orchestrator {
    fn named(name: &str) -> Self {
        Orchestrator {
            name: name.to_owned(),
            pane: None,
        }
    }
}

impl Fleet {
    pub fn worker(&self, pane: u64) -> Option<&Worker> {
        self.workers.iter().find(|w| w.pane == pane)
    }

    /// The worker's parent as a row and the drawer name it: the parent
    /// worker's label (else its pane), なし for an orchestrator itself or a
    /// worker started alone, 不明 when unknown, else the orchestrator.
    #[allow(
        dead_code,
        reason = "the worker list and drawer (dashboard-worker-list U3, U4) show it"
    )]
    pub fn parent_name(&self, worker: &Worker) -> String {
        match worker.parent {
            Some(pane) => self
                .worker(pane)
                .map_or_else(|| format!("ペイン {pane}"), |w| one_line(&w.label)),
            None if worker.orchestrator_pane == Some(worker.pane)
                || worker.orchestrator == crate::live::ALONE =>
            {
                "なし".to_owned()
            }
            None if worker.orchestrator.is_empty() => "不明".to_owned(),
            None => one_line(&worker.orchestrator),
        }
    }

    pub fn same_structure(&self, other: &Fleet) -> bool {
        self.orchestrators == other.orchestrators
            && self.workers.len() == other.workers.len()
            && self
                .workers
                .iter()
                .zip(&other.workers)
                .all(|(a, b)| a.structure() == b.structure())
    }
}

/// `text` as one paragraph: each control character (line breaks, tabs,
/// U+0085 NEL) and each line or paragraph separator (U+2028, U+2029)
/// becomes a space, character for character, so the result for a prefix of
/// `text` is a prefix of the result (a caret maps through). A label draws a line break as one; and
/// cosmic-text's shaper panics on a text that holds a paragraph separator
/// and both text directions ("A\u{2029}\u{5D0}"). Everything else, bidi
/// controls included, is kept.
///
/// The one rule for what enters the dashboard: the live feed's texts
/// (`Worker::on_one_line`), the header's status, the panels that draw
/// worker texts, and the filter field. The drawer's own `one_line`
/// (`dashboard/drawer.rs`) is the same rule but for CR LF, which it shows
/// as one space.
pub fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// A duration as the dashboard writes it: seconds under a minute, else whole
/// minutes.
pub fn minutes_text(minutes: f64) -> String {
    // Never negative, and never -0 (`-0.0 > 0.0` is false, like NaN).
    let minutes = if minutes > 0.0 { minutes } else { 0.0 };
    let seconds = (minutes * 60.0).round();
    if seconds < 60.0 {
        format!("{seconds:.0} 秒")
    } else {
        format!("{minutes:.0} 分")
    }
}

/// A group's name, and for orchestrators the pane that tells two of the same
/// name apart.
type GroupKey<'a> = (Option<&'a str>, Option<u64>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grouping {
    Orchestrator,
    Agent,
    Model,
    Flat,
}

impl Grouping {
    pub const ALL: [Grouping; 4] = [
        Grouping::Orchestrator,
        Grouping::Agent,
        Grouping::Model,
        Grouping::Flat,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Grouping::Orchestrator => "親",
            Grouping::Agent => "エージェント",
            Grouping::Model => "モデル",
            Grouping::Flat => "なし",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "parent" => Some(Grouping::Orchestrator),
            "agent" => Some(Grouping::Agent),
            "model" => Some(Grouping::Model),
            "none" => Some(Grouping::Flat),
            _ => None,
        }
    }

    fn key(self, worker: &Worker) -> GroupKey<'_> {
        match self {
            Grouping::Orchestrator => (Some(&worker.orchestrator), worker.orchestrator_pane),
            Grouping::Agent => (Some(&worker.kind), None),
            Grouping::Model if !worker.is_agent() => (Some("モデルなし"), None),
            Grouping::Model if worker.model.is_empty() => (Some("モデル既定"), None),
            Grouping::Model => (Some(&worker.model), None),
            Grouping::Flat => (None, None),
        }
    }

    pub fn split<'a>(
        self,
        workers: impl IntoIterator<Item = &'a Worker>,
    ) -> Vec<(Option<&'a str>, Vec<&'a Worker>)> {
        let mut groups: Vec<(GroupKey<'a>, Vec<&'a Worker>)> = Vec::new();
        for worker in workers {
            let key = self.key(worker);
            match groups.iter_mut().find(|(group, _)| *group == key) {
                Some((_, members)) => members.push(worker),
                None => groups.push((key, vec![worker])),
            }
        }
        groups
            .into_iter()
            .map(|((name, _), members)| (name, members))
            .collect()
    }
}

pub const FRESHNESS_MAX: f64 = 5.0;

pub fn canned() -> Fleet {
    let mut workers = vec![
        Worker {
            pane: 11,
            label: "api-refactor".into(),
            kind: "claude".into(),
            branch: "feat/api-v2".into(),
            model: "opus".into(),
            effort: "high".into(),
            orchestrator: "release-0.9.7".into(),
            orchestrator_pane: None,
            parent: None,
            state: State::Running,
            attention: false,
            task: "Handler の分割を noa-grid 側に反映し、テストを通す".into(),
            detail: "cargo test -p noa-grid を実行中".into(),
            since: "8 秒前に出力".into(),
            wait_minutes: 0.0,
            quiet: false,
            reports: 7,
            started_minutes: None,
            took_minutes: None,
            has_result: false,
            result: None,
            screen_tail: Vec::new(),
            lines_per_tick: vec![
                2.0, 6.0, 14.0, 9.0, 3.0, 18.0, 22.0, 11.0, 4.0, 7.0, 16.0, 12.0,
            ],
            freshness: 5.0,
            closed_pane: None,
            cwd: String::new(),
            phases: vec![
                (Phase::Running, 20.0),
                (Phase::Permission, 3.0),
                (Phase::Running, 14.0),
            ],
        },
        Worker {
            pane: 12,
            label: "docs-sync".into(),
            kind: "agy".into(),
            branch: "docs/sync".into(),
            model: "gemini-4-argon-medium".into(),
            effort: "".into(),
            orchestrator: "release-0.9.7".into(),
            orchestrator_pane: None,
            parent: None,
            state: State::Running,
            attention: false,
            task: "機能一覧を実装に合わせて更新する".into(),
            detail: "docs/FEATURES.md を更新中".into(),
            since: "2 秒前に出力".into(),
            wait_minutes: 0.0,
            quiet: false,
            reports: 3,
            started_minutes: None,
            took_minutes: None,
            has_result: false,
            result: None,
            screen_tail: Vec::new(),
            lines_per_tick: vec![1.0, 1.0, 3.0, 2.0, 5.0, 4.0, 2.0, 6.0, 3.0, 5.0, 4.0, 6.0],
            freshness: 5.0,
            closed_pane: None,
            cwd: String::new(),
            phases: vec![
                (Phase::Running, 6.0),
                (Phase::Input, 1.0),
                (Phase::Running, 6.0),
            ],
        },
        Worker {
            pane: 13,
            label: "perf-probe".into(),
            kind: "codex".into(),
            branch: "perf/reflow".into(),
            model: "gpt-6.2-astra".into(),
            effort: "high".into(),
            orchestrator: "perf-wish".into(),
            orchestrator_pane: None,
            parent: None,
            state: State::Running,
            attention: false,
            task: "reflow の計測を全端末で取り直す".into(),
            detail: "bench/run_all.sh 3/8".into(),
            since: "出力なし 3 分".into(),
            wait_minutes: 3.0,
            quiet: true,
            reports: 1,
            started_minutes: None,
            took_minutes: None,
            has_result: false,
            result: None,
            screen_tail: lines(&["[3/8] ascii を計測中 …", "（3 分間 出力なし）"]),
            lines_per_tick: vec![
                19.0, 24.0, 17.0, 21.0, 12.0, 6.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0,
            ],
            freshness: 1.0,
            closed_pane: None,
            cwd: String::new(),
            phases: vec![(Phase::Running, 38.0), (Phase::Quiet, 3.0)],
        },
        Worker {
            pane: 14,
            label: "fix-tests".into(),
            kind: "claude".into(),
            branch: "fix/flaky-pty".into(),
            model: "sonnet".into(),
            effort: "medium".into(),
            orchestrator: "release-0.9.7".into(),
            orchestrator_pane: None,
            parent: Some(11),
            state: State::Permission,
            attention: false,
            task: "不安定な pty テストの原因を特定して直す".into(),
            detail: "Bash: rm -rf target/tmp を実行してよいですか".into(),
            since: "2 分".into(),
            wait_minutes: 2.0,
            quiet: false,
            reports: 5,
            started_minutes: None,
            took_minutes: None,
            has_result: false,
            result: None,
            screen_tail: lines(&[
                "Bash command",
                "  rm -rf target/tmp",
                "Do you want to proceed?",
                "> 1. Yes    2. No",
            ]),
            lines_per_tick: Vec::new(),
            freshness: 0.0,
            closed_pane: None,
            cwd: String::new(),
            phases: vec![(Phase::Running, 18.0), (Phase::Permission, 2.0)],
        },
        Worker {
            pane: 15,
            label: "migrate-cfg".into(),
            kind: "codex".into(),
            branch: "feat/config-v2".into(),
            model: "gpt-6.2-sol".into(),
            effort: "medium".into(),
            orchestrator: "perf-wish".into(),
            orchestrator_pane: None,
            parent: None,
            state: State::Permission,
            attention: false,
            task: "設定キーの読み込みを新形式へ移す".into(),
            detail: "apply_patch: crates/noa-config/src/lib.rs".into(),
            since: "40 秒".into(),
            wait_minutes: 0.7,
            quiet: false,
            reports: 2,
            started_minutes: None,
            took_minutes: None,
            has_result: false,
            result: None,
            screen_tail: lines(&[
                "apply_patch crates/noa-config/src/lib.rs",
                "  +12 -3",
                "Allow this change? [y/n]",
            ]),
            lines_per_tick: Vec::new(),
            freshness: 0.0,
            closed_pane: None,
            cwd: String::new(),
            phases: vec![(Phase::Running, 9.0), (Phase::Permission, 0.7)],
        },
        Worker {
            pane: 16,
            label: "spec-review".into(),
            kind: "claude".into(),
            branch: "main".into(),
            model: "".into(),
            effort: "".into(),
            orchestrator: "release-0.9.7".into(),
            orchestrator_pane: None,
            parent: None,
            state: State::Input,
            attention: false,
            task: "仕様書の未決事項を洗い出して確認する".into(),
            detail: "質問: 既定値は A と B のどちらにしますか".into(),
            since: "6 分".into(),
            wait_minutes: 6.0,
            quiet: false,
            reports: 4,
            started_minutes: None,
            took_minutes: None,
            has_result: false,
            result: None,
            screen_tail: lines(&[
                "既定値は A と B のどちらにしますか",
                "> 1. A（現行どおり）",
                "  2. B（新しい既定）",
            ]),
            lines_per_tick: Vec::new(),
            freshness: 0.0,
            closed_pane: None,
            cwd: String::new(),
            phases: vec![(Phase::Running, 7.0), (Phase::Input, 6.0)],
        },
        Worker {
            pane: 17,
            label: "lint-pass".into(),
            kind: "agy".into(),
            branch: "chore/clippy".into(),
            model: "gemini-4-hydrogen-low".into(),
            effort: "".into(),
            orchestrator: "release-0.9.7".into(),
            orchestrator_pane: None,
            parent: Some(11),
            state: State::Finished,
            attention: false,
            task: "clippy の警告をすべて解消する".into(),
            detail: "clippy 警告 0 件".into(),
            since: "4 分前に完了".into(),
            wait_minutes: 0.0,
            quiet: false,
            reports: 6,
            started_minutes: None,
            took_minutes: None,
            has_result: false,
            result: None,
            screen_tail: Vec::new(),
            lines_per_tick: Vec::new(),
            freshness: 0.0,
            closed_pane: None,
            cwd: String::new(),
            phases: vec![
                (Phase::Running, 8.0),
                (Phase::Permission, 1.0),
                (Phase::Running, 7.0),
                (Phase::Ended, 4.0),
            ],
        },
        Worker {
            pane: 18,
            label: "changelog".into(),
            kind: "claude".into(),
            branch: "chore/release".into(),
            model: "sonnet".into(),
            effort: "low".into(),
            orchestrator: "release-0.9.7".into(),
            orchestrator_pane: None,
            parent: None,
            state: State::Finished,
            attention: false,
            task: "次のリリースの変更履歴を書く".into(),
            detail: "CHANGELOG を更新".into(),
            since: "11 分前に完了".into(),
            wait_minutes: 0.0,
            quiet: false,
            reports: 2,
            started_minutes: None,
            took_minutes: None,
            has_result: false,
            result: None,
            screen_tail: Vec::new(),
            lines_per_tick: Vec::new(),
            freshness: 0.0,
            closed_pane: None,
            cwd: String::new(),
            phases: vec![(Phase::Running, 6.0), (Phase::Ended, 11.0)],
        },
        Worker {
            pane: 19,
            label: "e2e-smoke".into(),
            kind: "codex".into(),
            branch: "test/e2e".into(),
            model: "gpt-6-luna".into(),
            effort: "low".into(),
            orchestrator: "perf-wish".into(),
            orchestrator_pane: None,
            parent: Some(13),
            state: State::Error,
            attention: false,
            task: "e2e シナリオを一通り流す".into(),
            detail: "exit 1 · e2e スクリプトが失敗".into(),
            since: "1 分".into(),
            wait_minutes: 1.0,
            quiet: false,
            reports: 3,
            started_minutes: None,
            took_minutes: None,
            has_result: false,
            result: None,
            screen_tail: lines(&["FAIL scenario 3", "1 failed, 6 passed", "[exited 1]"]),
            lines_per_tick: Vec::new(),
            freshness: 0.0,
            closed_pane: None,
            cwd: String::new(),
            phases: vec![(Phase::Running, 22.0), (Phase::Ended, 1.0)],
        },
    ];
    // Every canned time counts as known.
    for worker in &mut workers {
        worker.cwd = format!("/work/noa-wt/{}", worker.label);
        worker.started_minutes = Some(worker.total_minutes());
        worker.took_minutes = Some(worker.minutes_where(|phase| phase != Phase::Ended));
        worker.has_result = worker.state == State::Finished;
    }
    Fleet {
        workers,
        orchestrators: vec![
            Orchestrator::named("release-0.9.7"),
            Orchestrator::named("perf-wish"),
        ],
    }
}

/// The canned fleet with two workers whose panes are gone, as the live
/// feed rebuilds them from the spawners' journals (`--demo-closed`).
pub fn canned_with_closed() -> Fleet {
    let mut fleet = canned();
    let closed = |key: u64, pane: u64, label: &str, kind: &str, orchestrator: &str| Worker {
        pane: (1 << 63) | key,
        label: label.into(),
        kind: kind.into(),
        branch: String::new(),
        model: String::new(),
        effort: String::new(),
        orchestrator: orchestrator.into(),
        orchestrator_pane: None,
        parent: None,
        state: State::Finished,
        attention: false,
        task: String::new(),
        detail: String::new(),
        since: String::new(),
        wait_minutes: 0.0,
        quiet: false,
        reports: 0,
        started_minutes: None,
        took_minutes: None,
        has_result: false,
        result: None,
        screen_tail: Vec::new(),
        lines_per_tick: Vec::new(),
        freshness: 0.0,
        phases: Vec::new(),
        closed_pane: Some(pane),
        cwd: String::new(),
    };
    fleet.workers.extend([
        Worker {
            branch: "perf/ascii".into(),
            model: "gpt-6.2-astra".into(),
            effort: "high".into(),
            task: "ascii 軸を 3 回計測して中央値を出す".into(),
            detail: "最後の待ち結果: finished".into(),
            since: "25 分前に閉じた".into(),
            started_minutes: Some(43.0),
            took_minutes: Some(18.0),
            has_result: true,
            phases: vec![(Phase::Running, 18.0), (Phase::Ended, 25.0)],
            cwd: "/work/noa-wt/bench-ascii".into(),
            ..closed(1, 21, "bench-ascii", "codex", "perf-wish")
        },
        Worker {
            model: "sonnet".into(),
            task: "README の誤字を直す".into(),
            since: "閉じた".into(),
            started_minutes: Some(180.0),
            ..closed(2, 4, "docs-typo", "claude", "閉じた親 9f3a")
        },
    ]);
    fleet
        .orchestrators
        .push(Orchestrator::named("閉じた親 9f3a"));
    fleet
}

fn lines(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minutes_text_gives_seconds_under_a_minute() {
        assert_eq!(minutes_text(0.0), "0 秒");
        assert_eq!(minutes_text(42.0 / 60.0), "42 秒");
        assert_eq!(minutes_text(59.4 / 60.0), "59 秒");
        assert_eq!(minutes_text(59.6 / 60.0), "1 分");
        assert_eq!(minutes_text(13.7), "14 分");
    }

    #[test]
    fn minutes_text_never_prints_a_negative_or_minus_zero() {
        for minutes in [-0.0, -1e-9, -0.4 / 60.0, -5.0, f64::NAN] {
            assert_eq!(minutes_text(minutes), "0 秒", "{minutes}");
        }
    }

    #[test]
    fn minutes_where_nothing_matches_is_plus_zero() {
        let worker = canned().workers.remove(0);
        let none = worker.minutes_where(|_| false);
        assert!(none == 0.0 && none.is_sign_positive(), "{none}");
        assert_eq!(minutes_text(none), "0 秒");
    }

    #[test]
    fn a_volatile_change_keeps_the_structure() {
        let fleet = canned();
        let mut later = fleet.clone();
        for worker in &mut later.workers {
            worker.since = "later".into();
            worker.wait_minutes += 1.0;
            worker.reports += 1;
            worker.lines_per_tick.reverse();
            worker.freshness = 0.0;
            worker.phases.push((Phase::Running, 1.0));
            worker.started_minutes = Some(99.0);
            worker.took_minutes = None;
        }
        assert!(fleet.same_structure(&later));
        let mut changed = later.clone();
        changed.workers[0].state = State::Permission;
        assert!(!fleet.same_structure(&changed));
        let mut changed = later.clone();
        changed.workers[0].screen_tail.push("new".into());
        assert!(!fleet.same_structure(&changed));
        let mut changed = later;
        changed.workers.pop();
        assert!(!fleet.same_structure(&changed));
    }

    #[test]
    fn attention_is_a_need_only_when_nothing_else_is() {
        let mut worker = canned().workers.remove(0);
        worker.attention = true;
        assert_eq!(worker.need(), Some(Need::Notice));
        worker.state = State::Finished;
        assert_eq!(worker.need(), Some(Need::Notice));
        worker.state = State::Permission;
        assert_eq!(worker.need(), Some(Need::Approval));
        worker.state = State::Running;
        worker.quiet = true;
        assert_eq!(worker.need(), Some(Need::Quiet));
    }

    /// A canned worker in `state`, its pane open unless `closed`.
    fn worker_in(state: State, closed: bool) -> Worker {
        Worker {
            state,
            quiet: false,
            attention: false,
            closed_pane: closed.then_some(7),
            ..canned().workers.remove(0)
        }
    }

    /// (status, 現在, 要対応, 終了)
    fn row(worker: &Worker) -> (Status, bool, bool, bool) {
        (
            worker.status(),
            worker.in_scope(Scope::Current),
            worker.in_scope(Scope::Attention),
            worker.in_scope(Scope::Ended),
        )
    }

    #[test]
    fn open_waiting_workers_need_attention() {
        let approval = worker_in(State::Permission, false);
        assert_eq!(row(&approval), (Status::Approval, true, true, false));
        assert!(approval.needs_attention());
        let input = worker_in(State::Input, false);
        assert_eq!(row(&input), (Status::Input, true, true, false));
        assert!(input.needs_attention());
        let failed = worker_in(State::Error, false);
        assert_eq!(row(&failed), (Status::Failed, true, true, false));
        assert!(failed.needs_attention());
    }

    #[test]
    fn a_running_worker_is_current_quiet_or_not() {
        let running = worker_in(State::Running, false);
        assert_eq!(row(&running), (Status::Running, true, false, false));
        let quiet = Worker {
            quiet: true,
            ..running
        };
        assert_eq!(quiet.need(), Some(Need::Quiet));
        assert!(!quiet.needs_attention());
        assert_eq!(row(&quiet), (Status::Running, true, false, false));
    }

    #[test]
    fn a_notice_never_needs_attention() {
        for (state, expected) in [
            (State::Running, (Status::Running, true, false, false)),
            (State::Finished, (Status::Ended, false, false, true)),
        ] {
            let notice = Worker {
                attention: true,
                ..worker_in(state, false)
            };
            assert_eq!(notice.need(), Some(Need::Notice));
            assert!(!notice.needs_attention(), "{state:?}");
            assert_eq!(row(&notice), expected, "{state:?}");
        }
    }

    #[test]
    fn a_finished_or_closed_worker_has_ended() {
        let finished = worker_in(State::Finished, false);
        assert_eq!(row(&finished), (Status::Ended, false, false, true));
        let closed_failed = worker_in(State::Error, true);
        assert!(!closed_failed.needs_attention());
        assert_eq!(row(&closed_failed), (Status::Failed, false, false, true));
        let closed_finished = worker_in(State::Finished, true);
        assert!(!closed_finished.needs_attention());
        assert_eq!(row(&closed_finished), (Status::Ended, false, false, true));
    }

    #[test]
    fn status_words_never_say_success() {
        let titles = [
            Status::Approval,
            Status::Input,
            Status::Failed,
            Status::Running,
            Status::Ended,
        ]
        .map(|status| (status.rank(), status.title()));
        assert_eq!(
            titles,
            [
                (0, "承認待ち"),
                (1, "入力待ち"),
                (2, "失敗"),
                (3, "実行中"),
                (4, "終了")
            ]
        );
        for (_, title) in titles {
            assert!(
                !title.contains("成功") && !title.contains("完了"),
                "{title}"
            );
        }
    }

    #[test]
    fn scope_names_round_trip() {
        for scope in Scope::ALL {
            assert_eq!(Scope::parse(scope.name()), Some(scope));
        }
        assert_eq!(Scope::ALL.map(Scope::title), ["現在", "要対応", "終了"]);
        assert_eq!(Scope::parse("waiting"), None);
    }

    #[test]
    fn attention_is_within_current_and_current_and_ended_split_every_worker() {
        let mut fleet = canned_with_closed();
        // Cover what the canned fleet lacks: notices and a closed failure.
        fleet.workers[0].attention = true;
        fleet.workers[7].attention = true;
        fleet.workers.last_mut().unwrap().state = State::Error;
        for worker in &fleet.workers {
            let current = worker.in_scope(Scope::Current);
            let ended = worker.in_scope(Scope::Ended);
            assert!(current != ended, "{}: {current} {ended}", worker.label);
            if worker.in_scope(Scope::Attention) {
                assert!(current, "{}", worker.label);
            }
            assert_eq!(worker.in_scope(Scope::Attention), worker.needs_attention());
        }
        let attention: Vec<u64> = fleet
            .workers
            .iter()
            .filter(|w| w.needs_attention())
            .map(|w| w.pane)
            .collect();
        assert_eq!(attention, [14, 15, 16, 19]);
    }

    #[test]
    fn parent_name_says_who_started_the_worker() {
        let fleet = canned_with_closed();
        let named = |pane: u64| fleet.parent_name(fleet.worker(pane).unwrap());
        assert_eq!(named(14), "api-refactor");
        assert_eq!(named(11), "release-0.9.7");
        let mut orphan = fleet.worker(14).unwrap().clone();
        orphan.parent = Some(99);
        assert_eq!(fleet.parent_name(&orphan), "ペイン 99");
        orphan.parent = None;
        orphan.orchestrator = crate::live::ALONE.into();
        assert_eq!(fleet.parent_name(&orphan), "なし");
        orphan.orchestrator = "release-0.9.7".into();
        orphan.orchestrator_pane = Some(orphan.pane);
        assert_eq!(fleet.parent_name(&orphan), "なし");
        orphan.orchestrator_pane = None;
        orphan.orchestrator = String::new();
        assert_eq!(fleet.parent_name(&orphan), "不明");
    }
}
