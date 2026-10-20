//! The connection to the local noa-server, on its own thread. It owns the
//! synchronous `noa_ipc::Client`, polls `listPanels`, and publishes a
//! ready-to-draw [`Snapshot`]; the menu thread only reads that snapshot and
//! sends [`Command`]s back.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use noa_ipc::{Client, ClientError, Scope, ScopeSet};
use parking_lot::Mutex;

use crate::model::{PaneRow, Tracker};

const POLL_INTERVAL: Duration = Duration::from_millis(1500);
const RETRY_INTERVAL: Duration = Duration::from_secs(3);

/// Shown once, at pairing, next to the scope request (P-002 guardrail).
pub const CONTROL_REASON: &str = "Ember asks for control only to jump to the pane you pick.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    Connecting,
    /// `server-enable` is not on, so there is nothing to pair with.
    ServerOff,
    /// The server is on but no token is readable yet.
    NoToken,
    Unreachable(String),
    Paused,
    /// Paired. `control` is whether `focusPane` was granted.
    Live {
        control: bool,
    },
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub link: Link,
    pub rows: Vec<PaneRow>,
    /// `serverInstanceId` the rows' pane ids belong to (empty for servers
    /// that predate it, or when not live).
    pub instance: String,
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot {
            link: Link::Connecting,
            rows: Vec::new(),
            instance: String::new(),
        }
    }
}

pub enum Command {
    /// Jump to `pane` of the server instance the menu was built from; a pane
    /// id from a previous noa process is refused, since ids restart at 1.
    Focus {
        pane: u64,
        instance: String,
    },
    Pause(bool),
}

pub type Shared = Arc<Mutex<Snapshot>>;

/// Starts the link thread.
pub fn spawn() -> (Shared, Sender<Command>) {
    let shared: Shared = Arc::default();
    let (tx, rx) = crossbeam_channel::unbounded();
    let thread_shared = shared.clone();
    thread::Builder::new()
        .name("ember-link".to_string())
        .spawn(move || run(thread_shared, rx))
        .expect("spawn ember link thread");
    (shared, tx)
}

struct Pairing {
    url: String,
    token: String,
}

fn pairing() -> Result<Pairing, Link> {
    let overrides = match noa_config::load_file_overrides() {
        Ok((overrides, _)) => overrides,
        Err(err) => return Err(Link::Unreachable(format!("config: {err}"))),
    };
    if !overrides.server_enable.unwrap_or(false) {
        return Err(Link::ServerOff);
    }
    let port = overrides
        .server_port
        .unwrap_or(noa_config::DEFAULT_SERVER_PORT);
    let token = match overrides.server_token.filter(|t| !t.trim().is_empty()) {
        Some(token) => token,
        None => noa_config::server_token_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .ok_or(Link::NoToken)?,
    };
    Ok(Pairing {
        url: control_url(overrides.server_bind.as_deref(), port),
        token,
    })
}

/// The server listens only on `server-bind`, so connect there; a wildcard
/// bind is reached through the matching loopback. An unparsable value falls
/// back to 127.0.0.1, as the server itself does.
fn control_url(bind: Option<&str>, port: u16) -> String {
    let ip = match bind.and_then(|b| b.trim().parse::<IpAddr>().ok()) {
        Some(IpAddr::V4(ip)) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some(IpAddr::V6(ip)) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        Some(ip) => ip,
        None => IpAddr::V4(Ipv4Addr::LOCALHOST),
    };
    format!("ws://{}/", SocketAddr::new(ip, port))
}

fn requested_scopes() -> ScopeSet {
    let mut scopes = ScopeSet::empty();
    scopes.insert(Scope::Read);
    scopes.insert(Scope::Control);
    scopes
}

fn connect() -> Result<(Client, bool), Link> {
    let pairing = pairing()?;
    log::info!(
        "pairing with {}: requesting read + control. {CONTROL_REASON}",
        pairing.url
    );
    let client = Client::connect(&pairing.url, &pairing.token, requested_scopes())
        .map_err(|err| Link::Unreachable(describe(&err)))?;
    let control = client.granted_scopes().contains(Scope::Control);
    if !control {
        log::info!("control not granted; the list still works, jumping does not");
    }
    Ok((client, control))
}

fn describe(err: &ClientError) -> String {
    match err {
        ClientError::Rpc { code: -32001, .. } => "token rejected".to_string(),
        ClientError::Rpc { code: -32006, .. } => "noa version mismatch".to_string(),
        ClientError::Transport(_) => "noa isn't running".to_string(),
        other => other.to_string(),
    }
}

/// Pane ids restart at 1 in a new noa process, so per-pane state from another
/// instance must not carry over. A reconnect to the same instance keeps it.
fn adopt_instance(tracked: &mut Option<String>, tracker: &mut Tracker, instance: &str) {
    if tracked.as_deref() != Some(instance) {
        *tracker = Tracker::default();
        *tracked = Some(instance.to_string());
    }
}

