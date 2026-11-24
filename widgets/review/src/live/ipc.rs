//! What the session runner needs from Noa, as a trait ([`Noa`]), and that
//! trait over Noa's control server ([`IpcConnect`] / [`IpcNoa`], `noa-ipc`).
//! The runner sees only the trait, so its tests run against a fake Noa.

use std::path::PathBuf;
use std::time::Duration;

use noa_ipc::{
    Client, ClientError, EventKind, Notification, Panel, Scope, ScopeSet, SendTextOptions,
    SpawnConfig, SpawnParams, SplitPlacement, TextSource,
};

/// JSON-RPC codes the runner tells apart (as `noa +ctl` does,
/// crates/noa-app/src/ctl.rs).
const RPC_UNKNOWN_PANE: i64 = -32002;
const RPC_PANE_CLOSED: i64 = -32004;
/// A paste refused with `requirePasteMode` (the pane has not enabled
/// bracketed paste yet) or `refuseShell` (the pane is back at its shell):
/// nothing was written.
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
    /// The person works with this pane (a fix request): it opens with
    /// keyboard focus and without auto approval, whatever its placement.
    pub attended: bool,
    /// Environment the pane's process gets on top of Noa's.
    pub env: Vec<(String, String)>,
}

/// Where a new pane goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// A tab of its own (in the window Noa picks).
    NewTab,
    /// Split off to the right of this pane.
    Right(u64),
    /// Split off below this pane.
    Below(u64),
}

/// The Noa a session runner talks to over one connection.
pub trait Noa {
    fn panes(&mut self) -> Result<Vec<PaneView>, NoaError>;
    /// The next state change, waiting up to `wait`; `None` when none came.
    fn changes(&mut self, wait: Duration) -> Result<Option<Changes>, NoaError>;
    /// Starts a pane and returns its id. Keyboard focus stays where it was.
    fn spawn(&mut self, placement: Placement, spec: &PaneSpec) -> Result<u64, NoaError>;
    /// Whether `pane` takes a paste as one (it has enabled bracketed paste).
    fn paste_ready(&mut self, pane: u64) -> Result<bool, NoaError>;
    /// The text `pane` shows.
    fn screen(&mut self, pane: u64) -> Result<String, NoaError>;
    /// Types `text` into `pane`: as one bracketed paste with `paste`
    /// (refused with [`NoaError::NotReady`] while the pane has not enabled
    /// it, when `text` holds a control character), else as raw bytes.
    /// Refused with [`NoaError::NotReady`], nothing written, while the
    /// pane's foreground process is a plain shell (the agent exited).
    fn type_text(&mut self, pane: u64, text: &str, paste: bool) -> Result<(), NoaError>;
    /// Brings `pane` (and its window and tab) to the front with keyboard
    /// focus (`noa.focusPane`).
    fn focus(&mut self, pane: u64) -> Result<(), NoaError>;
    /// Closes `pane` (`noa.closePane`).
    fn close_pane(&mut self, pane: u64) -> Result<(), NoaError>;
    /// The version this connection's `noa.hello` gave (`serverVersion`):
    /// what a request needs of Noa is judged on the server that takes it.
    fn server_version(&self) -> String;
    /// This connection's place among every hello the window completed
    /// ([`next_hello`]): a later one is newer, whatever order the reports
    /// of them arrive in.
    fn hello_seq(&self) -> u64;
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
    /// `noa.hello`'s `serverVersion`.
    version: String,
    /// When its hello completed ([`next_hello`]).
    seq: u64,
}

/// Numbers every `noa.hello` the window completes, in the order they
/// complete, so the version of the newest connection wins on screen.
static HELLOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The next hello's number (starting at 1).
pub(crate) fn next_hello() -> u64 {
    HELLOS.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1
}

impl Connect for IpcConnect {
    type Conn = IpcNoa;

