//! What the session runner needs from Noa, as a trait ([`Noa`]), and that
//! trait over Noa's control server ([`IpcConnect`] / [`IpcNoa`], `noa-ipc`).
//! The runner sees only the trait, so its tests run against a fake Noa.

use std::path::PathBuf;
use std::time::Duration;

use noa_ipc::{
    Client, ClientError, EventKind, Notification, Panel, Scope, ScopeSet, SendTextOptions,
    SpawnConfig, SpawnParams, SplitPlacement, TextSource, WireId,
};

/// JSON-RPC codes the runner tells apart (as `noa +ctl` does,
/// crates/noa-app/src/ctl.rs).
const RPC_UNKNOWN_PANE: i64 = -32002;
const RPC_PANE_CLOSED: i64 = -32004;
/// A paste refused with `requirePasteMode`: the pane has not enabled
/// bracketed paste yet, and nothing was written.
const RPC_NOT_READY: i64 = -32010;
const SCREEN_MAX_BYTES: usize = 8192;

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

/// A pane, as far as the runner cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneView {
    pub pane_id: u64,
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
    /// The report's detail (`Panel.agent.detail`), e.g. `trust this folder`
    /// or Antigravity's `Sign in required`.
    pub agent_detail: Option<String>,
    /// The pane this one is stowed on (`Panel.parent_pane_id`): an agent's
    /// float names its board.
    pub parent_pane_id: Option<u64>,
}

/// What one `noa.stateChanged` said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// Panes that changed or were added.
    pub panes: Vec<PaneView>,
    /// Panes that closed.
    pub removed: Vec<u64>,
    /// Earlier notifications were lost: list the panes again.
    pub dropped: bool,
}

/// What a new pane runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSpec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub label: String,
}

/// Where a new pane goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// A new tab that is a board, then the pane as a float on it.
    Board,
    /// A float stowed on this board.
    Float(u64),
    /// A plain tab of its own, no board (the adopt tab, which is the user's).
    Tab,
}

/// A pane started by [`Noa::spawn`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spawned {
    pub pane: u64,
    /// The board the pane is stowed on (`None` for [`Placement::Tab`]).
    pub board: Option<u64>,
}

/// Most agent floats one board holds (`MAX_STOWED_PER_BOARD`, agent-orchestration D12).
const BOARD_MAX_PANES: u32 = 20;

/// The Noa a session runner talks to over one connection.
pub trait Noa {
    fn panes(&mut self) -> Result<Vec<PaneView>, NoaError>;
    /// The next state change, waiting up to `wait`; `None` when none came.
    fn changes(&mut self, wait: Duration) -> Result<Option<Changes>, NoaError>;
    /// Starts a pane (on a new board for [`Placement::Board`]). Keyboard
    /// focus stays where it was. [`NoaError::PaneGone`] means the board of a
    /// [`Placement::Float`] is gone.
    fn spawn(&mut self, placement: Placement, spec: &PaneSpec) -> Result<Spawned, NoaError>;
    /// `Err(version)` when Noa predates board tabs (`noa.newTab` with
    /// `board`): it would open a plain shell tab instead.
    fn board_tab_support(&self) -> Result<(), String>;
    /// Whether `pane` takes a paste as one (it has enabled bracketed paste).
    fn paste_ready(&mut self, pane: u64) -> Result<bool, NoaError>;
    /// The text `pane` shows.
    fn screen(&mut self, pane: u64) -> Result<String, NoaError>;
    /// Types `text` into `pane`: as one bracketed paste with `paste`
    /// (refused with [`NoaError::NotReady`] while the pane has not enabled
    /// it, when `text` holds a control character), else as raw bytes.
    fn type_text(&mut self, pane: u64, text: &str, paste: bool) -> Result<(), NoaError>;
    /// Brings `pane` (and its window and tab) to the front with keyboard
    /// focus (`noa.focusPane`).
    fn focus(&mut self, pane: u64) -> Result<(), NoaError>;
    /// Closes `pane` (`noa.closePane`).
    fn close_pane(&mut self, pane: u64) -> Result<(), NoaError>;
}

/// Opens connections to Noa; the runner calls it again after a lost one.
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
}

impl Connect for IpcConnect {
    type Conn = IpcNoa;

