//! Statistics query and report types (contract between units B and C).

use serde::{Deserialize, Serialize};

use crate::model::{Agent, Unix};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Period {
    Today,
    Days7,
    Days30,
    All,
}

impl Period {
    /// "today" | "7d" | "30d" | "all".
    pub fn parse(s: &str) -> Option<Period> {
        match s {
            "today" => Some(Period::Today),
            "7d" => Some(Period::Days7),
            "30d" => Some(Period::Days30),
            "all" => Some(Period::All),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Period::Today => "today",
            Period::Days7 => "7d",
            Period::Days30 => "30d",
            Period::All => "all",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentFilter {
    All,
    One(Agent),
}

impl AgentFilter {
    /// "all" or an agent id.
    pub fn parse(s: &str) -> Option<AgentFilter> {
        if s == "all" {
            Some(AgentFilter::All)
        } else {
            Agent::parse(s).map(AgentFilter::One)
        }
    }

    pub fn matches(self, agent: Agent) -> bool {
        match self {
            AgentFilter::All => true,
            AgentFilter::One(a) => a == agent,
        }
    }
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tokens {
    /// Fresh, uncached input.
    pub input: u64,
    /// Including reasoning.
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Part of `cache_write` written with the 1 h TTL.
    pub cache_write_1h: u64,
    /// Part of `output`.
    pub reasoning: u64,
}

impl Tokens {
    /// input + output + cache read + cache write.
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    pub fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.cache_write_1h += o.cache_write_1h;
        self.reasoning += o.reasoning;
    }
}

#[derive(Default, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Counters {
    pub turns: u64,
    pub messages: u64,
    pub tool_calls: u64,
    /// Skill uses (`stats::claude`, `stats::codex`; agy records none).
    #[serde(default)]
    pub skills: u64,
    pub active_secs: u64,
    pub tokens: Tokens,
}

/// Counters for one (UTC hour, agent, model, project); `model` and `project`
/// are interned string ids.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bucket {
    /// UTC hour start.
    pub hour: Unix,
    pub agent: Agent,
    pub model: u32,
    pub project: u32,
    pub c: Counters,
}

/// The UTC hours in which one session had activity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionSpan {
    pub agent: Agent,
    pub key: u64,
    pub project: u32,
    pub hours: Vec<Unix>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Availability {
    Full,
    /// agy: turns and sessions only.
    NoTokens,
    Missing(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Counts {
    pub sessions: u64,
    pub c: Counters,
    /// `None` when nothing in the selection is priced.
    pub cost_usd: Option<f64>,
    pub unpriced_tokens: u64,
    /// Noa workers spawned (from the ctl journals).
    pub workers: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelRow {
    pub agent: Agent,
    pub model: String,
    pub counts: Counts,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyRow {
    pub key: String,
    pub counts: Counts,
}

/// One skill of one agent in the period.
#[derive(Clone, Debug, PartialEq)]
pub struct SkillRow {
    pub skill: String,
    pub agent: Agent,
    pub uses: u64,
    /// Sessions that used it (a Codex sub-thread counts toward its root).
    pub sessions: u64,
    /// Projects it was used in.
    pub projects: u64,
    /// The time of the last use.
    pub last: Unix,
    /// `last` as local "MM/DD HH:MM".
    pub last_label: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Coverage {
    pub files_total: u64,
    pub files_done: u64,
    pub lines_bad: u64,
    pub lines_skipped: u64,
    pub running: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StatsReport {
    pub range: (Unix, Unix),
    pub total: Counts,
    pub per_agent: Vec<(Agent, Counts, Availability)>,
    pub per_model: Vec<ModelRow>,
    pub per_project: Vec<KeyRow>,
    /// Projects beyond the listed top 15.
    pub other_projects: u64,
    pub tools: Vec<(String, u64)>,
    /// Most used first.
    pub skills: Vec<SkillRow>,
    /// (label, tokens per `Agent::index`).
    pub days: Vec<(String, [u64; 3])>,
    pub cumulative: Vec<(String, [u64; 3])>,
    /// [weekday Monday = 0][local hour].
    pub heatmap: [[u64; 24]; 7],
    pub coverage: Coverage,
}

impl Counts {
    pub fn empty() -> Counts {
        Counts {
            sessions: 0,
            c: Counters::default(),
            cost_usd: None,
            unpriced_tokens: 0,
            workers: 0,
        }
    }
}

impl StatsReport {
    pub fn empty(range: (Unix, Unix)) -> StatsReport {
        StatsReport {
            range,
            total: Counts::empty(),
            per_agent: Vec::new(),
            per_model: Vec::new(),
            per_project: Vec::new(),
            other_projects: 0,
            tools: Vec::new(),
            skills: Vec::new(),
            days: Vec::new(),
            cumulative: Vec::new(),
            heatmap: [[0; 24]; 7],
            coverage: Coverage::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_total_and_add() {
        let mut a = Tokens {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            cache_write_1h: 1,
            reasoning: 1,
        };
        assert_eq!(a.total(), 10);
        a.add(&a.clone());
        assert_eq!((a.total(), a.cache_write_1h, a.reasoning), (20, 2, 2));
    }

    #[test]
    fn period_and_filter_parse() {
        assert_eq!(Period::parse("30d"), Some(Period::Days30));
        assert_eq!(Period::parse("x"), None);
        assert_eq!(AgentFilter::parse("all"), Some(AgentFilter::All));
        assert_eq!(
            AgentFilter::parse("codex"),
            Some(AgentFilter::One(Agent::Codex))
        );
    }
}