fn run(shared: Shared, commands: Receiver<Command>) {
    let mut tracker = Tracker::default();
    let mut client: Option<(Client, bool)> = None;
    // Instance the tracker's pane ids belong to; survives disconnects so a
    // reconnect to the same process keeps `done.` state.
    let mut tracked_instance: Option<String> = None;
    let mut paused = false;
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());

    let publish = |link: Link, rows: Vec<PaneRow>, instance: &str| {
        let mut snapshot = shared.lock();
        if snapshot.link != link || snapshot.rows != rows || snapshot.instance != instance {
            *snapshot = Snapshot {
                link,
                rows,
                instance: instance.to_string(),
            };
        }
    };

    loop {
        let wait = if paused {
            client = None;
            publish(Link::Paused, Vec::new(), "");
            None
        } else {
            if client.is_none() {
                match connect() {
                    Ok(paired) => {
                        adopt_instance(
                            &mut tracked_instance,
                            &mut tracker,
                            paired.0.server_instance_id(),
                        );
                        client = Some(paired);
                    }
                    Err(link) => publish(link, Vec::new(), ""),
                }
            }
            match client.as_mut() {
                Some((live, control)) => match live.list_panels() {
                    Ok(panels) => {
                        tracker.observe(&panels);
                        let rows = tracker.rows(&panels, home.as_deref());
                        publish(
                            Link::Live { control: *control },
                            rows,
                            live.server_instance_id(),
                        );
                        Some(POLL_INTERVAL)
                    }
                    Err(err) => {
                        client = None;
                        publish(Link::Unreachable(describe(&err)), Vec::new(), "");
                        Some(RETRY_INTERVAL)
                    }
                },
                None => Some(RETRY_INTERVAL),
            }
        };

        let command = match wait {
            Some(timeout) => match commands.recv_timeout(timeout) {
                Ok(command) => command,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            },
            None => match commands.recv() {
                Ok(command) => command,
                Err(_) => return,
            },
        };
        match command {
            Command::Focus { pane, instance } => {
                let Some((live, _)) = client.as_mut() else {
                    log::info!("focusPane {pane}: not connected; ignored");
                    continue;
                };
                if live.server_instance_id() != instance {
                    log::info!("focusPane {pane}: menu was built for another noa session; ignored");
                    continue;
                }
                tracker.acknowledge(pane);
                match live.focus_pane(pane) {
                    Ok(()) => log::debug!("focusPane {pane}: ok"),
                    Err(err) => {
                        log::warn!("focusPane {pane}: {err}");
                        if matches!(err, ClientError::Transport(_) | ClientError::Closed(_)) {
                            client = None;
                        }
                    }
                }
            }
            Command::Pause(pause) => paused = pause,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Tracker, adopt_instance, control_url};
    use noa_ipc::{Panel, WireId};

    fn panel(busy: bool) -> Panel {
        Panel {
            window_group_id: WireId(1),
            window_id: WireId(1),
            pane_id: WireId(1),
            name: "a".to_string(),
            cwd: "/".to_string(),
            branch: None,
            process: None,
            busy,
            attention: false,
            attachable: true,
            preview: Vec::new(),
        }
    }

    #[test]
    fn new_server_instance_drops_done_state_but_same_instance_keeps_it() {
        let mut tracked = None;
        let mut tracker = Tracker::default();
        adopt_instance(&mut tracked, &mut tracker, "A");
        tracker.observe(&[panel(true)]);
        tracker.observe(&[panel(false)]);
        let done = |t: &Tracker| t.state(&panel(false)) == crate::model::PaneState::Done;
        assert!(done(&tracker));

        adopt_instance(&mut tracked, &mut tracker, "A");
        assert!(done(&tracker), "reconnect to the same noa keeps done.");

        adopt_instance(&mut tracked, &mut tracker, "B");
        assert!(!done(&tracker), "pane 1 of a new noa is not the old pane 1");
    }

    #[test]
    fn control_url_follows_server_bind() {
        assert_eq!(control_url(None, 61771), "ws://127.0.0.1:61771/");
        assert_eq!(control_url(Some("0.0.0.0"), 1), "ws://127.0.0.1:1/");
        assert_eq!(control_url(Some("::"), 1), "ws://[::1]:1/");
        assert_eq!(control_url(Some("::1"), 1), "ws://[::1]:1/");
        assert_eq!(
            control_url(Some(" 192.168.1.20 "), 1),
            "ws://192.168.1.20:1/"
        );
        assert_eq!(control_url(Some("not-an-ip"), 1), "ws://127.0.0.1:1/");
    }
}
