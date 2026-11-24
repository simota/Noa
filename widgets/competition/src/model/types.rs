//! The competition's state. Everything here is the fold of the event log
//! (see `reduce`); nothing is mutated any other way.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::aggregate::PanelScores;

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

/// The most contestants one competition takes: each runs in a split of
/// one Noa tab.
pub const MAX_PANEL: usize = 6;

/// The id of the judge agent ("Judge"): the lead judge of a panel.
pub const JUDGE_ID: &str = "judge";

/// The most judges one competition seats: each runs in its own pane, and
/// six contestants with three judges fill the 3×3 grid of one Noa tab.
pub const MAX_JUDGES: usize = 3;

/// The id of the judge in seat `seat` (0 = lead): `judge`, `judge-2`,
/// `judge-3`. Ids follow the seat, not the kind, so they never collide
/// with contestant ids (kind names).
pub fn judge_id(seat: usize) -> PartId {
    match seat {
        0 => PartId::new(JUDGE_ID),
        n => PartId::new(format!("{JUDGE_ID}-{}", n + 1)),
    }
}

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
    /// The model and effort it is started on.
    #[serde(default, skip_serializing_if = "AgentSetup::is_default")]
    pub setup: AgentSetup,
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
    /// The concrete claims the judge relied on, each marked with how it was
    /// established. Shown in the UI; never sent to contestants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
    /// Set when a later round's judgment corrected this score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amended: Option<AmendNote>,
    /// The score split over the request's weighted criteria, when it has any.
    /// Sent to the contestant with its feedback.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub breakdown: Vec<AxisScore>,
}

/// Points on one weighted criterion of the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxisScore {
    pub axis: String,
    pub score: u8,
    pub max: u8,
}

/// How the judge established a claim it scored on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceBasis {
    /// The judge measured or inspected it in the artifacts.
    Measured,
    /// Taken from the contestant's report without re-checking.
    SelfReported,
    /// Neither measured nor reported; stated as an open question.
    Unverified,
}

impl EvidenceBasis {
    pub const ALL: [EvidenceBasis; 3] = [
        EvidenceBasis::Measured,
        EvidenceBasis::SelfReported,
        EvidenceBasis::Unverified,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceBasis::Measured => "measured",
            EvidenceBasis::SelfReported => "self_reported",
            EvidenceBasis::Unverified => "unverified",
        }
    }
}

/// One claim behind a score, with its basis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub claim: String,
    pub basis: EvidenceBasis,
}

/// A judge's correction of an earlier round's score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amendment {
    /// The earlier, already judged round being corrected.
    pub round: RoundNo,
    pub part: PartId,
    /// The corrected score, 0..=100.
    pub score: u8,
    /// Why the original score was wrong.
    pub reason: String,
}

/// What an amendment changed, kept on the corrected evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AmendNote {
    /// The score before the correction.
    pub from: u8,
    /// The round whose judgment made the correction.
    pub by_round: RoundNo,
    pub reason: String,
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
    /// Corrections to earlier rounds' scores, applied when this output is accepted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub amendments: Vec<Amendment>,
    /// The concrete checks the judge applies, published to every contestant.
    /// Set in round 1; a later non-empty list replaces it from the next round.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checklist: Vec<String>,
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
    /// One per contestant.
    pub briefs: Vec<Brief>,
    /// Accepted submissions in this round.
    #[serde(default)]
    pub submissions: BTreeMap<PartId, Submission>,
    /// The judge's evaluation brief, once the round reaches `Judging`; the
    /// lead's scorecard brief in a panel.
    pub judge_brief: Option<Brief>,
    /// The judge's accepted output for this round; in a panel, the combined
    /// judgment of every scorecard.
    pub outcome: Option<JudgeOutput>,
    /// The co-judges' scorecard briefs (panel only), in seat order of issue.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub co_judge_briefs: Vec<Brief>,
    /// Each judge's accepted scorecard (panel only).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scorecards: BTreeMap<PartId, JudgeOutput>,
    /// The lead's verdict brief, in a deciding round of a panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict_brief: Option<Brief>,
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
                .judge_briefs()
                .filter(|b| b.receipt.failed.is_some() && b.receipt.replied.is_none())
                .map(|b| b.to.clone())
                .collect(),
            Phase::Closed => Vec::new(),
        }
    }

    /// Every judge brief now open or done, lead first: the lead's is the
    /// verdict brief once issued (its scorecard brief is replied by then),
    /// else its scorecard brief; then the co-judges'.
    pub fn judge_briefs(&self) -> impl Iterator<Item = &Brief> {
        self.verdict_brief
            .as_ref()
            .or(self.judge_brief.as_ref())
            .into_iter()
            .chain(&self.co_judge_briefs)
    }

    /// The brief open or done for `to`: a contestant's, a judge's (for the
    /// lead, the verdict brief once issued).
    pub fn brief(&self, to: &PartId) -> Option<&Brief> {
        self.briefs
            .iter()
            .chain(self.judge_briefs())
            .find(|brief| &brief.to == to)
    }

    pub fn brief_mut(&mut self, to: &PartId) -> Option<&mut Brief> {
        self.briefs
            .iter_mut()
            .chain(self.verdict_brief.as_mut().or(self.judge_brief.as_mut()))
            .chain(&mut self.co_judge_briefs)
            .find(|brief| &brief.to == to)
    }

    /// Whether the lead's verdict is asked for and not in yet.
    pub fn verdict_pending(&self) -> bool {
        self.verdict_brief
            .as_ref()
            .is_some_and(|b| b.receipt.replied.is_none())
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
    /// The judge; the lead judge of a panel.
    pub judge: Participant,
    /// The judges after the lead (`judge-2`, `judge-3`), who only score.
    /// Empty in a single-judge session, and then not written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub co_judges: Vec<Participant>,
    pub contestants: Vec<Participant>,
    pub messages: Vec<Message>,
    pub rounds: Vec<Round>,
    /// Winning contestant id once decided (by judge or manually by user).
    #[serde(default)]
    pub winner: Option<PartId>,
    pub closed: bool,
    /// The user holds the competition after each judged round: the next
    /// round waits for them.
    #[serde(default)]
    pub hold: bool,
}

