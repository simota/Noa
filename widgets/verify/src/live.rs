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
use verify::sibling::{self, Launcher};
use verify::task_links::{self, Origin};

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
    /// 元のタスクを開く: starts Tasks on the task `origin` names
    /// (`docs/specs/task-links.md` L1-7).
    OpenTasks(Box<Origin>),
}

/// The task a verification is linked to, in the [`Channel::generation`]
/// of the command that started its runner.
pub type OriginOf = (u64, Option<Origin>);

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
    /// The task each runner's verification is linked to, once it is open.
    pub origins: Receiver<OriginOf>,
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
    let (origin_tx, origins) = mpsc::channel();
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
                    origins: origin_tx,
                    wake,
                },
                Box::new(sibling::System),
            )
        })
        .expect("spawn verify-commands thread");
    Channel {
        snapshots,
        notices,
        released,
        versions,
        panes,
        origins,
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
    origins: Sender<OriginOf>,
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

fn run_commands(
    commands: Receiver<(u64, Command)>,
    outlets: Outlets,
    launcher: Box<dyn Launcher + Send>,
) {
    let mut dispatch = Dispatch {
        handle: None,
        outlets,
        launcher,
    };
    for (generation, cmd) in commands {
        dispatch.handle(generation, cmd);
    }
    if let Some(held) = dispatch.handle {
        let _ = held.actions.send(Action::Stop);
    }
}

/// The runner the command thread holds: where its actions go, whether it
/// has ended, and the stored verification it was started on (a resume or
/// an extension; `None` for a new one).
struct Held {
    actions: Sender<Action>,
    ended: Arc<AtomicBool>,
    dir: Option<PathBuf>,
}

/// What the command thread keeps between commands.
struct Dispatch {
    handle: Option<Held>,
    outlets: Outlets,
    /// What starts Tasks for 元のタスクを開く.
    launcher: Box<dyn Launcher + Send>,
}

impl Dispatch {
    /// Carries out one command of the screen's, sent in `generation`.
    fn handle(&mut self, generation: u64, cmd: Command) {
        let outlets = &self.outlets;
        if self
            .handle
            .as_ref()
            .is_some_and(|held| held.ended.load(Ordering::Acquire))
        {
            self.handle = None;
        }
        let start = match (cmd, &self.handle) {
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
                return;
            }
            // Needs no runner: the stored view has its origin too.
            (Command::OpenTasks(origin), _) => {
                let opened = task_links::tasks_args(&origin)
                    .and_then(|args| self.launcher.launch(sibling::TASKS, args));
                if let Err(why) = opened {
                    outlets.notify(generation, why);
                }
                return;
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
                return;
            }
            (Command::Leave, _) => {
                if let Some(held) = self.handle.take() {
                    let _ = held.actions.send(Action::Stop);
                }
                let _ = outlets.snapshots.send(Snapshot {
                    link: Link::Connecting,
                    session: None,
                    reply_root: None,
                    session_dir: None,
                    generation,
                });
                (outlets.wake)();
                return;
            }
            (Command::ClosePanesStored { dir }, None) => {
                outlets.detached("verify-close-panes", generation, move |_| {
                    let (Ok(text) | Err(text)) = runner::close_stored_panes(&mut IpcConnect, &dir);
                    Some(text)
                });
                return;
            }
            (Command::RemoveWorktrees { dir }, None) => {
                outlets.detached("verify-worktrees", generation, move |outlets| {
                    let (Ok(text) | Err(text)) = runner::remove_stored_worktrees(&dir);
                    // The list shows the worktrees' state from the log.
                    let _ = outlets.released.send(());
                    Some(text)
                });
                return;
            }
            (Command::RemoveWorktrees { dir }, Some(held)) if held.dir.as_ref() == Some(&dir) => {
                let _ = held.actions.send(Action::RemoveWorktrees);
                return;
            }
            (Command::RemoveWorktrees { .. } | Command::ClosePanesStored { .. }, Some(_)) => {
                outlets.notify(generation, "検証の実行中は使えません");
                return;
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
                    return;
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
                return;
            }
            (cmd, None) => match stored_start(cmd) {
                Some(start) => start,
                None => {
                    outlets.notify(generation, "検証が始まっていません");
                    return;
                }
            },
            (cmd, Some(held)) => {
                if let Some(action) = forwarded(cmd) {
                    let _ = held.actions.send(action);
                }
                return;
            }
        };
        let dir = match &start {
            runner::Start::Resume { dir } | runner::Start::Extend { dir, .. } => Some(dir.clone()),
            runner::Start::New(_) => None,
        };
        let started = runner::spawn_session(start, || {});
        let ended = Arc::new(AtomicBool::new(false));
        self.handle = Some(Held {
            actions: started.actions(),
            ended: ended.clone(),
            dir,
        });
        let outlets = outlets.clone();
        std::thread::Builder::new()
            .name("verify-updates".into())
            .spawn(move || forward_updates(started, ended, outlets, generation))
            .expect("spawn verify-updates thread");
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
        | Command::OpenTasks(_)
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
        origins,
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
            Update::Origin(origin) => origins.send((generation, origin)).is_ok(),
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
    pub origins: Sender<OriginOf>,
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
        let (origin_tx, origins) = mpsc::channel();
        let (commands, command_rx) = mpsc::channel();
        let channel = Channel {
            snapshots,
            notices,
            released,
            versions,
            panes,
            origins,
            commands,
            generation: Arc::new(AtomicU64::new(0)),
        };
        let link = FakeLink {
            snapshots: snapshot_tx,
            notices: notice_tx,
            released: released_tx,
            versions: version_tx,
            panes: panes_tx,
            origins: origin_tx,
            commands: command_rx,
        };
        (channel, link)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(5);
    /// How long a request that should say nothing is given to say it.
    const QUIET: Duration = Duration::from_millis(300);

    /// Outlets whose far ends a test reads.
    struct Ends {
        snapshots: Receiver<Snapshot>,
        notices: Receiver<Notice>,
        released: Receiver<()>,
        panes: Receiver<OpenPanes>,
    }

    fn outlets() -> (Outlets, Ends) {
        let (snapshots, snapshot_rx) = mpsc::channel();
        let (notices, notice_rx) = mpsc::channel();
        let (released, released_rx) = mpsc::channel();
        let (panes, panes_rx) = mpsc::channel();
        let outlets = Outlets {
            snapshots,
            notices,
            released,
            versions: mpsc::channel().0,
            panes,
            origins: mpsc::channel().0,
            wake: Arc::new(|| {}),
        };
        let ends = Ends {
            snapshots: snapshot_rx,
            notices: notice_rx,
            released: released_rx,
            panes: panes_rx,
        };
        (outlets, ends)
    }

    /// A directory no verification is stored in.
    fn missing(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("noa-verify-no-such-{name}-{}", std::process::id()))
    }

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
    fn a_command_for_a_verification_that_is_not_running_says_so() {
        let channel = start(|| {}, Link::Connecting);
        for cmd in [
            Command::Focus {
                part: PartId::new("claude"),
            },
            Command::ClosePanes,
            Command::StartVerify,
        ] {
            channel.send(cmd);
            let (generation, notice) = channel.notices.recv_timeout(WAIT).unwrap();
            assert_eq!(generation, 0);
            assert!(notice.contains("始まっていません"), "{notice}");
        }
    }

    #[test]
    fn round_and_criteria_control_goes_to_the_running_verification() {
        assert_eq!(
            forwarded(Command::SetHold { hold: true }),
            Some(Action::SetHold { hold: true })
        );
        assert_eq!(forwarded(Command::Continue), Some(Action::Continue));
        assert_eq!(
            forwarded(Command::Extend { by: 3 }),
            Some(Action::Extend { by: 3 })
        );
        assert_eq!(forwarded(Command::StartVerify), Some(Action::StartVerify));
        assert_eq!(forwarded(Command::ClosePanes), Some(Action::ClosePanes));
        let edit = verify::model::types::CriteriaEdit::Remove { id: "AC-2".into() };
        assert_eq!(
            forwarded(Command::EditCriteria { edit: edit.clone() }),
            Some(Action::EditCriteria { edit })
        );
        let attachment = Box::new(Attachment {
            source: PathBuf::from("/x/shot.png"),
            caption: "c".into(),
            marks: Vec::new(),
            shows: vec!["AC-1".into()],
        });
        assert_eq!(
            forwarded(Command::Attach(attachment.clone())),
            Some(Action::Attach(attachment))
        );
        // What concerns a stored verification is never the runner's action.
        let dir = PathBuf::from("/x");
        for cmd in [
            Command::ExtendStored {
                dir: dir.clone(),
                by: 1,
            },
            Command::ClosePanesStored { dir: dir.clone() },
            Command::ProbePanes { dir: dir.clone() },
            Command::RemoveWorktrees { dir },
        ] {
            assert_eq!(forwarded(cmd), None);
        }
    }

    /// What each command on a stored verification starts (the runner's
    /// tests cover what `Start::Extend` and `Start::Resume` then do).
    #[test]
    fn a_stored_verification_is_extended_by_an_extending_start() {
        let dir = PathBuf::from("/x/ver-1");
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

    /// Extending a stored verification starts a runner on it, whose
    /// refusal comes back as a notice of the command's generation.
    #[test]
    fn extending_a_stored_verification_starts_a_runner_on_it() {
        let (outlets, ends) = outlets();
        let (commands, command_rx) = mpsc::channel();
        commands
            .send((
                2,
                Command::ExtendStored {
                    dir: missing("stored"),
                    by: 1,
                },
            ))
            .unwrap();
        drop(commands);
        run_commands(command_rx, outlets, Box::new(sibling::Offline("offline")));
        let (generation, notice) = ends.notices.recv_timeout(WAIT).unwrap();
        assert_eq!(generation, 2);
        assert!(notice.contains("を開けません"), "{notice}");
        ends.released.recv_timeout(WAIT).unwrap();
    }

    /// A runner started by a command is stamped with the generation the
    /// command was sent in, even when the screen has moved on since.
    #[test]
    fn a_runner_keeps_the_generation_of_the_command_that_started_it() {
        let (outlets, ends) = outlets();
        let (commands, command_rx) = mpsc::channel();
        // Sent in generation 0, handled after the screen left (generation 1).
        commands
            .send((
                0,
                Command::Resume {
                    dir: missing("log"),
                },
            ))
            .unwrap();
        drop(commands);
        run_commands(command_rx, outlets, Box::new(sibling::Offline("offline")));
        let (generation, notice) = ends.notices.recv_timeout(WAIT).unwrap();
        assert_eq!(generation, 0);
        assert!(notice.contains("続けられません"), "{notice}");
        let last = ends.snapshots.recv_timeout(WAIT).unwrap();
        assert_eq!(last.generation, 0);
        ends.released.recv_timeout(WAIT).unwrap();
    }

    /// The command thread holding a runner started on `dir`, and that
    /// runner's far end.
    fn holding(dir: &Path) -> (Dispatch, Ends, Receiver<Action>) {
        let (outlets, ends) = outlets();
        let (actions, action_rx) = mpsc::channel();
        let dispatch = Dispatch {
            handle: Some(Held {
                actions,
                ended: Arc::new(AtomicBool::new(false)),
                dir: Some(dir.to_owned()),
            }),
            outlets,
            launcher: Box::new(sibling::Offline("offline")),
        };
        (dispatch, ends, action_rx)
    }

    #[test]
    fn removing_the_worktrees_of_the_verification_a_runner_holds_goes_through_it() {
        let dir = missing("held");
        let (mut dispatch, ends, actions) = holding(&dir);
        dispatch.handle(4, Command::RemoveWorktrees { dir: dir.clone() });
        assert_eq!(actions.try_recv(), Ok(Action::RemoveWorktrees));
        assert_eq!(
            ends.notices.recv_timeout(QUIET),
            Err(RecvTimeoutError::Timeout)
        );
        // Another stored verification, or its panes, while one runs.
        for cmd in [
            Command::RemoveWorktrees {
                dir: missing("other"),
            },
            Command::ClosePanesStored { dir: dir.clone() },
        ] {
            dispatch.handle(4, cmd);
            let (generation, notice) = ends.notices.recv_timeout(WAIT).unwrap();
            assert_eq!(generation, 4);
            assert_eq!(notice, "検証の実行中は使えません");
        }
        assert!(actions.try_recv().is_err(), "the runner was asked");
    }

    #[test]
    fn removing_the_worktrees_of_a_stored_verification_needs_no_runner() {
        let (outlets, ends) = outlets();
        let mut dispatch = Dispatch {
            handle: None,
            outlets,
            launcher: Box::new(sibling::Offline("offline")),
        };
        dispatch.handle(
            3,
            Command::RemoveWorktrees {
                dir: missing("worktrees"),
            },
        );
        let (generation, notice) = ends.notices.recv_timeout(WAIT).unwrap();
        assert_eq!(generation, 3);
        assert!(notice.contains("を開けません"), "{notice}");
        // The list is read again: the log may have changed.
        ends.released.recv_timeout(WAIT).unwrap();
    }

    #[test]
    fn ac36_the_panes_of_a_stored_verification_are_closed_and_counted_without_a_runner() {
        let (outlets, ends) = outlets();
        let mut dispatch = Dispatch {
            handle: None,
            outlets,
            launcher: Box::new(sibling::Offline("offline")),
        };
        dispatch.handle(
            5,
            Command::ClosePanesStored {
                dir: missing("panes"),
            },
        );
        let (generation, notice) = ends.notices.recv_timeout(WAIT).unwrap();
        assert_eq!(generation, 5);
        assert!(notice.contains("を開けません"), "{notice}");
        // A count that cannot be made is not answered, and says nothing.
        dispatch.handle(
            5,
            Command::ProbePanes {
                dir: missing("probe"),
            },
        );
        assert_eq!(
            ends.notices.recv_timeout(QUIET),
            Err(RecvTimeoutError::Timeout)
        );
        assert_eq!(ends.panes.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert!(dispatch.handle.is_none(), "a runner was started");
    }

    /// What [`Recording`] was asked to start, in order.
    type Launched = Arc<std::sync::Mutex<Vec<(sibling::Helper, Vec<std::ffi::OsString>)>>>;

    /// A launcher that records what it was asked to start.
    struct Recording(Launched);

    impl Launcher for Recording {
        fn launch(
            &mut self,
            helper: sibling::Helper,
            args: Vec<std::ffi::OsString>,
        ) -> Result<(), String> {
            self.0.lock().unwrap().push((helper, args));
            Ok(())
        }
    }

    #[test]
    fn links_ac5_open_tasks_launches_tasks_on_the_origins_repo_and_task() {
        let repo = missing("links-repo");
        std::fs::create_dir_all(&repo).unwrap();
        let (outlets, ends) = outlets();
        let launched = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut dispatch = Dispatch {
            handle: None,
            outlets,
            launcher: Box::new(Recording(Arc::clone(&launched))),
        };
        let origin = Origin {
            key_dir: repo.join("key"),
            tasks_repo: Some(repo.clone()),
            task: task_links::TaskKey(serde_json::json!({"github": {"repo": "o/n", "number": 12}})),
            run: "run-1".into(),
        };
        dispatch.handle(1, Command::OpenTasks(Box::new(origin.clone())));
        let mut repo_arg = std::ffi::OsString::from("--repo=");
        repo_arg.push(&repo);
        assert_eq!(
            *launched.lock().unwrap(),
            [(sibling::TASKS, vec![repo_arg, "--select=#12".into()])]
        );
        assert_eq!(
            ends.notices.recv_timeout(QUIET),
            Err(RecvTimeoutError::Timeout)
        );
        // No repository known: nothing starts, the notice says why.
        let unknown = Origin {
            tasks_repo: None,
            ..origin.clone()
        };
        dispatch.handle(1, Command::OpenTasks(Box::new(unknown)));
        assert_eq!(launched.lock().unwrap().len(), 1);
        let (generation, notice) = ends.notices.recv_timeout(WAIT).unwrap();
        assert_eq!(generation, 1);
        assert_eq!(notice, "タスクのリポジトリがわかりません");
        // A launcher that fails says so.
        dispatch.launcher = Box::new(sibling::Offline("Tasks が見つかりません"));
        dispatch.handle(1, Command::OpenTasks(Box::new(origin)));
        let (_, notice) = ends.notices.recv_timeout(WAIT).unwrap();
        assert_eq!(notice, "Tasks が見つかりません");
        assert!(dispatch.handle.is_none(), "a runner was started");
        let _ = std::fs::remove_dir_all(repo);
    }
}
