//! The link between the UI and the running verification.
//!
//! The screen sends [`Command`]s over a [`Channel`] and draws the
//! [`Snapshot`]s it gets back; one command thread owns at most one
//! [`runner`] at a time. Requests that need no runner (修正を依頼, and the
//! pane and worktree requests about a stored verification) run on threads
//! of their own.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use verify::model::types::{
    AgentChoice, CriteriaEdit, Isolation, Mark, PartId, RoundNo, Session, Target, VerifierChoice,
};

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
    /// The judge's reply dir (`<workdir>/.noa/verify/<id>/`): `target/`,
    /// `evidence/`, `c/`, `r<N>/judge.json`.
    pub reply_root: Option<PathBuf>,
    /// The session's store directory (`events.jsonl`, `report.md`).
    pub session_dir: Option<PathBuf>,
    /// The [`Channel::generation`] it was sent in; one sent before the
    /// screen last left a verification is stale.
    pub generation: u64,
}

/// What a person attaches with 証跡を追加 (L1-5a): the file as chosen or
/// dropped, its caption, a video's marks and the criteria it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub source: PathBuf,
    pub caption: String,
    pub marks: Vec<Mark>,
    pub shows: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Start {
        request: String,
        /// Holds the spec file asked for (`Target::spec`).
        target: Box<Target>,
        isolation: Isolation,
        /// The person's criteria, one per line as typed.
        user_criteria: Vec<String>,
        /// 基準を確認してから検証.
        confirm: bool,
        max_rounds: RoundNo,
        /// The highest limit this start may ask for: `MAX_MAX_ROUNDS`, or a
        /// verification set up again with the limit it had after +N.
        round_cap: RoundNo,
        judge: AgentChoice,
        panel: Vec<VerifierChoice>,
    },
    /// Changes the criteria held for confirmation.
    EditCriteria {
        edit: CriteriaEdit,
    },
    /// 基準を確定して検証を開始: freezes the criteria, round 1 starts.
    StartVerify,
    /// 証跡を追加.
    Attach(Box<Attachment>),
    Intervene {
        to: Vec<PartId>,
        body: String,
    },
    Retry,
    Close,
    /// Continues the unfinished verification logged in `dir`.
    Resume {
        dir: PathBuf,
    },
    /// Brings `part`'s pane to the front in Noa.
    Focus {
        part: PartId,
    },
    /// Holds the running verification after each ruled round, or lets it
    /// go on.
    SetHold {
        hold: bool,
    },
    /// Starts the next round of the held verification.
    Continue,
    /// Raises the running verification's round limit by `by`.
    Extend {
        by: RoundNo,
    },
    /// ペインをすべて閉じる on the finished verification its runner holds.
    ClosePanes,
    /// ペインをすべて閉じる on the finished stored verification in `dir`
    /// (no runner): its judge's and verifiers' panes, found by label.
    ClosePanesStored {
        dir: PathBuf,
    },
    /// Asks how many panes of the stored verification in `dir` are still
    /// open in Noa (by label); answered on [`Channel::panes`].
    ProbePanes {
        dir: PathBuf,
    },
    /// ワークツリーを削除 for the ended or interrupted verification in
    /// `dir` (L1-3): through its runner when one holds it, else on its own.
    RemoveWorktrees {
        dir: PathBuf,
    },
    /// Gives the stored verification in `dir` `by` more rounds and goes on
    /// with it: a finished one that ran out of rounds continues.
    ExtendStored {
        dir: PathBuf,
        by: RoundNo,
    },
    /// Leaves the verification on screen: its runner stops (an unfinished
    /// one can be resumed from the list) and the start view comes back.
    /// Sent by [`Channel::leave`].
    Leave,
    /// Starts an agent on fixing failed criteria and defects of the
    /// verification on screen, in a new tab of its own.
    RequestFix(Box<FixRequest>),
}

/// A notice for the screen, in the [`Channel::generation`] of the command
/// (and so of the verification) it is about.
pub type Notice = (u64, String);

/// How many of a stored verification's panes are open in Noa
/// ([`Command::ProbePanes`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPanes {
    pub generation: u64,
    pub dir: PathBuf,
    pub count: usize,
}

pub struct Channel {
    pub snapshots: Receiver<Snapshot>,
    pub notices: Receiver<Notice>,
    /// One `()` each time a runner has stopped and let go of its log, so
    /// the list of past verifications is read again.
    pub released: Receiver<()>,
    /// The running Noa's version, once a look at it at startup could read
    /// it (what 修正を依頼 can offer depends on it).
    pub versions: Receiver<(u64, String)>,
    /// Answers to [`Command::ProbePanes`].
    pub panes: Receiver<OpenPanes>,
    commands: Sender<(u64, Command)>,
    generation: Arc<AtomicU64>,
}

