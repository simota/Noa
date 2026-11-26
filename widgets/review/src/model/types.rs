//! The review's state. Everything here is the fold of the event log (see
//! `reduce`); nothing is mutated any other way.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Wall-clock time in milliseconds since the Unix epoch. Always injected by
/// the caller, so the model and the engine never read a clock themselves.
pub type Millis = u64;

/// A round's number, counted from 1.
pub type RoundNo = u32;

/// The default round limit: one blind round and two of cross-examination.
pub const DEFAULT_MAX_ROUNDS: RoundNo = 3;

/// The highest round limit the start view offers.
pub const MAX_MAX_ROUNDS: RoundNo = 10;

/// The fewest reviewers one review takes.
pub const MIN_PANEL: usize = 2;

/// The most reviewers one review takes: with the lead, seven panes in a
/// grid of at most three columns of three in one Noa tab.
pub const MAX_PANEL: usize = 6;

/// The id of the lead, who consolidates each round and gives the verdict.
pub const LEAD_ID: &str = "lead";

/// The longest custom lens text.
pub const MAX_CUSTOM_LENS: usize = 500;

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
    /// Names one review and its directory on disk.
    SessionId
);
string_id!(
    /// A participant, as agents and the lead's JSON name it: `lead` for the
    /// lead, the agent kind (`claude`, `codex`, `agy`) for a reviewer, with
    /// `-2`, `-3`, … when a kind sits on the panel twice.
    PartId
);
string_id!(
    /// A timeline entry: `m1`, `m2`, … in posting order.
    MessageId
);

impl SessionId {
    /// The id a session created at `now` gets.
    pub fn at(now: Millis) -> Self {
        Self(format!("rev-{now}"))
    }
}

impl MessageId {
    /// The id of the `seq`-th message (counted from 1).
    pub fn nth(seq: usize) -> Self {
        Self(format!("m{seq}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentChoice {
    pub kind: AgentKind,
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

/// One of the angles a reviewer can be asked to review from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinLens {
    Correctness,
    Security,
    Performance,
    Design,
    Tests,
    UxDocs,
}

impl BuiltinLens {
    pub const ALL: [BuiltinLens; 6] = [
        BuiltinLens::Correctness,
        BuiltinLens::Security,
        BuiltinLens::Performance,
        BuiltinLens::Design,
        BuiltinLens::Tests,
        BuiltinLens::UxDocs,
    ];

    /// The id agents write as a finding's `category`.
    pub fn id(self) -> &'static str {
        match self {
            BuiltinLens::Correctness => "correctness",
            BuiltinLens::Security => "security",
            BuiltinLens::Performance => "performance",
            BuiltinLens::Design => "design",
            BuiltinLens::Tests => "tests",
            BuiltinLens::UxDocs => "ux_docs",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|lens| lens.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            BuiltinLens::Correctness => "正確性",
            BuiltinLens::Security => "セキュリティ",
            BuiltinLens::Performance => "性能",
            BuiltinLens::Design => "設計・アーキテクチャ",
            BuiltinLens::Tests => "テスト",
            BuiltinLens::UxDocs => "UX・ドキュメント",
        }
    }

    /// What a reviewer on this lens is told to look for.
    pub fn checklist(self) -> &'static str {
        match self {
            BuiltinLens::Correctness => {
                "Logic errors, unhandled edge cases (empty, zero, maximum, Unicode, concurrent \
                 access), error handling that loses or hides failures, data loss, contract \
                 violations between callers and callees, and regressions of existing behaviour."
            }
            BuiltinLens::Security => {
                "Validation of input at system boundaries, injection (shell, SQL, path, \
                 template), authentication and authorization, secrets in code or logs, path \
                 traversal, unsafe deserialization, privilege and sandbox escapes, and risky \
                 dependencies."
            }
            BuiltinLens::Performance => {
                "Algorithmic complexity, allocations and copies on hot paths, blocking I/O \
                 where latency matters, unbounded growth of memory, queues or logs, missing or \
                 wrong caching, and startup cost."
            }
            BuiltinLens::Design => {
                "Module boundaries and layering (including the rules in the repository's \
                 AGENTS.md or CLAUDE.md), coupling, duplication, the shape of APIs and data, \
                 naming, and more code than the problem needs."
            }
            BuiltinLens::Tests => {
                "Changed behaviour that no test covers, assertions too weak to catch a \
                 regression, flaky or order-dependent tests, untested error paths, and hacks \
                 that exist only to make a test pass."
            }
            BuiltinLens::UxDocs => {
                "User-visible text, error messages and their actionability, accessibility, \
                 documentation, changelog and feature lists that drift from the code, and \
                 comments that no longer match what the code does."
            }
        }
    }
}

/// The lens a reviewer sits on: a built-in one, or text the user wrote.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "lens", rename_all = "snake_case")]
pub enum Lens {
    Builtin { id: BuiltinLens },
    Custom { id: String, text: String },
}

impl Lens {
    /// The id findings use as their `category`.
    pub fn id(&self) -> &str {
        match self {
            Lens::Builtin { id } => id.id(),
            Lens::Custom { id, .. } => id,
        }
    }

