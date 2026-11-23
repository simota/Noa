//! The link between the UI and the running competition session.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// The [`Channel::generation`] it was sent in; one sent before the
    /// screen last left a competition is stale.
    pub generation: u64,
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
    /// Continues the unfinished competition logged in `dir`.
    Resume {
        dir: PathBuf,
    },
    /// Brings `part`'s pane to the front in Noa.
    Focus {
        part: PartId,
    },
    /// Leaves the competition on screen: its runner stops (an unfinished
    /// one can be resumed from the list) and the start view comes back.
    /// Sent by [`Channel::leave`].
    Leave,
}

pub struct Channel {
    pub snapshots: Receiver<Snapshot>,
    pub notices: Receiver<String>,
    commands: Sender<Command>,
    generation: Arc<AtomicU64>,
}

impl Channel {
    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }

    /// Bumped each time the screen leaves a competition, so snapshots
    /// still in flight from its runner are told apart and dropped.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Leaves the competition on screen ([`Command::Leave`]).
    pub fn leave(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.send(Command::Leave);
    }
}

pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Channel {
    let (snapshot_tx, snapshots) = mpsc::channel();
    let (notice_tx, notices) = mpsc::channel();
    let (commands, command_rx) = mpsc::channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
    let generation = Arc::new(AtomicU64::new(0));
    let _ = snapshot_tx.send(Snapshot {
        link: Link::Connecting,
        session: None,
        artifact_root: None,
        generation: 0,
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
        .spawn({
            let generation = generation.clone();
            move || run_commands(command_rx, snapshot_tx, notice_tx, wake, generation)
        })
        .expect("spawn competition-commands thread");
    Channel {
        snapshots,
        notices,
        commands,
        generation,
    }
}

fn run_commands(
    commands: Receiver<Command>,
    snapshots: Sender<Snapshot>,
    notices: Sender<String>,
    wake: Arc<dyn Fn() + Send + Sync>,
    generation: Arc<AtomicU64>,
) {
    let mut handle: Option<(Sender<Action>, Arc<AtomicBool>)> = None;
    let notify = |text: &str| {
        let _ = notices.send(text.to_owned());
        wake();
    };
    for cmd in commands {
        if handle
            .as_ref()
            .is_some_and(|(_, ended)| ended.load(Ordering::Acquire))
        {
            handle = None;
        }
        let start = match (cmd, &handle) {
            (Command::Leave, _) => {
                if let Some((actions, _)) = handle.take() {
                    let _ = actions.send(Action::Stop);
                }
                let _ = snapshots.send(Snapshot {
                    link: Link::Connecting,
                    session: None,
                    artifact_root: None,
                    generation: generation.load(Ordering::Acquire),
                });
                wake();
                continue;
            }
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
                    notify("保存先（Noa のデータディレクトリ）が見つかりません");
                    continue;
                };
                runner::Start::New {
                    request,
                    workdir,
                    max_rounds,
                    reveal_interval,
                    judge,
                    panel,
                    root,
                }
            }
            (Command::Resume { dir }, None) => runner::Start::Resume { dir },
            (Command::Start { .. } | Command::Resume { .. }, Some(_)) => {
                notify("コンペティションはすでに始まっています");
                continue;
            }
            (_, None) => {
                notify("コンペティションが始まっていません");
                continue;
            }
            (cmd, Some((actions, _))) => {
                if let Some(action) = forwarded(cmd) {
                    let _ = actions.send(action);
                }
                continue;
            }
        };
        let started = runner::spawn_session(start, || {});
        let ended = Arc::new(AtomicBool::new(false));
        handle = Some((started.actions(), ended.clone()));
        let (snapshots, notices, wake) = (snapshots.clone(), notices.clone(), wake.clone());
        let stamp = generation.load(Ordering::Acquire);
        std::thread::Builder::new()
            .name("competition-updates".into())
            .spawn(move || forward_updates(started, ended, snapshots, notices, wake, stamp))
            .expect("spawn competition-updates thread");
    }
    if let Some((actions, _)) = handle {
        let _ = actions.send(Action::Stop);
    }
}

/// The runner action a command on a running competition is.
fn forwarded(cmd: Command) -> Option<Action> {
    Some(match cmd {
        Command::Intervene { to, body } => Action::Intervene { to, body },
        Command::SelectWinner { winner } => Action::SelectWinner { winner },
        Command::Retry => Action::Retry,
        Command::Close => Action::Close,
        Command::Adopt { winner, agent } => Action::Adopt { winner, agent },
        Command::Focus { part } => Action::Focus { part },
        Command::Start { .. } | Command::Resume { .. } | Command::Leave => return None,
    })
}

fn forward_updates(
    handle: SessionHandle,
    ended: Arc<AtomicBool>,
    snapshots: Sender<Snapshot>,
    notices: Sender<String>,
    wake: Arc<dyn Fn() + Send + Sync>,
    generation: u64,
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
                        generation,
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
                        generation,
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
                    generation,
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

#[cfg(test)]
impl Channel {
    /// A channel with no command thread behind it, and the sender of its
    /// snapshots.
    pub fn fake() -> (Channel, Sender<Snapshot>) {
        let (snapshot_tx, snapshots) = mpsc::channel();
        let (_notice_tx, notices) = mpsc::channel();
        let (commands, _command_rx) = mpsc::channel();
        let channel = Channel {
            snapshots,
            notices,
            commands,
            generation: Arc::new(AtomicU64::new(0)),
        };
        (channel, snapshot_tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(5);

    #[test]
    fn leaving_sends_the_start_view_in_a_new_generation() {
        let channel = start(|| {});
        let first = channel.snapshots.recv_timeout(WAIT).unwrap();
        assert_eq!(first.generation, 0);
        channel.leave();
        assert_eq!(channel.generation(), 1);
        let next = channel.snapshots.recv_timeout(WAIT).unwrap();
        assert_eq!(next.generation, 1);
        assert!(next.session.is_none());
    }

    #[test]
    fn a_command_for_a_competition_that_is_not_running_says_so() {
        let channel = start(|| {});
        channel.send(Command::Focus {
            part: PartId::new("claude"),
        });
        let notice = channel.notices.recv_timeout(WAIT).unwrap();
        assert!(notice.contains("始まっていません"), "{notice}");
    }
}
