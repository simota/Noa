//! What runs need from Noa, as a trait ([`Noa`]), and that trait over
//! Noa's control server ([`IpcConnect`] / [`IpcNoa`], `noa-ipc`). Runs see
//! only the trait, so their tests run against a fake Noa.

use std::path::PathBuf;

use noa_ipc::{
    Client, ClientError, Panel, Scope, ScopeSet, SpawnConfig, SpawnParams, SplitPlacement, WireId,
};

use tasks::model::types::{AgentState, PaneFact};

/// JSON-RPC codes told apart (as `noa +ctl` does, crates/noa-app/src/ctl.rs).
const RPC_UNKNOWN_PANE: i64 = -32002;
const RPC_PANE_CLOSED: i64 = -32004;
/// A paste refused with `requirePasteMode`: nothing was written.
const RPC_NOT_READY: i64 = -32010;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoaError {
    /// The connection is gone; reconnect.
    Link(String),
    /// The pane is gone: closed, or its program exited.
    PaneGone,
    /// A paste was refused because the pane has not enabled bracketed paste
    /// yet. Nothing was written.
    NotReady,
    /// Noa refused the request; the connection is fine.
    Refused(String),
}

impl std::fmt::Display for NoaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoaError::Link(why) => write!(f, "connection lost: {why}"),
            NoaError::PaneGone => f.write_str("the pane is closed"),
            NoaError::NotReady => f.write_str("the pane has not enabled bracketed paste yet"),
            NoaError::Refused(why) => f.write_str(why),
        }
    }
}

/// A pane, as far as runs care.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneView {
    pub pane_id: u64,
    /// The pane it is stowed on (`Panel.parentPaneId`): the board tab for
    /// a run's float.
    pub parent_pane_id: Option<u64>,
    /// The label it was spawned with.
    pub label: Option<String>,
    /// `running` / `permission` / `input` / `finished` / `error`: the agent's
    /// last report (`Panel.agent.state`).
    pub agent_state: Option<String>,
    /// How many reports the agent has made (`Panel.agent.seq`).
    pub agent_seq: u64,
    /// What the agent is blocked on (`Panel.agent.blocked`: `permission` or
    /// `question`), such as Claude Code's folder trust prompt.
    pub agent_blocked: Option<String>,
    /// The agent Noa sees in front (`Panel.agent.kind`: `claude`, `codex`,
    /// `agy`), from the foreground process name.
    pub agent_kind: Option<String>,
    /// The pane's child exited and the pane is held open (`Panel.exit`).
    pub exited: bool,
    /// The report's detail (`Panel.agent.detail`), e.g. `trust this folder`
    /// or Antigravity's `Sign in required`.
    pub agent_detail: Option<String>,
}

/// What a new pane runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSpec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub label: String,
    /// The person works with this pane (every run, Decision 7): a float
    /// opens with keyboard focus; a board tab spawned with an attended spec
    /// is set to ask the person (no auto approval), and every float on it
    /// takes that setting.
    pub attended: bool,
    /// Environment the pane's process gets on top of Noa's.
    pub env: Vec<(String, String)>,
}

/// Where a new pane goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// A new tab that is a board (`noa.newTab` with `board: true`); the
    /// result is the board's pane id, and only the spec's cwd and env are
    /// used. The repository's runs float on it.
    Board,
    /// A float stowed on this board (`noa.split` float with the board as
    /// its parent).
    Float(u64),
}

/// The Noa runs talk to over one connection.
pub trait Noa {
    fn panes(&mut self) -> Result<Vec<PaneView>, NoaError>;
    /// Starts a pane and returns its id.
    fn spawn(&mut self, placement: Placement, spec: &PaneSpec) -> Result<u64, NoaError>;
    /// Brings `pane` (and its window and tab) to the front with keyboard
    /// focus (`noa.focusPane`).
    fn focus(&mut self, pane: u64) -> Result<(), NoaError>;
    /// Closes `pane` (`noa.closePane`).
    fn close_pane(&mut self, pane: u64) -> Result<(), NoaError>;
    /// The version this connection's `noa.hello` gave (`serverVersion`).
    fn server_version(&self) -> String;
    /// Whether this connection's Noa opens a board tab (`noa.newTab` with
    /// `board: true`): its `noa.hello` listed `boardTab`. An older one
    /// would open a plain shell tab instead.
    fn supports_board_tab(&self) -> bool;
}