    /// The short label the UI shows.
    pub fn label(&self) -> String {
        match self {
            Lens::Builtin { id } => id.label().to_owned(),
            Lens::Custom { text, .. } => custom_label(text),
        }
    }
}

/// The first 16 characters of a custom lens's text, on one line.
pub fn custom_label(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut label: String = line.chars().take(16).collect();
    if line.chars().count() > 16 {
        label.push('…');
    }
    label
}

/// What a reviewer row in the start view picks as its lens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LensChoice {
    Builtin(BuiltinLens),
    Custom(String),
}

/// One reviewer row of the start view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerChoice {
    pub agent: AgentChoice,
    pub lens: LensChoice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Lead,
    Reviewer,
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
    /// A reviewer's lens; `None` for the lead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens: Option<Lens>,
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
    Git,
    Files,
    Document,
}

impl TargetKind {
    pub const ALL: [TargetKind; 3] = [TargetKind::Git, TargetKind::Files, TargetKind::Document];

    pub fn label(self) -> &'static str {
        match self {
            TargetKind::Git => "Git 差分",
            TargetKind::Files => "ファイル / フォルダ",
            TargetKind::Document => "文書・計画",
        }
    }
}

/// The new side of a git target.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Head {
    /// `HEAD` with every uncommitted and untracked change.
    #[default]
    Worktree,
    Ref(String),
}

/// What the user asked to review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub kind: TargetKind,
    /// The repository (git), the root (files) or the document file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// The base ref of a git target; `None` picks the default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default)]
    pub head: Head,
    /// The pathspec filter (git) or the selected paths (files).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// A document typed into the start view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The working directory a document's reviewers get as context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<PathBuf>,
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
    /// A file of a files or document target: there is no old side.
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
    /// The new side's changed line ranges, inclusive (git targets).
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

/// What was captured: every agent reviews exactly this.
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
    /// The original file name of a document target read from a file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_name: Option<String>,
    #[serde(default)]
    pub files: Vec<TargetFile>,
    #[serde(default)]
    pub omitted: Vec<Omitted>,
    #[serde(default)]
    pub diff_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_digest: Option<u64>,
    /// A git work-tree target held changes not in `head_sha` when it was
    /// taken (read with the snapshot, not afterwards).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub uncommitted: bool,
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

// -------------------------------------------------------------- findings

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

/// Where a finding points.
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

/// A raw finding's key: its reviewer and the id the reviewer gave it.
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

/// One finding as a reviewer wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawFinding {
    pub key: RawKey,
    pub round: RoundNo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    /// The location names a captured file (or there is none).
    pub in_target: bool,
    /// For git targets: whether the lines overlap a changed range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_diff: Option<bool>,
    pub severity: Severity,
    pub category: String,
    pub confidence: Confidence,
    pub claim: String,
    pub evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StanceKind {
    Confirm,
    Dispute,
    Refine,
    Unsure,
    Withdraw,
}

impl StanceKind {
    pub const ALL: [StanceKind; 5] = [
        StanceKind::Confirm,
        StanceKind::Dispute,
        StanceKind::Refine,
        StanceKind::Unsure,
        StanceKind::Withdraw,
    ];

