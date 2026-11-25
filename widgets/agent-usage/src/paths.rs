//! Every root and file path the widget reads or writes. Only `own_dir` is ever
//! created or written; everything else is read-only.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roots {
    pub claude_projects: Option<PathBuf>,
    pub claude_credentials: Option<PathBuf>,
    pub codex_sessions: Vec<PathBuf>,
    pub agy_dir: Option<PathBuf>,
    pub ctl_dir: Option<PathBuf>,
    /// `<data dir>/noa/agent-usage`
    pub own_dir: Option<PathBuf>,
}

impl Roots {
    /// The real home and data dir, honouring `CLAUDE_CONFIG_DIR` and `CODEX_HOME`.
    pub fn system() -> Roots {
        match (dirs::home_dir(), dirs::data_dir()) {
            (Some(home), Some(data)) => Roots::under(&home, &data),
            (home, data) => {
                let mut roots = Roots::under(
                    home.as_deref().unwrap_or(Path::new("")),
                    data.as_deref().unwrap_or(Path::new("")),
                );
                if home.is_none() {
                    roots.claude_projects = None;
                    roots.claude_credentials = None;
                    roots.codex_sessions.clear();
                    roots.agy_dir = None;
                }
                if data.is_none() {
                    roots.ctl_dir = None;
                    roots.own_dir = None;
                }
                roots
            }
        }
    }

    /// Roots below a home and a data dir, honouring the environment overrides.
    pub fn under(home: &Path, data_dir: &Path) -> Roots {
        let env = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Roots::with_overrides(home, data_dir, env("CLAUDE_CONFIG_DIR"), env("CODEX_HOME"))
    }

    fn with_overrides(
        home: &Path,
        data_dir: &Path,
        claude_config: Option<PathBuf>,
        codex_home: Option<PathBuf>,
    ) -> Roots {
        let claude = claude_config.unwrap_or_else(|| home.join(".claude"));
        let codex = codex_home.unwrap_or_else(|| home.join(".codex"));
        // Both roots always: a directory created after start is found by the
        // next rescan, and enumeration skips one that is missing.
        let codex_sessions = vec![codex.join("sessions"), codex.join("archived_sessions")];
        Roots {
            claude_projects: Some(claude.join("projects")),
            claude_credentials: Some(claude.join(".credentials.json")),
            codex_sessions,
            agy_dir: Some(home.join(".gemini").join("antigravity-cli")),
            ctl_dir: Some(data_dir.join("noa").join("ctl")),
            own_dir: Some(data_dir.join("noa").join("agent-usage")),
        }
    }
}

pub fn index_path(own: &Path) -> PathBuf {
    own.join("index-v1.json")
}

pub fn history_path(own: &Path) -> PathBuf {
    own.join("limits.jsonl")
}

pub fn prefs_path(own: &Path) -> PathBuf {
    own.join("view.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_under_home_follow_env_overrides() {
        let home = tempfile::tempdir().unwrap();
        let data = Path::new("/data");
        let r = Roots::with_overrides(home.path(), data, None, None);
        assert_eq!(
            r.claude_projects,
            Some(home.path().join(".claude/projects"))
        );
        assert_eq!(
            r.claude_credentials,
            Some(home.path().join(".claude/.credentials.json"))
        );
        assert_eq!(
            r.codex_sessions,
            vec![
                home.path().join(".codex/sessions"),
                home.path().join(".codex/archived_sessions")
            ]
        );
        assert_eq!(r.agy_dir, Some(home.path().join(".gemini/antigravity-cli")));
        assert_eq!(r.ctl_dir, Some(PathBuf::from("/data/noa/ctl")));
        assert_eq!(r.own_dir, Some(PathBuf::from("/data/noa/agent-usage")));
        // Overrides move the Claude and Codex roots only.
        let o = Roots::with_overrides(
            home.path(),
            data,
            Some(PathBuf::from("/cfg/claude")),
            Some(PathBuf::from("/cfg/codex")),
        );
        assert_eq!(
            o.claude_projects,
            Some(PathBuf::from("/cfg/claude/projects"))
        );
        assert_eq!(
            o.claude_credentials,
            Some(PathBuf::from("/cfg/claude/.credentials.json"))
        );
        assert_eq!(
            o.codex_sessions,
            vec![
                PathBuf::from("/cfg/codex/sessions"),
                PathBuf::from("/cfg/codex/archived_sessions")
            ]
        );
        assert_eq!(o.agy_dir, r.agy_dir);
        let own = r.own_dir.unwrap();
        assert_eq!(index_path(&own), own.join("index-v1.json"));
        assert_eq!(history_path(&own), own.join("limits.jsonl"));
        assert_eq!(prefs_path(&own), own.join("view.json"));
    }
}
