//! The only terminal-facing adapter. Prompts are passed as one quoted
//! environment argument at spawn, never pasted into a possibly plain shell.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use noa_ipc::{
    Client, ClientError, Panel, Scope, ScopeSet, SendTextOptions, SpawnConfig, SpawnParams,
    SplitPlacement, WireId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Refused(String),
    Uncertain(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(s) | Self::Uncertain(s) => f.write_str(s),
        }
    }
}

fn error(error: ClientError) -> Error {
    match error {
        // A spawn can still execute after the UI-thread deadline; treating
        // that response as refused would allow a second worker to start.
        ClientError::Rpc { code, message }
            if code != noa_ipc::ErrorCode::OutcomeUnknown.code() =>
        {
            Error::Refused(format!("{message} ({code})"))
        }
        ClientError::InvalidInput(message) => Error::Refused(message),
        _ => Error::Uncertain(
            "Noaから応答を確認できません。重複実行を避けるため結果を照合してください".into(),
        ),
    }
}

pub struct Ipc {
    client: Client,
}

#[derive(Clone, Debug)]
pub struct AgentSpec {
    pub agent: String,
    pub directory: PathBuf,
    pub label: String,
    pub prompt: String,
    pub result: PathBuf,
}

pub fn agent_argv(agent: &str) -> Result<Vec<String>, String> {
    let suffix = match agent {
        "codex" | "claude" => "",
        "agy" => " -i",
        _ => return Err("利用できるエージェントは codex / claude / agy です".into()),
    };
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| Path::new(s).is_absolute())
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/bin/zsh".into()
            } else {
                "/bin/bash".into()
            }
        });
    Ok(vec![
        shell,
        "-l".into(),
        "-i".into(),
        "-c".into(),
        format!("exec {agent}{suffix} \"$NOA_WORKFLOW_PROMPT\""),
    ])
}

pub fn detect_agent() -> Result<String, String> {
    let argv = agent_argv("codex")?;
    let fish = Path::new(&argv[0])
        .file_name()
        .is_some_and(|name| name == "fish");
    let script = if fish {
        "for c in codex claude agy; if command -v $c >/dev/null 2>&1; printf '\\nnoa-workflow-agent:%s\\n' $c; end; end; exit 0"
    } else {
        "for c in codex claude agy; do if command -v \"$c\" >/dev/null 2>&1; then printf '\\nnoa-workflow-agent:%s\\n' \"$c\"; fi; done; exit 0"
    };
    let output = super::process::output(
        std::process::Command::new(&argv[0]).args(["-l", "-i", "-c", script]),
        std::time::Duration::from_secs(10),
    )?;
    let lines = String::from_utf8_lossy(&output);
    ["codex", "claude", "agy"].into_iter().find(|name| lines.lines().any(|line| line == format!("noa-workflow-agent:{name}"))).map(str::to_owned).ok_or_else(|| "利用できるエージェントがありません。Codex・Claude Code・agyのいずれかを導入してください".into())
}

impl Ipc {
    #[cfg(test)]
    pub(super) fn from_client(client: Client) -> Self {
        Self { client }
    }

    pub fn connect() -> Result<Self, String> {
        let (config, _) = noa_config::load_startup_config(noa_config::ConfigOverrides::default())
            .map_err(|_| "Noaの設定を読めません".to_owned())?;
        if !config.server_enable {
            return Err(
                "Noaの接続設定が無効です。設定で server-enable を有効にしてください".into(),
            );
        }
        let token = match config.server_token.filter(|s| !s.trim().is_empty()) {
            Some(token) => token,
            None => noa_config::server_token_path()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .ok_or("Noaの接続トークンを読めません")?,
        };
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
        let address = match config
            .server_bind
            .parse()
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
        {
            IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
            address => address,
        };
        let url = format!("ws://{}/", SocketAddr::new(address, config.server_port));
        let client = Client::connect(
            &url,
            &token,
            ScopeSet::from_strings(["read", "control", "input"]),
        )
        .map_err(|_| {
            "Noaへ接続できません。起動状態と read,control,input の接続権限を確認してください"
                .to_owned()
        })?;
        if ![Scope::Read, Scope::Control, Scope::Input]
            .iter()
            .all(|scope| client.granted_scopes().contains(*scope))
        {
            return Err("Workflowには read,control,input の接続権限が必要です".into());
        }
        if !client.supports_board_tab() || !supported_version(client.server_version()) {
            return Err(
                "安全なエージェント起動には Noa 0.9.13 以降と boardTab 機能が必要です".into(),
            );
        }
        Ok(Self { client })
    }

    pub fn instance(&self) -> String {
        self.client.server_instance_id().to_owned()
    }

    pub fn panels(&mut self) -> Result<Vec<Panel>, Error> {
        self.client.list_panels().map_err(error)
    }

    pub fn board(&mut self, directory: &Path) -> Result<u64, Error> {
        self.client
            .spawn_board_tab(
                None,
                &SpawnParams {
                    config: Some(SpawnConfig {
                        cwd: Some(directory.display().to_string()),
                        ..Default::default()
                    }),
                    focus: Some(false),
                    auto_approve: Some(false),
                    ..Default::default()
                },
            )
            .map_err(error)
    }

