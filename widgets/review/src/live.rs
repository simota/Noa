//! The link between the UI and the running review session.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use review::model::types::{AgentChoice, PartId, ReviewerChoice, RoundNo, Session, Target};
use review::sibling::{self, Launcher};
use review::task_links::{self, Origin};

use crate::session::fix::{self, FixRequest};
use crate::session::ipc::IpcConnect;
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
    /// Root of the snapshot and the reply files (`target/`, `r<N>/`).
    pub reply_root: Option<PathBuf>,
    /// The session's store directory (`events.jsonl`, `report.md`).
    pub session_dir: Option<PathBuf>,
    pub origin: Option<Origin>,
    /// The [`Channel::generation`] it was sent in; one sent before the
    /// screen last left a review is stale.
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Start {
        auto: Option<review::auto_assign::AutoRecord>,
        request: String,
        target: Box<Target>,
        max_rounds: RoundNo,
        /// The highest limit this start may ask for: `MAX_MAX_ROUNDS`, or a
        /// review set up again with the limit it had after +N.
        round_cap: RoundNo,
        lead: AgentChoice,
        panel: Vec<ReviewerChoice>,
    },
    Intervene {
        to: Vec<PartId>,
        body: String,
    },
    Retry,
    Close,
    /// Continues the unfinished review logged in `dir`.
    Resume {
        dir: PathBuf,
    },
    /// Brings `part`'s pane to the front in Noa.
    Focus {
        part: PartId,
    },
    /// Holds the running review after each consolidated round, or lets it
    /// go on.
    SetHold {
        hold: bool,
    },
    /// Starts the next round of the held review.
    Continue,
    /// Raises the running review's round limit by `by`.
    Extend {
        by: RoundNo,
    },
    /// Closes the agents' panes of the ended review on screen.
    ClosePanes,
    OpenOriginalTask(Origin),
    /// Gives the stored review in `dir` `by` more rounds and goes on
    /// with it: a finished one that ran out of rounds continues.
    ExtendStored {
        dir: PathBuf,
        by: RoundNo,
    },
    /// Leaves the review on screen: its runner stops (an unfinished
    /// one can be resumed from the list) and the start view comes back.
    /// Sent by [`Channel::leave`].
    Leave,
    /// Starts an agent on fixing findings of the review on screen, in a
    /// new tab of its own (running or stored, whatever its runner does).
    RequestFix(Box<FixRequest>),
}

/// A notice for the screen, in the [`Channel::generation`] of the command
/// (and so of the review) it is about.
pub type Notice = (u64, String);

pub struct Channel {
    pub wake: Arc<dyn Fn() + Send + Sync>,
    pub snapshots: Receiver<Snapshot>,
    pub notices: Receiver<Notice>,
    /// One `()` each time a runner has stopped and let go of its log, so
    /// the list of past reviews is read again.
    pub released: Receiver<()>,
    /// The running Noa's version, once a look at it at startup could read
    /// it (what 修正を依頼 can offer depends on it).
    pub versions: Receiver<(u64, String)>,
    commands: Sender<(u64, Command)>,
    generation: Arc<AtomicU64>,
}

impl Channel {
    /// Sends `cmd` in the current generation: what it starts, and what is
    /// said about it, belongs to the review on screen now.
    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send((self.generation(), cmd));
    }

    /// Bumped each time the screen leaves a review, so snapshots
    /// still in flight from its runner are told apart and dropped.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Leaves the review on screen ([`Command::Leave`]).
    pub fn leave(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.send(Command::Leave);
    }
}

/// What the window shows of Noa before any review connects: down at once
/// when the config turns the control server off (the review would only
/// find out when it starts), else waiting.
pub fn startup_link() -> Link {
    match noa_config::load_startup_config(noa_config::ConfigOverrides::default()) {
        Ok((config, _)) if !config.server_enable => {
            Link::Down(crate::session::ipc::SERVER_DISABLED.to_owned())
        }
        _ => Link::Connecting,
    }
}