impl Channel {
    /// Sends `cmd` in the current generation: what it starts, and what is
    /// said about it, belongs to the verification on screen now.
    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send((self.generation(), cmd));
    }

    /// Bumped each time the screen leaves a verification, so snapshots
    /// still in flight from its runner are told apart and dropped.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Leaves the verification on screen ([`Command::Leave`]).
    pub fn leave(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.send(Command::Leave);
    }
}

/// What the window shows of Noa before any verification connects: down at
/// once when the config turns the control server off, else waiting.
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
    let (panes_tx, panes) = mpsc::channel();
    let (commands, command_rx) = mpsc::channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
    let told = Arc::clone(&wake);
    let probed = version_tx.clone();
    std::thread::Builder::new()
        .name("verify-version".into())
        .spawn(move || {
            if let Some(version) = crate::session::ipc::probe_version() {
                let _ = probed.send(version);
                told();
            }
        })
        .expect("spawn verify-version thread");
    let generation = Arc::new(AtomicU64::new(0));
    let _ = snapshot_tx.send(Snapshot {
        link,
        session: None,
        reply_root: None,
        session_dir: None,
        generation: 0,
    });
    wake();
    let tick = Arc::clone(&wake);
    // Wakes the screen so the elapsed times in the progress strip move while
    // agents work without emitting events.
    std::thread::Builder::new()
        .name("verify-clock".into())
        .spawn(move || {
            loop {
                std::thread::sleep(CLOCK_TICK);
                tick();
            }
        })
        .expect("spawn verify-clock thread");
    std::thread::Builder::new()
        .name("verify-commands".into())
        .spawn(move || {
            run_commands(
                command_rx,
                Outlets {
                    snapshots: snapshot_tx,
                    notices: notice_tx,
                    released: released_tx,
                    versions: version_tx,
                    panes: panes_tx,
                    wake,
                },
            )
        })
        .expect("spawn verify-commands thread");
    Channel {
        snapshots,
        notices,
        released,
        versions,
        panes,
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
    /// The version of each Noa a runner or a request connected to, with
    /// the number of that connection's hello.
    versions: Sender<(u64, String)>,
    panes: Sender<OpenPanes>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Outlets {
    fn notify(&self, generation: u64, text: impl Into<String>) {
        let _ = self.notices.send((generation, text.into()));
        (self.wake)();
    }

    /// Runs `job` on a thread of its own and tells its outcome as a notice.
    fn detached(
        &self,
        name: &str,
        generation: u64,
        job: impl FnOnce(&Outlets) -> Option<String> + Send + 'static,
    ) {
        let outlets = self.clone();
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                if let Some(text) = job(&outlets) {
                    outlets.notify(generation, text);
                }
            })
            .expect("spawn verify request thread");
    }
}

fn run_commands(commands: Receiver<(u64, Command)>, outlets: Outlets) {
    let mut handle: Option<(Sender<Action>, Arc<AtomicBool>, Option<PathBuf>)> = None;
    for (generation, cmd) in commands {
        if handle
            .as_ref()
            .is_some_and(|(_, ended, _)| ended.load(Ordering::Acquire))
        {
            handle = None;
        }
        let start = match (cmd, &handle) {
            // A fix needs no runner: it is one request to Noa, on a link of
            // its own, so a stored verification asks for one as a running
            // one does.
            (Command::RequestFix(req), _) => {
                outlets.detached("verify-fix", generation, move |outlets| {
                    let mut seen = None;
                    let (Ok(text) | Err(text)) = fix::request(&mut IpcConnect, &req, &mut seen);
                    if let Some(version) = seen {
                        let _ = outlets.versions.send(version);
                    }
                    Some(text)
                });
                continue;
            }
            (Command::ProbePanes { dir }, _) => {
                outlets.detached("verify-panes", generation, move |outlets| {
                    if let Ok(count) = runner::count_stored_panes(&mut IpcConnect, &dir) {
                        let _ = outlets.panes.send(OpenPanes {
                            generation,
                            dir,
                            count,
                        });
                        (outlets.wake)();
                    }
                    None
                });
                continue;
            }
            (Command::Leave, _) => {
                if let Some((actions, _, _)) = handle.take() {
                    let _ = actions.send(Action::Stop);
                }
                let _ = outlets.snapshots.send(Snapshot {
                    link: Link::Connecting,
                    session: None,
                    reply_root: None,
                    session_dir: None,
                    generation,
                });
                (outlets.wake)();
                continue;
            }
            (Command::ClosePanesStored { dir }, None) => {
                outlets.detached("verify-close-panes", generation, move |_| {
                    let (Ok(text) | Err(text)) = runner::close_stored_panes(&mut IpcConnect, &dir);
                    Some(text)
                });
                continue;
            }
            (Command::RemoveWorktrees { dir }, None) => {
                outlets.detached("verify-worktrees", generation, move |outlets| {
                    let (Ok(text) | Err(text)) = runner::remove_stored_worktrees(&dir);
                    // The list shows the worktrees' state from the log.
                    let _ = outlets.released.send(());
                    Some(text)
                });
                continue;
            }
            (Command::RemoveWorktrees { dir }, Some((actions, _, held)))
                if held.as_ref() == Some(&dir) =>
            {
                let _ = actions.send(Action::RemoveWorktrees);
                continue;
            }
            (Command::RemoveWorktrees { .. } | Command::ClosePanesStored { .. }, Some(_)) => {
                outlets.notify(generation, "検証の実行中は使えません");
                continue;
            }
            (
                Command::Start {
                    request,
                    target,
                    isolation,
                    user_criteria,
                    confirm,
                    max_rounds,
                    round_cap,
                    judge,
                    panel,
                },
                None,
            ) => {
                let Some(root) = verify::store::default_root() else {
                    outlets.notify(
                        generation,
                        "保存先（Noa のデータディレクトリ）が見つかりません",
                    );
                    continue;
                };
                runner::Start::New(Box::new(runner::NewVerification {
                    request,
                    target: *target,
                    isolation,
                    user_criteria,
                    confirm,
                    max_rounds,
                    round_cap,
                    judge,
                    panel,
                    root,
                }))
            }
            (
                Command::Start { .. } | Command::Resume { .. } | Command::ExtendStored { .. },
                Some(_),
            ) => {
                outlets.notify(generation, "検証はすでに始まっています");
                continue;
            }
            (cmd, None) => match stored_start(cmd) {
                Some(start) => start,
                None => {
                    outlets.notify(generation, "検証が始まっていません");
                    continue;
                }
            },
            (cmd, Some((actions, _, _))) => {
                if let Some(action) = forwarded(cmd) {
                    let _ = actions.send(action);
                }
                continue;
            }
        };
        let held = match &start {
            runner::Start::Resume { dir } | runner::Start::Extend { dir, .. } => Some(dir.clone()),
            runner::Start::New(_) => None,
        };
        let started = runner::spawn_session(start, || {});
        let ended = Arc::new(AtomicBool::new(false));
        handle = Some((started.actions(), ended.clone(), held));
        let outlets = outlets.clone();
        std::thread::Builder::new()
            .name("verify-updates".into())
            .spawn(move || forward_updates(started, ended, outlets, generation))
            .expect("spawn verify-updates thread");
    }
    if let Some((actions, _, _)) = handle {
        let _ = actions.send(Action::Stop);
    }
}

