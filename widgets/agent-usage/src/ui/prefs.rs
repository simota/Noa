//! The view kept across runs: tab, period and agent, in
//! `<data dir>/noa/agent-usage/view.json` (`paths::prefs_path`). Flags win
//! over what is saved; a missing or damaged file is the defaults.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use agent_usage::stats::types::{AgentFilter, Period};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Limits,
    Stats,
}

impl Tab {
    pub fn parse(s: &str) -> Option<Tab> {
        match s {
            "limits" => Some(Tab::Limits),
            "stats" => Some(Tab::Stats),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Tab::Limits => "limits",
            Tab::Stats => "stats",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Tab::Limits => 0,
            Tab::Stats => 1,
        }
    }

    pub fn other(self) -> Tab {
        match self {
            Tab::Limits => Tab::Stats,
            Tab::Stats => Tab::Limits,
        }
    }
}

pub const PERIODS: [Period; 4] = [Period::Today, Period::Days7, Period::Days30, Period::All];
pub const FILTERS: [AgentFilter; 4] = [
    AgentFilter::All,
    AgentFilter::One(agent_usage::model::Agent::Claude),
    AgentFilter::One(agent_usage::model::Agent::Codex),
    AgentFilter::One(agent_usage::model::Agent::Agy),
];

pub fn filter_id(filter: AgentFilter) -> &'static str {
    match filter {
        AgentFilter::All => "all",
        AgentFilter::One(agent) => agent.id(),
    }
}

/// The next value after `current` in `all`, wrapping.
pub fn cycle<T: PartialEq + Copy>(all: &[T], current: T) -> T {
    let at = all.iter().position(|v| *v == current).unwrap_or(0);
    all[(at + 1) % all.len()]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefs {
    pub tab: Tab,
    pub period: Period,
    pub agent: AgentFilter,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            tab: Tab::Limits,
            period: Period::Days7,
            agent: AgentFilter::All,
        }
    }
}

/// What the command line asked for; each given value wins over the saved one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    pub tab: Option<Tab>,
    pub period: Option<Period>,
    pub agent: Option<AgentFilter>,
}

impl Prefs {
    pub fn with(self, o: Overrides) -> Prefs {
        Prefs {
            tab: o.tab.unwrap_or(self.tab),
            period: o.period.unwrap_or(self.period),
            agent: o.agent.unwrap_or(self.agent),
        }
    }

    pub fn to_json(self) -> String {
        serde_json::json!({
            "version": 1,
            "tab": self.tab.id(),
            "period": self.period.id(),
            "agent": filter_id(self.agent),
        })
        .to_string()
    }

    /// Each missing or unknown value is its default; text that is not a JSON
    /// object is `None`.
    pub fn from_json(text: &str) -> Option<Prefs> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        let object = value.as_object()?;
        let get = |key: &str| object.get(key).and_then(serde_json::Value::as_str);
        let base = Prefs::default();
        Some(Prefs {
            tab: get("tab").and_then(Tab::parse).unwrap_or(base.tab),
            period: get("period").and_then(Period::parse).unwrap_or(base.period),
            agent: get("agent")
                .and_then(AgentFilter::parse)
                .unwrap_or(base.agent),
        })
    }
}

/// A file larger than this is not a view.
const FILE_MAX_BYTES: u64 = 16 * 1024;

/// The prefs file and what it holds as far as this run knows; a view is
/// written only when it differs.
#[derive(Debug)]
pub struct PrefsStore {
    path: PathBuf,
    saved: Option<Prefs>,
}

impl PrefsStore {
    pub fn at(path: PathBuf) -> PrefsStore {
        PrefsStore { path, saved: None }
    }

    /// The saved view, or the defaults when there is none or it is damaged.
    pub fn load(&mut self) -> Prefs {
        let prefs = read(&self.path).and_then(|t| Prefs::from_json(&t));
        self.saved = prefs;
        prefs.unwrap_or_default()
    }

    /// Writes `prefs` when it differs from the file; a failed write is tried
    /// again at the next change.
    pub fn save(&mut self, prefs: &Prefs) {
        if self.saved.as_ref() == Some(prefs) {
            return;
        }
        if write(&self.path, &prefs.to_json()).is_ok() {
            self.saved = Some(*prefs);
        }
    }
}

fn read(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > FILE_MAX_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Temp file + rename, so a reader never sees half a view.
fn write(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    let result = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_usage::model::Agent;

    #[test]
    fn prefs_round_trip_and_damage_falls_back() {
        let p = Prefs {
            tab: Tab::Stats,
            period: Period::Days30,
            agent: AgentFilter::One(Agent::Codex),
        };
        assert_eq!(Prefs::from_json(&p.to_json()), Some(p));
        assert_eq!(Prefs::from_json("not json"), None);
        assert_eq!(
            Prefs::from_json(r#"{"tab":"x","period":"all"}"#),
            Some(Prefs {
                period: Period::All,
                ..Prefs::default()
            })
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("view.json");
        std::fs::write(&path, "{{{").unwrap();
        assert_eq!(PrefsStore::at(path).load(), Prefs::default());
    }

    #[test]
    fn cycle_wraps() {
        assert_eq!(cycle(&PERIODS, Period::All), Period::Today);
        assert_eq!(cycle(&FILTERS, AgentFilter::All), FILTERS[1]);
    }
}