    pub fn spawn(&mut self, board: u64, spec: &AgentSpec) -> Result<u64, Error> {
        let argv = agent_argv(&spec.agent).map_err(Error::Refused)?;
        let mut env = BTreeMap::new();
        env.insert("NOA_WORKFLOW_PROMPT".into(), spec.prompt.clone());
        env.insert("NOA_RESULT".into(), spec.result.display().to_string());
        self.client
            .spawn_split(
                board,
                SplitPlacement::Float,
                &SpawnParams {
                    config: Some(SpawnConfig {
                        command: Some(argv),
                        cwd: Some(spec.directory.display().to_string()),
                        env,
                        wait_after_command: true,
                        ..Default::default()
                    }),
                    label: Some(spec.label.clone()),
                    parent_pane_id: Some(WireId(board)),
                    focus: Some(false),
                    max_children: Some(20),
                    ..Default::default()
                },
            )
            .map_err(error)
    }

    pub fn focus(&mut self, pane: u64) -> Result<(), Error> {
        self.client.focus_pane(pane).map_err(error)
    }

    pub fn close(&mut self, pane: u64) -> Result<(), Error> {
        self.client.close_pane(pane).map_err(error)
    }

    /// A person's explicit answer is distinct from a scheduled node. The
    /// final Enter is guarded server-side too; an uncertain receipt is
    /// never automatically replayed.
    pub fn answer(&mut self, pane: &Panel, answer: &str) -> Result<(), Error> {
        if answer.len() > 16 * 1024
            || answer
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return Err(Error::Refused("入力が長すぎるか制御文字を含みます".into()));
        }
        if !pane
            .agent
            .as_ref()
            .is_some_and(|agent| matches!(agent.state.as_deref(), Some("input" | "permission")))
        {
            return Err(Error::Refused(
                "エージェントは現在入力を求めていません".into(),
            ));
        }
        self.client
            .send_text_with(
                pane.pane_id.0,
                answer,
                SendTextOptions {
                    paste: true,
                    require_paste_mode: true,
                    refuse_shell: true,
                },
            )
            .map_err(error)?;
        self.client
            .send_text_with(
                pane.pane_id.0,
                "\r",
                SendTextOptions {
                    paste: false,
                    require_paste_mode: false,
                    refuse_shell: true,
                },
            )
            .map_err(error)
    }
}

pub fn matched<'a>(
    panels: &'a [Panel],
    label: &str,
    directory: &Path,
) -> Result<Option<&'a Panel>, String> {
    let candidates: Vec<_> = panels
        .iter()
        .filter(|p| p.label.as_deref() == Some(label))
        .collect();
    if candidates.len() > 1 {
        return Err("同じ実行識別子のペインが複数あります。自動操作を停止しました".into());
    }
    let Some(panel) = candidates.first().copied() else {
        return Ok(None);
    };
    let actual =
        std::fs::canonicalize(&panel.cwd).map_err(|_| "ペインの作業場所を確認できません")?;
    let expected =
        std::fs::canonicalize(directory).map_err(|_| "保存済みworktreeを確認できません")?;
    if actual != expected {
        return Err("ペインの作業場所が実行記録と一致しません".into());
    }
    Ok(Some(panel))
}

fn supported_version(version: &str) -> bool {
    let version = version
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()
        .unwrap_or("");
    let parts: Vec<u64> = version.split('.').filter_map(|s| s.parse().ok()).collect();
    parts.len() == 3 && (parts[0], parts[1], parts[2]) >= (0, 9, 13)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_rpc_outcomes_require_reconciliation_before_retrying() {
        let result = error(ClientError::Rpc {
            code: noa_ipc::ErrorCode::OutcomeUnknown.code(),
            message: "request timed out; the worker may still start".into(),
        });
        assert!(matches!(result, Error::Uncertain(_)));
    }

    #[test]
    fn definite_rpc_and_local_refusals_remain_retryable_failures() {
        for code in [
            noa_ipc::ErrorCode::InvalidParams,
            noa_ipc::ErrorCode::NotReady,
            noa_ipc::ErrorCode::LimitReached,
        ] {
            assert_eq!(
                error(ClientError::Rpc {
                    code: code.code(),
                    message: "refused".into(),
                }),
                Error::Refused(format!("refused ({})", code.code()))
            );
        }
        assert_eq!(
            error(ClientError::InvalidInput("invalid request".into())),
            Error::Refused("invalid request".into())
        );
    }

    #[test]
    fn missing_responses_remain_uncertain() {
        for failure in [
            ClientError::Timeout("response deadline elapsed".into()),
            ClientError::Transport("connection reset".into()),
            ClientError::Closed("server disconnected".into()),
        ] {
            assert!(matches!(error(failure), Error::Uncertain(_)));
        }
    }

    #[test]
    fn only_fixed_agent_commands_can_be_spawned() {
        assert!(agent_argv("codex; echo secret").is_err());
        assert_eq!(
            agent_argv("codex").unwrap()[4],
            "exec codex \"$NOA_WORKFLOW_PROMPT\""
        );
        assert_eq!(
            agent_argv("agy").unwrap()[4],
            "exec agy -i \"$NOA_WORKFLOW_PROMPT\""
        );
    }

    #[test]
    fn old_servers_do_not_get_an_unguarded_fallback() {
        assert!(!supported_version("0.9.12"));
        assert!(!supported_version("unknown"));
        assert!(supported_version("0.9.20-dev"));
    }
}
