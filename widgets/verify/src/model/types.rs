//! The verification's state, and the event log it is folded from. Every
//! type the units share lives here (frozen after the skeleton lands);
//! behaviour on them goes in `impl` blocks of the module that needs it.
//! Nothing here is mutated except by `reduce`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Wall-clock time in milliseconds since the Unix epoch. Always injected by
/// the caller, so the model and the engine never read a clock themselves.
pub type Millis = u64;

/// A round's number, counted from 1. Brief lifecycle events use 0 for the
/// criteria phase, which comes before round 1.
pub type RoundNo = u32;

/// The round number brief lifecycle events give the criteria phase.
pub const CRITERIA_ROUND: RoundNo = 0;

/// The default round limit: one independent round and one of cross-checks.
pub const DEFAULT_MAX_ROUNDS: RoundNo = 2;

/// The highest round limit the start view offers.
pub const MAX_MAX_ROUNDS: RoundNo = 6;

/// The fewest verifiers one verification takes.
pub const MIN_PANEL: usize = 2;

/// The most verifiers one verification takes: with the judge, seven panes
/// in a grid of at most three columns of three in one Noa tab.
pub const MAX_PANEL: usize = 6;

/// The id of the judge, who drafts the criteria, rules on each round and
/// gives the verdict.
pub const JUDGE_ID: &str = "judge";

/// The most criteria one verification has (L1-2).
pub const MAX_CRITERIA: usize = 30;

/// The longest criterion statement, in characters (L1-2).
pub const MAX_STATEMENT: usize = 300;

/// The longest custom strategy text, in characters (L1-9).
pub const MAX_CUSTOM_STRATEGY: usize = 500;

/// The longest reported command, in characters (L1-6).
pub const MAX_CMD: usize = 2000;

/// The most evidence items one reply holds (L1-5).
pub const MAX_EVIDENCE_ITEMS: usize = 40;

/// The most bytes all files one reply cites may add up to (L1-5).
pub const MAX_REPLY_EVIDENCE_BYTES: u64 = 512 * 1024 * 1024;

/// The largest command log a reply may cite, and the cap a re-run log is
/// cut to (L1-5).
pub const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

/// The largest `file` evidence that is not a video (L1-5).
pub const MAX_FILE_BYTES: u64 = 15 * 1024 * 1024;

/// The largest image (L1-5a).
pub const MAX_IMAGE_BYTES: u64 = 15 * 1024 * 1024;

/// The largest video (L1-5a).
pub const MAX_VIDEO_BYTES: u64 = 200 * 1024 * 1024;

/// The longest media caption, in characters (L1-5a).
pub const MAX_CAPTION: usize = 300;

/// The most marks one video has (L1-5a).
pub const MAX_MARKS: usize = 20;

/// The most defects one reply raises (L1-6).
pub const MAX_DEFECTS: usize = 20;

/// The most re-runs per verifier per round (L1-5).
pub const MAX_RERUNS: usize = 8;

/// How long one re-run command may run, counted from its `.pid`
/// appearing, before its process group is killed (L1-5).
pub const RERUN_TIMEOUT: Millis = 10 * 60_000;

/// How long after SIGTERM a timed-out re-run gets SIGKILL (L1-5).
pub const RERUN_KILL_GRACE: Millis = 5_000;

/// How many trailing lines of a failing command's stored log the judge's
/// ruling brief shows (L2-4).
pub const JUDGE_TAIL_LINES: usize = 40;

macro_rules! string_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_id!(
    /// Names one verification and its directories on disk.
    SessionId
);
string_id!(
    /// A participant, as agents and the judge's JSON name it: `judge` for
    /// the judge, the agent kind (`claude`, `codex`, `agy`) for a verifier,
    /// with `-2`, `-3`, … when a kind sits on the panel twice.
    PartId
);
string_id!(
    /// A timeline entry: `m1`, `m2`, … in posting order.
    MessageId
);
string_id!(
    /// One evidence item: `<verifier>/r<N>/<id>` for what a verifier
    /// reported (`codex/r1/e3`), `user/<n>` for what the person attached.
    EvidenceKey
);

impl SessionId {
    /// The id a session created at `now` gets.
    pub fn at(now: Millis) -> Self {
        Self(format!("ver-{now}"))
    }
}

impl MessageId {
    /// The id of the `seq`-th message (counted from 1).
    pub fn nth(seq: usize) -> Self {
        Self(format!("m{seq}"))
    }
}

/// The owner of person-attached evidence keys (`user/<n>`).
pub const USER_EVIDENCE_OWNER: &str = "user";

impl EvidenceKey {
    /// The key of evidence `id` in `part`'s reply of `round`.
    pub fn reported(part: &PartId, round: RoundNo, id: &str) -> Self {
        Self(format!("{part}/r{round}/{id}"))
    }

    /// The key of the `n`-th item the person attached.
    pub fn user(n: u32) -> Self {
        Self(format!("{USER_EVIDENCE_OWNER}/{n}"))
    }

    /// `(verifier, round, id)` of a reported key; `None` for a person's key
    /// or text that is not a key.
    pub fn split(&self) -> Option<(PartId, RoundNo, &str)> {
        let mut parts = self.0.splitn(3, '/');
        let (part, round, id) = (parts.next()?, parts.next()?, parts.next()?);
        let round = round.strip_prefix('r')?.parse().ok()?;
        (!part.is_empty() && part != USER_EVIDENCE_OWNER && !id.is_empty())
            .then(|| (PartId::new(part), round, id))
    }

    /// Whether the person attached it.
    pub fn is_user(&self) -> bool {
        self.0
            .strip_prefix(USER_EVIDENCE_OWNER)
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    }
}

// ---------------------------------------------------------------- agents

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
    Agy,
}

impl AgentKind {
    pub const ALL: [AgentKind; 3] = [AgentKind::Claude, AgentKind::Codex, AgentKind::Agy];

    /// The command that starts the agent interactively, and its display name.
    pub fn command(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Agy => "agy",
        }
    }

    /// Models the start view suggests for this kind; any other name the CLI
    /// takes may be typed. Claude Code takes family aliases (latest of each);
    /// the Codex and Antigravity names are from `codex debug models` (0.171)
    /// and `agy models` (1.5.14).
    pub fn models(self) -> &'static [&'static str] {
        match self {
            AgentKind::Claude => &["opus", "sonnet", "haiku", "fable"],
            AgentKind::Codex => &[
                "gpt-6.2-sol",
                "gpt-6.2-astra",
                "gpt-6.1-terra",
                "gpt-6-luna",
            ],
            AgentKind::Agy => &[
                "gemini-4-neon-high",
                "gemini-4-carbon-high",
                "gemini-4-argon-medium",
                "gemini-4-hydrogen-low",
            ],
        }
    }

    /// The effort levels the CLI takes, lowest first.
    pub fn efforts(self) -> &'static [&'static str] {
        match self {
            AgentKind::Codex => &["low", "medium", "high", "xhigh", "max", "ultra"],
            AgentKind::Claude | AgentKind::Agy => &["low", "medium", "high", "xhigh", "max"],
        }
    }

    /// The flags that start this kind on `setup`: `--model M --effort E`,
    /// or Codex's `-m M -c model_reasoning_effort=E`. Each is a plain word
    /// once `setup` passes [`AgentSetup::check`], so the flags can be
    /// written into a script for any login shell (fish and csh have no `$@`).
    pub fn flags(self, setup: &AgentSetup) -> Vec<String> {
        let mut flags = Vec::new();
        if let Some(model) = &setup.model {
            let flag = if self == AgentKind::Codex {
                "-m"
            } else {
                "--model"
            };
            flags.extend([flag.to_owned(), model.clone()]);
        }
        if let Some(effort) = &setup.effort {
            match self {
                AgentKind::Codex => {
                    flags.extend(["-c".to_owned(), format!("model_reasoning_effort={effort}")]);
                }
                AgentKind::Claude | AgentKind::Agy => {
                    flags.extend(["--effort".to_owned(), effort.clone()]);
                }
            }
        }
        flags
    }
}

/// The model and effort an agent runs on; `None` leaves the CLI's default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentSetup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl AgentSetup {
    /// Why `kind` cannot start on this setup: a model name that is not one
    /// plain word, or an effort level the CLI does not take.
    pub fn check(&self, kind: AgentKind) -> Result<(), String> {
        if let Some(model) = &self.model {
            let plain = !model.starts_with(['-', '='])
                && !model.is_empty()
                && model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._:/@+=-".contains(c));
            if !plain {
                return Err(format!(
                    "{} のモデル名 {model:?} には英数字と . _ : / @ + = - だけが使えます（先頭の - と = は不可）",
                    kind.command()
                ));
            }
        }
        if let Some(effort) = &self.effort
            && !kind.efforts().contains(&effort.as_str())
        {
            return Err(format!(
                "{} はエフォート {effort:?} を受け付けません",
                kind.command()
            ));
        }
        Ok(())
    }

    pub fn is_default(&self) -> bool {
        self.model.is_none() && self.effort.is_none()
    }

    /// `model, effort` for display, or `None` when both are the defaults.
    pub fn label(&self) -> Option<String> {
        let parts: Vec<&str> = [self.model.as_deref(), self.effort.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

/// An agent picked to take part: its kind and what it runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentChoice {
    pub kind: AgentKind,
    #[serde(default, skip_serializing_if = "AgentSetup::is_default")]
    pub setup: AgentSetup,
}

impl AgentChoice {
    /// `claude`, or `claude (opus · high)` with a model or effort.
    pub fn label(&self) -> String {
        match self.setup.label() {
            Some(setup) => format!("{} ({setup})", self.kind.command()),
            None => self.kind.command().to_owned(),
        }
    }
}

impl From<AgentKind> for AgentChoice {
    fn from(kind: AgentKind) -> Self {
        Self {
            kind,
            setup: AgentSetup::default(),
        }
    }
}

// ------------------------------------------------------------ strategies

/// One of the ways a verifier can be asked to verify (L1-9). Labels and
/// checklists are in `model::strategy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinStrategy {
    Conformance,
    Falsify,
    Regression,
    E2e,
    Nonfunctional,
}

impl BuiltinStrategy {
    pub const ALL: [BuiltinStrategy; 5] = [
        BuiltinStrategy::Conformance,
        BuiltinStrategy::Falsify,
        BuiltinStrategy::Regression,
        BuiltinStrategy::E2e,
        BuiltinStrategy::Nonfunctional,
    ];