    pub fn name(self) -> &'static str {
        match self {
            StanceKind::Confirm => "confirm",
            StanceKind::Dispute => "dispute",
            StanceKind::Refine => "refine",
            StanceKind::Unsure => "unsure",
            StanceKind::Withdraw => "withdraw",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            StanceKind::Confirm => "支持",
            StanceKind::Dispute => "反論",
            StanceKind::Refine => "補足",
            StanceKind::Unsure => "不明",
            StanceKind::Withdraw => "取り下げ",
        }
    }
}

/// A reviewer's position on a finding in one round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stance {
    pub round: RoundNo,
    pub kind: StanceKind,
    #[serde(default)]
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingStatus {
    Open,
    Agreed,
    Disputed,
    Rejected,
    Withdrawn,
    Merged,
}

impl FindingStatus {
    pub const ALL: [FindingStatus; 6] = [
        FindingStatus::Open,
        FindingStatus::Agreed,
        FindingStatus::Disputed,
        FindingStatus::Rejected,
        FindingStatus::Withdrawn,
        FindingStatus::Merged,
    ];

    pub fn name(self) -> &'static str {
        match self {
            FindingStatus::Open => "open",
            FindingStatus::Agreed => "agreed",
            FindingStatus::Disputed => "disputed",
            FindingStatus::Rejected => "rejected",
            FindingStatus::Withdrawn => "withdrawn",
            FindingStatus::Merged => "merged",
        }
    }

    /// The statuses the lead may set.
    pub fn parse_lead(name: &str) -> Option<Self> {
        [
            FindingStatus::Open,
            FindingStatus::Agreed,
            FindingStatus::Disputed,
            FindingStatus::Rejected,
        ]
        .into_iter()
        .find(|s| s.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            FindingStatus::Open => "未検証",
            FindingStatus::Agreed => "合意",
            FindingStatus::Disputed => "対立",
            FindingStatus::Rejected => "却下",
            FindingStatus::Withdrawn => "取り下げ",
            FindingStatus::Merged => "統合",
        }
    }

    /// Still under review: open, agreed or disputed.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            FindingStatus::Open | FindingStatus::Agreed | FindingStatus::Disputed
        )
    }

    /// The order findings of equal severity are ranked in.
    fn rank(self) -> u8 {
        match self {
            FindingStatus::Agreed => 0,
            FindingStatus::Disputed => 1,
            FindingStatus::Open => 2,
            FindingStatus::Rejected => 3,
            FindingStatus::Withdrawn => 4,
            FindingStatus::Merged => 5,
        }
    }
}

/// A consolidated finding the lead keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub claim: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    pub in_target: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_diff: Option<bool>,
    pub severity: Severity,
    pub category: String,
    pub confidence: Confidence,
    pub status: FindingStatus,
    /// The finding this one was merged into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_into: Option<String>,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    pub sources: Vec<RawKey>,
    pub opened_in: RoundNo,
    pub changed_rev: u64,
    /// Every stance per reviewer, oldest first.
    #[serde(default)]
    pub stances: BTreeMap<PartId, Vec<Stance>>,
    /// Status and severity after each round that changed them.
    #[serde(default)]
    pub history: Vec<(RoundNo, FindingStatus, Severity)>,
}

impl Finding {
    /// The reviewers who raised it.
    pub fn origins(&self) -> BTreeSet<&PartId> {
        self.sources.iter().map(|key| &key.part).collect()
    }

