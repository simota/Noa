//! The link between the UI and the running discussion: the UI reads
//! [`Snapshot`]s and sends [`Command`]s over std mpsc, as repo-browser does.
//!
//! [`start`] bridges them to the session runner (`crate::session::runner`):
//! a [`Command::Start`] (or [`Command::Resume`]) spawns the runner, later
//! commands become its actions, and its updates become snapshots and
//! notices. [`Command::Open`] shows a stored discussion without a runner.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use crate::installed::Installed;
use crate::session::runner::{self, Action, SessionHandle, Update};

use discussion::model::types::{
    AgentChoice, AgentKind, DebateId, IssueId, PartId, RoundNo, Session,
};
use discussion::store::Summary;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Live,
    Down(String),
    /// The discussion stored at this directory, shown from its log: no
    /// runner and no link to Noa, so nothing can change it. The directory
    /// comes with the session it shows, so "再開" always names what is shown.
    Stored(PathBuf),
}

/// What the UI shows: the session as the engine folded it (none before the
/// user started one) and the state of the link to Noa.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub link: Link,
    pub session: Option<Arc<Session>>,
}

/// What the user asked for. Each maps onto one engine input (named in the
/// variant's doc), which the live layer stamps with the time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Create a session and spawn its agents (`Engine::create`, then `Start`).
    Start {
        topic: String,
        /// The directory (or repository) the agents run in; `None` for the
        /// discussions directory.
        workdir: Option<PathBuf>,
        /// After this many rounds the discussion ends; `None` for no limit.
        max_rounds: Option<RoundNo>,
        moderator: AgentChoice,
        panel: Vec<AgentChoice>,
    },
    /// Show the stored discussion at `dir` read-only: its log is folded,
    /// and no agent or runner starts.
    Open { dir: PathBuf },
    /// Pick up the stored discussion at `dir` where it stopped: its agents'
    /// panes are taken back or started again (`Engine::resume`, `Start`).
    Resume { dir: PathBuf },
    /// Leave the discussion shown (its runner stops; the agents' panes stay
    /// open) and go back to the start view.
    NewDiscussion,
    /// `UserIntervention`; `to` empty means everyone.
    Intervene { to: Vec<PartId>, body: String },
    /// `UserStartDebate` (⚔ on an issue) with the sides the user settled on.
    StartDebate {
        issue: IssueId,
        pro: Vec<PartId>,
        con: Vec<PartId>,
    },
    /// `UserApproveDebate` with the sides as shown (swapped or not).
    ApproveDebate {
        id: DebateId,
        pro: Vec<PartId>,
        con: Vec<PartId>,
    },
    /// `UserDeclineDebate`.
    DeclineDebate { id: DebateId },
    /// `UserRetry`: send the round's failed briefs again (a failed moderator
    /// halts the round until then).
    Retry,
    /// `UserClose`: end the discussion.
    Close,
    /// Start an agent of this kind in a new tab, in the working directory,
    /// on the discussion's result.
    Implement { agent: AgentKind },
    /// Bring a participant's pane to the front in Noa (`noa.focusPane`).
    Focus { part: PartId },
    /// `UserSetHold`: hold the discussion after each round, or stop.
    SetHold { hold: bool },
    /// `UserContinue`: start the next round of a held discussion.
    Continue,
    /// `UserExtend`: this many more rounds. On a stored discussion shown
    /// read-only, it is resumed first (its agents run again).
    Extend { by: RoundNo },
    /// Close every participant's pane of the ended discussion.
    ClosePanes,
}

pub struct Channel {
    pub snapshots: Receiver<Snapshot>,
    /// One-line messages for the user (a command that failed, and why).
    pub notices: Receiver<String>,
    /// The stored discussions, newest first: sent when the start view is
    /// shown (at launch and after [`Command::NewDiscussion`]).
    pub history: Receiver<Arc<Vec<Summary>>>,
    /// Which agents are installed: sent once, when the check is done (see
    /// [`start`]).
    pub installed: Receiver<Installed>,
    commands: Sender<Command>,
}

impl Channel {
    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }
}

/// The live channel. It shows the start view (no session) until the user
/// starts a discussion; then the runner opens the agents' panes in Noa.
/// It also checks, on its own thread, which agents the login shell finds.
pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Channel {
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
    let installed = crate::installed::spawn({
        let wake = wake.clone();
        move || wake()
    });
    let mut channel = start_in(discussion::store::default_root(), move || wake());
    channel.installed = installed;
    channel
}

