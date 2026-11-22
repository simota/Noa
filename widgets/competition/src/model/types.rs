//! The competition's state. Everything here is the fold of the event log
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

/// Default interval between peer-reveal rounds: every 5 completed rounds,
/// contestants can inspect each other's work before starting the next round.
pub const DEFAULT_REVEAL_INTERVAL: RoundNo = 5;

/// Default maximum rounds for a competition (allows two 5-round sprints).
pub const DEFAULT_MAX_ROUNDS: RoundNo = 10;

/// The id of the judge agent ("Judge").
pub const JUDGE_ID: &str = "judge";

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
    /// Names one competition and its directory on disk.
    SessionId
);
string_id!(
    /// A participant, as agents and the judge JSON name it: `judge` for the
    /// judge, the agent kind (`claude`, `codex`, `agy`) for a contestant,
    /// with `-2`, `-3`, … when a kind enters twice.
    PartId
);
string_id!(
    /// A timeline entry: `m1`, `m2`, … in posting order.
    MessageId
);

impl SessionId {
    /// The id a session created at `now` gets.
    pub fn at(now: Millis) -> Self {
        Self(format!("comp-{now}"))
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Judge,
    Contestant,
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
pub enum ArtifactKind {
    Html,
    Image,
    Svg,
    Code,
    Markdown,
    Other,
}

impl ArtifactKind {
    pub fn from_path(rel_path: &str) -> Self {
        let ext = rel_path
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "html" | "htm" => ArtifactKind::Html,
            "svg" => ArtifactKind::Svg,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "avif" => ArtifactKind::Image,
            "md" | "markdown" | "txt" => ArtifactKind::Markdown,
            "rs" | "ts" | "tsx" | "js" | "jsx" | "css" | "scss" | "json" | "toml" | "yaml"
            | "yml" | "py" | "go" | "sh" | "sql" | "swift" | "c" | "h" | "cpp" => {
                ArtifactKind::Code
            }
            _ => ArtifactKind::Other,
        }
    }

    pub fn is_visual(self) -> bool {
        matches!(
            self,
            ArtifactKind::Html | ArtifactKind::Image | ArtifactKind::Svg
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            ArtifactKind::Html => "HTML",
            ArtifactKind::Image => "画像",
            ArtifactKind::Svg => "SVG",
            ArtifactKind::Code => "コード",
            ArtifactKind::Markdown => "文書",
            ArtifactKind::Other => "ファイル",
        }
    }
}

/// One file produced by a contestant inside their entry directory for a round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactFile {
    /// Relative path inside the contestant's entry directory (POSIX `/` separators).
    pub rel_path: String,
    pub kind: ArtifactKind,
    pub bytes: u64,
}

/// A contestant's submission for one round: their Markdown report plus the
/// snapshot of files found in their entry directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    pub part: PartId,
    pub summary: String,
    pub body: String,
    #[serde(default)]
    pub artifacts: Vec<ArtifactFile>,
    pub at: Millis,
}

/// The judge's evaluation of one contestant for a round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryEvaluation {
    /// Score from 0 to 100.
    pub score: u8,
    /// Brief strengths noted by the judge.
    #[serde(default)]
    pub strengths: String,
    /// Actionable improvement feedback sent privately to this contestant.
    pub feedback: String,
}

/// The judge's validated output for a round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeOutput {
    /// Overall round overview (visible in the UI and in reveal rounds).
    pub overview: String,
    /// Per-contestant score and private feedback.
    pub evaluations: BTreeMap<PartId, EntryEvaluation>,
    /// Contestant ids ranked best-first.
    pub ranking: Vec<PartId>,
    /// Winning contestant id; required on the final round or when `close` is true.
    #[serde(default)]
    pub winner: Option<PartId>,
    /// Final verdict explaining why the winner won; required with `winner`.
    #[serde(default)]
    pub final_verdict: Option<String>,
    /// Set true to end the competition early.
    #[serde(default)]
    pub close: bool,
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
    /// A contestant's round submission report.
    Submission,
    /// The judge's round evaluation overview.
    Judgment,
    /// User intervention (addressed to everyone or specific contestants).
    Intervention,
    /// Final winner announcement and verdict.
    FinalVerdict,
}