/// Opens connections to Noa; called again after a lost one.
pub trait Connect {
    type Conn: Noa;
    fn connect(&mut self) -> Result<Self::Conn, String>;
}

/// [`Connect`] to the running Noa's control server, found the way
/// `widgets/repo-browser` finds it: the config's bind/port and token.
#[derive(Debug, Default)]
pub struct IpcConnect;

pub struct IpcNoa {
    client: Client,
    /// `noa.hello`'s `serverVersion`.
    version: String,
}

/// Why runs cannot start without the scopes: a board tab runs a program
/// (`input`) and a float is spawned, focused and closed (`control`).
const NEEDS_SCOPES: &str = "エージェントの起動には server-scopes = read,control,input が要ります";

/// The scopes asked for, each of which must be granted: `read` lists the
/// panes, `control` and `input` start a run.
const SCOPES: [Scope; 3] = [Scope::Read, Scope::Control, Scope::Input];

/// Whether `granted` holds every one of [`SCOPES`]; the server grants what
/// was asked for and allowed, so a missing one does not fail `noa.hello`.
fn has_scopes(granted: ScopeSet) -> bool {
    SCOPES.iter().all(|scope| granted.contains(*scope))
}

impl Connect for IpcConnect {
    type Conn = IpcNoa;

    fn connect(&mut self) -> Result<IpcNoa, String> {
        let (url, token) = endpoint()?;
        let scopes = ScopeSet::from_strings(["read", "control", "input"]);
        let client = match Client::connect(&url, &token, scopes) {
            Ok(client) => client,
            Err(ClientError::Rpc { .. }) => return Err(NEEDS_SCOPES.into()),
            Err(other) => return Err(format!("{url} に接続できません: {other}")),
        };
        let version = client.server_version().to_owned();
        if !has_scopes(client.granted_scopes()) {
            return Err(NEEDS_SCOPES.into());
        }
        Ok(IpcNoa { client, version })
    }
}

/// The control server's URL and token, from the config.
fn endpoint() -> Result<(String, String), String> {
    let (config, _diagnostics) =
        noa_config::load_startup_config(noa_config::ConfigOverrides::default())
            .map_err(|err| format!("設定を読めません: {err:#}"))?;
    if !config.server_enable {
        return Err(SERVER_DISABLED.into());
    }
    let token = match config.server_token.filter(|token| !token.trim().is_empty()) {
        Some(token) => token,
        None => {
            let path = noa_config::server_token_path()
                .ok_or_else(|| "トークンの場所がわかりません".to_owned())?;
            std::fs::read_to_string(&path)
                .map(|token| token.trim().to_owned())
                .ok()
                .filter(|token| !token.is_empty())
                .ok_or_else(|| format!("トークンがありません: {}", path.display()))?
        }
    };
    Ok((control_url(&config.server_bind, config.server_port), token))
}

fn control_url(bind: &str, port: u16) -> String {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    let ip = match bind.parse().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)) {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    format!("ws://{}/", SocketAddr::new(ip, port))
}

fn view(panel: &Panel) -> PaneView {
    PaneView {
        pane_id: panel.pane_id.0,
        parent_pane_id: panel.parent_pane_id.map(|id| id.0),
        label: panel.label.clone(),
        agent_state: panel.agent.as_ref().and_then(|a| a.state.clone()),
        agent_seq: panel.agent.as_ref().map_or(0, |a| a.seq),
        agent_blocked: panel.agent.as_ref().and_then(|a| a.blocked.clone()),
        agent_detail: panel.agent.as_ref().and_then(|a| a.detail.clone()),
        agent_kind: panel.agent.as_ref().and_then(|a| a.kind.clone()),
        exited: panel.exit.is_some(),
    }
}

/// What runs know of `pane`: an agent state Noa reports that this widget
/// does not know reads as none.
pub fn pane_fact(pane: &PaneView) -> PaneFact {
    PaneFact {
        pane_id: pane.pane_id,
        parent: pane.parent_pane_id,
        label: pane.label.clone(),
        agent: pane.agent_state.as_deref().and_then(agent_state),
        exited: pane.exited,
    }
}

fn agent_state(state: &str) -> Option<AgentState> {
    Some(match state {
        "running" => AgentState::Running,
        "permission" => AgentState::Permission,
        "input" => AgentState::Input,
        "finished" => AgentState::Finished,
        "error" => AgentState::Error,
        _ => return None,
    })
}