pub fn start(wake: impl Fn() + Send + Sync + 'static, link: Link) -> Channel {
    let (snapshot_tx, snapshots) = mpsc::channel();
    let (notice_tx, notices) = mpsc::channel();
    let (released_tx, released) = mpsc::channel();
    let (version_tx, versions) = mpsc::channel();
    let (commands, command_rx) = mpsc::channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
    let told = Arc::clone(&wake);
    let probed = version_tx.clone();
    std::thread::Builder::new()
        .name("review-version".into())
        .spawn(move || {
            if let Some(version) = crate::session::ipc::probe_version() {
                let _ = probed.send(version);
                told();
            }
        })
        .expect("spawn review-version thread");
    let generation = Arc::new(AtomicU64::new(0));
    let _ = snapshot_tx.send(Snapshot {
        link,
        session: None,
        reply_root: None,
        session_dir: None,
        origin: None,
        generation: 0,
    });
    wake();
    let tick = Arc::clone(&wake);
    // Wakes the screen so the elapsed times in the progress strip move while
    // agents work without emitting events; the screen rebuilds only when the
    // shown minute changes.
    std::thread::Builder::new()
        .name("review-clock".into())
        .spawn(move || {
            loop {
                std::thread::sleep(CLOCK_TICK);
                tick();
            }
        })
        .expect("spawn review-clock thread");
    let ui_wake = Arc::clone(&wake);
    std::thread::Builder::new()
        .name("review-commands".into())
        .spawn(move || {
            run_commands(
                command_rx,
                Outlets {
                    snapshots: snapshot_tx,
                    notices: notice_tx,
                    released: released_tx,
                    versions: version_tx,
                    wake,
                },
            )
        })
        .expect("spawn review-commands thread");
    Channel {
        wake: ui_wake,
        snapshots,
        notices,
        released,
        versions,
        commands,
        generation,
    }
}