    /// The id logs and briefs use.
    pub fn id(self) -> &'static str {
        match self {
            BuiltinStrategy::Conformance => "conformance",
            BuiltinStrategy::Falsify => "falsify",
            BuiltinStrategy::Regression => "regression",
            BuiltinStrategy::E2e => "e2e",
            BuiltinStrategy::Nonfunctional => "nonfunctional",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }
}

/// The strategy a verifier sits on: a built-in one, or text the person
/// wrote (`custom`, `custom-2`, … in panel order).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "strategy", rename_all = "snake_case")]
pub enum Strategy {
    Builtin { id: BuiltinStrategy },
    Custom { id: String, text: String },
}

impl Strategy {
    pub fn id(&self) -> &str {
        match self {
            Strategy::Builtin { id } => id.id(),
            Strategy::Custom { id, .. } => id,
        }
    }
}

/// The first 16 characters of a custom strategy's text, on one line.
pub fn custom_label(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut label: String = line.chars().take(16).collect();
    if line.chars().count() > 16 {
        label.push('…');
    }
    label
}

/// What a verifier row in the start view picks as its strategy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyChoice {
    Builtin(BuiltinStrategy),
    Custom(String),
}

/// One verifier row of the start view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierChoice {
    pub agent: AgentChoice,
    pub strategy: StrategyChoice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Judge,
    Verifier,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    pub id: PartId,
    pub kind: AgentKind,
    pub role: Role,
    /// The Noa pane the agent runs in, once spawned.
    pub pane_id: Option<u64>,
    /// The model and effort it is started on.
    #[serde(default, skip_serializing_if = "AgentSetup::is_default")]
    pub setup: AgentSetup,
    /// A verifier's strategy; `None` for the judge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<Strategy>,
}

impl Participant {
    /// `claude`, or `claude-2 (opus · high)` with a model or effort.
    pub fn label(&self) -> String {
        match self.setup.label() {
            Some(setup) => format!("{} ({setup})", self.id),
            None => self.id.to_string(),
        }
    }
}

// ---------------------------------------------------------------- target

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetKind {
    /// A change: base…head (L1-1 item 1).
    Diff,
    /// The tree as it is, no base (L1-1 item 2).
    State,
}

impl TargetKind {
    pub const ALL: [TargetKind; 2] = [TargetKind::Diff, TargetKind::State];

    pub fn label(self) -> &'static str {
        match self {
            TargetKind::Diff => "Git 差分",
            TargetKind::State => "現在の状態",
        }
    }
}

/// The new side of the target.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Head {
    /// `HEAD` with every uncommitted and untracked change.
    #[default]
    Worktree,
    Ref(String),
}

/// What the person asked to verify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub kind: TargetKind,
    /// The repository (any directory inside its work tree).
    pub repo: PathBuf,
    /// The base ref of a diff; `None` picks the default branch. Always
    /// `None` for a state target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default)]
    pub head: Head,
    /// The pathspec filter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// The spec file asked for: a path relative to the repository's top
    /// level (L1-1). What was captured is [`SpecFile`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed {
        from: String,
    },
    Untracked,
    /// A file of a state target: there is no old side.
    Plain,
}

impl FileStatus {
    pub fn label(&self) -> &'static str {
        match self {
            FileStatus::Added => "追加",
            FileStatus::Modified => "変更",
            FileStatus::Deleted => "削除",
            FileStatus::Renamed { .. } => "名前変更",
            FileStatus::Untracked => "未追跡",
            FileStatus::Plain => "",
        }
    }

    /// Short code for briefs: `A`, `M`, `D`, `R`, `U`, or `-`.
    pub fn code(&self) -> &'static str {
        match self {
            FileStatus::Added => "A",
            FileStatus::Modified => "M",
            FileStatus::Deleted => "D",
            FileStatus::Renamed { .. } => "R",
            FileStatus::Untracked => "U",
            FileStatus::Plain => "-",
        }
    }
}

/// One file of the snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetFile {
    /// Relative, with `/` separators.
    pub path: String,
    #[serde(flatten)]
    pub status: FileStatus,
    /// Lines of the captured (new) side; 0 for a deleted file.
    pub lines: u32,
    pub bytes: u64,
    #[serde(default)]
    pub additions: u32,
    #[serde(default)]
    pub deletions: u32,
    /// The new side's changed line ranges, inclusive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<(u32, u32)>,
    pub digest: u64,
}

/// A file left out of the snapshot, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Omitted {
    pub path: String,
    pub reason: String,
}

/// What was captured: every agent verifies exactly this.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetManifest {
    /// One line for lists and headers, e.g. `feat/x … main (12 files)`.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_base: Option<String>,
    /// The changed files (none for a state target, L1-1 item 2).
    #[serde(default)]
    pub files: Vec<TargetFile>,
    #[serde(default)]
    pub omitted: Vec<Omitted>,
    #[serde(default)]
    pub diff_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_digest: Option<u64>,
    pub captured_at: Millis,
}

impl TargetManifest {
    pub fn file(&self, path: &str) -> Option<&TargetFile> {
        self.files.iter().find(|f| f.path == path)
    }

    pub fn is_omitted(&self, path: &str) -> bool {
        self.omitted.iter().any(|o| o.path == path)
    }

    pub fn additions(&self) -> u32 {
        self.files.iter().map(|f| f.additions).sum()
    }

    pub fn deletions(&self) -> u32 {
        self.files.iter().map(|f| f.deletions).sum()
    }

    /// Whether `prefix` (a file, a directory ending in `/`, or `*`) names at
    /// least one captured file.
    pub fn matches(&self, prefix: &str) -> bool {
        self.files.iter().any(|f| covers(prefix, &f.path))
    }
}

/// Whether a coverage entry (a file, a directory ending in `/`, or `*`)
/// covers `path`.
pub fn covers(entry: &str, path: &str) -> bool {
    entry == "*" || entry == path || (entry.ends_with('/') && path.starts_with(entry))
}

/// What each verifier's worktree is built from (L1-1): the head commit,
/// and in work-tree mode the digest of `target/worktree.patch` and every
/// untracked file copied to `target/untracked/<path>` with its digest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rebuild {
    pub head_sha: String,
    /// [`crate::capture::digest_bytes`] of `target/worktree.patch`; `None`
    /// when the head is a ref (nothing uncommitted to apply).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_patch: Option<u64>,
    /// `(path, digest)` of each untracked, non-ignored file, path relative
    /// with `/` separators.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub untracked: Vec<(String, u64)>,
}

/// The spec file as captured into `target/spec.md` (L1-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecFile {
    /// Relative to the repository's top level, with `/` separators.
    pub path: String,
    pub bytes: u64,
    pub lines: u32,
    pub digest: u64,
}

/// Where verifiers run (L1-3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    /// One detached worktree per verifier.
    #[default]
    Worktree,
    /// Every verifier in the person's repository, edits forbidden, no
    /// re-runs.
    Shared,
}

impl Isolation {
    pub const ALL: [Isolation; 2] = [Isolation::Worktree, Isolation::Shared];

    pub fn label(self) -> &'static str {
        match self {
            Isolation::Worktree => "個別 worktree",
            Isolation::Shared => "共有（読み取りのみ）",
        }
    }
}

/// What became of a verifier's worktree (L1-3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorktreeState {
    Made,
    /// Building or checking it failed; round 1 is refused.
    Failed {
        reason: String,
    },
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    /// `<main worktree>-wt/verify-<session id>-<verifier>`.
    pub path: PathBuf,
    #[serde(flatten)]
    pub state: WorktreeState,
}

// -------------------------------------------------------------- criteria

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Must,
    Should,
}

impl Priority {
    pub const ALL: [Priority; 2] = [Priority::Must, Priority::Should];

    pub fn name(self) -> &'static str {
        match self {
            Priority::Must => "must",
            Priority::Should => "should",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Priority::Must => "必須",
            Priority::Should => "推奨",
        }
    }
}

/// Where a criterion came from. Written as agents write it: `user`,
/// `request`, `inferred` or `spec:<line>` (a line of `target/spec.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum CriterionSource {
    User,
    Request,
    Inferred,
    Spec { line: u32 },
}

impl CriterionSource {
    /// `spec:12` and the like; `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "user" => Some(CriterionSource::User),
            "request" => Some(CriterionSource::Request),
            "inferred" => Some(CriterionSource::Inferred),
            _ => {
                let line = text.strip_prefix("spec:")?;
                (!line.starts_with(['+', '-']))
                    .then(|| line.parse().ok())
                    .flatten()
                    .filter(|&line| line >= 1)
                    .map(|line| CriterionSource::Spec { line })
            }
        }
    }
}

impl fmt::Display for CriterionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CriterionSource::User => f.write_str("user"),
            CriterionSource::Request => f.write_str("request"),
            CriterionSource::Inferred => f.write_str("inferred"),
            CriterionSource::Spec { line } => write!(f, "spec:{line}"),
        }
    }
}

impl From<CriterionSource> for String {
    fn from(source: CriterionSource) -> Self {
        source.to_string()
    }
}

impl TryFrom<String> for CriterionSource {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        CriterionSource::parse(&text).ok_or_else(|| format!("unknown criterion source {text:?}"))
    }
}

/// One checkable statement of what "works" means (L1-2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Criterion {
    /// `AC-1`, `AC-2`, …
    pub id: String,
    pub statement: String,
    /// How to verify it.
    pub how: String,
    pub priority: Priority,
    pub source: CriterionSource,
    /// The person changed it before round 1.
    #[serde(default)]
    pub edited: bool,
}

/// One change the person made to the criteria before round 1 (L1-2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CriteriaEdit {
    /// A new criterion (source `user`); it gets the next free `AC-<n>`.
    Add {
        statement: String,
        how: String,
        priority: Priority,
    },
    /// Fields given replace the criterion's.
    Edit {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        statement: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        how: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        priority: Option<Priority>,
    },
    /// Only an `inferred` or `request` criterion may be removed.
    Remove { id: String },
    /// Moves the criterion to position `to` (counted from 0).
    Move { id: String, to: usize },
}

// ------------------------------------------------------------- evidence

/// One verifier's outcome on one criterion (L1-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Pass,
    Fail,
    Blocked,
    NotTested,
}