    pub fn latest_stance(&self, part: &PartId) -> Option<&Stance> {
        self.stances.get(part).and_then(|all| all.last())
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

/// A raw finding the lead dismissed instead of keeping it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dismissal {
    pub raw: RawKey,
    pub round: RoundNo,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    ApproveWithFixes,
    RequestChanges,
    Reject,
}

impl Decision {
    pub const ALL: [Decision; 4] = [
        Decision::Approve,
        Decision::ApproveWithFixes,
        Decision::RequestChanges,
        Decision::Reject,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Decision::Approve => "approve",
            Decision::ApproveWithFixes => "approve_with_fixes",
            Decision::RequestChanges => "request_changes",
            Decision::Reject => "reject",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Decision::Approve => "承認",
            Decision::ApproveWithFixes => "修正後承認",
            Decision::RequestChanges => "要修正",
            Decision::Reject => "差し戻し",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub decision: Decision,
    pub rationale: String,
    #[serde(default)]
    pub must_fix: Vec<String>,
}

/// One validated operation of the lead's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LeadOp {
    Add {
        id: String,
        /// Raw keys (`codex/3`) and finding ids.
        from: Vec<String>,
        claim: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        location: Option<Location>,
        severity: Severity,
        category: String,
        confidence: Confidence,
        status: FindingStatus,
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
        location: Option<Location>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        severity: Option<Severity>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        category: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confidence: Option<Confidence>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<FindingStatus>,
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

/// The lead's validated output for a round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadOutput {
    pub summary: String,
    #[serde(default)]
    pub ops: Vec<LeadOp>,
    #[serde(default)]
    pub questions: BTreeMap<PartId, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    #[serde(default)]
    pub close: bool,
}

/// The files a reviewer reported having reviewed or skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    #[serde(default)]
    pub reviewed: Vec<String>,
    /// `(path, reason)`.
    #[serde(default)]
    pub skipped: Vec<(String, String)>,
}

/// What one reviewer's accepted reply in a round held, for the cards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewNote {
    pub summary: String,
    pub findings: usize,
    pub stances: usize,
    pub at: Millis,
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
    /// Something the user said to the panel.
    Intervention,
    /// The lead's summary of a round.
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
    /// Reviewers work alone and see nothing of each other.
    Blind,
    /// Reviewers see every finding and take stances on them.
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
            RoundKind::Blind => "独立レビュー",
            RoundKind::Cross => "相互検証",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Briefs are built and being delivered to the reviewers.
    Briefing,
    /// Waiting for the reviewers' reply files.
    Reviewing,
    /// The lead has its brief; waiting for its JSON.
    Consolidating,
    Closed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// When the text last reached the agent's pane.
    pub sent: Option<Millis>,
    /// When the agent was first seen working after the send.
    pub started: Option<Millis>,
    /// When a valid reply file was taken in.
    pub replied: Option<Millis>,
    pub retries: u8,
    /// Why delivery or the reply gave up, shown to the user.
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
    /// The session `rev` whose state the brief describes.
    pub base_rev: u64,
    /// The exact text sent.
    pub body: String,
    pub reply_path: PathBuf,
    pub receipt: Receipt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Round {
    pub no: RoundNo,
    pub kind: RoundKind,
    pub phase: Phase,
    pub started_at: Millis,
    /// One per reviewer.
    pub briefs: Vec<Brief>,
    /// What each reviewer's accepted reply held.
    #[serde(default)]
    pub reviews: BTreeMap<PartId, ReviewNote>,
    /// The lead's brief, once the round reaches `Consolidating`.
    pub lead_brief: Option<Brief>,
    /// The lead's accepted output.
    pub outcome: Option<LeadOutput>,
    /// The bounded timeline post at session close, rebuilt from the event log.
    #[serde(skip)]
    pub(crate) verdict_post: Option<Box<noa_ipc::timeline::PostInput>>,
}

impl Round {
    /// The failed briefs a user retry sends again now.
    pub fn retryable(&self) -> Vec<PartId> {
        match self.phase {
            Phase::Briefing | Phase::Reviewing => self
                .briefs
                .iter()
                .filter(|b| b.receipt.failed.is_some() && b.receipt.replied.is_none())
                .map(|b| b.to.clone())
                .collect(),
            Phase::Consolidating => self
                .lead_brief
                .iter()
                .filter(|b| b.receipt.failed.is_some())
                .map(|b| b.to.clone())
                .collect(),
            Phase::Closed => Vec::new(),
        }
    }

    pub fn brief(&self, to: &PartId) -> Option<&Brief> {
        self.briefs
            .iter()
            .chain(self.lead_brief.as_ref())
            .find(|brief| &brief.to == to)
    }