    fn connect(&mut self) -> Result<IpcNoa, String> {
        let (config, _diagnostics) =
            noa_config::load_startup_config(noa_config::ConfigOverrides::default())
                .map_err(|err| format!("設定を読めません: {err:#}"))?;
        if !config.server_enable {
            return Err("設定でサーバーが無効です（server-enable = true が必要）".into());
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
        let url = control_url(&config.server_bind, config.server_port);
        let scopes = ScopeSet::from_strings(["read", "control", "input"]);
        let mut client = match Client::connect(&url, &token, scopes) {
            Ok(client) => client,
            Err(ClientError::Rpc { .. }) => {
                return Err(
                    "討論にはエージェントの起動と入力が要ります（server-scopes = read,control,input）"
                        .into(),
                );
            }
            Err(other) => return Err(format!("{url} に接続できません: {other}")),
        };
        let granted = client.granted_scopes();
        if !granted.contains(Scope::Control) || !granted.contains(Scope::Input) {
            return Err(
                "討論にはエージェントの起動と入力が要ります（server-scopes = read,control,input）"
                    .into(),
            );
        }
        client
            .subscribe(&[EventKind::StateChanged], None)
            .map_err(|err| format!("購読できません: {err}"))?;
        Ok(IpcNoa { client })
    }
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
        label: panel.label.clone(),
        agent_state: panel.agent.as_ref().and_then(|a| a.state.clone()),
        agent_seq: panel.agent.as_ref().map_or(0, |a| a.seq),
        agent_blocked: panel.agent.as_ref().and_then(|a| a.blocked.clone()),
        agent_detail: panel.agent.as_ref().and_then(|a| a.detail.clone()),
        parent_pane_id: panel.parent_pane_id.map(|id| id.0),
    }
}

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
    fn panes(&mut self) -> Result<Vec<PaneView>, NoaError> {
        let panels = self.client.list_panels().map_err(noa_error)?;
        Ok(panels.iter().map(view).collect())
    }

    fn changes(&mut self, wait: Duration) -> Result<Option<Changes>, NoaError> {
        match self.client.next_notification(wait).map_err(noa_error)? {
            None => Ok(None),
            Some(Notification::StateChanged {
                panels,
                removed_pane_ids,
                dropped,
            }) => Ok(Some(Changes {
                panes: panels.iter().map(view).collect(),
                removed: removed_pane_ids,
                dropped,
            })),
            Some(Notification::Other { dropped, .. }) => Ok(dropped.then(|| Changes {
                dropped: true,
                ..Changes::default()
            })),
        }
    }

    fn spawn(&mut self, placement: Placement, spec: &PaneSpec) -> Result<Spawned, NoaError> {
        let board = match placement {
            Placement::Tab => {
                let pane = self
                    .client
                    .spawn_tab(None, &tab_params(spec))
                    .map_err(noa_error)?;
                return Ok(Spawned { pane, board: None });
            }
            Placement::Float(board) => board,
            Placement::Board => self
                .client
                .spawn_board_tab(None, &board_params(spec))
                .map_err(noa_error)?,
        };
        match self
            .client
            .spawn_split(board, SplitPlacement::Float, &float_params(board, spec))
        {
            Ok(pane) => Ok(Spawned {
                pane,
                board: Some(board),
            }),
            Err(err) => {
                // A board opened for this pane would stay empty.
                if placement == Placement::Board {
                    let _ = self.client.close_pane(board);
                }
                Err(noa_error(err))
            }
        }
    }

    fn board_tab_support(&self) -> Result<(), String> {
        if self.client.supports_board_tab() {
            Ok(())
        } else {
            Err(self.client.server_version().to_owned())
        }
    }

    fn paste_ready(&mut self, pane: u64) -> Result<bool, NoaError> {
        // An empty paste that requires paste mode writes nothing either way
        // (`noa +ctl send`'s probe).
        let options = SendTextOptions {
            paste: true,
            require_paste_mode: true,
            refuse_shell: false,
        };
        match self.client.send_text_with(pane, "", options) {
            Ok(()) => Ok(true),
            Err(err) => match noa_error(err) {
                NoaError::NotReady => Ok(false),
                other => Err(other),
            },
        }
    }

    fn screen(&mut self, pane: u64) -> Result<String, NoaError> {
        self.client
            .get_text(pane, TextSource::Screen, Some(SCREEN_MAX_BYTES))
            .map(|text| text.text)
            .map_err(noa_error)
    }

    fn type_text(&mut self, pane: u64, text: &str, paste: bool) -> Result<(), NoaError> {
        // Without bracketed paste every control character reaches the agent
        // as a key (each newline an Enter), so such a paste is refused.
        let options = SendTextOptions {
            paste,
            require_paste_mode: paste && text.chars().any(char::is_control),
            refuse_shell: false,
        };
        self.client
            .send_text_with(pane, text, options)
            .map_err(noa_error)
    }

    fn focus(&mut self, pane: u64) -> Result<(), NoaError> {
        self.client.focus_pane(pane).map_err(noa_error)
    }

    fn close_pane(&mut self, pane: u64) -> Result<(), NoaError> {
        self.client.close_pane(pane).map_err(noa_error)
    }
}