    fn connect(&mut self) -> Result<IpcNoa, String> {
        let (url, token) = endpoint()?;
        let scopes = ScopeSet::from_strings(["read", "control", "input"]);
        let mut client = match Client::connect(&url, &token, scopes) {
            Ok(client) => client,
            Err(ClientError::Rpc { .. }) => {
                return Err(
                    "レビューにはエージェントの起動と入力が要ります（server-scopes = read,control,input）"
                        .into(),
                );
            }
            Err(other) => return Err(format!("{url} に接続できません: {other}")),
        };
        // Judged on this connection's own hello, the one every request goes
        // through: an older Noa ignores `refuseShell` and would type into a
        // shell.
        let version = client.server_version().to_owned();
        let seq = next_hello();
        if !honours_refuse_shell(&version) {
            return Err(UNSAFE_SEND.to_owned());
        }
        let granted = client.granted_scopes();
        if !granted.contains(Scope::Control) || !granted.contains(Scope::Input) {
            return Err(
                "レビューにはエージェントの起動と入力が要ります（server-scopes = read,control,input）"
                    .into(),
            );
        }
        client
            .subscribe(&[EventKind::StateChanged], None)
            .map_err(|err| format!("購読できません: {err}"))?;
        Ok(IpcNoa {
            client,
            version,
            seq,
        })
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

/// The running Noa's version (`noa.hello` only, read scope), for what the
/// window shows before any review connects; `None` when it cannot tell.
/// Only for display: what a request may do is judged on its own
/// connection ([`Noa::server_version`]).
pub fn probe_version() -> Option<(u64, String)> {
    let (url, token) = endpoint().ok()?;
    let version = server_version(&url, &token)?;
    Some((next_hello(), version))
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
    }
}

/// Why the review cannot reach Noa when its config turns the server off.
pub(crate) const SERVER_DISABLED: &str = "設定でサーバーが無効です（server-enable = true が必要）";

/// Why the review sends nothing to a Noa not known to honour `refuseShell`.
pub(crate) const UNSAFE_SEND: &str =
    "この Noa はシェルに戻ったペインへの送信を防げません（Noa 0.9.12 以降が必要）";

/// The first Noa whose `noa.sendText` honours `refuseShell`: an older one
/// ignores the field (as `widgets/agent-dashboard` checks it).
const REFUSE_SHELL_SINCE: (u64, u64, u64) = (0, 9, 12);

/// The first Noa that takes `autoApprove` on `noa.newTab` (the per-tab
/// setting came with v0.9.13; 0.9.12 reads the request without it).
const TAB_AUTO_APPROVE_SINCE: (u64, u64, u64) = (0, 9, 13);

/// Why 修正を依頼 is not offered on an older Noa.
pub(crate) fn no_tab_auto_approve(version: &str) -> String {
    format!(
        "この Noa（{version}）は新しいタブの自動承認を止められないため、修正を依頼できません（Noa 0.9.13 以降が必要）"
    )
}

/// `version` as `MAJOR.MINOR.PATCH`, read strictly: the whole string must
/// be three decimal numbers, optionally followed by `-<pre-release>` and
/// `+<build>` of dot-separated `[0-9A-Za-z-]` parts (semver's shape).
/// Anything else (`v0.9.13`, `0.9.13.1`, `0.9.13.invalid`, `0.9`) is `None`.
fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let ident = |part: &str| {
        !part.is_empty()
            && part
                .split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    };
    let (rest, build) = match version.split_once('+') {
        Some((rest, build)) => (rest, Some(build)),
        None => (version, None),
    };
    if build.is_some_and(|b| !ident(b)) {
        return None;
    }
    let (core, pre) = match rest.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (rest, None),
    };
    if pre.is_some_and(|p| !ident(p)) {
        return None;
    }
    let number = |n: &str| {
        (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .then(|| n.parse().ok())
            .flatten()
    };
    let mut parts = core.split('.');
    let parsed = (
        number(parts.next()?)?,
        number(parts.next()?)?,
        number(parts.next()?)?,
    );
    parts.next().is_none().then_some(parsed)
}

/// Whether `version` (`noa.hello`'s `serverVersion`) is `since` or later.
/// A version that does not parse strictly is not.
fn at_least(version: &str, since: (u64, u64, u64)) -> bool {
    parse_version(version).is_some_and(|v| v >= since)
}

fn honours_refuse_shell(version: &str) -> bool {
    at_least(version, REFUSE_SHELL_SINCE)
}

/// Whether a Noa of `version` takes `autoApprove` on `noa.newTab`.
pub(crate) fn sets_tab_auto_approve(version: &str) -> bool {
    at_least(version, TAB_AUTO_APPROVE_SINCE)
}