/// [`start`] with the discussions stored under `root` (`None`: no place to
/// store them was found), and without the check for installed agents: its
/// `installed` never delivers.
pub fn start_in(root: Option<PathBuf>, wake: impl Fn() + Send + Sync + 'static) -> Channel {
    start_with(root, wake, |start| runner::spawn_session(start, || {}))
}

/// [`start_in`] with the runners started by `spawn_session`.
fn start_with(
    root: Option<PathBuf>,
    wake: impl Fn() + Send + Sync + 'static,
    spawn_session: impl Fn(runner::Start) -> SessionHandle + Send + 'static,
) -> Channel {
    let (snapshot_tx, snapshots) = mpsc::channel();
    let (notice_tx, notices) = mpsc::channel();
    let (history_tx, history) = mpsc::channel();
    let (commands, command_rx) = mpsc::channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
    let out = Out {
        snapshots: snapshot_tx,
        notices: notice_tx,
        history: history_tx,
        wake,
    };
    out.start_view(root.as_deref());
    std::thread::Builder::new()
        .name("discussion-commands".into())
        .spawn(move || run_commands(command_rx, root, out, spawn_session))
        .expect("spawn discussion-commands thread");
    Channel {
        snapshots,
        notices,
        history,
        installed: mpsc::channel().1,
        commands,
    }
}

/// The UI's ends of the channel, as the commands thread holds them.
#[derive(Clone)]
struct Out {
    snapshots: Sender<Snapshot>,
    notices: Sender<String>,
    history: Sender<Arc<Vec<Summary>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Out {
    fn notice(&self, text: impl Into<String>) {
        let _ = self.notices.send(text.into());
        (self.wake)();
    }

    /// The start view: no session, and the stored discussions to pick from.
    fn start_view(&self, root: Option<&Path>) {
        let listed = match root.map(discussion::store::summaries) {
            Some(Ok(listed)) => listed,
            Some(Err(err)) => {
                let _ = self.notices.send(format!("過去の議論を読めません: {err}"));
                Vec::new()
            }
            None => Vec::new(),
        };
        let _ = self.history.send(Arc::new(listed));
        let _ = self.snapshots.send(Snapshot {
            link: Link::Connecting,
            session: None,
        });
        (self.wake)();
    }
}

/// A runner the commands thread started.
struct Running {
    actions: Sender<Action>,
    /// Set once the runner has ended: a session that failed to open or
    /// stopped leaves the user free to start another.
    ended: Arc<AtomicBool>,
    /// The thread forwarding its updates; it ends after the runner does.
    updates: JoinHandle<()>,
}

impl Running {
    fn ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }

    /// Stops the runner and waits until it has ended, so nothing of it
    /// reaches the UI (or the session's log) afterwards.
    fn stop(self) {
        let _ = self.actions.send(Action::Stop);
        drop(self.actions);
        let _ = self.updates.join();
    }
}

/// What the window shows, as far as the commands thread is concerned.
enum Shown {
    /// The start view.
    Nothing,
    /// The stored discussion at this directory, read-only.
    Stored(PathBuf),
    Running(Running),
}

impl Shown {
    /// Whether a running discussion is in the way of opening another.
    fn busy(&self) -> bool {
        matches!(self, Shown::Running(running) if !running.ended())
    }

    /// Leaves what is shown: a runner is stopped and waited for.
    fn leave(&mut self) {
        if let Shown::Running(running) = std::mem::replace(self, Shown::Nothing) {
            running.stop();
        }
    }
}

