//! The link between the UI and the running discussion: the UI reads
//! [`Snapshot`]s and sends [`Command`]s over std mpsc, as repo-browser does.
//!
//! [`start`] bridges them to the session runner (`crate::session::runner`):
//! a [`Command::Start`] spawns the runner, later commands become its
//! actions, and its updates become snapshots and notices.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use crate::session::runner::{self, Action, SessionHandle, Update};

use discussion::model::types::{
    AgentChoice, AgentKind, DebateId, IssueId, PartId, RoundNo, Session,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Live,
    Down(String),
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
        workdir: Option<std::path::PathBuf>,
        /// After this many rounds the discussion ends; `None` for no limit.
        max_rounds: Option<RoundNo>,
        moderator: AgentChoice,
        panel: Vec<AgentChoice>,
    },
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
}

pub struct Channel {
    pub snapshots: Receiver<Snapshot>,
    /// One-line messages for the user (a command that failed, and why).
    pub notices: Receiver<String>,
    commands: Sender<Command>,
}

impl Channel {
    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }
}

/// The live channel. It shows the start view (no session) until the user
/// starts a discussion; then the runner opens the agents' panes in Noa.
pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Channel {
    let (snapshot_tx, snapshots) = mpsc::channel();
    let (notice_tx, notices) = mpsc::channel();
    let (commands, command_rx) = mpsc::channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
    let _ = snapshot_tx.send(Snapshot {
        link: Link::Connecting,
        session: None,
    });
    wake();
    std::thread::Builder::new()
        .name("discussion-commands".into())
        .spawn(move || run_commands(command_rx, snapshot_tx, notice_tx, wake))
        .expect("spawn discussion-commands thread");
    Channel {
        snapshots,
        notices,
        commands,
    }
}

/// Turns the UI's commands into runner actions, starting the runner on the
/// first [`Command::Start`]. Returns when the UI drops its channel, after
/// telling the runner to stop.
fn run_commands(
    commands: Receiver<Command>,
    snapshots: Sender<Snapshot>,
    notices: Sender<String>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    // The running session's actions, and whether its runner has ended: a
    // session that failed to open or stopped leaves the user free to start
    // another.
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
                    topic,
                    workdir,
                    max_rounds,
                    moderator,
                    panel,
                },
                None,
            ) => {
                let Some(root) = discussion::store::default_root() else {
                    let _ = notices
                        .send("議論の保存先（Noa のデータディレクトリ）が見つかりません".into());
                    wake();
                    continue;
                };
                let started = runner::spawn_session(
                    runner::Start::New {
                        topic,
                        workdir,
                        max_rounds,
                        moderator,
                        panel,
                        root,
                    },
                    || {},
                );
                let ended = Arc::new(AtomicBool::new(false));
                handle = Some((started.actions(), ended.clone()));
                let (snapshots, notices, wake) = (snapshots.clone(), notices.clone(), wake.clone());
                std::thread::Builder::new()
                    .name("discussion-updates".into())
                    .spawn(move || forward_updates(started, ended, snapshots, notices, wake))
                    .expect("spawn discussion-updates thread");
                continue;
            }
            (Command::Start { .. }, Some(_)) => {
                let _ = notices.send("議論はすでに始まっています".into());
                wake();
                continue;
            }
            (_, None) => {
                let _ = notices.send("議論が始まっていません".into());
                wake();
                continue;
            }
            (Command::Intervene { to, body }, Some(_)) => Action::Intervene { to, body },
            (Command::StartDebate { issue, pro, con }, Some(_)) => Action::StartDebate {
                issue,
                pro: Some(pro),
                con: Some(con),
            },
            (Command::ApproveDebate { id, pro, con }, Some(_)) => Action::ApproveDebate {
                id,
                pro: Some(pro),
                con: Some(con),
            },
            (Command::DeclineDebate { id }, Some(_)) => Action::DeclineDebate { id },
            (Command::Retry, Some(_)) => Action::Retry,
            (Command::Close, Some(_)) => Action::Close,
            (Command::Implement { agent }, Some(_)) => Action::Implement { agent },
        };
        if let Some((actions, _)) = &handle {
            let _ = actions.send(action);
        }
    }
    // The UI is gone: end the runner (the updates thread holds its handle).
    if let Some((actions, _)) = handle {
        let _ = actions.send(Action::Stop);
    }
}

/// Turns runner updates into snapshots (the latest session with the latest
/// link) and notices, until the runner ends or the UI is gone.
fn forward_updates(
    handle: SessionHandle,
    ended: Arc<AtomicBool>,
    snapshots: Sender<Snapshot>,
    notices: Sender<String>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let mut link = Link::Connecting;
    let mut session = None;
    for update in &handle.updates {
        let sent = match update {
            Update::Session(next) => {
                session = Some(next);
                snapshots
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
                snapshots
                    .send(Snapshot {
                        link: link.clone(),
                        session: session.clone(),
                    })
                    .is_ok()
            }
            Update::Notice(text) => notices.send(text).is_ok(),
            Update::Ended(Ok(())) => break,
            Update::Ended(Err(why)) => {
                // Before the notice: a start the user tries after seeing it
                // must not meet the ended session.
                ended.store(true, Ordering::Release);
                let _ = notices.send(format!("議論を続けられません: {why}"));
                let _ = snapshots.send(Snapshot {
                    link: Link::Down(why),
                    session,
                });
                wake();
                break;
            }
        };
        if !sent {
            break;
        }
        wake();
    }
    ended.store(true, Ordering::Release);
}

#[cfg(test)]
pub(crate) struct TestFeed {
    pub snapshots: Sender<Snapshot>,
    pub notices: Sender<String>,
    pub commands: Receiver<Command>,
}

#[cfg(test)]
pub(crate) fn test_channel() -> (Channel, TestFeed) {
    let (snapshot_tx, snapshots) = mpsc::channel();
    let (notice_tx, notices) = mpsc::channel();
    let (commands, command_rx) = mpsc::channel();
    (
        Channel {
            snapshots,
            notices,
            commands,
        },
        TestFeed {
            snapshots: snapshot_tx,
            notices: notice_tx,
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
}