impl Outcome {
    pub const ALL: [Outcome; 4] = [
        Outcome::Pass,
        Outcome::Fail,
        Outcome::Blocked,
        Outcome::NotTested,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Outcome::Pass => "pass",
            Outcome::Fail => "fail",
            Outcome::Blocked => "blocked",
            Outcome::NotTested => "not_tested",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Outcome::Pass => "成功",
            Outcome::Fail => "失敗",
            Outcome::Blocked => "実行不能",
            Outcome::NotTested => "未実施",
        }
    }

    /// The matrix mark: ✓ ✗ ⊘ –.
    pub fn mark(self) -> &'static str {
        match self {
            Outcome::Pass => "✓",
            Outcome::Fail => "✗",
            Outcome::Blocked => "⊘",
            Outcome::NotTested => "–",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriterionResult {
    pub round: RoundNo,
    pub part: PartId,
    /// The criterion's id (`AC-1`).
    pub criterion: String,
    pub outcome: Outcome,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub evidence: Vec<EvidenceKey>,
}

/// A picture or recording, as its leading bytes say (L1-5a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Media {
    Png,
    Jpeg,
    Gif,
    Webp,
    Mp4,
    QuickTime,
    Webm,
}

impl Media {
    pub const ALL: [Media; 7] = [
        Media::Png,
        Media::Jpeg,
        Media::Gif,
        Media::Webp,
        Media::Mp4,
        Media::QuickTime,
        Media::Webm,
    ];

    pub fn is_video(self) -> bool {
        matches!(self, Media::Mp4 | Media::QuickTime | Media::Webm)
    }

    /// The format as cards show it: `PNG`, `MOV`, …
    pub fn name(self) -> &'static str {
        match self {
            Media::Png => "PNG",
            Media::Jpeg => "JPEG",
            Media::Gif => "GIF",
            Media::Webp => "WebP",
            Media::Mp4 => "MP4",
            Media::QuickTime => "MOV",
            Media::Webm => "WebM",
        }
    }

    /// The extension a person-attached copy is stored with.
    pub fn extension(self) -> &'static str {
        match self {
            Media::Png => "png",
            Media::Jpeg => "jpg",
            Media::Gif => "gif",
            Media::Webp => "webp",
            Media::Mp4 => "mp4",
            Media::QuickTime => "mov",
            Media::Webm => "webm",
        }
    }

    /// 画像 or 動画.
    pub fn label(self) -> &'static str {
        if self.is_video() { "動画" } else { "画像" }
    }
}

/// A moment of a video to look at (L1-5a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    /// From the start of the video.
    pub t_ms: u64,
    pub note: String,
}

/// What an evidence item is (L1-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceBody {
    Command {
        cmd: String,
        /// Relative to the worktree, no `..`.
        cwd: String,
        /// The exit status the verifier saw.
        exit: i32,
        /// The saved output, relative to the verifier's `r<N>/<verifier>/`.
        log: String,
        rerun: bool,
    },
    File {
        /// Relative to the verifier's `r<N>/<verifier>/` (a person's: the
        /// name it was attached from).
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        /// What its leading bytes say; `None` for a file that is not media.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media: Option<Media>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        marks: Vec<Mark>,
        /// The criteria ids it shows, when not every citing one.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        shows: Vec<String>,
    },
    Observation {
        text: String,
    },
}

/// A stored copy of an evidence file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored {
    /// Relative to the judge's reply dir (`evidence/r1/codex/logs/1.log`).
    pub path: PathBuf,
    pub bytes: u64,
    /// [`crate::capture::digest_bytes`] of the copy.
    pub digest: u64,
}

/// What the runner found at a cited path when a reply was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FileState {
    Regular { bytes: u64 },
    Missing,
    Link,
    NotRegular,
}

/// One cited path as [`crate::evidence::inspect`] found it, passed in with
/// the reply so validation (L1-6) stays pure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFact {
    /// As the reply cites it, relative to `r<N>/<verifier>/`.
    pub path: String,
    pub state: FileState,
    /// What its leading bytes say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sniffed: Option<Media>,
    /// What its extension says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_ext: Option<Media>,
}

/// How a re-run of a `command` item ended (L1-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Rerun {
    /// Same exit status as reported.
    Matched {
        exit: i32,
        log: Stored,
    },
    /// Another exit status (`None`: it ended without one).
    Differed {
        exit: Option<i32>,
        log: Stored,
    },
    TimedOut {
        log: Stored,
    },
    /// Not run, or not run to its end: 共有 mode, pane gone, did not start,
    /// stopped before it finished, script changed (L1-5 step 5). `log` is
    /// the command's `<n>.log` when one was written and ingested.
    NotRun {
        why: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log: Option<Stored>,
    },
}

impl Rerun {
    /// ↻ matched, ≠ differed, ⏱ timed out, – not run.
    pub fn mark(&self) -> &'static str {
        match self {
            Rerun::Matched { .. } => "↻",
            Rerun::Differed { .. } => "≠",
            Rerun::TimedOut { .. } => "⏱",
            Rerun::NotRun { .. } => "–",
        }
    }
}

/// One command a re-run request covers: `rerun/<n>.cmd` holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RerunItem {
    pub key: EvidenceKey,
    /// Counted from 1.
    pub n: u32,
    /// [`crate::capture::digest_bytes`] of `<n>.cmd`.
    pub cmd_digest: u64,
}

/// The re-run script written for one verifier in one round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RerunRequest {
    /// 32 hex digits; `done` must hold it.
    pub nonce: String,
    pub items: Vec<RerunItem>,
    /// [`crate::capture::digest_bytes`] of `run.sh`.
    pub script_digest: u64,
}

/// When the person attached an item (L1-5a): the round that was current
/// then (the last round started; the judge sees the item in the next
/// ruling brief), and the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub round: RoundNo,
    pub at: Millis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub key: EvidenceKey,
    pub body: EvidenceBody,
    /// The copy in the judge's reply dir, once ingested (or attached).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored: Option<Stored>,
    /// A `command` item's re-run, once it has an outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerun: Option<Rerun>,
    /// When the person attached it; `None` for what a verifier reported
    /// (its round is in its key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached: Option<Attachment>,
}

impl Evidence {
    /// The media an item is, if any.
    pub fn media(&self) -> Option<Media> {
        match &self.body {
            EvidenceBody::File { media, .. } => *media,
            _ => None,
        }
    }

    /// The file it needs stored (a command's log, a file's path), relative
    /// to its reply's `r<N>/<verifier>/`; `None` for an observation.
    pub fn file_path(&self) -> Option<&str> {
        match &self.body {
            EvidenceBody::Command { log, .. } => Some(log),
            EvidenceBody::File { path, .. } => Some(path),
            EvidenceBody::Observation { .. } => None,
        }
    }
}

// ----------------------------------------------------------- cross-checks

/// What a cross-check is about: another verifier's result on a criterion
/// (`codex/AC-2`) or a defect (`D1`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum Subject {
    Result { part: PartId, criterion: String },
    Defect { id: String },
}

impl Subject {
    /// `codex/AC-2` or `D1`; `None` for empty parts.
    pub fn parse(text: &str) -> Option<Self> {
        match text.split_once('/') {
            Some((part, criterion)) => (!part.is_empty()
                && !criterion.is_empty()
                && !criterion.contains('/'))
            .then(|| Subject::Result {
                part: PartId::new(part),
                criterion: criterion.to_owned(),
            }),
            None => (!text.is_empty()).then(|| Subject::Defect {
                id: text.to_owned(),
            }),
        }
    }
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Subject::Result { part, criterion } => write!(f, "{part}/{criterion}"),
            Subject::Defect { id } => f.write_str(id),
        }
    }
}

impl From<Subject> for String {
    fn from(subject: Subject) -> Self {
        subject.to_string()
    }
}

impl TryFrom<String> for Subject {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Subject::parse(&text).ok_or_else(|| format!("not a cross-check subject: {text:?}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossOutcome {
    Reproduced,
    NotReproduced,
    Disputed,
}

impl CrossOutcome {
    pub const ALL: [CrossOutcome; 3] = [
        CrossOutcome::Reproduced,
        CrossOutcome::NotReproduced,
        CrossOutcome::Disputed,
    ];

    pub fn name(self) -> &'static str {
        match self {
            CrossOutcome::Reproduced => "reproduced",
            CrossOutcome::NotReproduced => "not_reproduced",
            CrossOutcome::Disputed => "disputed",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            CrossOutcome::Reproduced => "再現",
            CrossOutcome::NotReproduced => "再現せず",
            CrossOutcome::Disputed => "異議",
        }
    }
}

/// A verifier reproducing or refuting another's result (round ≥ 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossCheck {
    pub round: RoundNo,
    pub by: PartId,
    pub subject: Subject,
    pub outcome: CrossOutcome,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub evidence: Vec<EvidenceKey>,
}

// --------------------------------------------------------------- defects

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl Severity {
    /// Most severe first.
    pub const ALL: [Severity; 5] = [
        Severity::Critical,
        Severity::High,
        Severity::Medium,
        Severity::Low,
        Severity::Info,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
            Severity::Info => "info",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Severity::Critical => "致命的",
            Severity::High => "高",
            Severity::Medium => "中",
            Severity::Low => "低",
            Severity::Info => "情報",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    pub const ALL: [Confidence; 3] = [Confidence::High, Confidence::Medium, Confidence::Low];

    pub fn name(self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Confidence::High => "確信度 高",
            Confidence::Medium => "確信度 中",
            Confidence::Low => "確信度 低",
        }
    }
}

/// Where a defect points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.path)?;
        match (self.line, self.end_line) {
            (Some(line), Some(end)) if end > line => write!(f, ":{line}-{end}"),
            (Some(line), _) => write!(f, ":{line}"),
            _ => Ok(()),
        }
    }
}

/// A raw defect's key: its verifier and the id the verifier gave it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RawKey {
    pub part: PartId,
    pub id: String,
}

impl RawKey {
    /// `codex/3` → the key; `None` without a `/`.
    pub fn parse(text: &str) -> Option<Self> {
        let (part, id) = text.split_once('/')?;
        (!part.is_empty() && !id.is_empty()).then(|| RawKey {
            part: PartId::new(part),
            id: id.to_owned(),
        })
    }
}