/// Why runs cannot reach Noa when its config turns the server off.
pub(crate) const SERVER_DISABLED: &str = "設定でサーバーが無効です（server-enable = true が必要）";

fn noa_error(err: ClientError) -> NoaError {
    match err {
        ClientError::Rpc {
            code: RPC_UNKNOWN_PANE | RPC_PANE_CLOSED,
            ..
        } => NoaError::PaneGone,
        ClientError::Rpc {
            code: RPC_NOT_READY,
            ..
        } => NoaError::NotReady,
        ClientError::Rpc { code, message } => NoaError::Refused(format!("{message} ({code})")),
        // The connection is still up after these.
        ClientError::InvalidInput(why) | ClientError::Timeout(why) => NoaError::Refused(why),
        other => NoaError::Link(other.to_string()),
    }
}

impl Noa for IpcNoa {
    fn server_version(&self) -> String {
        self.version.clone()
    }

    fn supports_board_tab(&self) -> bool {
        self.client.supports_board_tab()
    }

    fn panes(&mut self) -> Result<Vec<PaneView>, NoaError> {
        let panels = self.client.list_panels().map_err(noa_error)?;
        Ok(panels.iter().map(view).collect())
    }

    fn spawn(&mut self, placement: Placement, spec: &PaneSpec) -> Result<u64, NoaError> {
        let params = spawn_params(placement, spec);
        let pane = match placement {
            Placement::Board => self.client.spawn_board_tab(None, &params),
            Placement::Float(board) => {
                self.client
                    .spawn_split(board, SplitPlacement::Float, &params)
            }
        };
        pane.map_err(noa_error)
    }

    fn focus(&mut self, pane: u64) -> Result<(), NoaError> {
        self.client.focus_pane(pane).map_err(noa_error)
    }

    fn close_pane(&mut self, pane: u64) -> Result<(), NoaError> {
        self.client.close_pane(pane).map_err(noa_error)
    }
}

/// The most panes a board holds (`MAX_STOWED_PER_BOARD`, agent-orchestration
/// D12): a float over it is refused.
const BOARD_MAX_CHILDREN: u32 = 20;

/// The `noa.newTab`/`noa.split` request for one pane.
fn spawn_params(placement: Placement, spec: &PaneSpec) -> SpawnParams {
    let cwd = Some(spec.cwd.to_string_lossy().into_owned());
    match placement {
        // The server sets a board's command and label itself and refuses
        // them: only cwd and env. Auto approval is per tab and every float
        // takes its board tab's, so it is said outright here: off for an
        // attended spec (runs edit a work tree, Decision 7), since a tab
        // Noa opens otherwise takes the global `auto-approve` setting. The
        // board tab never takes focus.
        Placement::Board => SpawnParams {
            config: Some(SpawnConfig {
                cwd,
                env: spec.env.iter().cloned().collect(),
                ..SpawnConfig::default()
            }),
            focus: Some(false),
            auto_approve: Some(!spec.attended),
            ..SpawnParams::default()
        },
        // A float shares its board tab's auto approval: none is sent.
        Placement::Float(board) => SpawnParams {
            config: Some(SpawnConfig {
                command: Some(spec.argv.clone()),
                cwd,
                env: spec.env.iter().cloned().collect(),
                ..SpawnConfig::default()
            }),
            label: Some(spec.label.clone()),
            focus: Some(spec.attended),
            auto_approve: None,
            parent_pane_id: Some(WireId(board)),
            max_children: Some(BOARD_MAX_CHILDREN),
        },
    }
}