/// The server's version from a `noa.hello` of our own: `noa_ipc::Client`
/// reads it but does not keep it. `None` when it cannot be read.
fn server_version(url: &str, token: &str) -> Option<String> {
    use tungstenite::stream::MaybeTlsStream;
    let (mut socket, _) = tungstenite::connect(url).ok()?;
    if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .ok()?;
    }
    let hello = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "noa.hello",
        "params": {
            "protocolVersion": noa_ipc::PROTOCOL_VERSION,
            "token": token,
            "scopes": ["read"],
        },
    });
    socket
        .send(tungstenite::Message::Text(hello.to_string()))
        .ok()?;
    let version = loop {
        let tungstenite::Message::Text(text) = socket.read().ok()? else {
            continue;
        };
        let reply: serde_json::Value = serde_json::from_str(&text).ok()?;
        if reply["id"] == 1 {
            break reply["result"]["serverVersion"].as_str().map(str::to_owned);
        }
    };
    let _ = socket.close(None);
    version
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
    fn server_version(&self) -> String {
        self.version.clone()
    }

    fn hello_seq(&self) -> u64 {
        self.seq
    }

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

    fn spawn(&mut self, placement: Placement, spec: &PaneSpec) -> Result<u64, NoaError> {
        let params = spawn_params(placement, spec);
        let pane = match placement {
            Placement::NewTab => self.client.spawn_tab(None, &params),
            Placement::Right(pane) => {
                self.client
                    .spawn_split(pane, SplitPlacement::Horizontal, &params)
            }
            Placement::Below(pane) => {
                self.client
                    .spawn_split(pane, SplitPlacement::Vertical, &params)
            }
        };
        pane.map_err(noa_error)
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
            // A brief (or its Enter) typed into a shell would run as a
            // command line: Noa probes the foreground process and refuses.
            refuse_shell: true,
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

/// The `noa.newTab`/`noa.split` request for one review pane.
fn spawn_params(placement: Placement, spec: &PaneSpec) -> SpawnParams {
    SpawnParams {
        config: Some(SpawnConfig {
            command: Some(spec.argv.clone()),
            cwd: Some(spec.cwd.to_string_lossy().into_owned()),
            env: spec.env.iter().cloned().collect(),
            ..SpawnConfig::default()
        }),
        label: Some(spec.label.clone()),
        focus: Some(spec.attended),
        // Reviewers and the lead run unattended in their own tab, so
        // its agent prompts are answered without the user (the later
        // splits share the tab's setting). A fix edits the work tree: its
        // tab is set to ask the person, said outright, since a tab Noa
        // opens otherwise takes the global `auto-approve` setting.
        auto_approve: matches!(placement, Placement::NewTab).then_some(!spec.attended),
        ..SpawnParams::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_noa_that_refuses_shell_sends_is_written_to() {
        for (version, ok) in [
            ("0.9.12", true),
            ("0.9.14", true),
            ("1.0.0-rc.1", true),
            ("0.9.11", false),
            ("0.9", false),
            ("t", false),
        ] {
            assert_eq!(honours_refuse_shell(version), ok, "{version}");
        }
    }

    #[test]
    fn only_a_noa_from_0_9_13_sets_a_new_tabs_auto_approve() {
        for (version, ok) in [
            ("0.9.12", false),
            ("0.9.12-dev+abc", false),
            ("0.9.13", true),
            ("0.9.13-dev", true),
            ("0.9.13-rc.1+build.5", true),
            ("0.10.0", true),
            ("1.0.0", true),
            ("nope", false),
            // Read strictly: the whole string, not a prefix of it.
            ("0.9.13.invalid", false),
            ("0.9.13.1", false),
            ("v0.9.13", false),
            ("0.9", false),
            ("0.9.13-", false),
            ("0.9.13+", false),
            ("0.9.13-a..b", false),
            (" 0.9.13", false),
            ("0.9.+13", false),
        ] {
            assert_eq!(sets_tab_auto_approve(version), ok, "{version}");
        }
        assert!(no_tab_auto_approve("0.9.12").contains("0.9.13 以降が必要"));
    }

    #[test]
    fn only_the_review_tab_turns_auto_approve_on() {
        let spec = PaneSpec {
            argv: vec!["codex".into()],
            cwd: PathBuf::from("/tmp"),
            label: "codex".into(),
            attended: false,
            env: Vec::new(),
        };
        assert_eq!(
            spawn_params(Placement::NewTab, &spec).auto_approve,
            Some(true)
        );
        assert_eq!(spawn_params(Placement::NewTab, &spec).focus, Some(false));
        for split in [Placement::Right(7), Placement::Below(7)] {
            assert_eq!(spawn_params(split, &spec).auto_approve, None);
        }
    }

    #[test]
    fn an_attended_pane_takes_focus_and_never_auto_approves() {
        let spec = PaneSpec {
            argv: vec!["codex".into()],
            cwd: PathBuf::from("/tmp"),
            label: "fix · rev-1".into(),
            attended: true,
            env: vec![("K".into(), "v".into())],
        };
        // A new tab is told outright not to auto-approve: left out, Noa
        // would give it the global `auto-approve` setting.
        let tab = spawn_params(Placement::NewTab, &spec);
        assert_eq!(tab.auto_approve, Some(false));
        assert_eq!(tab.focus, Some(true));
        assert_eq!(
            tab.config.unwrap().env.get("K").map(String::as_str),
            Some("v")
        );
        // A split shares its tab's setting, so it sends none.
        for placement in [Placement::Right(7), Placement::Below(7)] {
            let params = spawn_params(placement, &spec);
            assert_eq!(params.auto_approve, None, "{placement:?}");
            assert_eq!(params.focus, Some(true), "{placement:?}");
        }
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