    pub fn brief_mut(&mut self, to: &PartId) -> Option<&mut Brief> {
        self.briefs
            .iter_mut()
            .chain(self.lead_brief.as_mut())
            .find(|brief| &brief.to == to)
    }
}

/// How far the panel agrees on a finding (L2-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// Who supports, disputes, or has not taken a side on a finding.
#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto: Option<crate::auto_assign::AutoRecord>,
    pub id: SessionId,
    /// What the user told the reviewers about the target; may be empty.
    pub request: String,
    pub target: Target,
    pub manifest: TargetManifest,
    /// The directory agents run in; `None` runs them in the session's.
    pub workdir: Option<PathBuf>,
    pub max_rounds: RoundNo,
    /// Bumped once per applied event batch.
    pub rev: u64,
    pub created_at: Millis,
    pub lead: Participant,
    pub reviewers: Vec<Participant>,
    pub raw: Vec<RawFinding>,
    pub findings: Vec<Finding>,
    pub dismissed: Vec<Dismissal>,
    /// The files each reviewer reported reviewing, over all rounds.
    pub coverage: BTreeMap<PartId, Coverage>,
    pub messages: Vec<Message>,
    pub rounds: Vec<Round>,
    pub closed: bool,
    /// The user holds the review after each consolidated round.
    pub hold: bool,
    /// The round whose lead output gave the review's verdict; cleared when
    /// rounds are added, so an old verdict is never the current one.
    #[serde(default)]
    pub verdict_round: Option<RoundNo>,
}

impl Session {
    pub fn participant(&self, id: &PartId) -> Option<&Participant> {
        std::iter::once(&self.lead)
            .chain(&self.reviewers)
            .find(|part| &part.id == id)
    }

    pub fn is_reviewer(&self, id: &PartId) -> bool {
        self.reviewers.iter().any(|part| &part.id == id)
    }

    pub fn reviewer_ids(&self) -> impl Iterator<Item = &PartId> {
        self.reviewers.iter().map(|part| &part.id)
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

    pub fn next_message_id(&self, offset: usize) -> MessageId {
        MessageId::nth(self.messages.len() + 1 + offset)
    }

    /// Whether the review waits for the user to start its next round.
    pub fn waits_to_continue(&self) -> bool {
        !self.closed
            && self.hold
            && self
                .current_round()
                .is_some_and(|round| round.phase == Phase::Closed)
    }

    /// Whether the user may add rounds, or why not. A running review may
    /// get more rounds until its last round is being consolidated; an ended
    /// one only when it ended by running all its rounds.
    pub fn extendable(&self) -> Result<(), &'static str> {
        let at_limit = self.rounds.len() as RoundNo >= self.max_rounds;
        let phase = self.current_round().map(|round| round.phase);
        if !self.closed {
            return if at_limit && phase == Some(Phase::Consolidating) {
                Err("最終ラウンドの取りまとめ中です（総合判定の後に追加できます）")
            } else {
                Ok(())
            };
        }
        if at_limit && phase == Some(Phase::Closed) {
            Ok(())
        } else {
            Err("ラウンド上限の前に終了したレビューです")
        }
    }

    /// Whether `round` is the review's last under its current limit.
    pub fn is_last(&self, round: RoundNo) -> bool {
        round >= self.max_rounds
    }

    /// The review's verdict: the lead's, from the round that ended the
    /// review. `None` while rounds added after it run (or were never run).
    pub fn verdict(&self) -> Option<&Verdict> {
        self.round(self.verdict_round?)?
            .outcome
            .as_ref()?
            .verdict
            .as_ref()
    }

    /// Whether the user ended the review without a verdict.
    pub fn ended_by_user(&self) -> bool {
        self.closed && self.verdict().is_none()
    }

    pub fn finding(&self, id: &str) -> Option<&Finding> {
        self.findings.iter().find(|f| f.id == id)
    }

    pub fn finding_mut(&mut self, id: &str) -> Option<&mut Finding> {
        self.findings.iter_mut().find(|f| f.id == id)
    }

    pub fn raw_finding(&self, key: &RawKey) -> Option<&RawFinding> {
        self.raw.iter().find(|r| &r.key == key)
    }

    /// The finding (or dismissal) a raw finding went to, if any.
    /// The raw findings in no finding and not dismissed: those of a round
    /// the lead has not consolidated (yet, or ever, when the review ended
    /// first). Nothing a reviewer wrote is dropped without a trace.
    pub fn unplaced(&self) -> Vec<&RawFinding> {
        self.raw
            .iter()
            .filter(|r| self.placement(&r.key).is_none())
            .collect()
    }

    pub fn placement(&self, key: &RawKey) -> Option<&str> {
        self.findings
            .iter()
            .find(|f| f.sources.contains(key))
            .map(|f| f.id.as_str())
            .or_else(|| {
                self.dismissed
                    .iter()
                    .any(|d| &d.raw == key)
                    .then_some("dismissed")
            })
    }

