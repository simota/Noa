//! The discussion's state. Everything here is the fold of the event log
//! (see `reduce`); nothing is mutated any other way.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Wall-clock time in milliseconds since the Unix epoch. Always injected by
/// the caller, so the model and the engine never read a clock themselves.
pub type Millis = u64;

/// A round's number, counted from 1.
pub type RoundNo = u32;

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
    /// Names one discussion and its directory on disk.
    SessionId
);
string_id!(
    /// A participant, as agents and the moderator JSON name it: `noa` for
    /// the moderator, the agent kind (`claude`, `codex`, `agy`) for a
    /// panelist, with `-2`, `-3`, … when a kind sits on the panel twice.
    PartId
);
string_id!(
    /// An issue in the tree, such as `A` or `B2`, chosen by the moderator.
    IssueId
);
string_id!(
    /// A chat message: `m1`, `m2`, … in posting order.
    MessageId
);
string_id!(
    /// A debate: `d1`, `d2`, … in proposal order.
    DebateId
);

impl SessionId {
    /// The id a session created at `now` gets.
    pub fn at(now: Millis) -> Self {
        Self(format!("disc-{now}"))
    }
}

impl MessageId {
    /// The id of the `seq`-th message (counted from 1).
    pub fn nth(seq: usize) -> Self {
        Self(format!("m{seq}"))
    }
}

impl DebateId {
    pub fn nth(seq: usize) -> Self {
        Self(format!("d{seq}"))
    }
}

/// The most panelists one discussion takes in the start view.
pub const MAX_PANEL: usize = 6;

/// The id of the moderator ("Noa").
pub const MODERATOR_ID: &str = "noa";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Moderator,
    Panelist,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueState {
    Open,
    Contested,
    Agreed,
    Conditional,
    Deferred,
}