/// Turns the UI's commands into runner actions, starting a runner on
/// [`Command::Start`] or [`Command::Resume`]. Returns when the UI drops its
/// channel, after stopping the runner.
fn run_commands(
    commands: Receiver<Command>,
    root: Option<PathBuf>,
    out: Out,
    spawn_session: impl Fn(runner::Start) -> SessionHandle,
) {
    let mut shown = Shown::Nothing;
    for cmd in commands {
        // Extending a stored discussion resumes it, then extends it.
        if let (Command::Extend { by }, Shown::Stored(dir)) = (&cmd, &shown) {
            if let Some(running) = resume_to_extend(dir.clone(), *by, &out, &spawn_session) {
                shown = Shown::Running(running);
            }
            continue;
        }
        let start = match cmd {
            Command::Start { .. } | Command::Resume { .. } | Command::Open { .. }
                if shown.busy() =>
            {
                out.notice("議論はすでに始まっています");
                continue;
            }
            Command::Start {
                topic,
                workdir,
                max_rounds,
                moderator,
                panel,
            } => {
                let Some(root) = root.clone() else {
                    out.notice("議論の保存先（Noa のデータディレクトリ）が見つかりません");
                    continue;
                };
                runner::Start::New {
                    topic,
                    workdir,
                    max_rounds,
                    moderator,
                    panel,
                    root,
                }
            }
            Command::Resume { dir } => {
                if discussion::store::held_elsewhere(&dir) {
                    out.notice(format!("{} は別のウィンドウで実行中です", dir.display()));
                    continue;
                }
                match discussion::store::load(&dir) {
                    // Ended meanwhile (in another window): nothing to run,
                    // so it is shown, not resumed.
                    Ok(session) if session.closed => {
                        out.notice("この議論は終了しています。記録を表示します");
                        show_stored(&mut shown, dir, session, &out);
                        continue;
                    }
                    Ok(_) => runner::Start::Resume { dir },
                    Err(err) => {
                        out.notice(format!("{} を開けません: {err}", dir.display()));
                        continue;
                    }
                }
            }
            Command::Open { dir } => {
                match discussion::store::load(&dir) {
                    Ok(session) => show_stored(&mut shown, dir, session, &out),
                    Err(err) => out.notice(format!("{} を開けません: {err}", dir.display())),
                }
                continue;
            }
            Command::NewDiscussion => {
                shown.leave();
                out.start_view(root.as_deref());
                continue;
            }
            other => {
                match (&shown, action_of(other)) {
                    (Shown::Running(running), Some(action)) if !running.ended() => {
                        let _ = running.actions.send(action);
                    }
                    (Shown::Stored(_), _) => {
                        out.notice("記録を表示しています。操作するには再開してください")
                    }
                    _ => out.notice("議論が始まっていません"),
                }
                continue;
            }
        };
        shown.leave();
        shown = Shown::Running(spawn(spawn_session(start), &out));
    }
    // The UI is gone: end the runner.
    shown.leave();
}

/// Shows the stored discussion at `dir` read-only, leaving what was shown.
fn show_stored(shown: &mut Shown, dir: PathBuf, session: Session, out: &Out) {
    shown.leave();
    let _ = out.snapshots.send(Snapshot {
        link: Link::Stored(dir.clone()),
        session: Some(Arc::new(session)),
    });
    (out.wake)();
    *shown = Shown::Stored(dir);
}

/// Resumes the stored discussion at `dir` and gives it `by` more rounds,
/// when it may get them; otherwise says why, and nothing starts. The
/// runner extends it before it starts any pane.
fn resume_to_extend(
    dir: PathBuf,
    by: RoundNo,
    out: &Out,
    spawn_session: &impl Fn(runner::Start) -> SessionHandle,
) -> Option<Running> {
    if discussion::store::held_elsewhere(&dir) {
        out.notice(format!("{} は別のウィンドウで実行中です", dir.display()));
        return None;
    }
    match discussion::store::load(&dir) {
        Ok(session) => {
            if let Err(why) = session.extendable() {
                out.notice(format!("ラウンドを追加できません: {why}"));
                return None;
            }
        }
        Err(err) => {
            out.notice(format!("{} を開けません: {err}", dir.display()));
            return None;
        }
    }
    Some(spawn(spawn_session(runner::Start::Extend { dir, by }), out))
}

/// The runner action a command in a running discussion becomes; `None`
/// for the commands handled above.
fn action_of(cmd: Command) -> Option<Action> {
    Some(match cmd {
        Command::Intervene { to, body } => Action::Intervene { to, body },
        Command::StartDebate { issue, pro, con } => Action::StartDebate {
            issue,
            pro: Some(pro),
            con: Some(con),
        },
        Command::ApproveDebate { id, pro, con } => Action::ApproveDebate {
            id,
            pro: Some(pro),
            con: Some(con),
        },
        Command::DeclineDebate { id } => Action::DeclineDebate { id },
        Command::Retry => Action::Retry,
        Command::Close => Action::Close,
        Command::Implement { agent } => Action::Implement { agent },
        Command::Focus { part } => Action::Focus { part },
        Command::SetHold { hold } => Action::SetHold { hold },
        Command::Continue => Action::Continue,
        Command::Extend { by } => Action::Extend { by },
        Command::ClosePanes => Action::ClosePanes,
        Command::Start { .. }
        | Command::Open { .. }
        | Command::Resume { .. }
        | Command::NewDiscussion => return None,
    })
}