    /// Every lens id a finding may use as its category, `other` included.
    pub fn categories(&self) -> Vec<String> {
        let mut ids: Vec<String> = BuiltinLens::ALL.iter().map(|l| l.id().to_owned()).collect();
        for part in &self.reviewers {
            if let Some(Lens::Custom { id, .. }) = &part.lens
                && !ids.contains(id)
            {
                ids.push(id.clone());
            }
        }
        ids.push("other".to_owned());
        ids
    }

    /// The label of a category: a lens label, or その他.
    pub fn category_label(&self, id: &str) -> String {
        if let Some(lens) = BuiltinLens::parse(id) {
            return lens.label().to_owned();
        }
        self.reviewers
            .iter()
            .filter_map(|p| p.lens.as_ref())
            .find(|lens| lens.id() == id)
            .map(|lens| lens.label())
            .unwrap_or_else(|| "その他".to_owned())
    }

    /// The lenses on the panel, in panel order, without repeats.
    pub fn panel_lenses(&self) -> Vec<&Lens> {
        let mut out: Vec<&Lens> = Vec::new();
        for lens in self.reviewers.iter().filter_map(|p| p.lens.as_ref()) {
            if !out.iter().any(|seen| seen.id() == lens.id()) {
                out.push(lens);
            }
        }
        out
    }

    /// Built-in lenses no reviewer holds.
    pub fn unheld_lenses(&self) -> Vec<BuiltinLens> {
        BuiltinLens::ALL
            .into_iter()
            .filter(|b| {
                !self
                    .reviewers
                    .iter()
                    .any(|p| p.lens.as_ref().is_some_and(|l| l.id() == b.id()))
            })
            .collect()
    }

    /// The findings `part` must take a stance on in its next reply: live,
    /// not raised by it, and without an earlier stance of its own.
    pub fn required_stances(&self, part: &PartId) -> Vec<&str> {
        self.findings
            .iter()
            .filter(|f| f.status.is_live())
            .filter(|f| !f.origins().contains(part))
            .filter(|f| !f.stances.contains_key(part))
            .map(|f| f.id.as_str())
            .collect()
    }

    /// The reviewers counted for agreement: those not failed in the latest
    /// round that briefed them.
    fn counted_reviewers(&self) -> Vec<&PartId> {
        let latest = self.rounds.iter().rev().find(|r| !r.briefs.is_empty());
        self.reviewer_ids()
            .filter(|part| {
                latest
                    .and_then(|r| r.brief(part))
                    .is_none_or(|b| b.receipt.failed.is_none() || b.receipt.replied.is_some())
            })
            .collect()
    }

    /// Who supports and disputes `finding` (L2-4).
    pub fn tally(&self, finding: &Finding) -> Tally {
        let origins = finding.origins();
        let mut supporters = Vec::new();
        let mut disputers = Vec::new();
        let mut silent = Vec::new();
        // A reviewer failed in the latest round is not counted (L2-4), even
        // one that opened the finding or took a stance on it earlier; the
        // thread keeps what it said.
        for part in self.counted_reviewers() {
            let stance = finding.latest_stance(part).map(|s| s.kind);
            let origin = origins.contains(part);
            match stance {
                Some(StanceKind::Withdraw) => silent.push(part.clone()),
                Some(StanceKind::Dispute) => disputers.push(part.clone()),
                Some(StanceKind::Confirm | StanceKind::Refine) => supporters.push(part.clone()),
                _ if origin => supporters.push(part.clone()),
                _ => silent.push(part.clone()),
            }
        }
        let agreement = if !disputers.is_empty() && disputers.len() >= supporters.len() {
            Agreement::Split
        } else if disputers.is_empty() && silent.is_empty() && supporters.len() > 1 {
            Agreement::Unanimous
        } else if disputers.is_empty() && supporters.len() <= 1 {
            Agreement::Single
        } else {
            Agreement::Majority
        };
        Tally {
            supporters,
            disputers,
            silent,
            agreement,
        }
    }