/// Why no run starts on an older Noa.
pub(crate) fn no_board_tab(version: &str) -> String {
    format!(
        "この Noa（{version}）はボードのタブを開けないため、エージェントを起動できません（Noa 0.9.16 以降が必要）"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(attended: bool) -> PaneSpec {
        PaneSpec {
            argv: vec!["codex".into()],
            cwd: PathBuf::from("/tmp"),
            label: "task:run-1".into(),
            attended,
            env: vec![("K".into(), "v".into())],
        }
    }

    #[test]
    fn a_connection_without_read_control_or_input_is_refused() {
        assert!(has_scopes(ScopeSet::from_strings([
            "read", "control", "input"
        ])));
        // `server-scopes = control,input`: hello succeeds without `read`,
        // and noa.listPanels would then be refused.
        assert!(!has_scopes(ScopeSet::from_strings(["control", "input"])));
        assert!(!has_scopes(ScopeSet::from_strings(["read", "input"])));
        assert!(!has_scopes(ScopeSet::from_strings(["read", "control"])));
    }

    #[test]
    fn an_attended_board_tab_is_told_not_to_auto_approve_and_takes_only_cwd_and_env() {
        let board = spawn_params(Placement::Board, &spec(true));
        assert_eq!(board.auto_approve, Some(false));
        // The board tab never takes focus; the run's float does.
        assert_eq!(board.focus, Some(false));
        assert_eq!(board.label, None);
        assert_eq!(board.parent_pane_id, None);
        assert_eq!(board.max_children, None);
        let config = board.config.unwrap();
        // The server refuses a command (and a label) on a board tab.
        assert_eq!(config.command, None);
        assert_eq!(config.cwd.as_deref(), Some("/tmp"));
        assert_eq!(config.env.get("K").map(String::as_str), Some("v"));
        assert_eq!(
            spawn_params(Placement::Board, &spec(false)).auto_approve,
            Some(true)
        );
    }

    #[test]
    fn a_float_names_its_board_as_parent_takes_focus_when_attended_and_sends_no_auto_approve() {
        let float = spawn_params(Placement::Float(7), &spec(true));
        assert_eq!(float.parent_pane_id, Some(WireId(7)));
        assert_eq!(float.max_children, Some(20));
        // It takes the board tab's setting.
        assert_eq!(float.auto_approve, None);
        assert_eq!(float.focus, Some(true));
        assert_eq!(float.label.as_deref(), Some("task:run-1"));
        let config = float.config.unwrap();
        assert_eq!(config.command, Some(vec!["codex".to_owned()]));
        assert_eq!(config.env.get("K").map(String::as_str), Some("v"));
        assert_eq!(
            spawn_params(Placement::Float(7), &spec(false)).focus,
            Some(false)
        );
    }

    #[test]
    fn a_pane_fact_keeps_parent_label_state_and_exit() {
        let pane = PaneView {
            pane_id: 4,
            parent_pane_id: Some(2),
            label: Some("task:run-1".into()),
            agent_state: Some("finished".into()),
            agent_seq: 3,
            agent_blocked: None,
            agent_kind: Some("claude".into()),
            exited: true,
            agent_detail: None,
        };
        assert_eq!(
            pane_fact(&pane),
            PaneFact {
                pane_id: 4,
                parent: Some(2),
                label: Some("task:run-1".into()),
                agent: Some(AgentState::Finished),
                exited: true,
            }
        );
        for (state, agent) in [
            ("running", Some(AgentState::Running)),
            ("permission", Some(AgentState::Permission)),
            ("input", Some(AgentState::Input)),
            ("error", Some(AgentState::Error)),
            ("dreaming", None),
        ] {
            let pane = PaneView {
                agent_state: Some(state.into()),
                ..pane.clone()
            };
            assert_eq!(pane_fact(&pane).agent, agent, "{state}");
        }
    }

    #[test]
    fn the_board_tab_refusal_names_the_version_and_what_is_needed() {
        assert!(no_board_tab("0.9.15").contains("0.9.15"));
        assert!(no_board_tab("0.9.15").contains("0.9.16 以降が必要"));
    }

    #[test]
    fn client_errors_map_to_what_runs_do_next() {
        let rpc = |code| ClientError::Rpc {
            code,
            message: "m".into(),
        };
        assert_eq!(noa_error(rpc(-32002)), NoaError::PaneGone);
        assert_eq!(noa_error(rpc(-32004)), NoaError::PaneGone);
        assert_eq!(noa_error(rpc(-32010)), NoaError::NotReady);
        assert_eq!(
            noa_error(rpc(-32003)),
            NoaError::Refused("m (-32003)".into())
        );
        assert_eq!(
            noa_error(ClientError::Timeout("t".into())),
            NoaError::Refused("t".into())
        );
        assert!(matches!(
            noa_error(ClientError::Closed("bye".into())),
            NoaError::Link(_)
        ));
    }

    #[test]
    fn an_unspecified_bind_connects_to_loopback() {
        assert_eq!(control_url("0.0.0.0", 7700), "ws://127.0.0.1:7700/");
        assert_eq!(control_url("::", 7700), "ws://[::1]:7700/");
        assert_eq!(control_url("nonsense", 1), "ws://127.0.0.1:1/");
    }
}