/// Where the command thread and the runners' forwarders deliver to.
#[derive(Clone)]
struct Outlets {
    snapshots: Sender<Snapshot>,
    notices: Sender<Notice>,
    released: Sender<()>,
    /// The version of each Noa a runner or a fix request connected to,
    /// with the number of that connection's hello.
    versions: Sender<(u64, String)>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

fn run_commands(commands: Receiver<(u64, Command)>, outlets: Outlets) {
    let mut handle: Option<(Sender<Action>, Arc<AtomicBool>)> = None;
    let Outlets {
        snapshots, wake, ..
    } = outlets.clone();
    for (generation, cmd) in commands {
        let notify = |text: &str| {
            let _ = outlets.notices.send((generation, text.to_owned()));
            wake();
        };
        if handle
            .as_ref()
            .is_some_and(|(_, ended)| ended.load(Ordering::Acquire))
        {
            handle = None;
        }
        if let Command::OpenOriginalTask(origin) = cmd {
            if let Err(why) = open_original_task(&mut sibling::System, &origin) {
                notify(&why);
            }
            continue;
        }
        // A fix needs no runner: it is one request to Noa, on a link of
        // its own, so a stored review asks for one as a running one does.
        if let Command::RequestFix(req) = cmd {
            let outlets = outlets.clone();
            std::thread::Builder::new()
                .name("review-fix".into())
                .spawn(move || {
                    let mut seen = None;
                    let (Ok(text) | Err(text)) = fix::request(&mut IpcConnect, &req, &mut seen);
                    if let Some(version) = seen {
                        let _ = outlets.versions.send(version);
                    }
                    let _ = outlets.notices.send((generation, text));
                    (outlets.wake)();
                })
                .expect("spawn review-fix thread");
            continue;
        }
        let start = match (cmd, &handle) {
            (Command::Leave, _) => {
                if let Some((actions, _)) = handle.take() {
                    let _ = actions.send(Action::Stop);
                }
                let _ = snapshots.send(Snapshot {
                    link: Link::Connecting,
                    session: None,
                    reply_root: None,
                    session_dir: None,
                    origin: None,
                    generation,
                });
                wake();
                continue;
            }
            (
                Command::Start {
                    auto,
                    request,
                    target,
                    max_rounds,
                    round_cap,
                    lead,
                    panel,
                },
                None,
            ) => {
                let Some(root) = review::store::default_root() else {
                    notify("保存先（Noa のデータディレクトリ）が見つかりません");
                    continue;
                };
                runner::Start::New(Box::new(runner::NewReview {
                    auto,
                    request,
                    target: *target,
                    max_rounds,
                    round_cap,
                    lead,
                    panel,
                    root,
                }))
            }
            (
                Command::Start { .. } | Command::Resume { .. } | Command::ExtendStored { .. },
                Some(_),
            ) => {
                notify("レビューはすでに始まっています");
                continue;
            }
            (cmd, None) => match stored_start(cmd) {
                Some(start) => start,
                None => {
                    notify("レビューが始まっていません");
                    continue;
                }
            },
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
        let outlets = outlets.clone();
        std::thread::Builder::new()
            .name("review-updates".into())
            .spawn(move || forward_updates(started, ended, outlets, generation))
            .expect("spawn review-updates thread");
    }
    if let Some((actions, _)) = handle {
        let _ = actions.send(Action::Stop);
    }
}

pub(crate) fn open_original_task(
    launcher: &mut impl Launcher,
    origin: &Origin,
) -> Result<(), String> {
    launcher.launch(sibling::TASKS, task_links::tasks_args(origin)?)
}

/// The runner start a command on a stored review asks for.
fn stored_start(cmd: Command) -> Option<runner::Start> {
    match cmd {
        Command::Resume { dir } => Some(runner::Start::Resume { dir }),
        Command::ExtendStored { dir, by } => Some(runner::Start::Extend { dir, by }),
        _ => None,
    }
}

/// The runner action a command on a running review is.
fn forwarded(cmd: Command) -> Option<Action> {
    Some(match cmd {
        Command::Intervene { to, body } => Action::Intervene { to, body },
        Command::Retry => Action::Retry,
        Command::Close => Action::Close,
        Command::Focus { part } => Action::Focus { part },
        Command::SetHold { hold } => Action::SetHold { hold },
        Command::Continue => Action::Continue,
        Command::Extend { by } => Action::Extend { by },
        Command::ClosePanes => Action::ClosePanes,
        Command::Start { .. }
        | Command::Resume { .. }
        | Command::ExtendStored { .. }
        | Command::OpenOriginalTask(_)
        | Command::RequestFix(_)
        | Command::Leave => return None,
    })
}

/// Passes a runner's updates on, stamped with the generation of the
/// command that started it.
fn forward_updates(
    handle: SessionHandle,
    ended: Arc<AtomicBool>,
    outlets: Outlets,
    generation: u64,
) {
    let Outlets {
        snapshots,
        notices,
        released,
        versions,
        wake,
    } = outlets;
    let mut link = Link::Connecting;
    let mut session = None;
    let mut reply_root = None;
    let mut session_dir = None;
    let mut origin = None;
    for update in &handle.updates {
        let sent = match update {
            Update::Session {
                session: next,
                reply_root: root,
                session_dir: dir,
                origin: next_origin,
            } => {
                session = Some(next);
                reply_root = Some(root);
                session_dir = Some(dir);
                origin = next_origin;
                snapshots
                    .send(Snapshot {
                        link: link.clone(),
                        session: session.clone(),
                        reply_root: reply_root.clone(),
                        session_dir: session_dir.clone(),
                        origin: origin.clone(),
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
                        reply_root: reply_root.clone(),
                        session_dir: session_dir.clone(),
                        origin: origin.clone(),
                        generation,
                    })
                    .is_ok()
            }
            Update::Notice(text) => notices.send((generation, text)).is_ok(),
            Update::Version(seq, version) => {
                let _ = versions.send((seq, version));
                true
            }
            Update::Ended(Ok(())) => break,
            Update::Ended(Err(why)) => {
                ended.store(true, Ordering::Release);
                let _ = notices.send((generation, format!("レビューを続けられません: {why}")));
                let _ = snapshots.send(Snapshot {
                    link: Link::Down(why),
                    session,
                    reply_root,
                    session_dir,
                    origin,
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
    // The runner is gone, and its lock on the log with it.
    let _ = released.send(());
    wake();
}

/// The far ends of a [`Channel::fake`].
#[cfg(test)]
pub struct FakeLink {
    pub snapshots: FakeSnapshots,
    pub notices: Sender<Notice>,
    pub released: Sender<()>,
    pub versions: Sender<(u64, String)>,
    pub commands: Receiver<(u64, Command)>,
}

// The existing UI tests build snapshots without task data.
#[cfg(test)]
pub struct TestSnapshot {
    pub link: Link,
    pub session: Option<Arc<Session>>,
    pub reply_root: Option<PathBuf>,
    pub session_dir: Option<PathBuf>,
    pub generation: u64,
}

#[cfg(test)]
pub struct FakeSnapshots(Sender<Snapshot>);

#[cfg(test)]
impl FakeSnapshots {
    pub fn send(&self, snapshot: TestSnapshot) -> Result<(), mpsc::SendError<Snapshot>> {
        let TestSnapshot {
            link,
            session,
            reply_root,
            session_dir,
            generation,
        } = snapshot;
        self.0.send(Snapshot {
            link,
            session,
            reply_root,
            session_dir,
            origin: None,
            generation,
        })
    }
}

#[cfg(test)]
impl Channel {
    /// A channel with no command thread behind it, and its far ends.
    pub fn fake() -> (Channel, FakeLink) {
        let (snapshot_tx, snapshots) = mpsc::channel();
        let (notice_tx, notices) = mpsc::channel();
        let (released_tx, released) = mpsc::channel();
        let (version_tx, versions) = mpsc::channel();
        let (commands, command_rx) = mpsc::channel();
        let channel = Channel {
            wake: Arc::new(|| {}),
            snapshots,
            notices,
            released,
            versions,
            commands,
            generation: Arc::new(AtomicU64::new(0)),
        };
        let link = FakeLink {
            snapshots: FakeSnapshots(snapshot_tx),
            notices: notice_tx,
            released: released_tx,
            versions: version_tx,
            commands: command_rx,
        };
        (channel, link)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(5);

    #[test]
    fn leaving_sends_the_start_view_in_a_new_generation() {
        let channel = start(|| {}, Link::Connecting);
        let first = channel.snapshots.recv_timeout(WAIT).unwrap();
        assert_eq!(first.generation, 0);
        channel.leave();
        assert_eq!(channel.generation(), 1);
        let next = channel.snapshots.recv_timeout(WAIT).unwrap();
        assert_eq!(next.generation, 1);
        assert!(next.session.is_none());
    }

    #[test]
    fn a_command_for_a_review_that_is_not_running_says_so() {
        let channel = start(|| {}, Link::Connecting);
        channel.send(Command::Focus {
            part: PartId::new("claude"),
        });
        let (generation, notice) = channel.notices.recv_timeout(WAIT).unwrap();
        assert_eq!(generation, 0);
        assert!(notice.contains("始まっていません"), "{notice}");
    }

    #[test]
    fn round_control_goes_to_the_running_review() {
        assert_eq!(
            forwarded(Command::SetHold { hold: true }),
            Some(Action::SetHold { hold: true })
        );
        assert_eq!(forwarded(Command::Continue), Some(Action::Continue));
        assert_eq!(
            forwarded(Command::Extend { by: 3 }),
            Some(Action::Extend { by: 3 })
        );
        let stored = Command::ExtendStored {
            dir: PathBuf::from("/x"),
            by: 1,
        };
        assert_eq!(forwarded(stored), None);
    }

    /// What each command on a stored review starts (the runner's tests
    /// cover what `Start::Extend` and `Start::Resume` then do).
    #[test]
    fn a_stored_review_is_extended_by_an_extending_start() {
        let dir = PathBuf::from("/x/comp-1");
        assert_eq!(
            stored_start(Command::ExtendStored {
                dir: dir.clone(),
                by: 3
            }),
            Some(runner::Start::Extend {
                dir: dir.clone(),
                by: 3
            })
        );
        assert_eq!(
            stored_start(Command::Resume { dir: dir.clone() }),
            Some(runner::Start::Resume { dir })
        );
        assert_eq!(stored_start(Command::Continue), None);
    }

    /// Extending a stored review starts a runner on it, whose refusal
    /// comes back as a notice of the command's generation.
    #[test]
    fn extending_a_stored_review_starts_a_runner_on_it() {
        let (snapshot_tx, _snapshots) = mpsc::channel();
        let (notice_tx, notices) = mpsc::channel();
        let (released_tx, released) = mpsc::channel();
        let (commands, command_rx) = mpsc::channel();
        let dir =
            std::env::temp_dir().join(format!("noa-review-no-such-stored-{}", std::process::id()));
        commands
            .send((2, Command::ExtendStored { dir, by: 1 }))
            .unwrap();
        drop(commands);
        run_commands(
            command_rx,
            Outlets {
                snapshots: snapshot_tx,
                notices: notice_tx,
                released: released_tx,
                versions: mpsc::channel().0,
                wake: Arc::new(|| {}),
            },
        );
        let (generation, notice) = notices.recv_timeout(WAIT).unwrap();
        assert_eq!(generation, 2);
        assert!(notice.contains("を開けません"), "{notice}");
        released.recv_timeout(WAIT).unwrap();
    }

    /// A runner started by a command is stamped with the generation the
    /// command was sent in, even when the screen has moved on since.
    #[test]
    fn a_runner_keeps_the_generation_of_the_command_that_started_it() {
        let (snapshot_tx, snapshots) = mpsc::channel();
        let (notice_tx, notices) = mpsc::channel();
        let (released_tx, released) = mpsc::channel();
        let (commands, command_rx) = mpsc::channel();
        let dir =
            std::env::temp_dir().join(format!("noa-review-no-such-log-{}", std::process::id()));
        // Sent in generation 0, handled after the screen left (generation 1).
        commands.send((0, Command::Resume { dir })).unwrap();
        drop(commands);
        run_commands(
            command_rx,
            Outlets {
                snapshots: snapshot_tx,
                notices: notice_tx,
                released: released_tx,
                versions: mpsc::channel().0,
                wake: Arc::new(|| {}),
            },
        );
        let (generation, notice) = notices.recv_timeout(WAIT).unwrap();
        assert_eq!(generation, 0);
        assert!(notice.contains("続けられません"), "{notice}");
        let last = snapshots.recv_timeout(WAIT).unwrap();
        assert_eq!(last.generation, 0);
        released.recv_timeout(WAIT).unwrap();
    }
}