/// Starts the thread that forwards a new runner's updates.
fn spawn(started: SessionHandle, out: &Out) -> Running {
    let ended = Arc::new(AtomicBool::new(false));
    let actions = started.actions();
    let (out, flag) = (out.clone(), ended.clone());
    let updates = std::thread::Builder::new()
        .name("discussion-updates".into())
        .spawn(move || forward_updates(started, flag, out))
        .expect("spawn discussion-updates thread");
    Running {
        actions,
        ended,
        updates,
    }
}

/// Turns runner updates into snapshots (the latest session with the latest
/// link) and notices, until the runner ends or the UI is gone.
fn forward_updates(handle: SessionHandle, ended: Arc<AtomicBool>, out: Out) {
    let mut link = Link::Connecting;
    let mut session = None;
    for update in &handle.updates {
        let sent = match update {
            Update::Session(next) => {
                session = Some(next);
                out.snapshots
                    .send(Snapshot {
                        link: link.clone(),
                        session: session.clone(),
                    })
                    .is_ok()
            }
            Update::Link(next) => {
                link = match next {
                    runner::Link::Connecting => Link::Connecting,
                    runner::Link::Live => Link::Live,
                    runner::Link::Down(why) => Link::Down(why),
                };
                out.snapshots
                    .send(Snapshot {
                        link: link.clone(),
                        session: session.clone(),
                    })
                    .is_ok()
            }
            Update::Notice(text) => out.notices.send(text).is_ok(),
            Update::Ended(Ok(())) => break,
            Update::Ended(Err(why)) => {
                // Before the notice: a start the user tries after seeing it
                // must not meet the ended session.
                ended.store(true, Ordering::Release);
                let _ = out.notices.send(format!("議論を続けられません: {why}"));
                let _ = out.snapshots.send(Snapshot {
                    link: Link::Down(why),
                    session,
                });
                (out.wake)();
                break;
            }
        };
        if !sent {
            break;
        }
        (out.wake)();
    }
    ended.store(true, Ordering::Release);
}

#[cfg(test)]
pub(crate) struct TestFeed {
    pub snapshots: Sender<Snapshot>,
    pub notices: Sender<String>,
    pub history: Sender<Arc<Vec<Summary>>>,
    pub installed: Sender<Installed>,
    pub commands: Receiver<Command>,
}

