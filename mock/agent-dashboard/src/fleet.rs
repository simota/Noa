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
    Quiet,
}

impl Need {
    pub fn symbol(self) -> &'static str {
        match self {
            Need::Approval => "!",
            Need::Failure => "×",
            Need::Question => "?",
            Need::Quiet => "…",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Need::Approval => "承認待ち",
            Need::Failure => "失敗",
            Need::Question => "質問",
            Need::Quiet => "静止",
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Need::Approval | Need::Failure => 0,
            Need::Question => 1,
            Need::Quiet => 2,
        }
    }
}

pub struct Worker {
    pub label: &'static str,
    pub kind: &'static str,
    pub branch: &'static str,
    pub model: &'static str,
    pub effort: &'static str,
    pub orchestrator: &'static str,
    pub parent: Option<&'static str>,
    pub state: State,
    pub task: &'static str,
    pub detail: &'static str,
    pub since: &'static str,
    pub wait_minutes: f64,
    pub quiet: bool,
    pub reports: u32,
    pub screen_tail: &'static [&'static str],
    pub lines_per_tick: &'static [f64],
    pub freshness: f64,
    pub phases: &'static [(Phase, f64)],
}

impl Worker {
    pub fn need(&self) -> Option<Need> {
        match self.state {
            State::Permission => Some(Need::Approval),
            State::Error => Some(Need::Failure),
            State::Input => Some(Need::Question),
            State::Running if self.quiet => Some(Need::Quiet),
            State::Running | State::Finished => None,
        }
    }

    pub fn model_line(&self) -> String {
        match (self.model, self.effort) {
            ("", _) => "モデル既定".to_owned(),
            (model, "") => model.to_owned(),
            (model, effort) => format!("{model} · {effort}"),
        }
    }

    pub fn minutes_where(&self, matches: impl Fn(Phase) -> bool) -> f64 {
        self.phases
            .iter()
            .filter(|(phase, _)| matches(*phase))
            .map(|(_, minutes)| minutes)
            .sum()
    }

    pub fn total_minutes(&self) -> f64 {
        self.minutes_where(|_| true)
    }
}

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

    fn key(self, worker: &Worker) -> Option<&'static str> {
        match self {
            Grouping::Orchestrator => Some(worker.orchestrator),
            Grouping::Agent => Some(worker.kind),
            Grouping::Model if worker.model.is_empty() => Some("モデル既定"),
            Grouping::Model => Some(worker.model),
            Grouping::Flat => None,
        }
    }

    pub fn split<'a>(
        self,
        workers: impl IntoIterator<Item = &'a Worker>,
    ) -> Vec<(Option<&'static str>, Vec<&'a Worker>)> {
        let mut groups: Vec<(Option<&'static str>, Vec<&'a Worker>)> = Vec::new();
        for worker in workers {
            let key = self.key(worker);
            match groups.iter_mut().find(|(name, _)| *name == key) {
                Some((_, members)) => members.push(worker),
                None => groups.push((key, vec![worker])),
            }
        }
        groups
    }
}

pub const ORCHESTRATORS: [&str; 2] = ["release-0.9.7", "perf-wish"];

pub const FRESHNESS_MAX: f64 = 5.0;