    /// Findings ranked (L2-4): severity, status, confidence, supporters,
    /// opening round, then their id (F2 before F10).
    pub fn ranked(&self) -> Vec<&Finding> {
        let mut out: Vec<(&Finding, usize)> = self
            .findings
            .iter()
            .map(|f| (f, self.tally(f).supporters.len()))
            .collect();
        out.sort_by(|(a, sa), (b, sb)| {
            a.severity
                .cmp(&b.severity)
                .then(a.status.rank().cmp(&b.status.rank()))
                .then(a.confidence.cmp(&b.confidence))
                .then(sb.cmp(sa))
                .then(a.opened_in.cmp(&b.opened_in))
                .then_with(|| natural_cmp(&a.id, &b.id))
        });
        out.into_iter().map(|(f, _)| f).collect()
    }

    /// The next free `F<n>` id.
    pub fn next_finding_id(&self) -> String {
        let mut n = self.findings.len() + 1;
        while self.finding(&format!("F{n}")).is_some() {
            n += 1;
        }
        format!("F{n}")
    }

    /// The question the lead asked `part` in the round before `round`.
    pub fn question_for(&self, part: &PartId, round: RoundNo) -> Option<&str> {
        let prev = self.round(round.checked_sub(1)?)?;
        prev.outcome
            .as_ref()?
            .questions
            .get(part)
            .map(String::as_str)
    }

    /// The lead's summary of the latest consolidated round before `round`.
    pub fn summary_before(&self, round: RoundNo) -> Option<(RoundNo, &str)> {
        self.rounds
            .iter()
            .rev()
            .filter(|r| r.no < round)
            .find_map(|r| r.outcome.as_ref().map(|o| (r.no, o.summary.as_str())))
    }

    pub fn author_name(&self, author: &Author) -> String {
        match author {
            Author::User => "ユーザー".to_owned(),
            Author::Part(id) if id.as_str() == LEAD_ID => "リード".to_owned(),
            Author::Part(id) => id.to_string(),
        }
    }

    /// Live findings per severity, most severe first.
    pub fn severity_counts(&self) -> [usize; 5] {
        let mut counts = [0; 5];
        for f in self.findings.iter().filter(|f| f.status.is_live()) {
            counts[f.severity as usize] += 1;
        }
        counts
    }
}

/// Orders ids as people read them: runs of digits by value, so `F2` comes
/// before `F10`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_model_names_and_known_efforts_pass() {
        let setup = |model: &str, effort: Option<&str>| AgentSetup {
            model: Some(model.into()),
            effort: effort.map(Into::into),
        };
        assert!(
            setup("gpt-6.2-sol", Some("ultra"))
                .check(AgentKind::Codex)
                .is_ok()
        );
        for bad in ["a b", "-x", "=opus", "$(x)", "a;b", "a'b", "\"a\""] {
            assert!(setup(bad, None).check(AgentKind::Claude).is_err(), "{bad}");
        }
        assert!(
            setup("opus", Some("ultra"))
                .check(AgentKind::Claude)
                .is_err()
        );
    }

    #[test]
    fn locations_print_as_path_and_lines() {
        let loc = |line, end| Location {
            path: "src/a.rs".into(),
            line,
            end_line: end,
        };
        assert_eq!(loc(None, None).to_string(), "src/a.rs");
        assert_eq!(loc(Some(4), None).to_string(), "src/a.rs:4");
        assert_eq!(loc(Some(4), Some(9)).to_string(), "src/a.rs:4-9");
        assert_eq!(loc(Some(4), Some(4)).to_string(), "src/a.rs:4");
    }

    #[test]
    fn raw_keys_parse_from_reviewer_and_id() {
        assert_eq!(
            RawKey::parse("codex-2/7"),
            Some(RawKey {
                part: PartId::new("codex-2"),
                id: "7".into()
            })
        );
        assert_eq!(RawKey::parse("F1"), None);
        assert_eq!(RawKey::parse("/1"), None);
    }

    #[test]
    fn coverage_entries_match_files_directories_and_everything() {
        assert!(covers("*", "a/b.rs"));
        assert!(covers("a/", "a/b.rs"));
        assert!(covers("a/b.rs", "a/b.rs"));
        assert!(!covers("a", "a/b.rs"));
        assert!(!covers("b/", "a/b.rs"));
    }

    #[test]
    fn custom_lens_labels_keep_sixteen_characters_on_one_line() {
        assert_eq!(custom_label("short"), "short");
        assert_eq!(
            custom_label("国際化と\nローカライズの観点で全体を確認"),
            "国際化と ローカライズの観点で全…"
        );
    }
}