impl fmt::Display for RawKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.part, self.id)
    }
}

/// One defect as a verifier reported it: a problem no criterion covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawDefect {
    pub key: RawKey,
    pub round: RoundNo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    /// The location names a captured file (or there is none).
    pub in_target: bool,
    /// Whether the lines overlap a changed range (diff targets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_diff: Option<bool>,
    pub severity: Severity,
    pub category: String,
    pub confidence: Confidence,
    pub claim: String,
    pub repro: String,
    pub evidence: Vec<EvidenceKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefectStatus {
    Open,
    Agreed,
    Disputed,
    Rejected,
    Withdrawn,
    Merged,
}

impl DefectStatus {
    pub const ALL: [DefectStatus; 6] = [
        DefectStatus::Open,
        DefectStatus::Agreed,
        DefectStatus::Disputed,
        DefectStatus::Rejected,
        DefectStatus::Withdrawn,
        DefectStatus::Merged,
    ];

    pub fn name(self) -> &'static str {
        match self {
            DefectStatus::Open => "open",
            DefectStatus::Agreed => "agreed",
            DefectStatus::Disputed => "disputed",
            DefectStatus::Rejected => "rejected",
            DefectStatus::Withdrawn => "withdrawn",
            DefectStatus::Merged => "merged",
        }
    }

    /// The statuses the judge may set.
    pub fn parse_judge(name: &str) -> Option<Self> {
        [
            DefectStatus::Open,
            DefectStatus::Agreed,
            DefectStatus::Disputed,
            DefectStatus::Rejected,
        ]
        .into_iter()
        .find(|s| s.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            DefectStatus::Open => "未検証",
            DefectStatus::Agreed => "合意",
            DefectStatus::Disputed => "対立",
            DefectStatus::Rejected => "却下",
            DefectStatus::Withdrawn => "取り下げ",
            DefectStatus::Merged => "統合",
        }
    }

    /// Still under verification: open, agreed or disputed.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            DefectStatus::Open | DefectStatus::Agreed | DefectStatus::Disputed
        )
    }

    /// The order defects of equal severity are ranked in.
    pub fn rank(self) -> u8 {
        match self {
            DefectStatus::Agreed => 0,
            DefectStatus::Disputed => 1,
            DefectStatus::Open => 2,
            DefectStatus::Rejected => 3,
            DefectStatus::Withdrawn => 4,
            DefectStatus::Merged => 5,
        }
    }
}

/// A consolidated defect the judge keeps (`D1`, `D2`, …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Defect {
    pub id: String,
    pub claim: String,
    pub repro: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    pub in_target: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_diff: Option<bool>,
    pub severity: Severity,
    pub category: String,
    pub confidence: Confidence,
    pub status: DefectStatus,
    /// The defect this one was merged into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_into: Option<String>,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    pub sources: Vec<RawKey>,
    pub opened_in: RoundNo,
    pub changed_rev: u64,
    /// Status and severity after each round that changed them.
    #[serde(default)]
    pub history: Vec<(RoundNo, DefectStatus, Severity)>,
}

impl Defect {
    /// The verifiers who raised it.
    pub fn origins(&self) -> BTreeSet<&PartId> {
        self.sources.iter().map(|key| &key.part).collect()
    }

    /// Records the state as of `round`, unless it is unchanged.
    pub fn record(&mut self, round: RoundNo) {
        let now = (round, self.status, self.severity);
        match self.history.last_mut() {
            Some(last) if last.1 == now.1 && last.2 == now.2 => {}
            Some(last) if last.0 == round => *last = now,
            _ => self.history.push(now),
        }
    }
}

/// A raw defect the judge dismissed instead of keeping it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dismissal {
    pub raw: RawKey,
    pub round: RoundNo,
    pub reason: String,
}

/// One validated defect operation of the judge's output (review's lead
/// ops, with `repro`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DefectOp {
    Add {
        id: String,
        /// Raw keys (`codex/3`) and defect ids.
        from: Vec<String>,
        claim: String,
        repro: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        location: Option<Location>,
        severity: Severity,
        category: String,
        confidence: Confidence,
        status: DefectStatus,
        rationale: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fix: Option<String>,
    },
    Merge {
        id: String,
        from: Vec<String>,
    },
    Update {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claim: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repro: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        location: Option<Location>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        severity: Option<Severity>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        category: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confidence: Option<Confidence>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<DefectStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rationale: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fix: Option<String>,
    },
    Dismiss {
        raw: RawKey,
        reason: String,
    },
}

/// How far the panel agrees on a defect (review L2-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Agreement {
    Unanimous,
    Majority,
    Split,
    Single,
}

impl Agreement {
    pub const ALL: [Agreement; 4] = [
        Agreement::Unanimous,
        Agreement::Majority,
        Agreement::Split,
        Agreement::Single,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Agreement::Unanimous => "全員一致",
            Agreement::Majority => "多数",
            Agreement::Split => "対立",
            Agreement::Single => "単独",
        }
    }
}

/// Who supports, disputes, or has not taken a side on a defect, from its
/// sources and the cross-checks on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    pub supporters: Vec<PartId>,
    pub disputers: Vec<PartId>,
    pub silent: Vec<PartId>,
    pub agreement: Agreement,
}

impl Tally {
    /// `支持 3 · 反論 1 · 未表明 1`.
    pub fn label(&self) -> String {
        let mut out = format!("支持 {}", self.supporters.len());
        if !self.disputers.is_empty() {
            out.push_str(&format!(" · 反論 {}", self.disputers.len()));
        }
        if !self.silent.is_empty() {
            out.push_str(&format!(" · 未表明 {}", self.silent.len()));
        }
        out
    }
}

// --------------------------------------------------------------- rulings

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RulingKind {
    Verified,
    Failed,
    Inconclusive,
    NotApplicable,
}

impl RulingKind {
    pub const ALL: [RulingKind; 4] = [
        RulingKind::Verified,
        RulingKind::Failed,
        RulingKind::Inconclusive,
        RulingKind::NotApplicable,
    ];

    pub fn name(self) -> &'static str {
        match self {
            RulingKind::Verified => "verified",
            RulingKind::Failed => "failed",
            RulingKind::Inconclusive => "inconclusive",
            RulingKind::NotApplicable => "not_applicable",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            RulingKind::Verified => "検証済",
            RulingKind::Failed => "不合格",
            RulingKind::Inconclusive => "判定不能",
            RulingKind::NotApplicable => "対象外",
        }
    }
}

/// The judge's decision on one criterion in one round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ruling {
    pub round: RoundNo,
    pub kind: RulingKind,
    pub rationale: String,
    #[serde(default)]
    pub evidence: Vec<EvidenceKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Pass,
    PassWithIssues,
    Fail,
    Inconclusive,
}

impl Decision {
    pub const ALL: [Decision; 4] = [
        Decision::Pass,
        Decision::PassWithIssues,
        Decision::Fail,
        Decision::Inconclusive,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Decision::Pass => "pass",
            Decision::PassWithIssues => "pass_with_issues",
            Decision::Fail => "fail",
            Decision::Inconclusive => "inconclusive",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Decision::Pass => "合格",
            Decision::PassWithIssues => "条件付き合格",
            Decision::Fail => "不合格",
            Decision::Inconclusive => "判定不能",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub decision: Decision,
    pub rationale: String,
    /// Criteria ids and defect ids that hold the change back.
    #[serde(default)]
    pub blocking: Vec<String>,
}

/// The judge's validated output for a round (`r<N>/judge.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeOutput {
    pub summary: String,
    /// One per criterion, keyed by its id; a `"same"` ruling is resolved
    /// to a copy of the previous one with this round's number.
    #[serde(default)]
    pub rulings: BTreeMap<String, Ruling>,
    #[serde(default)]
    pub defect_ops: Vec<DefectOp>,
    /// Cross-check assignments for the next round.
    #[serde(default)]
    pub assign: BTreeMap<PartId, Vec<Subject>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    #[serde(default)]
    pub close: bool,
}

/// The judge's validated criteria (`c/criteria.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriteriaOutput {
    pub summary: String,
    pub criteria: Vec<Criterion>,
}

// ---------------------------------------------------------------- rounds

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Author {
    User,
    Part(PartId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MsgKind {
    /// Something the person said to the panel.
    Intervention,
    /// The judge's summary of a round.
    Summary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub round: RoundNo,
    pub author: Author,
    pub kind: MsgKind,
    /// Who it is addressed to; empty means everyone.
    pub to: Vec<PartId>,
    pub body: String,
    pub at: Millis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoundKind {
    /// Verifiers work alone and see nothing of each other.
    Blind,
    /// Verifiers reproduce or refute each other's results.
    Cross,
}

impl RoundKind {
    pub fn for_round(no: RoundNo) -> Self {
        if no <= 1 {
            RoundKind::Blind
        } else {
            RoundKind::Cross
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RoundKind::Blind => "独立検証",
            RoundKind::Cross => "追試",
        }
    }
}

/// Where a verification stands (L1-4). `Criteria` and `Confirming` come
/// before round 1 and are never a round's phase; a round runs `Briefing`
/// → `Verifying` → `Ingesting` → `Ruling` → `Closed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// The judge drafts the criteria.
    Criteria,
    /// The criteria wait for the person to confirm them.
    Confirming,
    /// Briefs are built and being delivered to the verifiers.
    Briefing,
    /// Waiting for the verifiers' reply files.
    Verifying,
    /// Evidence is copied and hashed, and re-runs run.
    Ingesting,
    /// The judge has its brief; waiting for its JSON.
    Ruling,
    Closed,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Criteria => "基準策定",
            Phase::Confirming => "基準確認待ち",
            Phase::Briefing => "配信",
            Phase::Verifying => "検証",
            Phase::Ingesting => "証跡収集",
            Phase::Ruling => "判定",
            Phase::Closed => "終了",
        }
    }
}

