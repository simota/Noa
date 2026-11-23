//! The link between the UI and the running competition session.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use competition::model::types::{AgentChoice, AgentKind, PartId, RoundNo, Session};

use crate::session::runner::{self, Action, SessionHandle, Update};

/// How often the screen is woken to refresh elapsed times.
const CLOCK_TICK: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Live,
    Down(String),
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub link: Link,
    pub session: Option<Arc<Session>>,
    /// Root directory holding `entries/<part>/`, `r<N>/artifacts/`, and `compare.html`.
    pub artifact_root: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Start {
        request: String,
        workdir: Option<PathBuf>,
        max_rounds: Option<RoundNo>,
        reveal_interval: RoundNo,
        judge: AgentChoice,
        panel: Vec<AgentChoice>,
    },
    Intervene {
        to: Vec<PartId>,
        body: String,
    },
    SelectWinner {
        winner: PartId,
    },
    Retry,
    Close,
    Adopt {
        winner: PartId,
        agent: AgentKind,
    },
}

pub struct Channel {
    pub snapshots: Receiver<Snapshot>,
    pub notices: Receiver<String>,
    commands: Sender<Command>,
}

impl Channel {
    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }
}

pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Channel {
    let (snapshot_tx, snapshots) = mpsc::channel();
    let (notice_tx, notices) = mpsc::channel();
    let (commands, command_rx) = mpsc::channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
    let _ = snapshot_tx.send(Snapshot {
        link: Link::Connecting,
        session: None,
        artifact_root: None,
    });
    wake();
    let tick = Arc::clone(&wake);
    // Wakes the screen so the elapsed times in the progress strip move while
    // agents work without emitting events; the screen rebuilds only when the
    // shown minute changes.
    std::thread::Builder::new()
        .name("competition-clock".into())
        .spawn(move || {
            loop {
                std::thread::sleep(CLOCK_TICK);
                tick();
            }
        })
        .expect("spawn competition-clock thread");
    std::thread::Builder::new()
        .name("competition-commands".into())
        .spawn(move || run_commands(command_rx, snapshot_tx, notice_tx, wake))
        .expect("spawn competition-commands thread");
    Channel {
        snapshots,
        notices,
        commands,
    }
}

fn run_commands(
    commands: Receiver<Command>,
    snapshots: Sender<Snapshot>,
    notices: Sender<String>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let mut handle: Option<(Sender<Action>, Arc<AtomicBool>)> = None;
    for cmd in commands {
        if handle
            .as_ref()
            .is_some_and(|(_, ended)| ended.load(Ordering::Acquire))
        {
            handle = None;
        }
        let action = match (cmd, &handle) {
            (
                Command::Start {
                    request,
                    workdir,
                    max_rounds,
                    reveal_interval,
                    judge,
                    panel,
                },
                None,
            ) => {
                let Some(root) = competition::store::default_root() else {
                    let _ =
                        notices.send("保存先（Noa のデータディレクトリ）が見つかりません".into());
                    wake();
                    continue;
                };
                let started = runner::spawn_session(
                    runner::Start::New {
                        request,
                        workdir,
                        max_rounds,
                        reveal_interval,
                        judge,
                        panel,
                        root,
                    },
                    || {},
                );
                let ended = Arc::new(AtomicBool::new(false));
                handle = Some((started.actions(), ended.clone()));
                let (snapshots, notices, wake) = (snapshots.clone(), notices.clone(), wake.clone());
                std::thread::Builder::new()
                    .name("competition-updates".into())
                    .spawn(move || forward_updates(started, ended, snapshots, notices, wake))
                    .expect("spawn competition-updates thread");
                continue;
            }
            (Command::Start { .. }, Some(_)) => {
                let _ = notices.send("コンペティションはすでに始まっています".into());
                wake();
                continue;
            }
            (_, None) => {
                let _ = notices.send("コンペティションが始まっていません".into());
                wake();
                continue;
            }
            (Command::Intervene { to, body }, Some(_)) => Action::Intervene { to, body },
            (Command::SelectWinner { winner }, Some(_)) => Action::SelectWinner { winner },
            (Command::Retry, Some(_)) => Action::Retry,
            (Command::Close, Some(_)) => Action::Close,
            (Command::Adopt { winner, agent }, Some(_)) => Action::Adopt { winner, agent },
        };
        if let Some((actions, _)) = &handle {
            let _ = actions.send(action);
        }
    }
    if let Some((actions, _)) = handle {
        let _ = actions.send(Action::Stop);
    }
}

fn forward_updates(
    handle: SessionHandle,
    ended: Arc<AtomicBool>,
    snapshots: Sender<Snapshot>,
    notices: Sender<String>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let mut link = Link::Connecting;
    let mut session = None;
    let mut artifact_root = None;
    for update in &handle.updates {
        let sent = match update {
            Update::Session {
                session: next,
                artifact_root: root,
            } => {
                session = Some(next);
                artifact_root = Some(root);
                snapshots
                    .send(Snapshot {
                        link: link.clone(),
                        session: session.clone(),
                        artifact_root: artifact_root.clone(),
                    })
                    .is_ok()
            }
            Update::Link(next) => {
                link = match next {
                    runner::Link::Connecting => Link::Connecting,
                    runner::Link::Live => Link::Live,
                    runner::Link::Down(why) => Link::Down(why),
                };
                snapshots
                    .send(Snapshot {
                        link: link.clone(),
                        session: session.clone(),
                        artifact_root: artifact_root.clone(),
                    })
                    .is_ok()
            }
            Update::Notice(text) => notices.send(text).is_ok(),
            Update::Ended(Ok(())) => break,
            Update::Ended(Err(why)) => {
                ended.store(true, Ordering::Release);
                let _ = notices.send(format!("コンペティションを続けられません: {why}"));
                let _ = snapshots.send(Snapshot {
                    link: Link::Down(why),
                    session,
                    artifact_root,
                });
                wake();
                break;
            }
        };
        wake();
        if !sent {
            break;
        }
    }
    ended.store(true, Ordering::Release);
}