fn default_reveal_interval() -> RoundNo {
    DEFAULT_REVEAL_INTERVAL
}

impl Session {
    /// Whether the competition waits for the user to start its next round:
    /// held, with the last round judged.
    pub fn waits_to_continue(&self) -> bool {
        !self.closed
            && self.hold
            && self
                .current_round()
                .is_some_and(|round| round.phase == Phase::Closed)
    }

    /// Whether the user may add rounds, or why not. The competition must
    /// have a round limit; once ended, it must have ended by running all
    /// its rounds (not by the judge closing it early or by the user). While
    /// the final round runs, its briefs say FINAL (those sent and those
    /// waiting to go out), so rounds are added once its verdict is in.
    pub fn extendable(&self) -> Result<(), &'static str> {
        // Between the combined scores and the lead's verdict: an early
        // close is decided, though the competition is not closed yet.
        if !self.closed && self.current_round().is_some_and(Round::verdict_pending) {
            return Err("最終講評の待機中です");
        }
        let Some(max) = self.max_rounds else {
            return Err("ラウンド上限のないコンペです");
        };
        let at_limit = self.rounds.len() as RoundNo >= max;
        let phase = self.current_round().map(|round| round.phase);
        if !self.closed {
            return if at_limit {
                Err("最終ラウンドの進行中です（最終判定の後に追加できます）")
            } else if self.lead_closes_current_round() {
                // Co-judges still scoring: the round will end the
                // competition early, and an extension would be lost.
                Err("リード審判が早期終了を決めています")
            } else {
                Ok(())
            };
        }
        if at_limit && phase == Some(Phase::Closed) {
            Ok(())
        } else {
            Err("ラウンド上限の前に終了したコンペです")
        }
    }

    /// Whether the lead's accepted scorecard of the current round asks to
    /// end the competition early (panel only).
    fn lead_closes_current_round(&self) -> bool {
        self.current_round()
            .and_then(|round| round.scorecards.get(&self.judge.id))
            .is_some_and(|card| card.close)
    }

    /// The judge's final verdict on the winner now decided: that of the
    /// last judged round, when it named the current winner. An earlier
    /// verdict (before the competition was extended, or one a later manual
    /// choice overrode) stays with its round and is not the result.
    pub fn final_verdict(&self) -> Option<&str> {
        let winner = self.winner.as_ref()?;
        let outcome = self.rounds.iter().rev().find_map(|r| r.outcome.as_ref())?;
        outcome
            .final_verdict
            .as_deref()
            .filter(|_| outcome.winner.as_ref() == Some(winner))
    }

    pub fn participant(&self, id: &PartId) -> Option<&Participant> {
        self.judges()
            .chain(&self.contestants)
            .find(|part| &part.id == id)
    }

    /// The judge panel in seat order, lead first.
    pub fn judges(&self) -> impl Iterator<Item = &Participant> {
        std::iter::once(&self.judge).chain(&self.co_judges)
    }

    pub fn is_judge(&self, id: &PartId) -> bool {
        self.judges().any(|part| &part.id == id)
    }

    /// Whether more than one judge scores (a multi-judge session).
    pub fn is_panel(&self) -> bool {
        !self.co_judges.is_empty()
    }

    /// Every judge's score of `part` in round `no`, in seat order, once
    /// every judge's scorecard of that round is in (a panel round); `None`
    /// for a single-judge round or one still being scored.
    pub fn panel_scores(&self, no: RoundNo, part: &PartId) -> Option<PanelScores> {
        let round = self.round(no)?;
        let scores = self
            .judges()
            .map(|judge| {
                let card = round.scorecards.get(&judge.id)?;
                Some((judge.id.clone(), card.evaluations.get(part)?.score))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(PanelScores { scores })
    }

    /// `part`'s round-`no` score as every view shows it: the mean to one
    /// decimal (`81.3`) in a panel round, the score (`81`) otherwise.
    pub fn score_label(&self, no: RoundNo, part: &PartId) -> Option<String> {
        if let Some(panel) = self.panel_scores(no, part) {
            return Some(panel.mean_label());
        }
        let outcome = self.round(no)?.outcome.as_ref()?;
        Some(outcome.evaluations.get(part)?.score.to_string())
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
        self.rounds.iter().rev().find_map(|r| {
            r.outcome
                .as_ref()
                .and_then(|o| o.evaluations.get(part))
                .map(|ev| ev.score)
        })
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

    /// The judge's published checklist in force for `round`: the latest
    /// non-empty one from a judged round before it, with that round.
    pub fn checklist_before(&self, round: RoundNo) -> Option<(RoundNo, &[String])> {
        self.rounds
            .iter()
            .rev()
            .filter(|r| r.no < round)
            .find_map(|r| {
                r.outcome
                    .as_ref()
                    .filter(|o| !o.checklist.is_empty())
                    .map(|o| (r.no, o.checklist.as_slice()))
            })
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
            Author::Part(id) => match self.co_judges.iter().position(|p| &p.id == id) {
                Some(seat) => format!("審判 {} ({id})", seat + 2),
                None => id.to_string(),
            },
        }
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
        let old = r#"{"id":"claude","kind":"claude","role":"contestant","pane_id":7}"#;
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

    #[test]
    fn round_and_latest_score_handle_sequential_sparse_and_amended_rounds() {
        let p = |id: &str, kind, role| Participant {
            id: PartId::new(id),
            kind,
            role,
            pane_id: None,
            setup: Default::default(),
        };
        let claude = PartId::new("claude");
        let codex = PartId::new("codex");
        let make_round = |no: RoundNo, claude_score: Option<u8>| {
            let outcome = claude_score.map(|score| {
                let mut evaluations = BTreeMap::new();
                evaluations.insert(
                    claude.clone(),
                    EntryEvaluation {
                        score,
                        strengths: String::new(),
                        feedback: String::new(),
                        evidence: Vec::new(),
                        amended: None,
                        breakdown: Vec::new(),
                    },
                );
                JudgeOutput {
                    overview: String::new(),
                    evaluations,
                    ranking: vec![claude.clone()],
                    winner: None,
                    final_verdict: None,
                    close: false,
                    amendments: Vec::new(),
                    checklist: Vec::new(),
                }
            });
            Round {
                no,
                kind: RoundKind::Blind,
                phase: Phase::Closed,
                started_at: u64::from(no),
                briefs: Vec::new(),
                submissions: BTreeMap::new(),
                judge_brief: None,
                co_judge_briefs: Vec::new(),
                scorecards: BTreeMap::new(),
                verdict_brief: None,
                outcome,
            }
        };
        let mut s = Session {
            id: SessionId::new("comp-t"),
            request: "req".into(),
            workdir: None,
            max_rounds: None,
            reveal_interval: 5,
            rev: 1,
            created_at: 0,
            judge: p(JUDGE_ID, AgentKind::Claude, Role::Judge),
            co_judges: Vec::new(),
            contestants: vec![
                p("claude", AgentKind::Claude, Role::Contestant),
                p("codex", AgentKind::Codex, Role::Contestant),
            ],
            messages: Vec::new(),
            rounds: vec![
                make_round(1, Some(72)),
                make_round(2, None),
                make_round(3, Some(85)),
            ],
            winner: None,
            closed: false,
            hold: false,
        };
        assert_eq!(s.round(0), None);
        assert_eq!(s.round(1).map(|r| r.no), Some(1));
        assert_eq!(s.round(3).map(|r| r.no), Some(3));
        assert_eq!(s.round(4), None);
        assert_eq!(s.latest_score(&claude), Some(85));
        assert_eq!(s.latest_score(&codex), None);
        s.round_mut(3)
            .unwrap()
            .outcome
            .as_mut()
            .unwrap()
            .evaluations
            .get_mut(&claude)
            .unwrap()
            .score = 88;
        assert_eq!(s.latest_score(&claude), Some(88));
        // Non-sequential round numbers must still be found via fallback.
        s.rounds.push(make_round(10, Some(91)));
        assert_eq!(s.round(10).map(|r| r.no), Some(10));
        assert_eq!(s.round_mut(10).map(|r| r.no), Some(10));
        assert_eq!(s.latest_score(&claude), Some(91));
    }

    #[test]
    fn judge_ids_follow_the_seat() {
        assert_eq!(
            [judge_id(0), judge_id(1), judge_id(2)].map(|id| id.0),
            ["judge", "judge-2", "judge-3"]
        );
        // Three judges of one kind: ids by seat, never colliding with the
        // contestants' kind names.
        let claude = |model: &str| AgentChoice {
            kind: AgentKind::Claude,
            setup: AgentSetup {
                model: Some(model.into()),
                effort: None,
            },
        };
        let seats = crate::engine::participants(
            &[claude("opus"), claude("sonnet"), claude("haiku")],
            &[AgentKind::Claude.into(), AgentKind::Claude.into()],
        );
        let judges: Vec<(&str, Role, Option<&str>)> = std::iter::once(&seats.judge)
            .chain(&seats.co_judges)
            .map(|p| (p.id.as_str(), p.role, p.setup.model.as_deref()))
            .collect();
        assert_eq!(
            judges,
            [
                ("judge", Role::Judge, Some("opus")),
                ("judge-2", Role::Judge, Some("sonnet")),
                ("judge-3", Role::Judge, Some("haiku")),
            ]
        );
        let ids: Vec<&str> = seats.contestants.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["claude", "claude-2"]);

        let session = crate::model::aggregate::tests::panel_session(Some(3));
        assert!(session.is_panel());
        let seated: Vec<&str> = session.judges().map(|p| p.id.as_str()).collect();
        assert_eq!(seated, ["judge", "judge-2", "judge-3"]);
        assert!(session.is_judge(&judge_id(2)) && !session.is_judge(&PartId::new("claude")));
        assert_eq!(
            session.participant(&judge_id(1)).map(|p| p.kind),
            Some(AgentKind::Claude)
        );
        let name = |id: &str| session.author_name(&Author::Part(PartId::new(id)));
        assert_eq!(
            [
                name("judge"),
                name("judge-2"),
                name("judge-3"),
                name("codex")
            ],
            [
                "審判 (Judge)",
                "審判 2 (judge-2)",
                "審判 3 (judge-3)",
                "codex"
            ]
        );
    }

    #[test]
    fn rounds_are_not_added_while_a_verdict_is_pending() {
        use crate::model::aggregate::tests::{cards, panel_session, settle};
        use crate::model::reduce::{Event, apply};
        let mut session = panel_session(Some(3));
        let mut all = cards(
            &[("codex", 90), ("claude", 70)],
            &[("codex", 85), ("claude", 75)],
            &[("codex", 80), ("claude", 79)],
        );
        // The lead asks to close early, before the limit.
        all.get_mut(&judge_id(0)).unwrap().close = true;
        settle(&mut session, 1, &all).unwrap();
        assert_eq!(
            session.extendable(),
            Err("リード審判が早期終了を決めています")
        );
        let brief = Brief {
            to: judge_id(0),
            base_rev: session.rev,
            body: "verdict".into(),
            reply_path: PathBuf::from("/c/r1/verdict.json"),
            receipt: Receipt::default(),
        };
        apply(&mut session, &[Event::VerdictRequested { round: 1, brief }]).unwrap();
        assert_eq!(session.extendable(), Err("最終講評の待機中です"));
        let extend = Event::RoundsExtended {
            max_rounds: 4,
            at: 9,
        };
        assert!(apply(&mut session.clone(), std::slice::from_ref(&extend)).is_err());
        // In: the competition ended early, and is not extended either.
        apply(
            &mut session,
            &[
                Event::ReplyAccepted {
                    round: 1,
                    to: judge_id(0),
                    at: 10,
                },
                Event::VerdictAccepted {
                    round: 1,
                    winner: PartId::new("codex"),
                    final_verdict: "codex".into(),
                },
                Event::PhaseChanged {
                    round: 1,
                    phase: Phase::Closed,
                },
                Event::SessionClosed,
            ],
        )
        .unwrap();
        assert!(!session.current_round().unwrap().verdict_pending());
        assert_eq!(
            session.extendable(),
            Err("ラウンド上限の前に終了したコンペです")
        );
    }
}