impl MsgKind {
    pub fn name(self) -> &'static str {
        match self {
            MsgKind::Submission => "submission",
            MsgKind::Judgment => "judgment",
            MsgKind::Intervention => "intervention",
            MsgKind::FinalVerdict => "final_verdict",
        }
    }
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

/// Whether contestants can see peer work from the prior round in this round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoundKind {
    /// Contestants only see their own previous work and private judge feedback.
    Blind,
    /// Peer-reveal round: contestants inspect peer submissions & artifacts from
    /// the completed round right before this one (every `reveal_interval` rounds).
    Reveal,
}

impl RoundKind {
    /// Whether `round` is a peer-reveal round under `reveal_interval`.
    /// With interval 5: rounds 1..=5 are Blind; round 6 reveals round 5;
    /// rounds 7..=10 are Blind; round 11 reveals round 10, etc.
    pub fn for_round(round: RoundNo, reveal_interval: RoundNo) -> Self {
        if reveal_interval > 0 && round > 1 && (round - 1).is_multiple_of(reveal_interval) {
            RoundKind::Reveal
        } else {
            RoundKind::Blind
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Briefs are built and being delivered to contestants.
    Briefing,
    /// Waiting for contestants to work and write their submission files.
    Competing,
    /// Judge has its brief; waiting for its JSON evaluation.
    Judging,
    /// Round complete.
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
    /// One per contestant.
    pub briefs: Vec<Brief>,
    /// Accepted submissions in this round.
    #[serde(default)]
    pub submissions: BTreeMap<PartId, Submission>,
    /// The judge's evaluation brief, once the round reaches `Judging`.
    pub judge_brief: Option<Brief>,
    /// The judge's accepted output for this round.
    pub outcome: Option<JudgeOutput>,
}

impl Round {
    pub fn retryable(&self) -> Vec<PartId> {
        match self.phase {
            Phase::Briefing | Phase::Competing => self
                .briefs
                .iter()
                .filter(|b| b.receipt.failed.is_some() && b.receipt.replied.is_none())
                .map(|b| b.to.clone())
                .collect(),
            Phase::Judging => self
                .judge_brief
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
            .chain(self.judge_brief.as_ref())
            .find(|brief| &brief.to == to)
    }

    pub fn brief_mut(&mut self, to: &PartId) -> Option<&mut Brief> {
        self.briefs
            .iter_mut()
            .chain(self.judge_brief.as_mut())
            .find(|brief| &brief.to == to)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    /// The user's request / prompt that contestants compete on.
    pub request: String,
    /// The directory (or repository) the agents read from, when named; `None`
    /// runs them in the competitions directory.
    pub workdir: Option<PathBuf>,
    /// Maximum rounds the competition runs; `None` for no limit.
    pub max_rounds: Option<RoundNo>,
    /// Peer-reveal interval in rounds (e.g., 5 means R6 reveals R5, R11 reveals R10).
    /// `0` disables peer reveals (strictly blind throughout).
    #[serde(default = "default_reveal_interval")]
    pub reveal_interval: RoundNo,
    /// Bumped once per applied event batch.
    pub rev: u64,
    pub created_at: Millis,
    pub judge: Participant,
    pub contestants: Vec<Participant>,
    pub messages: Vec<Message>,
    pub rounds: Vec<Round>,
    /// Winning contestant id once decided (by judge or manually by user).
    #[serde(default)]
    pub winner: Option<PartId>,
    pub closed: bool,
}

fn default_reveal_interval() -> RoundNo {
    DEFAULT_REVEAL_INTERVAL
}

impl Session {
    pub fn participant(&self, id: &PartId) -> Option<&Participant> {
        std::iter::once(&self.judge)
            .chain(&self.contestants)
            .find(|part| &part.id == id)
    }

    pub fn is_contestant(&self, id: &PartId) -> bool {
        self.contestants.iter().any(|part| &part.id == id)
    }

    pub fn contestant_ids(&self) -> impl Iterator<Item = &PartId> {
        self.contestants.iter().map(|part| &part.id)
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

    pub fn next_message_id(&self, offset: usize) -> MessageId {
        MessageId::nth(self.messages.len() + 1 + offset)
    }

    /// Score history `(round, score)` for `part`, oldest first.
    pub fn score_history(&self, part: &PartId) -> Vec<(RoundNo, u8)> {
        self.rounds
            .iter()
            .filter_map(|r| {
                r.outcome
                    .as_ref()
                    .and_then(|o| o.evaluations.get(part))
                    .map(|ev| (r.no, ev.score))
            })
            .collect()
    }

    /// Latest known score for `part`.
    pub fn latest_score(&self, part: &PartId) -> Option<u8> {
        self.score_history(part).last().map(|(_, s)| *s)
    }

    /// Latest ranking from the most recent judged round, or contestants in
    /// registration order when no round has been judged yet.
    pub fn current_ranking(&self) -> Vec<PartId> {
        if let Some(outcome) = self.rounds.iter().rev().find_map(|r| r.outcome.as_ref()) {
            let mut ranked = outcome.ranking.clone();
            for part in self.contestant_ids() {
                if !ranked.contains(part) {
                    ranked.push(part.clone());
                }
            }
            ranked
        } else {
            self.contestant_ids().cloned().collect()
        }
    }

    /// Latest judge feedback for `part` from any round before `before`.
    pub fn latest_evaluation_before(
        &self,
        part: &PartId,
        before: RoundNo,
    ) -> Option<(RoundNo, &EntryEvaluation)> {
        self.rounds
            .iter()
            .rev()
            .filter(|r| r.no < before)
            .find_map(|r| {
                r.outcome
                    .as_ref()
                    .and_then(|o| o.evaluations.get(part))
                    .map(|ev| (r.no, ev))
            })
    }

    /// Latest submission by `part` from any round before `before`.
    pub fn latest_submission_before(
        &self,
        part: &PartId,
        before: RoundNo,
    ) -> Option<(RoundNo, &Submission)> {
        self.rounds
            .iter()
            .rev()
            .filter(|r| r.no < before)
            .find_map(|r| r.submissions.get(part).map(|s| (r.no, s)))
    }

    /// How many rounds remain until the next peer-reveal round after `round`.
    pub fn rounds_until_reveal(&self, round: RoundNo) -> Option<RoundNo> {
        if self.reveal_interval == 0 {
            return None;
        }
        let next =
            ((round.saturating_sub(1)) / self.reveal_interval + 1) * self.reveal_interval + 1;
        if self.max_rounds.is_some_and(|max| next > max) {
            return None;
        }
        Some(next.saturating_sub(round))
    }

    pub fn author_name(&self, author: &Author) -> String {
        match author {
            Author::User => "ユーザー".to_owned(),
            Author::Part(id) if id.as_str() == JUDGE_ID => "審判 (Judge)".to_owned(),
            Author::Part(id) => id.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_round_schedule_matches_every_five_rounds() {
        assert_eq!(RoundKind::for_round(1, 5), RoundKind::Blind);
        assert_eq!(RoundKind::for_round(5, 5), RoundKind::Blind);
        assert_eq!(RoundKind::for_round(6, 5), RoundKind::Reveal);
        assert_eq!(RoundKind::for_round(7, 5), RoundKind::Blind);
        assert_eq!(RoundKind::for_round(10, 5), RoundKind::Blind);
        assert_eq!(RoundKind::for_round(11, 5), RoundKind::Reveal);
        // Interval 0 disables reveals completely.
        assert_eq!(RoundKind::for_round(6, 0), RoundKind::Blind);
        // Interval 1 reveals every round after R1.
        assert_eq!(RoundKind::for_round(1, 1), RoundKind::Blind);
        assert_eq!(RoundKind::for_round(2, 1), RoundKind::Reveal);
    }

    #[test]
    fn artifact_kind_classifies_design_and_code_files() {
        assert_eq!(ArtifactKind::from_path("index.html"), ArtifactKind::Html);
        assert_eq!(
            ArtifactKind::from_path("assets/hero.PNG"),
            ArtifactKind::Image
        );
        assert_eq!(ArtifactKind::from_path("logo.svg"), ArtifactKind::Svg);
        assert_eq!(ArtifactKind::from_path("src/app.tsx"), ArtifactKind::Code);
        assert_eq!(ArtifactKind::from_path("README.md"), ArtifactKind::Markdown);
        assert!(ArtifactKind::Html.is_visual());
        assert!(ArtifactKind::Svg.is_visual());
        assert!(!ArtifactKind::Code.is_visual());
    }
}