pub fn canned() -> Vec<Worker> {
    vec![
        Worker {
            label: "api-refactor",
            kind: "claude",
            branch: "feat/api-v2",
            model: "opus",
            effort: "high",
            orchestrator: "release-0.9.7",
            parent: None,
            state: State::Running,
            task: "Handler の分割を noa-grid 側に反映し、テストを通す",
            detail: "cargo test -p noa-grid を実行中",
            since: "8 秒前に出力",
            wait_minutes: 0.0,
            quiet: false,
            reports: 7,
            screen_tail: &[],
            lines_per_tick: &[2.0, 6.0, 14.0, 9.0, 3.0, 18.0, 22.0, 11.0, 4.0, 7.0, 16.0, 12.0],
            freshness: 5.0,
            phases: &[
                (Phase::Running, 20.0),
                (Phase::Permission, 3.0),
                (Phase::Running, 14.0),
            ],
        },
        Worker {
            label: "docs-sync",
            kind: "agy",
            branch: "docs/sync",
            model: "gemini-4-argon-medium",
            effort: "",
            orchestrator: "release-0.9.7",
            parent: None,
            state: State::Running,
            task: "機能一覧を実装に合わせて更新する",
            detail: "docs/FEATURES.md を更新中",
            since: "2 秒前に出力",
            wait_minutes: 0.0,
            quiet: false,
            reports: 3,
            screen_tail: &[],
            lines_per_tick: &[1.0, 1.0, 3.0, 2.0, 5.0, 4.0, 2.0, 6.0, 3.0, 5.0, 4.0, 6.0],
            freshness: 5.0,
            phases: &[
                (Phase::Running, 6.0),
                (Phase::Input, 1.0),
                (Phase::Running, 6.0),
            ],
        },
        Worker {
            label: "perf-probe",
            kind: "codex",
            branch: "perf/reflow",
            model: "gpt-6.2-astra",
            effort: "high",
            orchestrator: "perf-wish",
            parent: None,
            state: State::Running,
            task: "reflow の計測を全端末で取り直す",
            detail: "bench/run_all.sh 3/8",
            since: "出力なし 3 分",
            wait_minutes: 3.0,
            quiet: true,
            reports: 1,
            screen_tail: &["[3/8] ascii を計測中 …", "（3 分間 出力なし）"],
            lines_per_tick: &[19.0, 24.0, 17.0, 21.0, 12.0, 6.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            freshness: 1.0,
            phases: &[(Phase::Running, 38.0), (Phase::Quiet, 3.0)],
        },
        Worker {
            label: "fix-tests",
            kind: "claude",
            branch: "fix/flaky-pty",
            model: "sonnet",
            effort: "medium",
            orchestrator: "release-0.9.7",
            parent: Some("api-refactor"),
            state: State::Permission,
            task: "不安定な pty テストの原因を特定して直す",
            detail: "Bash: rm -rf target/tmp を実行してよいですか",
            since: "2 分",
            wait_minutes: 2.0,
            quiet: false,
            reports: 5,
            screen_tail: &[
                "Bash command",
                "  rm -rf target/tmp",
                "Do you want to proceed?",
                "> 1. Yes    2. No",
            ],
            lines_per_tick: &[],
            freshness: 0.0,
            phases: &[(Phase::Running, 18.0), (Phase::Permission, 2.0)],
        },
        Worker {
            label: "migrate-cfg",
            kind: "codex",
            branch: "feat/config-v2",
            model: "gpt-6.2-sol",
            effort: "medium",
            orchestrator: "perf-wish",
            parent: None,
            state: State::Permission,
            task: "設定キーの読み込みを新形式へ移す",
            detail: "apply_patch: crates/noa-config/src/lib.rs",
            since: "40 秒",
            wait_minutes: 0.7,
            quiet: false,
            reports: 2,
            screen_tail: &[
                "apply_patch crates/noa-config/src/lib.rs",
                "  +12 -3",
                "Allow this change? [y/n]",
            ],
            lines_per_tick: &[],
            freshness: 0.0,
            phases: &[(Phase::Running, 9.0), (Phase::Permission, 0.7)],
        },
        Worker {
            label: "spec-review",
            kind: "claude",
            branch: "main",
            model: "",
            effort: "",
            orchestrator: "release-0.9.7",
            parent: None,
            state: State::Input,
            task: "仕様書の未決事項を洗い出して確認する",
            detail: "質問: 既定値は A と B のどちらにしますか",
            since: "6 分",
            wait_minutes: 6.0,
            quiet: false,
            reports: 4,
            screen_tail: &[
                "既定値は A と B のどちらにしますか",
                "> 1. A（現行どおり）",
                "  2. B（新しい既定）",
            ],
            lines_per_tick: &[],
            freshness: 0.0,
            phases: &[(Phase::Running, 7.0), (Phase::Input, 6.0)],
        },
        Worker {
            label: "lint-pass",
            kind: "agy",
            branch: "chore/clippy",
            model: "gemini-4-hydrogen-low",
            effort: "",
            orchestrator: "release-0.9.7",
            parent: Some("api-refactor"),
            state: State::Finished,
            task: "clippy の警告をすべて解消する",
            detail: "clippy 警告 0 件",
            since: "4 分前に完了",
            wait_minutes: 0.0,
            quiet: false,
            reports: 6,
            screen_tail: &[],
            lines_per_tick: &[],
            freshness: 0.0,
            phases: &[
                (Phase::Running, 8.0),
                (Phase::Permission, 1.0),
                (Phase::Running, 7.0),
                (Phase::Ended, 4.0),
            ],
        },
        Worker {
            label: "changelog",
            kind: "claude",
            branch: "chore/release",
            model: "sonnet",
            effort: "low",
            orchestrator: "release-0.9.7",
            parent: None,
            state: State::Finished,
            task: "次のリリースの変更履歴を書く",
            detail: "CHANGELOG を更新",
            since: "11 分前に完了",
            wait_minutes: 0.0,
            quiet: false,
            reports: 2,
            screen_tail: &[],
            lines_per_tick: &[],
            freshness: 0.0,
            phases: &[(Phase::Running, 6.0), (Phase::Ended, 11.0)],
        },
        Worker {
            label: "e2e-smoke",
            kind: "codex",
            branch: "test/e2e",
            model: "gpt-6-luna",
            effort: "low",
            orchestrator: "perf-wish",
            parent: Some("perf-probe"),
            state: State::Error,
            task: "e2e シナリオを一通り流す",
            detail: "exit 1 · e2e スクリプトが失敗",
            since: "1 分",
            wait_minutes: 1.0,
            quiet: false,
            reports: 3,
            screen_tail: &[
                "FAIL scenario 3",
                "1 failed, 6 passed",
                "[exited 1]",
            ],
            lines_per_tick: &[],
            freshness: 0.0,
            phases: &[(Phase::Running, 22.0), (Phase::Ended, 1.0)],
        },
    ]
}