impl IssueState {
    pub fn name(self) -> &'static str {
        match self {
            IssueState::Open => "open",
            IssueState::Contested => "contested",
            IssueState::Agreed => "agreed",
            IssueState::Conditional => "conditional",
            IssueState::Deferred => "deferred",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "open" => IssueState::Open,
            "contested" => IssueState::Contested,
            "agreed" => IssueState::Agreed,
            "conditional" => IssueState::Conditional,
            "deferred" => IssueState::Deferred,
            _ => return None,
        })
    }

    /// Whether a debate verdict may end in this state.
    pub fn is_verdict(self) -> bool {
        matches!(
            self,
            IssueState::Agreed | IssueState::Conditional | IssueState::Deferred
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    For,
    Against,
    Neutral,
    Unknown,
}

impl Position {
    pub fn name(self) -> &'static str {
        match self {
            Position::For => "for",
            Position::Against => "against",
            Position::Neutral => "neutral",
            Position::Unknown => "unknown",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "for" => Position::For,
            "against" => Position::Against,
            "neutral" => Position::Neutral,
            "unknown" => Position::Unknown,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stance {
    pub pos: Position,
    /// The message the stance was taken in.
    pub basis: MessageId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub id: IssueId,
    pub parent: Option<IssueId>,
    pub title: String,
    pub state: IssueState,
    pub stances: BTreeMap<PartId, Stance>,
    pub opened_in: RoundNo,
    /// The session `rev` the issue was added in.
    pub added_rev: u64,
    /// The session `rev` the issue last changed in (state or a stance).
    pub changed_rev: u64,
    /// Each state the issue took and the round it took it in, oldest first:
    /// how the discussion moved on it. Rebuilt from the log on replay.
    #[serde(default)]
    pub history: Vec<(RoundNo, IssueState)>,
}

impl Issue {
    /// Records `state` as of `round`, unless the issue is already in it.
    pub fn record(&mut self, round: RoundNo, state: IssueState) {
        if self.history.last().map(|(_, s)| *s) != Some(state) {
            self.history.push((round, state));
        }
        self.state = state;
    }
}

/// The issues in the order they were added; a child always comes after its
/// parent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueTree {
    pub issues: Vec<Issue>,
}

impl IssueTree {
    pub fn get(&self, id: &IssueId) -> Option<&Issue> {
        self.issues.iter().find(|issue| &issue.id == id)
    }

    pub fn get_mut(&mut self, id: &IssueId) -> Option<&mut Issue> {
        self.issues.iter_mut().find(|issue| &issue.id == id)
    }

    pub fn contains(&self, id: &IssueId) -> bool {
        self.get(id).is_some()
    }

    pub fn contains_str(&self, id: &str) -> bool {
        self.issues.iter().any(|issue| issue.id.as_str() == id)
    }

    pub fn roots(&self) -> impl Iterator<Item = &Issue> {
        self.issues.iter().filter(|issue| issue.parent.is_none())
    }

    pub fn children<'a>(&'a self, parent: &'a IssueId) -> impl Iterator<Item = &'a Issue> {
        self.issues
            .iter()
            .filter(move |issue| issue.parent.as_ref() == Some(parent))
    }

    /// Every issue depth-first with its depth (0 for a root), the order the
    /// tree is drawn and written into briefs in.
    pub fn walk(&self) -> Vec<(usize, &Issue)> {
        let n = self.issues.len();
        if n == 0 {
            return Vec::new();
        }
        let mut by_id: HashMap<&str, usize> = HashMap::with_capacity(n);
        for (idx, issue) in self.issues.iter().enumerate() {
            by_id.insert(issue.id.as_str(), idx);
        }
        let mut roots = Vec::new();
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (idx, issue) in self.issues.iter().enumerate() {
            match &issue.parent {
                None => roots.push(idx),
                Some(parent) => {
                    if let Some(&pidx) = by_id.get(parent.as_str()) {
                        children[pidx].push(idx);
                    }
                }
            }
        }
        fn visit<'a>(
            issues: &'a [Issue],
            children: &[Vec<usize>],
            idx: usize,
            depth: usize,
            out: &mut Vec<(usize, &'a Issue)>,
        ) {
            out.push((depth, &issues[idx]));
            for &child in &children[idx] {
                visit(issues, children, child, depth + 1, out);
            }
        }
        let mut out = Vec::with_capacity(n);
        for root in roots {
            visit(&self.issues, &children, root, 0, &mut out);
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Author {
    User,
    Part(PartId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MsgKind {
    /// The moderator's question to one recipient, as put in that round's brief.
    Brief,
    /// A panelist's reply in an open round, or an observer's in a debate round.
    Reply,
    /// The moderator's summary of a round.
    Summary,
    /// Something the user said into the meeting.
    Intervention,
    /// A debate side's reply.
    Argument,
    /// The moderator's verdict that ends a debate.
    Verdict,
    /// The moderator's conclusion on the topic that ends the discussion.
    Conclusion,
}

impl MsgKind {
    pub fn name(self) -> &'static str {
        match self {
            MsgKind::Brief => "question",
            MsgKind::Reply => "reply",
            MsgKind::Summary => "summary",
            MsgKind::Intervention => "intervention",
            MsgKind::Argument => "argument",
            MsgKind::Verdict => "verdict",
            MsgKind::Conclusion => "conclusion",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub round: RoundNo,
    pub author: Author,
    pub kind: MsgKind,
    pub issues: Vec<IssueId>,
    pub reply_to: Vec<MessageId>,
    /// Who it is addressed to; empty means everyone.
    pub to: Vec<PartId>,
    pub debate: Option<DebateId>,
    pub body: String,
    pub at: Millis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MinuteSection {
    Decisions,
    Contested,
    OpenQuestions,
    Actions,
}

impl MinuteSection {
    pub const ALL: [MinuteSection; 4] = [
        MinuteSection::Decisions,
        MinuteSection::Contested,
        MinuteSection::OpenQuestions,
        MinuteSection::Actions,
    ];

    pub fn name(self) -> &'static str {
        match self {
            MinuteSection::Decisions => "decisions",
            MinuteSection::Contested => "contested",
            MinuteSection::OpenQuestions => "open_questions",
            MinuteSection::Actions => "actions",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        MinuteSection::ALL.into_iter().find(|s| s.name() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinuteItem {
    pub text: String,
    pub issues: Vec<IssueId>,
    pub since: RoundNo,
    /// The session `rev` the item was added in.
    pub rev: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Minutes {
    pub decisions: Vec<MinuteItem>,
    pub contested: Vec<MinuteItem>,
    pub open_questions: Vec<MinuteItem>,
    pub actions: Vec<MinuteItem>,
}

impl Minutes {
    pub fn section(&self, section: MinuteSection) -> &[MinuteItem] {
        match section {
            MinuteSection::Decisions => &self.decisions,
            MinuteSection::Contested => &self.contested,
            MinuteSection::OpenQuestions => &self.open_questions,
            MinuteSection::Actions => &self.actions,
        }
    }

    pub fn section_mut(&mut self, section: MinuteSection) -> &mut Vec<MinuteItem> {
        match section {
            MinuteSection::Decisions => &mut self.decisions,
            MinuteSection::Contested => &mut self.contested,
            MinuteSection::OpenQuestions => &mut self.open_questions,
            MinuteSection::Actions => &mut self.actions,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoundKind {
    Open,
    Debate(DebateId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Briefs are built and being delivered.
    Briefing,
    /// Every brief is delivered; waiting for agents to start and reply.
    Awaiting,
    /// The moderator has its brief; waiting for its JSON.
    Summarizing,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Brief {
    pub to: PartId,
    /// The session `rev` whose state the brief describes.
    pub base_rev: u64,
    /// The last message the recipient's previous brief carried: this brief
    /// carries every message after it (all of them when `None`).
    pub since: Option<MessageId>,
    /// The last message this brief carries.
    pub through: Option<MessageId>,
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
    /// One per panelist.
    pub briefs: Vec<Brief>,
    /// The moderator's summarize brief, once the round reaches `Summarizing`.
    pub moderator_brief: Option<Brief>,
    /// The moderator's accepted output for this round.
    pub outcome: Option<ModeratorOutput>,
}

impl Round {
    /// The failed briefs a user retry sends again now: panelists' while
    /// the round still waits for replies, the moderator's while it
    /// summarizes. Once the round moved on, a failed panelist stays failed.
    pub fn retryable(&self) -> Vec<PartId> {
        match self.phase {
            Phase::Briefing | Phase::Awaiting => self
                .briefs
                .iter()
                .filter(|b| b.receipt.failed.is_some() && b.receipt.replied.is_none())
                .map(|b| b.to.clone())
                .collect(),
            Phase::Summarizing => self
                .moderator_brief
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
            .chain(self.moderator_brief.as_ref())
            .find(|brief| &brief.to == to)
    }

    pub fn brief_mut(&mut self, to: &PartId) -> Option<&mut Brief> {
        self.briefs
            .iter_mut()
            .chain(self.moderator_brief.as_mut())
            .find(|brief| &brief.to == to)
    }

    pub fn debate(&self) -> Option<&DebateId> {
        match &self.kind {
            RoundKind::Open => None,
            RoundKind::Debate(id) => Some(id),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DebateStatus {
    /// Proposed by the moderator; waiting for the user.
    Proposed,
    /// Approved (or started by the user); runs after the current round.
    Approved,
    Active,
    Closed,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub outcome: IssueState,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Debate {
    pub id: DebateId,
    pub issue: IssueId,
    pub pro: Vec<PartId>,
    pub con: Vec<PartId>,
    pub max_exchanges: u8,
    pub status: DebateStatus,
    /// The sides' arguments, in order.
    pub turns: Vec<MessageId>,
    pub verdict: Option<Verdict>,
}

impl Debate {
    pub fn side(&self, part: &PartId) -> Option<Side> {
        if self.pro.contains(part) {
            Some(Side::Pro)
        } else if self.con.contains(part) {
            Some(Side::Con)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Pro,
    Con,
}

/// One validated operation on the issue tree, from the moderator's JSON or a
/// panelist's reply header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum IssueOp {
    Add {
        id: IssueId,
        parent: Option<IssueId>,
        title: String,
    },
    SetState {
        id: IssueId,
        state: IssueState,
    },
    SetStance {
        id: IssueId,
        part: PartId,
        pos: Position,
        basis: MessageId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinuteOp {
    pub section: MinuteSection,
    pub text: String,
    pub issues: Vec<IssueId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebateProposal {
    pub issue: IssueId,
    pub pro: Vec<PartId>,
    pub con: Vec<PartId>,
}

/// The moderator's output for a round, after validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeratorOutput {
    pub summary: String,
    pub issue_ops: Vec<IssueOp>,
    pub minutes_ops: Vec<MinuteOp>,
    pub questions: BTreeMap<PartId, String>,
    pub propose_debate: Option<DebateProposal>,
    /// Only in a debate round; required in its last exchange and in the
    /// discussion's last round.
    pub verdict: Option<Verdict>,
    /// The moderator's answer to the topic; required in the last round and
    /// whenever `close` is set.
    #[serde(default)]
    pub conclusion: Option<String>,
    /// The new round limit, when the moderator moved it because the
    /// discussion could not converge in the rounds left.
    #[serde(default)]
    pub round_limit: Option<RoundNo>,
    pub close: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto: Option<crate::auto_assign::AutoRecord>,
    pub id: SessionId,
    pub topic: String,
    /// The directory (or repository) the agents run in, when the user named
    /// one; `None` runs them in the discussions directory.
    pub workdir: Option<std::path::PathBuf>,
    /// The most rounds the discussion runs; after the last one it closes.
    /// `None`: no limit (logs written before limits existed).
    pub max_rounds: Option<RoundNo>,
    /// The limit the user set. The moderator may move `max_rounds`, but
    /// never past [`Session::round_limit_cap`].
    #[serde(default)]
    pub user_max_rounds: Option<RoundNo>,
    /// Bumped once per applied event batch.
    pub rev: u64,
    pub created_at: Millis,
    pub moderator: Participant,
    pub panelists: Vec<Participant>,
    pub issues: IssueTree,
    pub minutes: Minutes,
    pub messages: Vec<Message>,
    pub rounds: Vec<Round>,
    pub debates: Vec<Debate>,
    pub closed: bool,
    /// The user holds the discussion after each round: the next one starts
    /// only when the user continues. Absent in logs written before.
    #[serde(default)]
    pub hold: bool,
}

impl Session {
    /// Whether the discussion waits for the user to start its next round:
    /// held, with the last round over and no proposed debate (which waits
    /// for approval first).
    pub fn waits_to_continue(&self) -> bool {
        !self.closed
            && self.hold
            && self
                .current_round()
                .is_some_and(|round| round.phase == Phase::Closed)
            && self.pending_debate().is_none()
    }

    /// Whether the user may add rounds, or why not: the discussion must
    /// have a round limit, and once it has ended it must have ended by
    /// running all its rounds (not by an early conclusion or the user).
    /// While the moderator summarizes the last round it was told to
    /// conclude, so rounds are added once that conclusion is in.
    pub fn extendable(&self) -> Result<(), &'static str> {
        let Some(max) = self.max_rounds else {
            return Err("this discussion has no round limit");
        };
        if !self.closed {
            let concluding = self.rounds.len() as RoundNo >= max
                && self
                    .current_round()
                    .is_some_and(|round| round.phase == Phase::Summarizing);
            return if concluding {
                Err("the moderator is concluding the last round; add rounds once it has")
            } else {
                Ok(())
            };
        }
        let ran_out = self.rounds.len() as RoundNo >= max
            && self
                .current_round()
                .is_some_and(|round| round.phase == Phase::Closed);
        if ran_out {
            Ok(())
        } else {
            Err("the discussion ended before its round limit")
        }
    }

    /// The highest round limit the moderator may set: twice the user's.
    pub fn round_limit_cap(&self) -> Option<RoundNo> {
        self.user_max_rounds.map(|max| max.saturating_mul(2))
    }

    pub fn participant(&self, id: &PartId) -> Option<&Participant> {
        std::iter::once(&self.moderator)
            .chain(&self.panelists)
            .find(|part| &part.id == id)
    }

    pub fn is_panelist(&self, id: &PartId) -> bool {
        self.panelists.iter().any(|part| &part.id == id)
    }

    pub fn panelist_ids(&self) -> impl Iterator<Item = &PartId> {
        self.panelists.iter().map(|part| &part.id)
    }

    pub fn current_round(&self) -> Option<&Round> {
        self.rounds.last()
    }

    pub fn round(&self, no: RoundNo) -> Option<&Round> {
        if let Some(idx) = no.checked_sub(1)
            && let Some(round) = self.rounds.get(idx as usize)
            && round.no == no
        {
            return Some(round);
        }
        self.rounds.iter().find(|round| round.no == no)
    }

    pub fn round_mut(&mut self, no: RoundNo) -> Option<&mut Round> {
        if let Some(idx) = no.checked_sub(1)
            && self.rounds.get(idx as usize).is_some_and(|r| r.no == no)
        {
            return self.rounds.get_mut(idx as usize);
        }
        self.rounds.iter_mut().find(|round| round.no == no)
    }

    fn debate_index(&self, id: &DebateId) -> Option<usize> {
        if let Some(idx) = id
            .as_str()
            .strip_prefix('d')
            .and_then(|s| s.parse::<usize>().ok())
            .and_then(|seq| seq.checked_sub(1))
            && self.debates.get(idx).is_some_and(|d| &d.id == id)
        {
            return Some(idx);
        }
        self.debates.iter().position(|debate| &debate.id == id)
    }

    pub fn debate(&self, id: &DebateId) -> Option<&Debate> {
        self.debate_index(id).map(|i| &self.debates[i])
    }

    pub fn debate_mut(&mut self, id: &DebateId) -> Option<&mut Debate> {
        self.debate_index(id).map(|i| &mut self.debates[i])
    }

    pub fn message_by_str(&self, id: &str) -> Option<&Message> {
        self.message_index_by_str(id).map(|i| &self.messages[i])
    }

    pub fn message(&self, id: &MessageId) -> Option<&Message> {
        self.message_index(id).map(|i| &self.messages[i])
    }

    fn message_index_by_str(&self, id: &str) -> Option<usize> {
        if let Some(idx) = id
            .strip_prefix('m')
            .and_then(|s| s.parse::<usize>().ok())
            .and_then(|seq| seq.checked_sub(1))
            && self.messages.get(idx).is_some_and(|m| m.id.as_str() == id)
        {
            return Some(idx);
        }
        self.messages
            .iter()
            .position(|message| message.id.as_str() == id)
    }

    /// The position of `id` in the log.
    pub fn message_index(&self, id: &MessageId) -> Option<usize> {
        self.message_index_by_str(id.as_str())
    }

    /// Every message after `since` (all of them when `None`).
    pub fn messages_after(&self, since: Option<&MessageId>) -> &[Message] {
        let start = since
            .and_then(|id| self.message_index(id))
            .map_or(0, |index| index + 1);
        &self.messages[start..]
    }

    /// The id the next posted message gets; `offset` skips that many more,
    /// for posting several in one batch.
    pub fn next_message_id(&self, offset: usize) -> MessageId {
        MessageId::nth(self.messages.len() + 1 + offset)
    }

    /// The most recent brief to `to` in a round before `before`.
    pub fn previous_brief(&self, to: &PartId, before: RoundNo) -> Option<&Brief> {
        self.rounds
            .iter()
            .rev()
            .filter(|round| round.no < before)
            .filter_map(|round| round.brief(to))
            // A brief that never went out told its agent nothing: the next
            // one starts where the last one that was sent ended.
            .find(|brief| brief.receipt.sent.is_some())
    }

    /// The display name of a message's author.
    pub fn author_name(&self, author: &Author) -> String {
        match author {
            Author::User => "user".to_owned(),
            Author::Part(id) if id.as_str() == MODERATOR_ID => "Noa (moderator)".to_owned(),
            Author::Part(id) => id.to_string(),
        }
    }

    /// The debate that is proposed and waits for the user, if any.
    pub fn pending_debate(&self) -> Option<&Debate> {
        self.debates
            .iter()
            .find(|debate| debate.status == DebateStatus::Proposed)
    }
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
        assert!(
            setup("claude-opus-5-5", None)
                .check(AgentKind::Claude)
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
    fn a_participant_stored_before_model_and_effort_reads_with_the_defaults() {
        let old = r#"{"id":"claude","kind":"claude","role":"panelist","pane_id":7}"#;
        let part: Participant = serde_json::from_str(old).unwrap();
        assert!(part.setup.is_default());
        assert_eq!(part.pane_id, Some(7));
        let json = serde_json::to_string(&part).unwrap();
        assert!(!json.contains("setup"), "{json}");
        let part = Participant {
            setup: AgentSetup {
                model: Some("opus".into()),
                effort: None,
            },
            ..part
        };
        let back: Participant =
            serde_json::from_str(&serde_json::to_string(&part).unwrap()).unwrap();
        assert_eq!(back, part);
    }

    #[test]
    fn issue_tree_walk_and_session_lookups_handle_sequential_and_nonsequential_ids() {
        let mut tree = IssueTree::default();
        for (id, parent) in [
            ("A", None),
            ("B", None),
            ("A1", Some("A")),
            ("A2", Some("A")),
            ("A1a", Some("A1")),
            ("B1", Some("B")),
        ] {
            tree.issues.push(Issue {
                id: IssueId::new(id),
                parent: parent.map(IssueId::new),
                title: id.into(),
                state: IssueState::Open,
                stances: Default::default(),
                opened_in: 1,
                added_rev: 1,
                changed_rev: 1,
                history: vec![(1, IssueState::Open)],
            });
        }
        let walked: Vec<(usize, &str)> = tree
            .walk()
            .into_iter()
            .map(|(d, i)| (d, i.id.as_str()))
            .collect();
        assert_eq!(
            walked,
            vec![
                (0, "A"),
                (1, "A1"),
                (2, "A1a"),
                (1, "A2"),
                (0, "B"),
                (1, "B1"),
            ]
        );

        let p = |id: &str, role| Participant {
            id: PartId::new(id),
            kind: AgentKind::Claude,
            role,
            pane_id: None,
            setup: Default::default(),
        };
        let msg = |id: &str| Message {
            id: MessageId::new(id),
            round: 1,
            author: Author::User,
            kind: MsgKind::Intervention,
            issues: vec![],
            reply_to: vec![],
            to: vec![],
            debate: None,
            body: id.into(),
            at: 0,
        };
        let round = |no: RoundNo| Round {
            no,
            kind: RoundKind::Open,
            phase: Phase::Closed,
            started_at: 0,
            briefs: vec![],
            moderator_brief: None,
            outcome: None,
        };
        let debate = |id: &str| Debate {
            id: DebateId::new(id),
            issue: IssueId::new("A"),
            pro: vec![],
            con: vec![],
            max_exchanges: 2,
            status: DebateStatus::Closed,
            turns: vec![],
            verdict: None,
        };
        let mut session = Session {
            auto: None,
            id: SessionId::new("s"),
            topic: "t".into(),
            workdir: None,
            max_rounds: None,
            user_max_rounds: None,
            rev: 1,
            created_at: 0,
            moderator: p("noa", Role::Moderator),
            panelists: vec![p("claude", Role::Panelist)],
            issues: tree,
            minutes: Default::default(),
            messages: vec![msg("m1"), msg("m2"), msg("custom")],
            rounds: vec![round(1), round(2), round(5)],
            debates: vec![debate("d1"), debate("d9")],
            closed: false,
            hold: false,
        };
        assert_eq!(session.message_index(&MessageId::new("m1")), Some(0));
        assert_eq!(session.message_index(&MessageId::new("m2")), Some(1));
        assert_eq!(session.message_index(&MessageId::new("custom")), Some(2));
        assert_eq!(session.message_index(&MessageId::new("m99")), None);
        assert_eq!(session.round(1).map(|r| r.no), Some(1));
        assert_eq!(session.round(5).map(|r| r.no), Some(5));
        assert_eq!(session.round(3), None);
        assert_eq!(session.round_mut(5).map(|r| r.no), Some(5));
        assert_eq!(
            session.debate(&DebateId::new("d1")).map(|d| d.id.as_str()),
            Some("d1")
        );
        assert_eq!(
            session.debate(&DebateId::new("d9")).map(|d| d.id.as_str()),
            Some("d9")
        );
        assert_eq!(session.debate(&DebateId::new("d2")), None);
    }
}