/// What a brief asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BriefKind {
    /// Criteria, a verifier's round or a ruling: a reply file follows.
    #[default]
    Work,
    /// Start the re-run script; no reply file (L1-5).
    Rerun,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// When the text last reached the agent's pane.
    pub sent: Option<Millis>,
    /// When the agent was first seen working after the send (for a re-run
    /// brief: when the script's first `.pid` appeared).
    pub started: Option<Millis>,
    /// When a valid reply file was taken in (for a re-run brief: when every
    /// item had an outcome).
    pub replied: Option<Millis>,
    pub retries: u8,
    /// Why delivery or the reply gave up, shown to the person.
    pub failed: Option<String>,
    /// Why the last reply file was turned down; the agent is asked to fix it.
    pub rejected: Option<String>,
    /// [`crate::store::digest`] of the reply that was turned down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_digest: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Brief {
    pub to: PartId,
    #[serde(default)]
    pub kind: BriefKind,
    /// The session `rev` whose state the brief describes.
    pub base_rev: u64,
    /// The exact text sent.
    pub body: String,
    /// The reply file; for a re-run brief, the `rerun/` directory watched.
    pub reply_path: PathBuf,
    pub receipt: Receipt,
}

/// What one verifier's accepted reply in a round held, for the cards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyNote {
    pub summary: String,
    pub environment: String,
    pub results: usize,
    pub evidence: usize,
    pub defects: usize,
    pub cross_checks: usize,
    pub at: Millis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Round {
    pub no: RoundNo,
    pub kind: RoundKind,
    pub phase: Phase,
    pub started_at: Millis,
    /// One work brief per verifier.
    pub briefs: Vec<Brief>,
    /// The re-run briefs of this round, one per verifier with re-runs.
    #[serde(default)]
    pub rerun_briefs: Vec<Brief>,
    /// The judge's ruling brief, once the round reaches `Ruling`.
    pub judge_brief: Option<Brief>,
    /// What each verifier's accepted reply held.
    #[serde(default)]
    pub replies: BTreeMap<PartId, ReplyNote>,
    /// The verifiers whose evidence was ingested this round.
    #[serde(default)]
    pub ingested: BTreeSet<PartId>,
    /// The re-run script written for each verifier this round.
    #[serde(default)]
    pub reruns: BTreeMap<PartId, RerunRequest>,
    /// The log tails `EvidenceIngested` recorded this round, by evidence
    /// key: what the judge's ruling brief shows of failing commands.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tails: BTreeMap<EvidenceKey, String>,
    /// What each verifier must cross-check this round (round ≥ 2): the
    /// previous judge's `assign`, or the default (L1-4 item 4).
    #[serde(default)]
    pub assignments: BTreeMap<PartId, Vec<Subject>>,
    /// The judge's accepted output.
    pub outcome: Option<JudgeOutput>,
}

impl Round {
    /// The brief of `kind` sent to `to` this round (the judge's for the
    /// judge's id).
    pub fn brief(&self, to: &PartId, kind: BriefKind) -> Option<&Brief> {
        self.briefs
            .iter()
            .chain(&self.rerun_briefs)
            .chain(self.judge_brief.as_ref())
            .find(|brief| &brief.to == to && brief.kind == kind)
    }

    pub fn brief_mut(&mut self, to: &PartId, kind: BriefKind) -> Option<&mut Brief> {
        self.briefs
            .iter_mut()
            .chain(&mut self.rerun_briefs)
            .chain(self.judge_brief.as_mut())
            .find(|brief| &brief.to == to && brief.kind == kind)
    }
}

// --------------------------------------------------------------- session

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    /// What the person told the panel about the change (依頼); may be empty.
    pub request: String,
    pub target: Target,
    pub manifest: TargetManifest,
    pub rebuild: Rebuild,
    pub isolation: Isolation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<SpecFile>,
    /// The person's own criteria, one per line as typed (trimmed).
    #[serde(default)]
    pub user_criteria: Vec<String>,
    /// The top level of the target's checkout (`capture::repo_root` of
    /// `target.repo`; a linked worktree when the target is in one): the
    /// judge runs here, the judge's reply dir is
    /// `<workdir>/.noa/verify/<id>/`, verifiers run here in 共有 mode, and
    /// 修正を依頼 opens its agent here.
    pub workdir: PathBuf,
    /// The repository's main worktree (`worktree::main_worktree`), whose
    /// `<main>-wt/` holds the verifiers' worktrees (L1-3). The same as
    /// `workdir` unless the target is a linked worktree.
    pub main_worktree: PathBuf,
    pub max_rounds: RoundNo,
    /// Bumped once per applied event batch.
    pub rev: u64,
    pub created_at: Millis,
    pub judge: Participant,
    pub verifiers: Vec<Participant>,
    /// 基準を確認してから検証: hold for the person after the criteria.
    pub confirm: bool,
    /// The judge's criteria brief.
    #[serde(default)]
    pub criteria_brief: Option<Brief>,
    /// Whether the judge's criteria were accepted.
    #[serde(default)]
    pub criteria_accepted: bool,
    /// The judge's summary from `c/criteria.json`.
    #[serde(default)]
    pub criteria_summary: String,
    /// The criteria in order, as edited.
    #[serde(default)]
    pub criteria: Vec<Criterion>,
    /// Every edit the person made, in order, with when.
    #[serde(default)]
    pub criteria_edits: Vec<(CriteriaEdit, Millis)>,
    /// Criteria the person removed, as they were.
    #[serde(default)]
    pub removed_criteria: Vec<Criterion>,
    /// When the criteria were frozen (round 1 starts).
    #[serde(default)]
    pub frozen_at: Option<Millis>,
    /// Each verifier's worktree (worktree isolation).
    #[serde(default)]
    pub worktrees: BTreeMap<PartId, Worktree>,
    /// Every result, in the order replies were recorded.
    #[serde(default)]
    pub results: Vec<CriterionResult>,
    /// Every evidence item, reported and attached.
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    #[serde(default)]
    pub cross_checks: Vec<CrossCheck>,
    #[serde(default)]
    pub raw_defects: Vec<RawDefect>,
    #[serde(default)]
    pub defects: Vec<Defect>,
    #[serde(default)]
    pub dismissed: Vec<Dismissal>,
    /// Every ruling per criterion id, oldest first.
    #[serde(default)]
    pub rulings: BTreeMap<String, Vec<Ruling>>,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub rounds: Vec<Round>,
    pub closed: bool,
    /// The person holds the verification after each ruled round.
    pub hold: bool,
    /// The round whose judge output gave the verdict; cleared when rounds
    /// are added, so an old verdict is never the current one.
    #[serde(default)]
    pub verdict_round: Option<RoundNo>,
}

impl Session {
    pub fn participant(&self, id: &PartId) -> Option<&Participant> {
        std::iter::once(&self.judge)
            .chain(&self.verifiers)
            .find(|part| &part.id == id)
    }

    pub fn is_verifier(&self, id: &PartId) -> bool {
        self.verifiers.iter().any(|part| &part.id == id)
    }

    pub fn verifier_ids(&self) -> impl Iterator<Item = &PartId> {
        self.verifiers.iter().map(|part| &part.id)
    }

    pub fn current_round(&self) -> Option<&Round> {
        self.rounds.last()
    }

    pub fn round(&self, no: RoundNo) -> Option<&Round> {
        if let Some(idx) = no.checked_sub(1).map(|i| i as usize)
            && let Some(round) = self.rounds.get(idx)
            && round.no == no
        {
            return Some(round);
        }
        self.rounds.iter().find(|round| round.no == no)
    }

    pub fn round_mut(&mut self, no: RoundNo) -> Option<&mut Round> {
        if let Some(idx) = no.checked_sub(1).map(|i| i as usize)
            && self.rounds.get(idx).is_some_and(|round| round.no == no)
        {
            return self.rounds.get_mut(idx);
        }
        self.rounds.iter_mut().find(|round| round.no == no)
    }

    /// Where the verification stands: the current round's phase; before
    /// round 1, `Criteria` until the judge's criteria are accepted, then
    /// `Confirming` until they are frozen, then `Briefing` while the
    /// worktrees are made; `Closed` once ended.
    pub fn phase(&self) -> Phase {
        if self.closed {
            return Phase::Closed;
        }
        match self.current_round() {
            Some(round) => round.phase,
            None if self.frozen_at.is_some() => Phase::Briefing,
            None if self.criteria_accepted => Phase::Confirming,
            None => Phase::Criteria,
        }
    }

    pub fn next_message_id(&self, offset: usize) -> MessageId {
        MessageId::nth(self.messages.len() + 1 + offset)
    }

    /// Whether `round` is the verification's last under its current limit.
    pub fn is_last(&self, round: RoundNo) -> bool {
        round >= self.max_rounds
    }

    pub fn criterion(&self, id: &str) -> Option<&Criterion> {
        self.criteria.iter().find(|c| c.id == id)
    }

    /// The latest ruling on criterion `id`.
    pub fn ruling(&self, id: &str) -> Option<&Ruling> {
        self.rulings.get(id).and_then(|all| all.last())
    }

    pub fn evidence_item(&self, key: &EvidenceKey) -> Option<&Evidence> {
        self.evidence.iter().find(|e| &e.key == key)
    }

    pub fn evidence_item_mut(&mut self, key: &EvidenceKey) -> Option<&mut Evidence> {
        self.evidence.iter_mut().find(|e| &e.key == key)
    }

    pub fn defect(&self, id: &str) -> Option<&Defect> {
        self.defects.iter().find(|d| d.id == id)
    }

    pub fn defect_mut(&mut self, id: &str) -> Option<&mut Defect> {
        self.defects.iter_mut().find(|d| d.id == id)
    }

    pub fn raw_defect(&self, key: &RawKey) -> Option<&RawDefect> {
        self.raw_defects.iter().find(|r| &r.key == key)
    }

    /// The verdict: the judge's, from the round that ended the
    /// verification. `None` while rounds added after it run (or were never
    /// run).
    pub fn verdict(&self) -> Option<&Verdict> {
        self.round(self.verdict_round?)?
            .outcome
            .as_ref()?
            .verdict
            .as_ref()
    }

    /// Whether the person ended the verification without a verdict.
    pub fn ended_by_user(&self) -> bool {
        self.closed && self.verdict().is_none()
    }

    pub fn author_name(&self, author: &Author) -> String {
        match author {
            Author::User => "ユーザー".to_owned(),
            Author::Part(id) if id.as_str() == JUDGE_ID => "判定役".to_owned(),
            Author::Part(id) => id.to_string(),
        }
    }
}

