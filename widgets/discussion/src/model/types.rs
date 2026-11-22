//! The discussion's state. Everything here is the fold of the event log
//! (see `reduce`); nothing is mutated any other way.

use std::collections::BTreeMap;
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
        fn visit<'a>(
            tree: &'a IssueTree,
            issue: &'a Issue,
            depth: usize,
            out: &mut Vec<(usize, &'a Issue)>,
        ) {
            out.push((depth, issue));
            for child in tree.children(&issue.id) {
                visit(tree, child, depth + 1, out);
            }
        }
        let mut out = Vec::with_capacity(self.issues.len());
        for root in self.roots() {
            visit(self, root, 0, &mut out);
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
    /// Only in a debate round; required in its last exchange.
    pub verdict: Option<Verdict>,
    pub close: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub topic: String,
    /// The directory (or repository) the agents run in, when the user named
    /// one; `None` runs them in the discussions directory.
    pub workdir: Option<std::path::PathBuf>,
    /// The most rounds the discussion runs; after the last one it closes.
    /// `None`: no limit (logs written before limits existed).
    pub max_rounds: Option<RoundNo>,
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
}

impl Session {
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
        self.rounds.iter().find(|round| round.no == no)
    }

    pub fn round_mut(&mut self, no: RoundNo) -> Option<&mut Round> {
        self.rounds.iter_mut().find(|round| round.no == no)
    }

    pub fn debate(&self, id: &DebateId) -> Option<&Debate> {
        self.debates.iter().find(|debate| &debate.id == id)
    }

    pub fn debate_mut(&mut self, id: &DebateId) -> Option<&mut Debate> {
        self.debates.iter_mut().find(|debate| &debate.id == id)
    }

    pub fn message(&self, id: &MessageId) -> Option<&Message> {
        self.messages.iter().find(|message| &message.id == id)
    }

    /// The position of `id` in the log.
    pub fn message_index(&self, id: &MessageId) -> Option<usize> {
        self.messages.iter().position(|message| &message.id == id)
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
