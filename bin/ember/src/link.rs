//! The connection to the local noa-server, on its own thread. It owns the
//! synchronous `noa_ipc::Client`, polls `listPanels`, and publishes a
//! ready-to-draw [`Snapshot`]; the menu thread only reads that snapshot and
//! sends [`Command`]s back.

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
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot {
            link: Link::Connecting,
            rows: Vec::new(),
        }
    }
}

pub enum Command {
    Focus(u64),
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
    // Whatever `server-bind` is, the server is reachable on loopback.
    Ok(Pairing {
        url: format!("ws://127.0.0.1:{port}/"),
        token,
    })
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

fn run(shared: Shared, commands: Receiver<Command>) {
    let mut tracker = Tracker::default();
    let mut client: Option<(Client, bool)> = None;
    let mut paused = false;
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());

    let publish = |link: Link, rows: Vec<PaneRow>| {
        let mut snapshot = shared.lock();
        if snapshot.link != link || snapshot.rows != rows {
            *snapshot = Snapshot { link, rows };
        }
    };

    loop {
        let wait = if paused {
            client = None;
            publish(Link::Paused, Vec::new());
            None
        } else {
            if client.is_none() {
                match connect() {
                    Ok(paired) => client = Some(paired),
                    Err(link) => publish(link, Vec::new()),
                }
            }
            match client.as_mut() {
                Some((live, control)) => match live.list_panels() {
                    Ok(panels) => {
                        tracker.observe(&panels);
                        let rows = tracker.rows(&panels, home.as_deref());
                        publish(Link::Live { control: *control }, rows);
                        Some(POLL_INTERVAL)
                    }
                    Err(err) => {
                        client = None;
                        publish(Link::Unreachable(describe(&err)), Vec::new());
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
            Command::Focus(pane) => {
                tracker.acknowledge(pane);
                if let Some((live, _)) = client.as_mut() {
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
            }
            Command::Pause(pause) => paused = pause,
        }
    }
}