#[cfg(test)]
pub(crate) fn test_channel() -> (Channel, TestFeed) {
    let (snapshot_tx, snapshots) = mpsc::channel();
    let (notice_tx, notices) = mpsc::channel();
    let (history_tx, history) = mpsc::channel();
    let (installed_tx, installed) = mpsc::channel();
    let (commands, command_rx) = mpsc::channel();
    (
        Channel {
            snapshots,
            notices,
            history,
            installed,
            commands,
        },
        TestFeed {
            snapshots: snapshot_tx,
            notices: notice_tx,
            history: history_tx,
            installed: installed_tx,
            commands: command_rx,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The notices that arrive within `wait`.
    fn notices_within(channel: &Channel, wait: Duration) -> Vec<String> {
        let mut seen = Vec::new();
        while let Ok(notice) = channel.notices.recv_timeout(wait) {
            seen.push(notice);
        }
        seen
    }

    #[test]
    fn a_start_that_fails_can_be_tried_again() {
        // A working directory that is a file: the runner stops before it
        // connects to Noa or stores anything.
        let file =
            std::env::temp_dir().join(format!("noa-discussion-not-a-dir-{}", std::process::id()));
        std::fs::write(&file, "").unwrap();
        let channel = start(|| {});
        let start = || Command::Start {
            topic: "t".into(),
            workdir: Some(file.clone()),
            max_rounds: None,
            moderator: AgentKind::Claude.into(),
            panel: vec![AgentKind::Codex.into()],
        };
        channel.send(start());
        let first = notices_within(&channel, Duration::from_millis(500));
        assert!(
            first
                .iter()
                .any(|n| n.contains("ディレクトリではありません")),
            "{first:?}"
        );
        channel.send(start());
        let second = notices_within(&channel, Duration::from_millis(500));
        assert!(
            !second.iter().any(|n| n.contains("すでに始まっています")),
            "{second:?}"
        );
        assert!(
            second
                .iter()
                .any(|n| n.contains("ディレクトリではありません")),
            "{second:?}"
        );
        let _ = std::fs::remove_file(&file);
    }

    fn temp_root(name: &str) -> PathBuf {
        use std::sync::atomic::AtomicUsize;
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "noa-discussion-channel-{}-{name}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Stores a session `id` under `root`; closed after one round when
    /// `closed`.
    fn stored(root: &Path, id: &str, closed: bool) -> PathBuf {
        use discussion::engine::{Config, Engine, Setup};
        use discussion::model::reduce::Event;
        use discussion::store::{self, Store};
        let dir = store::session_dir(root, &discussion::model::types::SessionId::new(id));
        let setup = Setup {
            topic: "Pick a database",
            workdir: None,
            max_rounds: Some(1),
            moderator: AgentKind::Claude.into(),
            panel: &[AgentKind::Codex.into()],
        };
        let (_, out) = Engine::create_in(
            discussion::model::types::SessionId::new(id),
            setup,
            &dir,
            Config::default(),
            7,
        )
        .unwrap();
        let mut store = Store::create(&dir, &out.batches[0]).unwrap();
        if closed {
            store.append(&[Event::SessionClosed]).unwrap();
        }
        dir
    }

    fn snapshot_within(channel: &Channel, wait: Duration) -> Option<Snapshot> {
        channel.snapshots.recv_timeout(wait).ok()
    }

    /// A stand-in runner per start: it publishes `session` and, once
    /// stopped, takes `linger` to end. Every start it got goes to `starts`.
    fn fake_runner(
        session: Session,
        gate: Gate,
        starts: Sender<runner::Start>,
    ) -> impl Fn(runner::Start) -> SessionHandle + Send + 'static {
        fake_runner_seeing(session, gate, starts, mpsc::channel().0)
    }

    /// Holds a stopped stand-in runner until the test lets it end; `None`
    /// lets it end at once.
    type Gate = Option<Arc<std::sync::Mutex<Receiver<()>>>>;

    /// [`fake_runner`] that passes on every action it gets but `Stop`.
    fn fake_runner_seeing(
        session: Session,
        gate: Gate,
        starts: Sender<runner::Start>,
        seen: Sender<Action>,
    ) -> impl Fn(runner::Start) -> SessionHandle + Send + 'static {
        move |start| {
            let _ = starts.send(start);
            let (seen, gate) = (seen.clone(), gate.clone());
            let (update_tx, updates) = mpsc::channel();
            let (actions, action_rx) = mpsc::channel();
            let session = Arc::new(session.clone());
            std::thread::spawn(move || {
                let _ = update_tx.send(Update::Session(session));
                for action in action_rx {
                    if action == Action::Stop {
                        break;
                    }
                    let _ = seen.send(action);
                }
                if let Some(gate) = gate {
                    let _ = gate.lock().unwrap().recv();
                }
                // Sent late: it must not reach the UI after the start view.
                let _ = update_tx.send(Update::Notice("late".into()));
                let _ = update_tx.send(Update::Ended(Ok(())));
            });
            SessionHandle::new(updates, actions)
        }
    }

    #[test]
    fn the_start_view_lists_the_stored_discussions_and_opens_one_read_only() {
        let root = temp_root("open");
        let closed = stored(&root, "disc-1", true);
        stored(&root, "disc-2", false);
        let channel = start_in(Some(root.clone()), || {});
        let listed = channel
            .history
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let ids: Vec<&str> = listed.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["disc-2", "disc-1"]);
        let first = snapshot_within(&channel, Duration::from_secs(1)).unwrap();
        assert!(first.session.is_none());

        channel.send(Command::Open {
            dir: closed.clone(),
        });
        let shown = snapshot_within(&channel, Duration::from_secs(1)).unwrap();
        assert_eq!(shown.link, Link::Stored(closed.clone()));
        let session = shown.session.unwrap();
        assert!(session.closed);
        assert_eq!(session.id.as_str(), "disc-1");
        // Nothing runs: no further snapshot, and controls are refused.
        channel.send(Command::Retry);
        let notices = notices_within(&channel, Duration::from_millis(300));
        assert!(
            notices.iter().any(|n| n.contains("記録を表示しています")),
            "{notices:?}"
        );
        assert!(snapshot_within(&channel, Duration::from_millis(100)).is_none());

        // Back to the start view, with the list again.
        channel.send(Command::NewDiscussion);
        let listed = channel
            .history
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(listed.len(), 2);
        let back = snapshot_within(&channel, Duration::from_secs(1)).unwrap();
        assert!(back.session.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_damaged_log_is_refused_and_the_start_view_stays() {
        let root = temp_root("damaged");
        let dir = root.join("disc-9");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(discussion::store::EVENTS_FILE), "garbage\n").unwrap();
        let channel = start_in(Some(root.clone()), || {});
        let listed = channel
            .history
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(listed[0].state.is_err());
        let _ = snapshot_within(&channel, Duration::from_secs(1));
        channel.send(Command::Open { dir: dir.clone() });
        let notices = notices_within(&channel, Duration::from_millis(300));
        assert!(
            notices.iter().any(|n| n.contains("を開けません")),
            "{notices:?}"
        );
        // Resuming it fails in the runner before it connects to Noa; it can
        // be tried again.
        for _ in 0..2 {
            channel.send(Command::Resume { dir: dir.clone() });
            let notices = notices_within(&channel, Duration::from_millis(500));
            assert!(
                notices.iter().any(|n| n.contains("を開けません")),
                "{notices:?}"
            );
            assert!(
                !notices.iter().any(|n| n.contains("すでに始まっています")),
                "{notices:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_new_discussion_stops_the_runner_first_and_another_can_start() {
        let root = temp_root("new");
        let dir = stored(&root, "disc-3", false);
        let session = discussion::store::load(&dir).unwrap();
        let (starts_tx, starts) = mpsc::channel();
        let (open_gate, gate) = mpsc::channel();
        let channel = start_with(
            Some(root.clone()),
            || {},
            fake_runner(
                session,
                Some(Arc::new(std::sync::Mutex::new(gate))),
                starts_tx,
            ),
        );
        let _ = channel.history.recv_timeout(Duration::from_secs(1));
        let _ = snapshot_within(&channel, Duration::from_secs(1));

        channel.send(Command::Resume { dir: dir.clone() });
        let live = snapshot_within(&channel, Duration::from_secs(1)).unwrap();
        assert!(live.session.is_some());
        assert_eq!(
            starts.recv_timeout(Duration::from_secs(1)).unwrap(),
            runner::Start::Resume { dir: dir.clone() }
        );
        // Another start while it runs is refused.
        channel.send(Command::Resume { dir: dir.clone() });
        let notices = notices_within(&channel, Duration::from_millis(200));
        assert!(
            notices.iter().any(|n| n.contains("すでに始まっています")),
            "{notices:?}"
        );

        channel.send(Command::NewDiscussion);
        // The old runner has not ended: the start view waits for it.
        assert!(snapshot_within(&channel, Duration::from_millis(300)).is_none());
        assert!(channel.history.try_recv().is_err());
        open_gate.send(()).unwrap();
        // Its last words come, then the start view, and nothing of it after.
        assert_eq!(
            channel
                .notices
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            "late"
        );
        let back = snapshot_within(&channel, Duration::from_secs(2)).unwrap();
        assert!(back.session.is_none(), "the start view");
        assert!(notices_within(&channel, Duration::from_millis(200)).is_empty());
        assert!(snapshot_within(&channel, Duration::from_millis(100)).is_none());

        channel.send(Command::Resume { dir: dir.clone() });
        assert!(starts.recv_timeout(Duration::from_secs(1)).is_ok());
        assert!(
            snapshot_within(&channel, Duration::from_secs(1))
                .unwrap()
                .session
                .is_some()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_focus_reaches_the_runner_as_its_action() {
        assert_eq!(
            action_of(Command::Focus {
                part: PartId::new("codex")
            }),
            Some(Action::Focus {
                part: PartId::new("codex")
            })
        );
        assert_eq!(action_of(Command::NewDiscussion), None);
    }

    #[test]
    fn extending_a_stored_discussion_resumes_it_first_when_it_ran_out_of_rounds() {
        let root = temp_root("extend");
        let ran_out = stored(&root, "disc-4", true);
        // Ran its one round, then ended.
        {
            use discussion::model::reduce::Event;
            use discussion::model::types::{Phase, RoundKind};
            let text =
                std::fs::read_to_string(ran_out.join(discussion::store::EVENTS_FILE)).unwrap();
            let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
            let closed = lines.pop().unwrap();
            for batch in [
                vec![Event::RoundStarted {
                    no: 1,
                    kind: RoundKind::Open,
                    at: 8,
                }],
                vec![Event::PhaseChanged {
                    round: 1,
                    phase: Phase::Closed,
                }],
            ] {
                lines.push(serde_json::to_string(&batch).unwrap());
            }
            lines.push(closed);
            std::fs::write(
                ran_out.join(discussion::store::EVENTS_FILE),
                lines.join("\n") + "\n",
            )
            .unwrap();
        }
        // Ended before its limit (no round ran).
        let early = stored(&root, "disc-5", true);
        let session = discussion::store::load(&ran_out).unwrap();
        assert!(session.extendable().is_ok());
        let (starts_tx, starts) = mpsc::channel();
        let (seen_tx, seen) = mpsc::channel();
        let channel = start_with(
            Some(root.clone()),
            || {},
            fake_runner_seeing(session, None, starts_tx, seen_tx),
        );
        let _ = channel.history.recv_timeout(Duration::from_secs(1));
        let _ = snapshot_within(&channel, Duration::from_secs(1));

        channel.send(Command::Open { dir: early.clone() });
        let _ = snapshot_within(&channel, Duration::from_secs(1));
        channel.send(Command::Extend { by: 2 });
        let notices = notices_within(&channel, Duration::from_millis(300));
        assert!(
            notices
                .iter()
                .any(|n| n.contains("ラウンドを追加できません")),
            "{notices:?}"
        );
        assert!(starts.try_recv().is_err(), "nothing started");

        channel.send(Command::NewDiscussion);
        let _ = channel.history.recv_timeout(Duration::from_secs(1));
        let _ = snapshot_within(&channel, Duration::from_secs(1));
        channel.send(Command::Open {
            dir: ran_out.clone(),
        });
        let _ = snapshot_within(&channel, Duration::from_secs(1));
        channel.send(Command::Extend { by: 2 });
        // One start that extends before any pane: no separate action.
        assert_eq!(
            starts.recv_timeout(Duration::from_secs(1)).unwrap(),
            runner::Start::Extend {
                dir: ran_out,
                by: 2
            }
        );
        // Running now: later commands go to the runner.
        channel.send(Command::SetHold { hold: true });
        assert_eq!(
            seen.recv_timeout(Duration::from_secs(1)).unwrap(),
            Action::SetHold { hold: true }
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_resume_of_a_discussion_running_elsewhere_or_ended_starts_nothing() {
        let root = temp_root("elsewhere");
        let going = stored(&root, "disc-6", false);
        let ended = stored(&root, "disc-7", true);
        let (starts_tx, starts) = mpsc::channel();
        let session = discussion::store::load(&going).unwrap();
        let channel = start_with(
            Some(root.clone()),
            || {},
            fake_runner(session, None, starts_tx),
        );
        let _ = channel.history.recv_timeout(Duration::from_secs(1));
        let _ = snapshot_within(&channel, Duration::from_secs(1));

        // Another window's runner holds it.
        let held = discussion::store::Store::open(&going).unwrap();
        channel.send(Command::Resume { dir: going.clone() });
        // A held log is told apart from a starting child's hold only after
        // `LOCK_GRACE`.
        let notices = notices_within(
            &channel,
            discussion::store::LOCK_GRACE + Duration::from_secs(1),
        );
        assert!(
            notices.iter().any(|n| n.contains("別のウィンドウで実行中")),
            "{notices:?}"
        );
        assert!(starts.try_recv().is_err());
        drop(held);

        // Ended since the list was read: shown, not resumed.
        channel.send(Command::Resume { dir: ended.clone() });
        let shown = snapshot_within(&channel, Duration::from_secs(1)).unwrap();
        assert_eq!(shown.link, Link::Stored(ended));
        assert!(shown.session.unwrap().closed);
        assert!(starts.try_recv().is_err());
        let notices = notices_within(&channel, Duration::from_millis(200));
        assert!(
            notices.iter().any(|n| n.contains("終了しています")),
            "{notices:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
