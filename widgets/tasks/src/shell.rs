//! The login shell agents (and `gh`) run through, and a run's agent argv.
//!
//! A widget started from `Noa.app` inherits a GUI `PATH` without
//! `/opt/homebrew/bin` and the like, so agents start through the person's
//! login shell (`$SHELL -l -i -c …`), whose `PATH` is theirs.

use crate::model::types::AgentKind;

/// The login shell: `$SHELL` when it is an absolute path, else `/bin/zsh`.
pub fn login_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|shell| shell.starts_with('/'))
        .unwrap_or_else(|| "/bin/zsh".to_owned())
}

/// The variable a run's prompt reaches its pane's shell in.
pub const PROMPT_VAR: &str = "NOA_TASKS_PROMPT";

/// The argv of a run's pane: the login shell running the agent on the
/// prompt in [`PROMPT_VAR`]. The code the shell runs is fixed and holds no
/// path: `"$VAR"` is one word, as is, in sh, bash, zsh and fish alike.
pub fn argv(kind: AgentKind) -> Vec<String> {
    // agy takes a prompt to start an interactive session with only after -i.
    let interactive = if kind == AgentKind::Agy { " -i" } else { "" };
    let script = format!("exec {}{interactive} \"${PROMPT_VAR}\"", kind.command());
    vec![login_shell(), "-l".into(), "-i".into(), "-c".into(), script]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_runs_on_the_prompt_variable_through_the_login_shell() {
        let shell = login_shell();
        assert!(shell.starts_with('/'), "{shell}");
        assert_eq!(
            argv(AgentKind::Claude),
            [
                shell.as_str(),
                "-l",
                "-i",
                "-c",
                "exec claude \"$NOA_TASKS_PROMPT\""
            ]
        );
        assert_eq!(
            argv(AgentKind::Codex)[4],
            "exec codex \"$NOA_TASKS_PROMPT\""
        );
        assert_eq!(argv(AgentKind::Agy)[4], "exec agy -i \"$NOA_TASKS_PROMPT\"");
    }
}