/// Orders ids as people read them: runs of digits by value, so `AC-2`
/// comes before `AC-10`.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut a, mut b) = (a, b);
    loop {
        let (Some(x), Some(y)) = (a.chars().next(), b.chars().next()) else {
            return a.len().cmp(&b.len());
        };
        let order = if x.is_ascii_digit() && y.is_ascii_digit() {
            let run = |s: &str| s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
            let (na, nb) = (run(a), run(b));
            let (da, db) = (
                a[..na].trim_start_matches('0'),
                b[..nb].trim_start_matches('0'),
            );
            let order = da
                .len()
                .cmp(&db.len())
                .then_with(|| da.cmp(db))
                .then(na.cmp(&nb));
            (a, b) = (&a[na..], &b[nb..]);
            order
        } else {
            (a, b) = (&a[x.len_utf8()..], &b[y.len_utf8()..]);
            x.cmp(&y)
        };
        if order != Ordering::Equal {
            return order;
        }
    }
}

/// The first `n` characters of `id` (a git object id, shown short); safe
/// on any text a hand-edited log may hold.
pub fn short_id(id: &str, n: usize) -> String {
    id.chars().take(n).collect()
}

/// Whether `id` looks like a git object id: 4 to 64 hex digits.
pub fn is_object_id(id: &str) -> bool {
    (4..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit())
}

// ----------------------------------------------------------------- events