/// The runner start a command on a stored verification asks for.
fn stored_start(cmd: Command) -> Option<runner::Start> {
    match cmd {
        Command::Resume { dir } => Some(runner::Start::Resume { dir }),
        Command::ExtendStored { dir, by } => Some(runner::Start::Extend { dir, by }),
        _ => None,
    }
}

/// The runner action a command on a running verification is.
fn forwarded(cmd: Command) -> Option<Action> {
    Some(match cmd {
        Command::EditCriteria { edit } => Action::EditCriteria { edit },
        Command::StartVerify => Action::StartVerify,
        Command::Attach(attachment) => Action::Attach(attachment),
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
        | Command::ClosePanesStored { .. }
        | Command::ProbePanes { .. }
        | Command::RemoveWorktrees { .. }
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
        ..
    } = outlets;
    let mut link = Link::Connecting;
    let mut session = None;
    let mut reply_root = None;
    let mut session_dir = None;
    for update in &handle.updates {
        let sent = match update {
            Update::Session {
                session: next,
                reply_root: root,
                session_dir: dir,
            } => {
                session = Some(next);
                reply_root = Some(root);
                session_dir = Some(dir);
                snapshots
                    .send(Snapshot {
                        link: link.clone(),
                        session: session.clone(),
                        reply_root: reply_root.clone(),
                        session_dir: session_dir.clone(),
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
                let _ = notices.send((generation, format!("検証を続けられません: {why}")));
                let _ = snapshots.send(Snapshot {
                    link: Link::Down(why),
                    session,
                    reply_root,
                    session_dir,
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
    pub snapshots: Sender<Snapshot>,
    pub notices: Sender<Notice>,
    pub released: Sender<()>,
    pub versions: Sender<(u64, String)>,
    pub panes: Sender<OpenPanes>,
    pub commands: Receiver<(u64, Command)>,
}

#[cfg(test)]
impl Channel {
    /// A channel with no command thread behind it, and its far ends.
    pub fn fake() -> (Channel, FakeLink) {
        let (snapshot_tx, snapshots) = mpsc::channel();
        let (notice_tx, notices) = mpsc::channel();
        let (released_tx, released) = mpsc::channel();
        let (version_tx, versions) = mpsc::channel();
        let (panes_tx, panes) = mpsc::channel();
        let (commands, command_rx) = mpsc::channel();
        let channel = Channel {
            snapshots,
            notices,
            released,
            versions,
            panes,
            commands,
            generation: Arc::new(AtomicU64::new(0)),
        };
        let link = FakeLink {
            snapshots: snapshot_tx,
            notices: notice_tx,
            released: released_tx,
            versions: version_tx,
            panes: panes_tx,
            commands: command_rx,
        };
        (channel, link)
    }
}