/// What every agent pane is spawned with.
fn agent_params(spec: &PaneSpec) -> SpawnParams {
    SpawnParams {
        config: Some(SpawnConfig {
            command: Some(spec.argv.clone()),
            cwd: Some(spec.cwd.to_string_lossy().into_owned()),
            ..SpawnConfig::default()
        }),
        label: Some(spec.label.clone()),
        focus: Some(false),
        ..SpawnParams::default()
    }
}

/// The `noa.newTab` request for a competition's board. Contestants and the
/// judges run unattended, so its agent prompts are answered without the user
/// (the floats take the tab's setting). The server takes only `cwd` and
/// `env` of the config, and no label or parent.
fn board_params(spec: &PaneSpec) -> SpawnParams {
    SpawnParams {
        config: Some(SpawnConfig {
            cwd: Some(spec.cwd.to_string_lossy().into_owned()),
            ..SpawnConfig::default()
        }),
        focus: Some(false),
        auto_approve: Some(true),
        ..SpawnParams::default()
    }
}

/// The `noa.split` request for one agent float stowed on `board`.
fn float_params(board: u64, spec: &PaneSpec) -> SpawnParams {
    SpawnParams {
        parent_pane_id: Some(WireId(board)),
        max_children: Some(BOARD_MAX_PANES),
        ..agent_params(spec)
    }
}

/// The `noa.newTab` request for a plain tab of one agent (the adopt tab).
fn tab_params(spec: &PaneSpec) -> SpawnParams {
    SpawnParams {
        auto_approve: Some(true),
        ..agent_params(spec)
    }
}

/// Why the agents are not started on an older Noa.
pub(crate) fn no_board_tab(version: &str) -> String {
    let (major, minor, patch) = noa_ipc::BOARD_TAB_SINCE;
    format!(
        "この Noa（{version}）はボードのタブを開けないため、エージェントを起動できません（Noa {major}.{minor}.{patch} 以降が必要）"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> PaneSpec {
        PaneSpec {
            argv: vec!["codex".into()],
            cwd: PathBuf::from("/tmp"),
            label: "codex".into(),
        }
    }

    #[test]
    fn only_the_board_tab_turns_auto_approve_on() {
        let board = board_params(&spec());
        assert_eq!(board.auto_approve, Some(true));
        // The server refuses a label, a parent or a command on a board tab.
        assert!(board.label.is_none() && board.parent_pane_id.is_none());
        let config = board.config.unwrap();
        assert!(config.command.is_none());
        assert_eq!(config.cwd.as_deref(), Some("/tmp"));
        assert_eq!(float_params(7, &spec()).auto_approve, None);
        assert_eq!(tab_params(&spec()).auto_approve, Some(true));
    }

    #[test]
    fn a_float_is_stowed_on_its_board() {
        let float = float_params(7, &spec());
        assert_eq!(float.parent_pane_id, Some(WireId(7)));
        assert_eq!(float.max_children, Some(20));
        assert_eq!(float.label.as_deref(), Some("codex"));
        assert_eq!(float.focus, Some(false));
    }

    #[test]
    fn client_errors_map_to_what_the_runner_does_next() {
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
        assert!(matches!(
            noa_error(ClientError::Transport("x".into())),
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