/// The event log (L2-3). `reduce` folds it into a [`Session`]; each batch
/// bumps `rev` once. Brief lifecycle events name the criteria phase as
/// round [`CRITERIA_ROUND`] and carry the brief's [`BriefKind`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Always the first event of the log, and only there.
    Created {
        id: SessionId,
        request: String,
        target: Box<Target>,
        manifest: Box<TargetManifest>,
        rebuild: Rebuild,
        isolation: Isolation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spec: Option<SpecFile>,
        #[serde(default)]
        user_criteria: Vec<String>,
        workdir: PathBuf,
        main_worktree: PathBuf,
        max_rounds: RoundNo,
        judge: Participant,
        verifiers: Vec<Participant>,
        confirm: bool,
        at: Millis,
    },
    PaneAssigned {
        part: PartId,
        pane_id: u64,
    },
    BriefIssued {
        round: RoundNo,
        brief: Brief,
    },
    BriefSent {
        round: RoundNo,
        to: PartId,
        #[serde(default)]
        kind: BriefKind,
        at: Millis,
    },
    BriefStarted {
        round: RoundNo,
        to: PartId,
        #[serde(default)]
        kind: BriefKind,
        at: Millis,
    },
    /// Delivery is tried again: the brief counts as unsent.
    BriefRetried {
        round: RoundNo,
        to: PartId,
        #[serde(default)]
        kind: BriefKind,
        reason: String,
    },
    BriefFailed {
        round: RoundNo,
        to: PartId,
        #[serde(default)]
        kind: BriefKind,
        reason: String,
    },
    /// The person asked to try a failed brief again from the start.
    BriefReset {
        round: RoundNo,
        to: PartId,
        #[serde(default)]
        kind: BriefKind,
    },
    /// A reply file was turned down; the agent is asked to fix it.
    ReplyRejected {
        round: RoundNo,
        to: PartId,
        error: String,
        /// [`crate::store::digest`] of the turned-down reply.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<u64>,
    },
    ReplyAccepted {
        round: RoundNo,
        to: PartId,
        at: Millis,
    },
    /// The judge's validated `c/criteria.json`.
    CriteriaAccepted {
        #[serde(default)]
        summary: String,
        criteria: Vec<Criterion>,
    },
    /// One change the person made before round 1.
    CriteriaEdited {
        edit: CriteriaEdit,
        at: Millis,
    },
    /// Round 1 starts: the criteria no longer change.
    CriteriaFrozen {
        at: Millis,
    },
    WorktreeMade {
        part: PartId,
        path: PathBuf,
    },
    WorktreeFailed {
        part: PartId,
        path: PathBuf,
        reason: String,
    },
    WorktreeRemoved {
        part: PartId,
    },
    RoundStarted {
        no: RoundNo,
        kind: RoundKind,
        /// What each verifier must cross-check (round ≥ 2).
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        assignments: BTreeMap<PartId, Vec<Subject>>,
        at: Millis,
    },
    MessagePosted {
        message: Message,
    },
    /// A verifier's validated reply. Its evidence is not stored yet.
    ReplyRecorded {
        round: RoundNo,
        part: PartId,
        summary: String,
        #[serde(default)]
        environment: String,
        #[serde(default)]
        results: Vec<CriterionResult>,
        #[serde(default)]
        evidence: Vec<Evidence>,
        #[serde(default)]
        defects: Vec<RawDefect>,
        #[serde(default)]
        cross_checks: Vec<CrossCheck>,
        at: Millis,
    },
    /// A verifier's evidence files were copied into the judge's reply dir.
    /// A file item of `part`'s reply missing from `items` was gone. `tails`
    /// holds, for each `command` item whose reported exit status is not 0
    /// and whose log was stored, the last [`JUDGE_TAIL_LINES`] lines of the
    /// stored copy, for the judge's ruling brief.
    EvidenceIngested {
        round: RoundNo,
        part: PartId,
        items: Vec<(EvidenceKey, Stored)>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        tails: BTreeMap<EvidenceKey, String>,
    },
    /// A re-run script was written for `part` (L1-5 step 1).
    RerunRequested {
        round: RoundNo,
        part: PartId,
        nonce: String,
        items: Vec<RerunItem>,
        script_digest: u64,
    },
    RerunFinished {
        key: EvidenceKey,
        rerun: Rerun,
    },
    /// Media the person attached (L1-5a), while `round` was the current
    /// round.
    EvidenceAttached {
        key: EvidenceKey,
        round: RoundNo,
        body: EvidenceBody,
        stored: Stored,
        #[serde(default)]
        shows: Vec<String>,
        at: Millis,
    },
    PhaseChanged {
        round: RoundNo,
        phase: Phase,
    },
    /// The judge's validated ruling output.
    JudgeAccepted {
        round: RoundNo,
        output: JudgeOutput,
    },
    /// The person holds the verification after each ruled round (`true`),
    /// or lets rounds follow each other again.
    HoldSet {
        #[serde(default)]
        hold: bool,
    },
    /// The person raised the round limit to `max_rounds`. A verification
    /// that ended at its limit is open again.
    RoundsExtended {
        max_rounds: RoundNo,
        #[serde(default)]
        at: Millis,
    },
    SessionClosed,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str) -> PartId {
        PartId::new(id)
    }

    fn key(text: &str) -> EvidenceKey {
        EvidenceKey::new(text)
    }

    fn stored(path: &str) -> Stored {
        Stored {
            path: PathBuf::from(path),
            bytes: 120,
            digest: 0xdead_beef,
        }
    }

    fn participant(id: &str, kind: AgentKind, strategy: Option<Strategy>) -> Participant {
        Participant {
            id: p(id),
            kind,
            role: if strategy.is_some() {
                Role::Verifier
            } else {
                Role::Judge
            },
            pane_id: Some(7),
            setup: AgentSetup {
                model: Some("opus".into()),
                effort: None,
            },
            strategy,
        }
    }

    fn brief(to: &str, kind: BriefKind) -> Brief {
        Brief {
            to: p(to),
            kind,
            base_rev: 3,
            body: "## 1. Request and round\n…".into(),
            reply_path: PathBuf::from("/repo/.noa/verify/ver-1/r1/claude/reply.json"),
            receipt: Receipt {
                sent: Some(10),
                started: Some(11),
                replied: None,
                retries: 1,
                failed: None,
                rejected: Some("results: AC-2 missing".into()),
                rejected_digest: Some(42),
            },
        }
    }

    fn criterion(id: &str, source: CriterionSource) -> Criterion {
        Criterion {
            id: id.into(),
            statement: "Opening a tab inherits the focused pane's cwd.".into(),
            how: "cd /tmp, open a tab, pwd".into(),
            priority: Priority::Must,
            source,
            edited: false,
        }
    }

    fn judge_output() -> JudgeOutput {
        JudgeOutput {
            summary: "AC-1 fails.".into(),
            rulings: BTreeMap::from([(
                "AC-1".to_owned(),
                Ruling {
                    round: 1,
                    kind: RulingKind::Failed,
                    rationale: "re-run differed".into(),
                    evidence: vec![key("claude/r1/e1")],
                },
            )]),
            defect_ops: vec![
                DefectOp::Add {
                    id: "D1".into(),
                    from: vec!["claude/1".into()],
                    claim: "cwd lost".into(),
                    repro: "1. cd /tmp 2. ⌘T".into(),
                    location: Some(Location {
                        path: "crates/a.rs".into(),
                        line: Some(4),
                        end_line: None,
                    }),
                    severity: Severity::High,
                    category: "conformance".into(),
                    confidence: Confidence::High,
                    status: DefectStatus::Open,
                    rationale: "seen twice".into(),
                    fix: None,
                },
                DefectOp::Merge {
                    id: "D1".into(),
                    from: vec!["codex/2".into()],
                },
                DefectOp::Update {
                    id: "D1".into(),
                    claim: None,
                    repro: Some("1. … 2. …".into()),
                    location: None,
                    severity: Some(Severity::Critical),
                    category: None,
                    confidence: None,
                    status: Some(DefectStatus::Agreed),
                    rationale: None,
                    fix: None,
                },
                DefectOp::Dismiss {
                    raw: RawKey {
                        part: p("claude-2"),
                        id: "1".into(),
                    },
                    reason: "duplicate".into(),
                },
            ],
            assign: BTreeMap::from([(
                p("codex"),
                vec![
                    Subject::Result {
                        part: p("claude"),
                        criterion: "AC-1".into(),
                    },
                    Subject::Defect { id: "D1".into() },
                ],
            )]),
            verdict: Some(Verdict {
                decision: Decision::Fail,
                rationale: "AC-1 failed".into(),
                blocking: vec!["AC-1".into(), "D1".into()],
            }),
            close: false,
        }
    }

    fn every_event() -> Vec<Event> {
        let shot = EvidenceBody::File {
            path: "video/new-tab.mov".into(),
            caption: Some("cd /tmp, ⌘T, pwd".into()),
            media: Some(Media::QuickTime),
            marks: vec![Mark {
                t_ms: 6_500,
                note: "new tab shows $HOME".into(),
            }],
            shows: vec!["AC-1".into()],
        };
        vec![
            Event::Created {
                id: SessionId::at(1),
                request: "Verify cwd inheritance".into(),
                target: Box::new(Target {
                    kind: TargetKind::Diff,
                    repo: PathBuf::from("/repo"),
                    base: Some("main".into()),
                    head: Head::Ref("feat/x".into()),
                    paths: vec!["crates/".into()],
                    spec: Some("docs/spec.md".into()),
                }),
                manifest: Box::new(TargetManifest {
                    label: "feat/x … main (1 file)".into(),
                    repo: Some(PathBuf::from("/repo")),
                    head_sha: Some("abcd1234".into()),
                    files: vec![TargetFile {
                        path: "crates/a.rs".into(),
                        status: FileStatus::Renamed {
                            from: "crates/b.rs".into(),
                        },
                        lines: 10,
                        bytes: 300,
                        additions: 2,
                        deletions: 1,
                        changed: vec![(3, 4)],
                        digest: 9,
                    }],
                    omitted: vec![Omitted {
                        path: "big.bin".into(),
                        reason: "binary".into(),
                    }],
                    captured_at: 1,
                    ..TargetManifest::default()
                }),
                rebuild: Rebuild {
                    head_sha: "abcd1234".into(),
                    worktree_patch: Some(5),
                    untracked: vec![("notes.txt".into(), 6)],
                },
                isolation: Isolation::Worktree,
                spec: Some(SpecFile {
                    path: "docs/spec.md".into(),
                    bytes: 100,
                    lines: 4,
                    digest: 8,
                }),
                user_criteria: vec!["Tabs inherit cwd".into()],
                workdir: PathBuf::from("/src/app-wt/feature"),
                main_worktree: PathBuf::from("/src/app"),
                max_rounds: DEFAULT_MAX_ROUNDS,
                judge: participant(JUDGE_ID, AgentKind::Claude, None),
                verifiers: vec![
                    participant(
                        "claude",
                        AgentKind::Claude,
                        Some(Strategy::Builtin {
                            id: BuiltinStrategy::Conformance,
                        }),
                    ),
                    participant(
                        "codex",
                        AgentKind::Codex,
                        Some(Strategy::Custom {
                            id: "custom".into(),
                            text: "Try odd locales".into(),
                        }),
                    ),
                ],
                confirm: true,
                at: 1,
            },
            Event::PaneAssigned {
                part: p("claude"),
                pane_id: 4,
            },
            Event::BriefIssued {
                round: CRITERIA_ROUND,
                brief: brief(JUDGE_ID, BriefKind::Work),
            },
            Event::BriefSent {
                round: 1,
                to: p("claude"),
                kind: BriefKind::Rerun,
                at: 2,
            },
            Event::BriefStarted {
                round: 1,
                to: p("claude"),
                kind: BriefKind::Work,
                at: 3,
            },
            Event::BriefRetried {
                round: 1,
                to: p("claude"),
                kind: BriefKind::Work,
                reason: "no start".into(),
            },
            Event::BriefFailed {
                round: 1,
                to: p("codex"),
                kind: BriefKind::Rerun,
                reason: "pane gone".into(),
            },
            Event::BriefReset {
                round: 1,
                to: p("codex"),
                kind: BriefKind::Work,
            },
            Event::ReplyRejected {
                round: 1,
                to: p("claude"),
                error: "results: AC-2 missing".into(),
                digest: Some(7),
            },
            Event::ReplyAccepted {
                round: 1,
                to: p("claude"),
                at: 4,
            },
            Event::CriteriaAccepted {
                summary: "One criterion.".into(),
                criteria: vec![
                    criterion("AC-1", CriterionSource::User),
                    criterion("AC-2", CriterionSource::Spec { line: 12 }),
                ],
            },
            Event::CriteriaEdited {
                edit: CriteriaEdit::Add {
                    statement: "Quits cleanly".into(),
                    how: "⌘Q".into(),
                    priority: Priority::Should,
                },
                at: 5,
            },
            Event::CriteriaEdited {
                edit: CriteriaEdit::Edit {
                    id: "AC-2".into(),
                    statement: None,
                    how: Some("run it".into()),
                    priority: Some(Priority::Should),
                },
                at: 5,
            },
            Event::CriteriaEdited {
                edit: CriteriaEdit::Remove { id: "AC-3".into() },
                at: 5,
            },
            Event::CriteriaEdited {
                edit: CriteriaEdit::Move {
                    id: "AC-2".into(),
                    to: 0,
                },
                at: 5,
            },
            Event::CriteriaFrozen { at: 6 },
            Event::WorktreeMade {
                part: p("claude"),
                path: PathBuf::from("/repo-wt/verify-ver-1-claude"),
            },
            Event::WorktreeFailed {
                part: p("codex"),
                path: PathBuf::from("/repo-wt/verify-ver-1-codex"),
                reason: "crates/a.rs differs".into(),
            },
            Event::WorktreeRemoved { part: p("claude") },
            Event::RoundStarted {
                no: 2,
                kind: RoundKind::Cross,
                assignments: BTreeMap::from([(
                    p("claude"),
                    vec![Subject::Result {
                        part: p("codex"),
                        criterion: "AC-1".into(),
                    }],
                )]),
                at: 7,
            },
            Event::MessagePosted {
                message: Message {
                    id: MessageId::nth(1),
                    round: 1,
                    author: Author::Part(p(JUDGE_ID)),
                    kind: MsgKind::Summary,
                    to: vec![p("codex")],
                    body: "Look again".into(),
                    at: 8,
                },
            },
            Event::ReplyRecorded {
                round: 1,
                part: p("claude"),
                summary: "Ran the tests.".into(),
                environment: "macOS 26.1".into(),
                results: vec![CriterionResult {
                    round: 1,
                    part: p("claude"),
                    criterion: "AC-1".into(),
                    outcome: Outcome::Fail,
                    note: "opens in $HOME".into(),
                    evidence: vec![key("claude/r1/e1"), key("claude/r1/e2")],
                }],
                evidence: vec![
                    Evidence {
                        key: key("claude/r1/e1"),
                        body: EvidenceBody::Command {
                            cmd: "echo \"$(pwd)\"\nexit 3".into(),
                            cwd: ".".into(),
                            exit: 101,
                            log: "logs/1.log".into(),
                            rerun: true,
                        },
                        stored: None,
                        rerun: None,
                        attached: None,
                    },
                    Evidence {
                        key: key("claude/r1/e2"),
                        body: shot.clone(),
                        stored: None,
                        rerun: None,
                        attached: None,
                    },
                    Evidence {
                        key: key("claude/r1/e3"),
                        body: EvidenceBody::Observation {
                            text: "the tab flickers".into(),
                        },
                        stored: None,
                        rerun: None,
                        attached: None,
                    },
                ],
                defects: vec![RawDefect {
                    key: RawKey {
                        part: p("claude"),
                        id: "1".into(),
                    },
                    round: 1,
                    location: None,
                    in_target: true,
                    in_diff: Some(false),
                    severity: Severity::Medium,
                    category: "conformance".into(),
                    confidence: Confidence::Low,
                    claim: "flicker".into(),
                    repro: "1. ⌘T".into(),
                    evidence: vec![key("claude/r1/e3")],
                    fix: Some("debounce".into()),
                }],
                cross_checks: vec![CrossCheck {
                    round: 2,
                    by: p("claude"),
                    subject: Subject::Result {
                        part: p("codex"),
                        criterion: "AC-1".into(),
                    },
                    outcome: CrossOutcome::NotReproduced,
                    note: "passes here".into(),
                    evidence: vec![key("claude/r2/e1")],
                }],
                at: 9,
            },
            Event::EvidenceIngested {
                round: 1,
                part: p("claude"),
                items: vec![(key("claude/r1/e1"), stored("evidence/r1/claude/logs/1.log"))],
                tails: BTreeMap::from([(
                    key("claude/r1/e1"),
                    "test cwd_inherit ... FAILED\n".into(),
                )]),
            },
            Event::RerunRequested {
                round: 1,
                part: p("claude"),
                nonce: "00112233445566778899aabbccddeeff".into(),
                items: vec![RerunItem {
                    key: key("claude/r1/e1"),
                    n: 1,
                    cmd_digest: 11,
                }],
                script_digest: 12,
            },
            Event::RerunFinished {
                key: key("claude/r1/e1"),
                rerun: Rerun::Differed {
                    exit: Some(0),
                    log: stored("evidence/r1/claude/rerun-1.log"),
                },
            },
            Event::RerunFinished {
                key: key("codex/r1/e1"),
                rerun: Rerun::NotRun {
                    why: "共有 mode".into(),
                    log: None,
                },
            },
            Event::RerunFinished {
                key: key("codex/r1/e2"),
                rerun: Rerun::NotRun {
                    why: "re-run script was changed".into(),
                    log: Some(stored("evidence/r1/codex/rerun-2.log")),
                },
            },
            Event::EvidenceAttached {
                key: EvidenceKey::user(1),
                round: 2,
                body: shot,
                stored: stored("evidence/user/1.mov"),
                shows: vec!["AC-1".into()],
                at: 10,
            },
            Event::PhaseChanged {
                round: 1,
                phase: Phase::Ingesting,
            },
            Event::JudgeAccepted {
                round: 1,
                output: judge_output(),
            },
            Event::HoldSet { hold: true },
            Event::RoundsExtended {
                max_rounds: 3,
                at: 11,
            },
            Event::SessionClosed,
        ]
    }

    #[test]
    fn every_event_survives_a_json_round_trip() {
        let events = every_event();
        let line = serde_json::to_string(&events).unwrap();
        let back: Vec<Event> = serde_json::from_str(&line).unwrap();
        assert_eq!(back, events);
        // The log's tags are the snake_case names L2-3 gives.
        assert!(line.contains(r#""event":"criteria_accepted""#));
        assert!(line.contains(r#""source":"spec:12""#));
        assert!(line.contains(r#""claude/AC-1""#));
        // A not-run re-run without a log writes no `log` field.
        assert!(line.contains(r#""outcome":"not_run","why":"共有 mode"}"#));
    }

    #[test]
    fn the_start_views_choices_and_the_tally_survive_a_json_round_trip() {
        let panel = vec![
            VerifierChoice {
                agent: AgentChoice {
                    kind: AgentKind::Claude,
                    setup: AgentSetup {
                        model: Some("opus".into()),
                        effort: Some("high".into()),
                    },
                },
                strategy: StrategyChoice::Builtin(BuiltinStrategy::Falsify),
            },
            VerifierChoice {
                agent: AgentKind::Codex.into(),
                strategy: StrategyChoice::Custom("Try odd locales".into()),
            },
        ];
        let json = serde_json::to_value(&panel).unwrap();
        assert_eq!(
            json[1],
            serde_json::json!({
                "agent": {"kind": "codex"},
                "strategy": {"custom": "Try odd locales"}
            })
        );
        assert_eq!(
            json[0]["strategy"],
            serde_json::json!({"builtin": "falsify"})
        );
        let back: Vec<VerifierChoice> = serde_json::from_value(json).unwrap();
        assert_eq!(back, panel);

        let judge = AgentChoice::from(AgentKind::Agy);
        let back: AgentChoice =
            serde_json::from_value(serde_json::to_value(&judge).unwrap()).unwrap();
        assert_eq!(back, judge);

        for agreement in Agreement::ALL {
            let tally = Tally {
                supporters: vec![p("claude"), p("codex")],
                disputers: vec![p("agy")],
                silent: vec![p("claude-2")],
                agreement,
            };
            let json = serde_json::to_string(&tally).unwrap();
            let back: Tally = serde_json::from_str(&json).unwrap();
            assert_eq!(back, tally);
        }
        assert_eq!(
            serde_json::to_value(Agreement::Unanimous).unwrap(),
            serde_json::json!("unanimous")
        );
    }

    #[test]
    fn a_session_survives_a_json_round_trip() {
        let Some(Event::Created {
            id,
            request,
            target,
            manifest,
            rebuild,
            isolation,
            spec,
            user_criteria,
            workdir,
            main_worktree,
            max_rounds,
            judge,
            verifiers,
            confirm,
            at,
        }) = every_event().into_iter().next()
        else {
            unreachable!()
        };
        let round = Round {
            no: 1,
            kind: RoundKind::Blind,
            phase: Phase::Ruling,
            started_at: 2,
            briefs: vec![brief("claude", BriefKind::Work)],
            rerun_briefs: vec![brief("claude", BriefKind::Rerun)],
            judge_brief: Some(brief(JUDGE_ID, BriefKind::Work)),
            replies: BTreeMap::from([(
                p("claude"),
                ReplyNote {
                    summary: "Ran".into(),
                    environment: "macOS".into(),
                    results: 1,
                    evidence: 2,
                    defects: 0,
                    cross_checks: 0,
                    at: 3,
                },
            )]),
            ingested: BTreeSet::from([p("claude")]),
            reruns: BTreeMap::from([(
                p("claude"),
                RerunRequest {
                    nonce: "00".repeat(16),
                    items: vec![RerunItem {
                        key: key("claude/r1/e1"),
                        n: 1,
                        cmd_digest: 1,
                    }],
                    script_digest: 2,
                },
            )]),
            tails: BTreeMap::from([(key("claude/r1/e1"), "error[E0425]\n".into())]),
            assignments: BTreeMap::new(),
            outcome: Some(judge_output()),
        };
        let session = Session {
            id,
            request,
            target: *target,
            manifest: *manifest,
            rebuild,
            isolation,
            spec,
            user_criteria,
            workdir,
            main_worktree,
            max_rounds,
            rev: 9,
            created_at: at,
            judge,
            verifiers,
            confirm,
            criteria_brief: Some(brief(JUDGE_ID, BriefKind::Work)),
            criteria_accepted: true,
            criteria_summary: "One criterion.".into(),
            criteria: vec![Criterion {
                edited: true,
                ..criterion("AC-1", CriterionSource::Inferred)
            }],
            criteria_edits: vec![(CriteriaEdit::Remove { id: "AC-2".into() }, 5)],
            removed_criteria: vec![criterion("AC-2", CriterionSource::Request)],
            frozen_at: Some(6),
            worktrees: BTreeMap::from([
                (
                    p("claude"),
                    Worktree {
                        path: PathBuf::from("/repo-wt/verify-ver-1-claude"),
                        state: WorktreeState::Made,
                    },
                ),
                (
                    p("codex"),
                    Worktree {
                        path: PathBuf::from("/repo-wt/verify-ver-1-codex"),
                        state: WorktreeState::Failed {
                            reason: "differs".into(),
                        },
                    },
                ),
            ]),
            results: vec![CriterionResult {
                round: 1,
                part: p("claude"),
                criterion: "AC-1".into(),
                outcome: Outcome::Pass,
                note: String::new(),
                evidence: vec![key("claude/r1/e1")],
            }],
            evidence: vec![
                Evidence {
                    key: key("claude/r1/e1"),
                    body: EvidenceBody::Command {
                        cmd: "cargo test".into(),
                        cwd: ".".into(),
                        exit: 0,
                        log: "logs/1.log".into(),
                        rerun: true,
                    },
                    stored: Some(stored("evidence/r1/claude/logs/1.log")),
                    rerun: Some(Rerun::Matched {
                        exit: 0,
                        log: stored("evidence/r1/claude/rerun-1.log"),
                    }),
                    attached: None,
                },
                Evidence {
                    key: EvidenceKey::user(1),
                    body: EvidenceBody::File {
                        path: "tab.png".into(),
                        caption: Some("pwd in the new tab".into()),
                        media: Some(Media::Png),
                        marks: Vec::new(),
                        shows: vec!["AC-1".into()],
                    },
                    stored: Some(stored("evidence/user/1.png")),
                    rerun: None,
                    attached: Some(Attachment { round: 2, at: 12 }),
                },
            ],
            cross_checks: Vec::new(),
            raw_defects: Vec::new(),
            defects: vec![Defect {
                id: "D1".into(),
                claim: "cwd lost".into(),
                repro: "1. ⌘T".into(),
                location: None,
                in_target: true,
                in_diff: None,
                severity: Severity::High,
                category: "falsify".into(),
                confidence: Confidence::Medium,
                status: DefectStatus::Merged,
                merged_into: Some("D2".into()),
                rationale: "same".into(),
                fix: None,
                sources: vec![RawKey {
                    part: p("codex"),
                    id: "1".into(),
                }],
                opened_in: 1,
                changed_rev: 4,
                history: vec![(1, DefectStatus::Open, Severity::High)],
            }],
            dismissed: vec![Dismissal {
                raw: RawKey {
                    part: p("claude"),
                    id: "2".into(),
                },
                round: 1,
                reason: "dup".into(),
            }],
            rulings: BTreeMap::from([(
                "AC-1".to_owned(),
                vec![Ruling {
                    round: 1,
                    kind: RulingKind::Verified,
                    rationale: "matched re-run".into(),
                    evidence: vec![key("claude/r1/e1")],
                }],
            )]),
            messages: Vec::new(),
            rounds: vec![round],
            closed: false,
            hold: true,
            verdict_round: Some(1),
        };
        let json = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back, session);
        assert_eq!(back.phase(), Phase::Ruling);
        assert_eq!(
            back.verdict().map(|v| v.decision),
            Some(Decision::Fail),
            "the verdict is read from the verdict round's output"
        );
    }

    #[test]
    fn evidence_keys_name_their_verifier_round_and_id() {
        let k = EvidenceKey::reported(&p("codex-2"), 3, "e7");
        assert_eq!(k.as_str(), "codex-2/r3/e7");
        assert_eq!(k.split(), Some((p("codex-2"), 3, "e7")));
        assert!(!k.is_user());
        let user = EvidenceKey::user(4);
        assert_eq!(user.as_str(), "user/4");
        assert!(user.is_user());
        assert_eq!(user.split(), None);
        assert_eq!(key("claude/x/e1").split(), None);
    }

    #[test]
    fn criterion_sources_and_subjects_read_as_agents_write_them() {
        for text in ["user", "request", "inferred", "spec:12"] {
            assert_eq!(CriterionSource::parse(text).unwrap().to_string(), text);
        }
        for bad in ["spec:", "spec:0", "spec:+3", "spec:x", "User"] {
            assert_eq!(CriterionSource::parse(bad), None, "{bad}");
        }
        assert_eq!(
            Subject::parse("codex/AC-2"),
            Some(Subject::Result {
                part: p("codex"),
                criterion: "AC-2".into()
            })
        );
        assert_eq!(
            Subject::parse("D3"),
            Some(Subject::Defect { id: "D3".into() })
        );
        for bad in ["", "/AC-1", "codex/", "a/b/c"] {
            assert_eq!(Subject::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_session_phase_comes_before_round_one_from_the_criteria() {
        let mut session = minimal_session();
        assert_eq!(session.phase(), Phase::Criteria);
        session.criteria_accepted = true;
        assert_eq!(session.phase(), Phase::Confirming);
        session.frozen_at = Some(1);
        assert_eq!(session.phase(), Phase::Briefing);
        session.closed = true;
        assert_eq!(session.phase(), Phase::Closed);
    }

    fn minimal_session() -> Session {
        Session {
            id: SessionId::at(1),
            request: String::new(),
            target: Target {
                kind: TargetKind::State,
                repo: PathBuf::from("/repo"),
                base: None,
                head: Head::Worktree,
                paths: Vec::new(),
                spec: None,
            },
            manifest: TargetManifest::default(),
            rebuild: Rebuild::default(),
            isolation: Isolation::Shared,
            spec: None,
            user_criteria: Vec::new(),
            workdir: PathBuf::from("/repo"),
            main_worktree: PathBuf::from("/repo"),
            max_rounds: 1,
            rev: 1,
            created_at: 1,
            judge: participant(JUDGE_ID, AgentKind::Agy, None),
            verifiers: Vec::new(),
            confirm: true,
            criteria_brief: None,
            criteria_accepted: false,
            criteria_summary: String::new(),
            criteria: Vec::new(),
            criteria_edits: Vec::new(),
            removed_criteria: Vec::new(),
            frozen_at: None,
            worktrees: BTreeMap::new(),
            results: Vec::new(),
            evidence: Vec::new(),
            cross_checks: Vec::new(),
            raw_defects: Vec::new(),
            defects: Vec::new(),
            dismissed: Vec::new(),
            rulings: BTreeMap::new(),
            messages: Vec::new(),
            rounds: Vec::new(),
            closed: false,
            hold: false,
            verdict_round: None,
        }
    }
}
