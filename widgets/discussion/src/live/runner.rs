//! Runs one discussion against a live Noa, on a thread of its own.
//!
//! The runner owns the [`Engine`] and the [`Store`]. It opens a Noa tab for
//! the discussion, starts the moderator and every panelist there as
//! interactive agents (`claude`, `codex`, `agy`) in splits, carries out the
//! engine's effects (briefs pasted and submitted, reply files watched), and
//! feeds back what it sees: deliveries, agent state reports, reply files,
//! the user's actions and the time. A lost connection is retried with
//! backoff; the discussion keeps its state meanwhile.
//!
//! The UI starts it with [`spawn_session`] and gets a [`SessionHandle`]:
//! [`Update`]s come in on `handle.updates` (call the `wake` it was given
//! for each), and [`Action`]s go out with `handle.send`. Dropping the
//! handle, or sending [`Action::Stop`], ends the thread; the agents' panes
//! stay open.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use discussion::engine::{AgentState, Config, Effect, Engine, EngineError, Input, Setup};
use discussion::model::export;
use discussion::model::types::{
    AgentChoice, AgentKind, AgentSetup, DebateId, IssueId, Millis, PartId, Phase, RoundNo, Session,
    SessionId,
};
use discussion::store::{self, Store};

use super::ipc::{Changes, Connect, IpcConnect, Noa, NoaError, PaneSpec, PaneView, Placement};

/// How long one step waits for Noa's state changes (or sleeps while the
/// link is down): the runner's reaction time.
const POLL: Duration = Duration::from_millis(200);
/// How often the engine gets `Tick` for its timeouts.
const TICK_EVERY: Millis = 1_000;
/// Waits between connection attempts: 0.5 s doubling up to 8 s.
const BACKOFF_MIN: Millis = 500;
const BACKOFF_MAX: Millis = 8_000;
/// Pause between a pasted brief and the Enter that submits it, so a TUI
/// that debounces paste sees two events (`noa +ctl send`'s `SUBMIT_DELAY`).
const SUBMIT_DELAY: Duration = Duration::from_millis(150);
/// A fresh pane is ready for its first brief once its agent has reported
/// that it waits for input (`input` without `blocked`, or `finished`), or
/// its screen shows an input line and has stayed the same this long
/// (`noa +ctl spawn --task`'s rule).
const QUIET_FOR: Millis = 1_500;
/// A brief whose pane is not ready this long after it was queued fails,
/// unless its agent is held on a prompt (`permission`, `error` or
/// `blocked`: a trust prompt, a sign-in), which is the user's to answer:
/// that brief waits, and the user is told once.
const READY_LIMIT: Millis = 30_000;
/// What Noa reports for Claude Code's folder trust prompt (`detail`, with
/// `blocked: permission`), and the choice that trusts the folder. The
/// runner answers this prompt itself only for agents in the discussions
/// directory it made for them. A working directory the user named is a
/// repository with its own settings and hooks, so its trust prompt is left
/// to the user.
const CLAUDE_TRUST_DETAIL: &str = "trust this folder";
const CLAUDE_TRUST_YES: &str = "Yes, I trust this folder";
/// Keys typed into one trust prompt before it is left to the user.
const TRUST_KEYS_MAX: u8 = 4;
/// How long a typed key may take to show on the trust prompt before the
/// prompt is left to the user.
const TRUST_KEY_SHOWS_IN: Millis = 2_000;
/// A reply file is taken in once its text has stayed the same this long, so
/// a file still being written is not read half-done.
const REPLY_STABLE_FOR: Millis = 600;
/// Panes started for one brief before its send counts as failed: an agent
/// that exits at once would otherwise be respawned forever.
const MAX_SPAWNS: u8 = 2;

/// What starts a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// A new discussion, stored under `root` (usually
    /// [`store::default_root`]).
    New {
        topic: String,
        /// The directory (or repository) the agents run in; `None` runs
        /// them in the discussions directory.
        workdir: Option<PathBuf>,
        /// After this many rounds the discussion ends; `None` for no limit.
        max_rounds: Option<RoundNo>,
        moderator: AgentChoice,
        panel: Vec<AgentChoice>,
        root: PathBuf,
    },
    /// The discussion stored at `dir`, picked up where it stopped. One
    /// that has ended is refused: nothing of it would run.
    Resume { dir: PathBuf },
    /// The discussion stored at `dir`, given `by` more rounds before any
    /// pane is started (it may have ended by running all its rounds).
    Extend { dir: PathBuf, by: RoundNo },
}

/// What the user does. Times are stamped by the runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Speak into the meeting; `to` empty means everyone.
    Intervene {
        to: Vec<PartId>,
        body: String,
    },
    /// Approve the proposed debate, optionally with other sides.
    ApproveDebate {
        id: DebateId,
        pro: Option<Vec<PartId>>,
        con: Option<Vec<PartId>>,
    },
    DeclineDebate {
        id: DebateId,
    },
    /// Start a debate on an issue (⚔); sides default to the latest stances.
    StartDebate {
        issue: IssueId,
        pro: Option<Vec<PartId>>,
        con: Option<Vec<PartId>>,
    },
    /// Try the current round's failed briefs again.
    Retry,
    /// End the discussion: nothing more is sent and no round starts. The
    /// agents' panes stay open.
    Close,
    /// Start an agent of this kind in a new tab, in the working directory,
    /// on the discussion's result (written to a task file there).
    Implement {
        agent: AgentKind,
    },
    /// Bring a participant's pane to the front in Noa.
    Focus {
        part: PartId,
    },
    /// Hold the discussion after each round, or stop holding it.
    SetHold {
        hold: bool,
    },
    /// Start the next round of a held discussion.
    Continue,
    /// Give the discussion this many more rounds (an ended one that ran all
    /// its rounds goes on).
    Extend {
        by: RoundNo,
    },
    /// Close every participant's pane in Noa. Refused until the discussion
    /// has ended, so no brief is cut off and no pane is started again.
    ClosePanes,
    /// End the runner. The agents' panes stay open.
    Stop,
}

/// The state of the connection to Noa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Live,
    /// Why it is down; the runner keeps retrying.
    Down(String),
}

/// What the runner tells the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// The session after a change (sent first once it is open).
    Session(Arc<Session>),
    Link(Link),
    /// Something to show briefly: a refused action, a pane that closed.
    Notice(String),
    /// The runner stopped: `Ok` after [`Action::Stop`], `Err` with why it
    /// could not go on (the session could not be opened or written).
    Ended(Result<(), String>),
}

/// The UI's end of a running session.
pub struct SessionHandle {
    pub updates: Receiver<Update>,
    actions: Sender<Action>,
}

impl SessionHandle {
    /// A handle over channels the caller drives: a stand-in runner.
    #[cfg(test)]
    pub(crate) fn new(updates: Receiver<Update>, actions: Sender<Action>) -> Self {
        SessionHandle { updates, actions }
    }

    /// Another sender of actions, for a thread other than the one reading
    /// [`Self::updates`]. The runner ends once the handle and every such
    /// sender are dropped.
    pub fn actions(&self) -> Sender<Action> {
        self.actions.clone()
    }
}

/// Runs `start` against the running Noa on a new thread. `wake` is called
/// after every update sent.
pub fn spawn_session(start: Start, wake: impl Fn() + Send + 'static) -> SessionHandle {
    let (update_tx, updates) = mpsc::channel();
    let (actions, action_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("discussion-live".into())
        .spawn(move || {
            run_session(
                start,
                IpcConnect,
                SystemClock,
                Config::default(),
                action_rx,
                update_tx,
                wake,
            )
        })
        .expect("spawn discussion-live thread");
    SessionHandle { updates, actions }
}

/// The runner's loop, with Noa, the clock and the engine's config given:
/// [`spawn_session`] runs it with the real ones.
pub fn run_session<C: Connect, K: Clock>(
    start: Start,
    connector: C,
    clock: K,
    config: Config,
    actions: Receiver<Action>,
    updates: Sender<Update>,
    wake: impl Fn() + 'static,
) {
    let wake: Rc<dyn Fn()> = Rc::new(wake);
    let opened = Runner::open(
        start,
        connector,
        clock,
        config,
        actions,
        updates.clone(),
        wake.clone(),
    );
    let result = opened.and_then(|mut runner| runner.run());
    let _ = updates.send(Update::Ended(result));
    wake();
}

/// Wall-clock time in ms, and sleeping: injected so the runner is tested
/// without waiting.
pub trait Clock {
    fn now(&self) -> Millis;
    fn sleep(&self, duration: Duration);
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Millis {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as Millis)
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// How an agent is started in its pane: through the user's login shell, so
/// it finds `claude`/`codex`/`agy` on the PATH the user's shell sets up
/// (Noa started from Finder has a bare one), then `exec`ed so the agent is
/// the pane's foreground program.
fn agent_argv(kind: AgentKind, setup: &AgentSetup) -> Vec<String> {
    // Plain words only (`AgentSetup::check`, enforced when the session is
    // created), written into the script itself: `"$@"` is not valid in
    // every login shell.
    let script = std::iter::once(format!("exec {}", kind.command()))
        .chain(kind.flags(setup))
        .collect::<Vec<_>>()
        .join(" ");
    vec![login_shell(), "-l".into(), "-i".into(), "-c".into(), script]
}

pub(crate) fn login_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|shell| shell.starts_with('/'))
        .unwrap_or_else(|| "/bin/zsh".to_owned())
}

/// `exec KIND "$@"` through the login shell, with `args` as the script's
/// positional parameters (`$0` is `name`), so a prompt is never spliced
/// into the script. Takes a POSIX login shell, as the first prompt always has.
fn launch_argv(kind: AgentKind, name: &str, args: Vec<String>) -> Vec<String> {
    let mut argv = vec![
        login_shell(),
        "-l".into(),
        "-i".into(),
        "-c".into(),
        format!("exec {} \"$@\"", kind.command()),
        name.to_owned(),
    ];
    argv.extend(args);
    argv
}

fn implement_argv(kind: AgentKind, prompt: &str) -> Vec<String> {
    let mut args = Vec::new();
    if kind == AgentKind::Agy {
        args.push("-i".to_owned());
    }
    args.push(prompt.to_owned());
    launch_argv(kind, "noa-implement", args)
}

fn agent_state(state: Option<&str>) -> Option<AgentState> {
    Some(match state? {
        "running" => AgentState::Running,
        "permission" => AgentState::Permission,
        "input" => AgentState::Input,
        "finished" => AgentState::Finished,
        "error" => AgentState::Error,
        _ => return None,
    })
}

/// Whether the engine still waits for `part`'s brief of `round` to be
/// delivered or answered (the engine's own rule for a brief in play).
fn in_play(session: &Session, round: RoundNo, part: &PartId) -> bool {
    // An ended discussion sends nothing, whatever its round's phase says.
    if session.closed {
        return false;
    }
    let Some(current) = session.current_round() else {
        return false;
    };
    if current.no != round {
        return false;
    }
    let phase_ok = if *part == session.moderator.id {
        current.phase == Phase::Summarizing
    } else {
        matches!(current.phase, Phase::Briefing | Phase::Awaiting)
    };
    phase_ok
        && current
            .brief(part)
            .is_some_and(|b| b.receipt.replied.is_none() && b.receipt.failed.is_none())
}

enum Fail {
    /// The connection is gone: reconnect, keep everything else.
    Link(String),
    /// The session cannot go on.
    Fatal(String),
}

impl From<String> for Fail {
    fn from(why: String) -> Self {
        Fail::Fatal(why)
    }
}

fn link(err: NoaError) -> Fail {
    match err {
        NoaError::Link(why) => Fail::Link(why),
        other => Fail::Link(other.to_string()),
    }
}

/// An agent's pane.
#[derive(Debug, Clone)]
struct Pane {
    id: u64,
    /// Ready for a brief (see [`QUIET_FOR`]).
    ready: bool,
    /// The agent's last report count, state, `blocked` and detail.
    seq: u64,
    state: Option<String>,
    blocked: Option<String>,
    detail: Option<String>,
    /// The screen text last seen and since when, while waiting for ready.
    quiet: Option<(String, Millis)>,
    /// The user was told this pane waits on a prompt (once per hold).
    told_held: bool,
    /// A brief has been typed into this pane. Before that, `error` is
    /// Antigravity signing in; after it, a failed turn.
    briefed: bool,
    /// Keys typed to answer Claude Code's trust prompt on this pane, and
    /// the screen the last one was typed on: the next key waits for the
    /// screen to change, so a slow redraw does not get a key twice.
    trust_keys: u8,
    trust_screen: Option<(String, Millis)>,
}

impl Pane {
    fn new(id: u64) -> Self {
        Pane {
            id,
            ready: false,
            seq: 0,
            state: None,
            blocked: None,
            detail: None,
            quiet: None,
            told_held: false,
            briefed: false,
            trust_keys: 0,
            trust_screen: None,
        }
    }

    /// Whether the agent's last report holds it on a prompt the user must
    /// answer before it can take a brief: Claude Code's folder trust prompt
    /// reports `permission` + `blocked`, Antigravity reports `error` while it
    /// signs in.
    fn held(&self) -> bool {
        self.seq > 0
            && (self.blocked.is_some()
                || self.state.as_deref() == Some("permission")
                // Antigravity's sign-in. Once the pane has taken a brief,
                // `error` is a failed turn: the engine resends, and the agent
                // is back at its input box.
                || (!self.briefed && self.state.as_deref() == Some("error")))
    }

    /// Whether the agent's last report says it is working on a turn.
    fn busy(&self) -> bool {
        self.seq > 0 && self.state.as_deref() == Some("running")
    }

    /// Whether the agent's last report says it waits for input.
    fn idle(&self) -> bool {
        self.seq > 0
            && self.blocked.is_none()
            && matches!(self.state.as_deref(), Some("input" | "finished"))
    }
}

/// The key that moves Claude Code's trust prompt toward trusting the
/// folder: Enter when "Yes, I trust this folder" is selected (`❯`), Down
/// when another choice is; `None` when the screen shows no such prompt.
fn trust_key(screen: &str) -> Option<&'static str> {
    if !screen.contains(CLAUDE_TRUST_YES) {
        return None;
    }
    let selected = screen
        .lines()
        .find(|line| line.trim_start().starts_with('❯'))?;
    Some(if selected.contains(CLAUDE_TRUST_YES) {
        "\r"
    } else {
        "\x1b[B"
    })
}

/// Whether the bottom of `screen` shows an agent's input line: a rule whose
/// next non-blank line starts with `>` or `❯` (the top of Claude Code's and
/// Antigravity's input box; in a short pane Claude Code draws no rule
/// under it), or Codex's `›` input line. A rule alone (a banner) is not an
/// input box. Neither is a dialog under a rule, whose first line is its
/// title (`Accessing workspace:`), nor a numbered choice (`❯ 1. Yes`).
fn has_input(screen: &str) -> bool {
    fn is_rule(line: &str) -> bool {
        let line = line.trim();
        line.chars().count() >= 20 && line.chars().all(|ch| matches!(ch, '─' | '━' | '═'))
    }
    const BOX_REACH: usize = 12;
    const CODEX_REACH: usize = 4;
    let mut lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    fn numbered(rest: &str) -> bool {
        let rest = rest.trim_start();
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && rest[digits..].starts_with('.')
    }
    let tail = &lines[lines.len().saturating_sub(BOX_REACH)..];
    let boxed = (0..tail.len()).filter(|&i| is_rule(tail[i])).any(|i| {
        tail[i + 1..]
            .iter()
            .map(|line| line.trim_start())
            .find(|line| !line.is_empty())
            .and_then(|line| line.strip_prefix('>').or_else(|| line.strip_prefix('❯')))
            .is_some_and(|rest| !numbered(rest))
    });
    let codex = lines
        .iter()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(CODEX_REACH)
        .filter_map(|line| line.trim_start().strip_prefix('›'))
        .any(|rest| !numbered(rest));
    boxed || codex
}

/// A brief waiting to be typed into its agent's pane.
#[derive(Debug, Clone)]
struct Delivery {
    round: RoundNo,
    part: PartId,
    body: String,
    queued: Millis,
    /// The text is in; only the Enter is left (the link went down between).
    pasted: bool,
    spawns: u8,
}

/// A reply file being watched.
#[derive(Debug, Clone)]
struct Watch {
    round: RoundNo,
    part: PartId,
    /// The text last read and since when it has been the same.
    seen: Option<(String, Millis)>,
    /// The text last given to the engine.
    reported: Option<String>,
}

pub(crate) struct Runner<C: Connect, K: Clock> {
    engine: Engine,
    store: Store,
    connector: C,
    clock: K,
    actions: Receiver<Action>,
    updates: Sender<Update>,
    wake: Rc<dyn Fn()>,
    conn: Option<C::Conn>,
    next_connect: Millis,
    backoff: Millis,
    /// `Start` has gone to the engine (after the first connection).
    started: bool,
    /// Every participant's pane was started once (on the first connection).
    spawned_all: bool,
    panes: BTreeMap<PartId, Pane>,
    /// Panes asked to close whose outcome Noa could not tell (a timeout,
    /// a lost link): when one closes after all, it goes without a notice.
    closing: BTreeSet<u64>,
    deliveries: Vec<Delivery>,
    watches: BTreeMap<PathBuf, Watch>,
    next_tick: Millis,
    ui_gone: bool,
}

impl<C: Connect, K: Clock> Runner<C, K> {
    fn open(
        start: Start,
        connector: C,
        clock: K,
        config: Config,
        actions: Receiver<Action>,
        updates: Sender<Update>,
        wake: Rc<dyn Fn()>,
    ) -> Result<Self, String> {
        let now = clock.now();
        let (engine, store) = match start {
            Start::New {
                topic,
                workdir,
                max_rounds,
                moderator,
                panel,
                root,
            } => {
                if let Some(workdir) = &workdir {
                    if !workdir.is_dir() {
                        return Err(format!(
                            "{} はディレクトリではありません",
                            workdir.display()
                        ));
                    }
                    store::prepare_workdir(workdir)
                        .map_err(|e| format!("{} に .noa を作れません: {e}", workdir.display()))?;
                }
                let id = SessionId::at(now);
                let dir = store::session_dir(&root, &id);
                let setup = Setup {
                    topic: &topic,
                    workdir,
                    max_rounds,
                    moderator,
                    panel: &panel,
                };
                let (engine, out) = Engine::create_in(id, setup, &dir, config, now)
                    .map_err(|e| format!("討論を始められません: {e}"))?;
                let first = out.batches.first().map(Vec::as_slice).unwrap_or_default();
                let store = Store::create(&dir, first)
                    .map_err(|e| format!("{} を作れません: {e}", dir.display()))?;
                (engine, store)
            }
            Start::Resume { dir } => {
                let (engine, store) = resume(&dir, config)?;
                if engine.session().closed {
                    return Err(format!("{} は終了しています", dir.display()));
                }
                (engine, store)
            }
            Start::Extend { dir, by } => {
                let (mut engine, mut store) = resume(&dir, config)?;
                // Extended and stored before anything starts: an extension
                // that is refused leaves no pane behind.
                match engine.handle(Input::UserExtend { by, at: now }) {
                    Ok(out) => {
                        store
                            .append_all(&out.batches)
                            .map_err(|e| format!("討論の記録を書けません: {e}"))?;
                    }
                    Err(EngineError::Rejected(why)) => {
                        return Err(format!("ラウンドを追加できません: {why}"));
                    }
                    Err(err @ EngineError::Reduce(_)) => return Err(err.to_string()),
                }
                (engine, store)
            }
        };
        let mut runner = Runner {
            engine,
            store,
            connector,
            clock,
            actions,
            updates,
            wake,
            conn: None,
            next_connect: now,
            backoff: BACKOFF_MIN,
            started: false,
            spawned_all: false,
            panes: BTreeMap::new(),
            closing: BTreeSet::new(),
            deliveries: Vec::new(),
            watches: BTreeMap::new(),
            next_tick: now + TICK_EVERY,
            ui_gone: false,
        };
        runner.publish();
        runner.update(Update::Link(Link::Connecting));
        Ok(runner)
    }

    fn run(&mut self) -> Result<(), String> {
        while self.step()? {}
        Ok(())
    }

    /// One turn of the loop; `false` once the runner should stop.
    fn step(&mut self) -> Result<bool, String> {
        loop {
            match self.actions.try_recv() {
                Ok(Action::Stop) | Err(TryRecvError::Disconnected) => return Ok(false),
                Ok(action) => self.user(action)?,
                Err(TryRecvError::Empty) => break,
            }
        }
        if self.conn.is_none() && self.clock.now() >= self.next_connect {
            self.connect()?;
        }
        match self.conn.take() {
            Some(mut conn) => match self.connected(&mut conn) {
                Ok(()) => self.conn = Some(conn),
                Err(Fail::Link(why)) => self.lost(why),
                Err(Fail::Fatal(why)) => return Err(why),
            },
            None => self.clock.sleep(POLL),
        }
        self.poll_replies()?;
        let now = self.clock.now();
        if now >= self.next_tick {
            self.next_tick = now + TICK_EVERY;
            self.feed(Input::Tick { now })?;
        }
        Ok(!self.ui_gone)
    }

    // ---- talking to the UI ----

    fn update(&mut self, update: Update) {
        if self.updates.send(update).is_err() {
            self.ui_gone = true;
        }
        (self.wake)();
    }

    fn publish(&mut self) {
        let session = Arc::new(self.engine.session().clone());
        self.update(Update::Session(session));
    }

    fn notice(&mut self, text: String) {
        self.update(Update::Notice(text));
    }

    fn user(&mut self, action: Action) -> Result<(), String> {
        let at = self.clock.now();
        let input = match action {
            Action::Intervene { to, body } => Input::UserIntervention { to, body, at },
            Action::ApproveDebate { id, pro, con } => Input::UserApproveDebate { id, pro, con, at },
            Action::DeclineDebate { id } => Input::UserDeclineDebate { id, at },
            Action::StartDebate { issue, pro, con } => Input::UserStartDebate {
                issue,
                pro,
                con,
                at,
            },
            Action::Retry => Input::UserRetry,
            Action::Close => Input::UserClose,
            Action::SetHold { hold } => Input::UserSetHold { hold, at },
            Action::Continue => Input::UserContinue { at },
            Action::Extend { by } => Input::UserExtend { by, at },
            Action::Implement { agent } => {
                self.implement(agent);
                return Ok(());
            }
            Action::Focus { part } => {
                self.focus(&part);
                return Ok(());
            }
            Action::ClosePanes => {
                self.close_panes();
                return Ok(());
            }
            Action::Stop => return Ok(()),
        };
        if let Some(why) = self.feed(input)? {
            self.notice(why);
        }
        Ok(())
    }

    /// Brings `part`'s pane to the front. Refused, with a notice, for a
    /// participant without a pane yet (nothing is asked of Noa then) or
    /// without a link to Noa.
    fn focus(&mut self, part: &PartId) {
        let name = if *part == self.engine.session().moderator.id {
            "司会 (Noa)".to_owned()
        } else {
            part.to_string()
        };
        if self.engine.session().participant(part).is_none() {
            self.notice(format!("{part} はこの議論の参加者ではありません"));
            return;
        }
        let Some(pane) = self.panes.get(part).map(|p| p.id) else {
            self.notice(format!("{name} のペインはまだありません"));
            return;
        };
        let Some(conn) = self.conn.as_mut() else {
            self.notice(
                "Noa に接続していません。接続が戻ってから、もう一度押してください".to_owned(),
            );
            return;
        };
        if let Err(err) = conn.focus(pane) {
            self.notice(format!("{name} のペインを表示できません: {err}"));
        }
    }

    /// Closes the participants' panes of an ended discussion. A pane is
    /// forgotten before Noa reports it gone, so its closing is no agent
    /// error. The implementation tab is the user's and stays open.
    fn close_panes(&mut self) {
        if !self.engine.session().closed {
            self.notice("ペインは議論が終わってから閉じられます".to_owned());
            return;
        }
        if self.panes.is_empty() {
            self.notice("閉じるペインはありません".to_owned());
            return;
        }
        let Some(conn) = self.conn.as_mut() else {
            self.notice(
                "Noa に接続していません。接続が戻ってから、もう一度押してください".to_owned(),
            );
            return;
        };
        let mut failed = Vec::new();
        let mut closed = 0;
        for (part, pane) in std::mem::take(&mut self.panes) {
            match conn.close_pane(pane.id) {
                Ok(()) | Err(NoaError::PaneGone) => closed += 1,
                // Noa may still close it (a timeout says nothing of the
                // outcome): kept until it is reported gone.
                Err(err) => {
                    failed.push(format!("{part}: {err}"));
                    self.closing.insert(pane.id);
                    self.panes.insert(part, pane);
                }
            }
        }
        let notice = if failed.is_empty() {
            format!("{closed} 個のペインを閉じました")
        } else {
            format!(
                "{closed} 個のペインを閉じました。閉じたか確かめられなかったもの: {}",
                failed.join(", ")
            )
        };
        self.notice(notice);
    }

    /// Writes the result as a task file under the working directory and
    /// starts `agent` on it in a new tab there. Refused, with a notice,
    /// without a working directory or a link to Noa.
    fn implement(&mut self, agent: AgentKind) {
        let session = self.engine.session();
        let Some(workdir) = session.workdir.clone() else {
            self.notice("実装の依頼には、作業ディレクトリを指定した議論が必要です".to_owned());
            return;
        };
        let task = self.engine.reply_dir().join("implementation.md");
        let text = export::implementation_task(session, &task);
        let label = format!("implement · {}", session.id);
        let written = task
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(&task, text));
        if let Err(err) = written {
            self.notice(format!("{} を書けません: {err}", task.display()));
            return;
        }
        let Some(conn) = self.conn.as_mut() else {
            self.notice(
                "Noa に接続していません。接続が戻ってから、もう一度押してください".to_owned(),
            );
            return;
        };
        let prompt = format!(
            "Read {} and implement what it asks. It is the outcome of a moderated \
             discussion between coding agents.",
            task.display()
        );
        let spec = PaneSpec {
            argv: implement_argv(agent, &prompt),
            cwd: workdir,
            label,
        };
        let notice = match conn.spawn(Placement::NewTab, &spec) {
            Ok(_) => format!("{} に実装を依頼しました（新しいタブ）", agent.command()),
            Err(err) => format!("実装用のペインを開けません: {err}"),
        };
        self.notice(notice);
    }

    // ---- the engine ----

    /// Gives `input` to the engine, stores what it committed and carries
    /// out its effects. `Ok(Some(why))`: the engine refused the input.
    fn feed(&mut self, input: Input) -> Result<Option<String>, String> {
        match self.engine.handle(input) {
            Ok(out) => {
                self.store
                    .append_all(&out.batches)
                    .map_err(|e| format!("討論の記録を書けません: {e}"))?;
                for effect in out.effects {
                    self.effect(effect);
                }
                if !out.batches.is_empty() {
                    self.publish();
                }
                Ok(None)
            }
            Err(EngineError::Rejected(why)) => Ok(Some(why)),
            Err(err @ EngineError::Reduce(_)) => Err(err.to_string()),
        }
    }

    fn effect(&mut self, effect: Effect) {
        match effect {
            Effect::SendBrief { round, part, body } => {
                let queued = self.clock.now();
                self.queue(Delivery {
                    round,
                    part,
                    body,
                    queued,
                    pasted: false,
                    spawns: 0,
                });
            }
            Effect::WatchReply { round, part, path } => {
                if let Err(err) = store::prepare_reply(&path) {
                    self.notice(format!("{} を用意できません: {err}", path.display()));
                }
                let watch = Watch {
                    round,
                    part,
                    seen: None,
                    reported: None,
                };
                self.watches.insert(path, watch);
            }
            Effect::ClearReply { path } => {
                if let Err(err) = store::clear_reply(&path) {
                    self.notice(format!("{} を消せません: {err}", path.display()));
                }
                if let Some(watch) = self.watches.get_mut(&path) {
                    watch.seen = None;
                    watch.reported = None;
                }
            }
        }
    }

    /// Queues a delivery; a newer one to the same agent replaces the older.
    fn queue(&mut self, delivery: Delivery) {
        self.deliveries.retain(|d| d.part != delivery.part);
        self.deliveries.push(delivery);
    }

    // ---- the connection ----

    fn connect(&mut self) -> Result<(), String> {
        match self.connector.connect() {
            Ok(mut conn) => match self.sync(&mut conn) {
                Ok(()) => {
                    self.conn = Some(conn);
                    self.backoff = BACKOFF_MIN;
                    self.update(Update::Link(Link::Live));
                }
                Err(Fail::Link(why)) => self.lost(why),
                Err(Fail::Fatal(why)) => return Err(why),
            },
            Err(why) => self.lost(why),
        }
        Ok(())
    }

    fn lost(&mut self, why: String) {
        self.conn = None;
        self.next_connect = self.clock.now() + self.backoff;
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
        self.update(Update::Link(Link::Down(why)));
    }

    /// After a connect: match the session's panes to Noa's, start the
    /// missing ones the first time, and start the engine.
    fn sync(&mut self, conn: &mut C::Conn) -> Result<(), Fail> {
        let views = conn.panes().map_err(link)?;
        self.relist(&views)?;
        if !self.spawned_all {
            let parts: Vec<PartId> = self.order();
            for part in parts {
                if !self.panes.contains_key(&part)
                    && let Err(why) = self.spawn(conn, &part)?
                {
                    self.notice(format!("{part} を起動できません: {why}"));
                }
            }
            self.spawned_all = true;
        }
        if !self.started {
            self.started = true;
            let now = self.clock.now();
            self.feed(Input::Start { now })?;
        }
        Ok(())
    }

    fn connected(&mut self, conn: &mut C::Conn) -> Result<(), Fail> {
        if let Some(changes) = conn.changes(POLL).map_err(link)? {
            self.apply_changes(conn, changes)?;
        }
        self.deliver(conn)
    }

    // ---- panes ----

    /// The moderator, then the panelists.
    fn order(&self) -> Vec<PartId> {
        let session = self.engine.session();
        std::iter::once(&session.moderator)
            .chain(&session.panelists)
            .map(|p| p.id.clone())
            .collect()
    }

    /// The label a participant's pane gets. It names the session, so a pane
    /// of this discussion is told from another pane that got the same id
    /// after Noa restarted.
    fn label(&self, part: &PartId) -> String {
        format!("{part} · {}", self.engine.session().id)
    }

    fn part_of(&self, pane_id: u64) -> Option<PartId> {
        self.panes
            .iter()
            .find(|(_, pane)| pane.id == pane_id)
            .map(|(part, _)| part.clone())
    }

    /// Takes a full pane list: panes no longer listed are gone, the
    /// session's own panes still listed (a resume) are taken back, and
    /// every agent's state is read.
    fn relist(&mut self, views: &[PaneView]) -> Result<(), String> {
        // A pane is still the agent's only if both its id and its label
        // match: Noa numbers panes afresh when it restarts, so an id alone
        // may now name some other pane.
        let gone: Vec<PartId> = self
            .panes
            .iter()
            .filter(|(part, pane)| {
                let label = self.label(part);
                !views
                    .iter()
                    .any(|v| v.pane_id == pane.id && v.label.as_deref() == Some(label.as_str()))
            })
            .map(|(part, _)| part.clone())
            .collect();
        for part in gone {
            self.pane_gone(&part)?;
        }
        let session = self.engine.session();
        let adopt: Vec<(PartId, u64)> = std::iter::once(&session.moderator)
            .chain(&session.panelists)
            .filter(|p| !self.panes.contains_key(&p.id))
            .filter_map(|p| {
                let id = p.pane_id?;
                let label = self.label(&p.id);
                views
                    .iter()
                    .any(|v| v.pane_id == id && v.label.as_deref() == Some(label.as_str()))
                    .then(|| (p.id.clone(), id))
            })
            .collect();
        for (part, id) in adopt {
            self.panes.insert(part, Pane::new(id));
        }
        for view in views {
            self.observe(view)?;
        }
        Ok(())
    }

    fn apply_changes(&mut self, conn: &mut C::Conn, changes: Changes) -> Result<(), Fail> {
        if changes.dropped {
            let views = conn.panes().map_err(link)?;
            return Ok(self.relist(&views)?);
        }
        for id in changes.removed {
            if let Some(part) = self.part_of(id) {
                self.pane_gone(&part)?;
            }
        }
        for view in &changes.panes {
            self.observe(view)?;
        }
        Ok(())
    }

    /// A new report from an agent goes to the engine.
    fn observe(&mut self, view: &PaneView) -> Result<(), String> {
        let Some(part) = self.part_of(view.pane_id) else {
            return Ok(());
        };
        let Some(pane) = self.panes.get_mut(&part) else {
            return Ok(());
        };
        if view.agent_seq <= pane.seq {
            return Ok(());
        }
        pane.seq = view.agent_seq;
        pane.state.clone_from(&view.agent_state);
        pane.blocked.clone_from(&view.agent_blocked);
        pane.detail.clone_from(&view.agent_detail);
        if !pane.held() {
            pane.told_held = false;
        }
        // Any dialog Noa reports as `blocked` (an approval, or a question
        // shown with `state: input`) waits on the user, like `permission`.
        let state = match view.agent_blocked {
            Some(_) => AgentState::Permission,
            None => match agent_state(view.agent_state.as_deref()) {
                Some(state) => state,
                None => return Ok(()),
            },
        };
        let at = self.clock.now();
        self.feed(Input::AgentStateChanged { part, state, at })?;
        Ok(())
    }

    /// `part`'s pane closed. A brief it was working on is sent again (to a
    /// new pane) as for an agent error.
    fn pane_gone(&mut self, part: &PartId) -> Result<(), String> {
        let Some(pane) = self.panes.remove(part) else {
            return Ok(());
        };
        if self.closing.remove(&pane.id) {
            return Ok(());
        }
        self.notice(format!("{part} のペインが閉じました"));
        let at = self.clock.now();
        self.feed(Input::AgentStateChanged {
            part: part.clone(),
            state: AgentState::Error,
            at,
        })?;
        Ok(())
    }

    /// Where `part`'s new pane goes: the discussion's own tab for the first,
    /// then the moderator on the left and the panelists stacked on the right.
    fn placement(&self, part: &PartId) -> Placement {
        let session = self.engine.session();
        let live = |id: &PartId| self.panes.get(id).map(|pane| pane.id);
        let Some(any) = self.panes.values().next().map(|pane| pane.id) else {
            return Placement::NewTab;
        };
        if session.is_panelist(part) {
            if let Some(last) = session.panelist_ids().filter_map(live).last() {
                return Placement::Below(last);
            }
            if let Some(moderator) = live(&session.moderator.id) {
                return Placement::Right(moderator);
            }
        }
        Placement::Below(any)
    }

    /// Starts `part`'s agent. `Ok(Err(why))`: Noa refused.
    fn spawn(&mut self, conn: &mut C::Conn, part: &PartId) -> Result<Result<(), String>, Fail> {
        let Some((kind, setup)) = self
            .engine
            .session()
            .participant(part)
            .map(|p| (p.kind, p.setup.clone()))
        else {
            return Ok(Err(format!("no participant {part}")));
        };
        let cwd = match &self.engine.session().workdir {
            Some(workdir) => workdir.clone(),
            None => agent_cwd(self.engine.dir()),
        };
        loop {
            let placement = self.placement(part);
            let spec = PaneSpec {
                argv: agent_argv(kind, &setup),
                cwd: cwd.clone(),
                label: self.label(part),
            };
            match conn.spawn(placement, &spec) {
                Ok(id) => {
                    self.panes.insert(part.clone(), Pane::new(id));
                    self.feed(Input::PaneSpawned {
                        part: part.clone(),
                        pane_id: id,
                    })?;
                    return Ok(Ok(()));
                }
                // The pane to split off closed: forget it, place again.
                Err(NoaError::PaneGone) => match placement {
                    Placement::Right(beside) | Placement::Below(beside) => {
                        if let Some(gone) = self.part_of(beside) {
                            self.pane_gone(&gone)?;
                        } else {
                            return Ok(Err(NoaError::PaneGone.to_string()));
                        }
                    }
                    Placement::NewTab => return Ok(Err(NoaError::PaneGone.to_string())),
                },
                Err(NoaError::Link(why)) => return Err(Fail::Link(why)),
                Err(other) => return Ok(Err(other.to_string())),
            }
        }
    }

    /// Whether `part`'s pane is ready for a brief (see [`QUIET_FOR`]). A
    /// pane held on a prompt is not, and the user is told once.
    fn ready(&mut self, conn: &mut C::Conn, part: &PartId) -> Result<bool, Fail> {
        let now = self.clock.now();
        let auto_trust = self.engine.session().workdir.is_none();
        let Some(pane) = self.panes.get_mut(part) else {
            return Ok(false);
        };
        if auto_trust
            && pane.held()
            && pane.blocked.as_deref() == Some("permission")
            && pane.detail.as_deref() == Some(CLAUDE_TRUST_DETAIL)
            && pane.trust_keys < TRUST_KEYS_MAX
        {
            let id = pane.id;
            let seen = pane.trust_screen.clone();
            let (key, unchanged) = match conn.screen(id) {
                Ok(text) if seen.as_ref().is_some_and(|(before, _)| *before == text) => {
                    (None, true)
                }
                Ok(text) => (trust_key(&text).map(|key| (key, text)), false),
                Err(NoaError::Link(why)) => return Err(Fail::Link(why)),
                Err(_) => (None, false),
            };
            let typed_at = seen.map_or(now, |(_, at)| at);
            if unchanged && now.saturating_sub(typed_at) < TRUST_KEY_SHOWS_IN {
                // The last key has not shown yet: never type it twice.
                return Ok(false);
            }
            if let Some((key, text)) = key {
                match conn.type_text(id, key, false) {
                    Ok(()) => {}
                    Err(NoaError::Link(why)) => return Err(Fail::Link(why)),
                    Err(_) => return Ok(false),
                }
                if let Some(pane) = self.panes.get_mut(part) {
                    pane.trust_keys += 1;
                    pane.trust_screen = Some((text, now));
                }
                if key == "\r" {
                    self.notice(format!(
                        "{part}: 議論フォルダの信頼確認を自動で承認しました"
                    ));
                }
                return Ok(false);
            }
        }
        let Some(pane) = self.panes.get_mut(part) else {
            return Ok(false);
        };
        // Checked before the pane counts as ready from an earlier brief: an
        // agent may sit on a prompt by a later round.
        if pane.held() {
            pane.ready = false;
            pane.quiet = None;
            if !pane.told_held {
                pane.told_held = true;
                let what = pane
                    .detail
                    .clone()
                    .or_else(|| pane.state.clone())
                    .unwrap_or_default();
                self.notice(format!(
                    "{part} のペインが確認待ちです（{what}）。ペインで答えるとブリーフを送ります"
                ));
            }
            return Ok(false);
        }
        // A brief resent while the agent still works on the last one waits
        // for the turn to end, however ready the pane was before.
        if pane.busy() {
            pane.ready = false;
            pane.quiet = None;
            return Ok(false);
        }
        if pane.ready {
            return Ok(true);
        }
        match conn.paste_ready(pane.id) {
            Ok(true) => {}
            Ok(false) | Err(NoaError::PaneGone | NoaError::NotReady | NoaError::Refused(_)) => {
                return Ok(false);
            }
            Err(NoaError::Link(why)) => return Err(Fail::Link(why)),
        }
        let (quiet, input) = match conn.screen(pane.id) {
            Ok(text) if text.trim().is_empty() => {
                pane.quiet = None;
                (false, false)
            }
            Ok(text) => {
                let input = has_input(&text);
                let quiet = match &pane.quiet {
                    Some((seen, since)) if *seen == text => now.saturating_sub(*since) >= QUIET_FOR,
                    _ => {
                        pane.quiet = Some((text, now));
                        false
                    }
                };
                (quiet, input)
            }
            Err(NoaError::Link(why)) => return Err(Fail::Link(why)),
            Err(_) => (false, false),
        };
        if pane.idle() || (input && quiet) {
            pane.ready = true;
        }
        Ok(pane.ready)
    }

    // ---- deliveries ----

    fn deliver(&mut self, conn: &mut C::Conn) -> Result<(), Fail> {
        let mut pending = std::mem::take(&mut self.deliveries).into_iter();
        let mut keep = Vec::new();
        let mut result = Ok(());
        while let Some(mut delivery) = pending.next() {
            match self.try_deliver(conn, &mut delivery) {
                Ok(true) => {}
                Ok(false) => keep.push(delivery),
                Err(fail) => {
                    keep.push(delivery);
                    keep.extend(pending.by_ref());
                    result = Err(fail);
                }
            }
        }
        // Deliveries queued meanwhile (a retry) replace the kept ones.
        let added = std::mem::replace(&mut self.deliveries, keep);
        for delivery in added {
            self.queue(delivery);
        }
        result
    }

    fn send_failed(&mut self, delivery: &Delivery, error: String) -> Result<(), String> {
        self.feed(Input::SendFailed {
            round: delivery.round,
            part: delivery.part.clone(),
            error,
        })?;
        Ok(())
    }

    /// Moves `delivery` on; `true` once it is done with (sent, failed or no
    /// longer wanted).
    fn try_deliver(&mut self, conn: &mut C::Conn, delivery: &mut Delivery) -> Result<bool, Fail> {
        let part = delivery.part.clone();
        if !in_play(self.engine.session(), delivery.round, &part) {
            return Ok(true);
        }
        let Some(pane) = self.panes.get(&part).map(|pane| pane.id) else {
            if delivery.spawns >= MAX_SPAWNS {
                self.send_failed(delivery, "the agent's pane keeps closing".to_owned())?;
                return Ok(true);
            }
            delivery.spawns += 1;
            delivery.pasted = false;
            if let Err(why) = self.spawn(conn, &part)? {
                self.send_failed(delivery, why)?;
                return Ok(true);
            }
            return Ok(false);
        };
        if !delivery.pasted {
            if !self.ready(conn, &part)? {
                let now = self.clock.now();
                let held = self
                    .panes
                    .get(&part)
                    .is_some_and(|pane| pane.held() || pane.busy());
                if held {
                    // The user answers the prompt, or the agent ends its
                    // turn; the time limit starts again from then.
                    delivery.queued = now;
                } else if now.saturating_sub(delivery.queued) >= READY_LIMIT {
                    self.send_failed(delivery, NoaError::NotReady.to_string())?;
                    return Ok(true);
                }
                return Ok(false);
            }
            match conn.type_text(pane, &delivery.body, true) {
                Ok(()) => {
                    delivery.pasted = true;
                    if let Some(pane) = self.panes.get_mut(&part) {
                        pane.briefed = true;
                    }
                }
                Err(NoaError::NotReady) => {
                    if let Some(pane) = self.panes.get_mut(&part) {
                        pane.ready = false;
                    }
                    return Ok(false);
                }
                Err(NoaError::PaneGone) => {
                    self.pane_gone(&part)?;
                    return Ok(false);
                }
                Err(NoaError::Refused(why)) => {
                    self.send_failed(delivery, why)?;
                    return Ok(true);
                }
                Err(NoaError::Link(why)) => return Err(Fail::Link(why)),
            }
            self.clock.sleep(SUBMIT_DELAY);
        }
        match conn.type_text(pane, "\r", false) {
            Ok(()) => {}
            Err(NoaError::PaneGone) => {
                delivery.pasted = false;
                self.pane_gone(&part)?;
                return Ok(false);
            }
            Err(NoaError::Link(why)) => return Err(Fail::Link(why)),
            Err(other) => {
                self.send_failed(delivery, other.to_string())?;
                return Ok(true);
            }
        }
        let at = self.clock.now();
        self.feed(Input::Sent {
            round: delivery.round,
            part,
            at,
        })?;
        Ok(true)
    }

    // ---- reply files ----

    fn poll_replies(&mut self) -> Result<(), String> {
        let now = self.clock.now();
        let mut written = Vec::new();
        for (path, watch) in &mut self.watches {
            let text = match store::read_reply(path) {
                Ok(Some(text)) if !text.trim().is_empty() => text,
                _ => {
                    watch.seen = None;
                    continue;
                }
            };
            if watch.reported.as_ref() == Some(&text) {
                continue;
            }
            match &watch.seen {
                Some((seen, since)) if *seen == text => {
                    if now.saturating_sub(*since) >= REPLY_STABLE_FOR {
                        written.push((path.clone(), watch.round, watch.part.clone(), text));
                    }
                }
                _ => watch.seen = Some((text, now)),
            }
        }
        for (path, round, part, content) in written {
            if let Some(watch) = self.watches.get_mut(&path) {
                watch.reported = Some(content.clone());
                watch.seen = None;
            }
            self.feed(Input::ReplyFileWritten {
                round,
                part,
                content,
                at: now,
            })?;
        }
        let session = self.engine.session();
        self.watches.retain(|_, watch| {
            session
                .round(watch.round)
                .and_then(|r| r.brief(&watch.part))
                .is_none_or(|b| b.receipt.replied.is_none())
        });
        Ok(())
    }
}

/// Opens the stored discussion at `dir` for a runner: its log is locked
/// until the runner ends (another runner gets "busy"), and what a log
/// edited by hand or a moved repository could turn into a pane started
/// or a file touched outside the discussion is refused first.
fn resume(dir: &Path, config: Config) -> Result<(Engine, Store), String> {
    let (store, session) =
        Store::open(dir).map_err(|e| format!("{} を開けません: {e}", dir.display()))?;
    let refuse = |why: String| format!("{} を再開できません: {why}", dir.display());
    // The setup is written into the login shell's script, so a log edited
    // by hand gets the same check a new session does.
    for part in std::iter::once(&session.moderator).chain(&session.panelists) {
        part.setup.check(part.kind).map_err(refuse)?;
    }
    // Reply files would otherwise be made again where the repository was.
    if let Some(workdir) = &session.workdir
        && !workdir.is_dir()
    {
        return Err(refuse(format!(
            "作業ディレクトリ {} がありません（移動または削除されています）",
            workdir.display()
        )));
    }
    let engine = Engine::resume(session, dir, config);
    if let Some(path) = engine.foreign_reply_path() {
        return Err(refuse(format!(
            "返信ファイル {} がこの議論の場所にありません",
            path.display()
        )));
    }
    let base = engine
        .session()
        .workdir
        .clone()
        .unwrap_or_else(|| dir.to_owned());
    if let Some(path) = engine
        .pending_reply_paths()
        .into_iter()
        .find(|path| through_symlink(path, &base))
    {
        return Err(refuse(format!(
            "返信ファイル {} がシンボリックリンクを通ります",
            path.display()
        )));
    }
    Ok((engine, store))
}

/// Whether `path`, or a directory between it and `base`, is a symbolic
/// link.
fn through_symlink(path: &Path, base: &Path) -> bool {
    path.ancestors()
        .take_while(|p| *p != base)
        .any(|p| fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()))
}

/// Where the agents run: the discussions directory, so the reply files are
/// inside their working directory and an agent that asks to trust its
/// folder asks once for all discussions.
fn agent_cwd(dir: &Path) -> PathBuf {
    dir.parent().unwrap_or(dir).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use discussion::model::brief::PANELIST_SECTIONS;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An agent at its input box, as Claude Code and Antigravity draw it.
    const FAKE_IDLE: &str = "welcome\n────────────────────────────────────────\n> \n────────────────────────────────────────\n  status line";

    #[derive(Debug, Clone)]
    struct FakePane {
        label: String,
        paste: bool,
        screen: String,
        state: Option<String>,
        blocked: Option<String>,
        detail: Option<String>,
        seq: u64,
    }

    #[derive(Default)]
    struct World {
        now: Millis,
        up: bool,
        attempts: Vec<Millis>,
        next_pane: u64,
        /// Whether new panes take a paste at once.
        cold: bool,
        /// New panes whose label starts with this exit at once.
        doomed: Option<String>,
        panes: BTreeMap<u64, FakePane>,
        events: VecDeque<Changes>,
        typed: Vec<(u64, String, bool)>,
        spawned: Vec<(Placement, PaneSpec, u64)>,
        rpc_panes: usize,
        rpc_changes: usize,
        rpc_paste_ready: usize,
        rpc_screen: usize,
        focused: Vec<u64>,
        /// Panes closed through `close_pane`.
        closed: Vec<u64>,
        /// `close_pane` closes the pane but answers as Noa does when its
        /// UI thread is slow (-32009): the outcome is unknown.
        close_unsure: bool,
    }

    /// A fake Noa, its connector and the clock, sharing one world.
    #[derive(Clone)]
    struct Fake(Rc<RefCell<World>>);

    impl Fake {
        fn new() -> Self {
            Fake(Rc::new(RefCell::new(World {
                up: true,
                next_pane: 100,
                ..World::default()
            })))
        }

        fn w(&self) -> std::cell::RefMut<'_, World> {
            self.0.borrow_mut()
        }

        fn view(id: u64, pane: &FakePane) -> PaneView {
            PaneView {
                pane_id: id,
                label: Some(pane.label.clone()),
                agent_state: pane.state.clone(),
                agent_seq: pane.seq,
                agent_blocked: pane.blocked.clone(),
                agent_detail: pane.detail.clone(),
            }
        }

        fn report(&self, pane: u64, state: &str) {
            let mut w = self.w();
            let p = w.panes.get_mut(&pane).unwrap();
            p.seq += 1;
            p.state = Some(state.to_owned());
            p.blocked = None;
            p.detail = None;
            let view = Fake::view(pane, p);
            w.events.push_back(Changes {
                panes: vec![view],
                ..Changes::default()
            });
        }

        /// A report that holds the agent on a prompt (`blocked`).
        fn report_blocked(&self, pane: u64, state: &str, blocked: &str, detail: &str) {
            let mut w = self.w();
            let p = w.panes.get_mut(&pane).unwrap();
            p.seq += 1;
            p.state = Some(state.to_owned());
            p.blocked = Some(blocked.to_owned());
            p.detail = Some(detail.to_owned());
            let view = Fake::view(pane, p);
            w.events.push_back(Changes {
                panes: vec![view],
                ..Changes::default()
            });
        }

        fn close(&self, pane: u64) {
            let mut w = self.w();
            w.panes.remove(&pane);
            w.events.push_back(Changes {
                removed: vec![pane],
                ..Changes::default()
            });
        }

        /// The texts typed into `pane`: (text, paste).
        fn typed(&self, pane: u64) -> Vec<(String, bool)> {
            self.0
                .borrow()
                .typed
                .iter()
                .filter(|(p, ..)| *p == pane)
                .map(|(_, text, paste)| (text.clone(), *paste))
                .collect()
        }

        fn pane_of(&self, label_start: &str) -> u64 {
            let w = self.0.borrow();
            *w.panes
                .iter()
                .find(|(_, p)| p.label.starts_with(label_start))
                .unwrap()
                .0
        }

        fn up(&self) -> Result<(), NoaError> {
            if self.0.borrow().up {
                Ok(())
            } else {
                Err(NoaError::Link("closed".into()))
            }
        }
    }

    impl Clock for Fake {
        fn now(&self) -> Millis {
            self.0.borrow().now
        }

        fn sleep(&self, duration: Duration) {
            self.w().now += duration.as_millis() as Millis;
        }
    }

    impl Connect for Fake {
        type Conn = Fake;

        fn connect(&mut self) -> Result<Fake, String> {
            let mut w = self.w();
            let now = w.now;
            w.attempts.push(now);
            if w.up {
                Ok(self.clone())
            } else {
                Err("Noa is not running".into())
            }
        }
    }

    impl Noa for Fake {
        fn panes(&mut self) -> Result<Vec<PaneView>, NoaError> {
            self.up()?;
            self.w().rpc_panes += 1;
            let w = self.0.borrow();
            Ok(w.panes.iter().map(|(id, p)| Fake::view(*id, p)).collect())
        }

        fn changes(&mut self, wait: Duration) -> Result<Option<Changes>, NoaError> {
            self.up()?;
            let mut w = self.w();
            w.rpc_changes += 1;
            w.now += wait.as_millis() as Millis;
            Ok(w.events.pop_front())
        }

        fn spawn(&mut self, placement: Placement, spec: &PaneSpec) -> Result<u64, NoaError> {
            self.up()?;
            let mut w = self.w();
            if let Placement::Right(beside) | Placement::Below(beside) = placement
                && !w.panes.contains_key(&beside)
            {
                return Err(NoaError::PaneGone);
            }
            let id = w.next_pane;
            w.next_pane += 1;
            let paste = !w.cold;
            w.panes.insert(
                id,
                FakePane {
                    label: spec.label.clone(),
                    paste,
                    screen: FAKE_IDLE.into(),
                    state: None,
                    blocked: None,
                    detail: None,
                    seq: 0,
                },
            );
            w.spawned.push((placement, spec.clone(), id));
            if w.doomed
                .as_ref()
                .is_some_and(|d| spec.label.starts_with(d.as_str()))
            {
                w.panes.remove(&id);
                w.events.push_back(Changes {
                    removed: vec![id],
                    ..Changes::default()
                });
            }
            Ok(id)
        }

        fn paste_ready(&mut self, pane: u64) -> Result<bool, NoaError> {
            self.up()?;
            self.w().rpc_paste_ready += 1;
            let w = self.0.borrow();
            w.panes
                .get(&pane)
                .map(|p| p.paste)
                .ok_or(NoaError::PaneGone)
        }

        fn screen(&mut self, pane: u64) -> Result<String, NoaError> {
            self.up()?;
            self.w().rpc_screen += 1;
            let w = self.0.borrow();
            w.panes
                .get(&pane)
                .map(|p| p.screen.clone())
                .ok_or(NoaError::PaneGone)
        }

        fn type_text(&mut self, pane: u64, text: &str, paste: bool) -> Result<(), NoaError> {
            self.up()?;
            let mut w = self.w();
            let p = w.panes.get(&pane).ok_or(NoaError::PaneGone)?;
            if paste && !p.paste {
                return Err(NoaError::NotReady);
            }
            w.typed.push((pane, text.to_owned(), paste));
            Ok(())
        }

        fn focus(&mut self, pane: u64) -> Result<(), NoaError> {
            self.up()?;
            let mut w = self.w();
            if !w.panes.contains_key(&pane) {
                return Err(NoaError::PaneGone);
            }
            w.focused.push(pane);
            Ok(())
        }

        fn close_pane(&mut self, pane: u64) -> Result<(), NoaError> {
            self.up()?;
            if !self.0.borrow().panes.contains_key(&pane) {
                return Err(NoaError::PaneGone);
            }
            self.w().closed.push(pane);
            Fake::close(self, pane);
            if self.0.borrow().close_unsure {
                return Err(NoaError::Refused("timed out (-32009)".into()));
            }
            Ok(())
        }
    }

    const CONFIG: Config = Config {
        start_timeout: 60_000,
        reply_timeout: 600_000,
        max_retries: 2,
        debate_max_exchanges: 2,
    };

    fn temp_root(name: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "noa-discussion-live-{}-{name}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn p(id: &str) -> PartId {
        PartId::new(id)
    }

    struct Rig {
        fake: Fake,
        runner: Runner<Fake, Fake>,
        actions: Sender<Action>,
        updates: Receiver<Update>,
    }

    impl Rig {
        fn open(fake: &Fake, start: Start) -> Rig {
            let (actions, action_rx) = mpsc::channel();
            let (update_tx, updates) = mpsc::channel();
            let runner = Runner::open(
                start,
                fake.clone(),
                fake.clone(),
                CONFIG,
                action_rx,
                update_tx,
                Rc::new(|| {}),
            )
            .unwrap();
            Rig {
                fake: fake.clone(),
                runner,
                actions,
                updates,
            }
        }

        fn new(fake: &Fake, root: &Path) -> Rig {
            fake.w().now = 1_000;
            Rig::open(
                fake,
                Start::New {
                    topic: "Pick a database".into(),
                    workdir: None,
                    max_rounds: None,
                    moderator: AgentKind::Claude.into(),
                    panel: vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
                    root: root.to_owned(),
                },
            )
        }

        fn session(&self) -> &Session {
            self.runner.engine.session()
        }

        fn dir(&self) -> PathBuf {
            self.runner.engine.dir().to_owned()
        }

        fn step(&mut self) {
            assert!(self.runner.step().unwrap());
        }

        /// Steps until `done`, at most `max` times.
        fn until(&mut self, max: usize, done: impl Fn(&Rig) -> bool) {
            for _ in 0..max {
                if done(self) {
                    return;
                }
                self.step();
            }
            assert!(done(self), "condition not met after {max} steps");
        }

        fn updates(&self) -> Vec<Update> {
            self.updates.try_iter().collect()
        }

        /// The notices sent since the updates were last read.
        fn notices(&self) -> Vec<String> {
            self.updates()
                .into_iter()
                .filter_map(|update| match update {
                    Update::Notice(text) => Some(text),
                    _ => None,
                })
                .collect()
        }

        fn receipt(&self, round: RoundNo, part: &str) -> discussion::model::types::Receipt {
            self.session()
                .round(round)
                .unwrap()
                .brief(&p(part))
                .unwrap()
                .receipt
                .clone()
        }

        fn submitted(&self, pane: u64) -> usize {
            self.fake
                .typed(pane)
                .iter()
                .filter(|(text, paste)| text == "\r" && !paste)
                .count()
        }
    }

    #[test]
    fn a_new_session_opens_its_tab_and_runs_a_whole_round() {
        let root = temp_root("round");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();

        // One tab: the moderator, then the panelists split to its right.
        let spawned = fake.0.borrow().spawned.clone();
        let placements: Vec<Placement> = spawned.iter().map(|s| s.0).collect();
        assert_eq!(
            placements,
            vec![
                Placement::NewTab,
                Placement::Right(100),
                Placement::Below(101)
            ]
        );
        let id = rig.session().id.clone();
        for ((_, spec, _), (part, cmd)) in
            spawned
                .iter()
                .zip([("noa", "claude"), ("claude", "claude"), ("codex", "codex")])
        {
            assert_eq!(spec.label, format!("{part} · {id}"));
            assert_eq!(spec.argv[4], format!("exec {cmd}"));
            assert_eq!(spec.cwd, root);
        }
        let parts = &rig.session().panelists;
        assert_eq!(parts[0].pane_id, Some(101));
        assert_eq!(rig.session().moderator.pane_id, Some(100));

        // Each panelist's brief is pasted whole, then submitted with a raw
        // Enter, once its pane has settled; the moderator gets nothing yet.
        rig.until(40, |r| r.submitted(101) == 1 && r.submitted(102) == 1);
        for pane in [101, 102] {
            let typed = fake.typed(pane);
            assert_eq!(typed.len(), 2);
            assert!(typed[0].1 && typed[0].0.contains("Pick a database"));
            assert_eq!(typed[1], ("\r".to_owned(), false));
        }
        assert!(fake.typed(100).is_empty());
        let brief = &rig.session().round(1).unwrap().briefs[0];
        assert_eq!(fake.typed(101)[0].0, brief.body);
        assert!(rig.receipt(1, "claude").sent.is_some());

        // claude reports; codex never does, but its reply counts as started.
        fake.report(101, "running");
        rig.step();
        assert!(rig.receipt(1, "claude").started.is_some());
        let dir = rig.dir();
        fs::write(store::reply_path(&dir, 1, &p("claude")), "Postgres.").unwrap();
        fs::write(store::reply_path(&dir, 1, &p("codex")), "SQLite.").unwrap();
        // claude ends its turn, as the real agents report.
        fake.report(101, "finished");
        rig.until(20, |r| {
            r.session().current_round().unwrap().phase == Phase::Summarizing
        });
        assert!(rig.receipt(1, "codex").started.is_some());

        // The moderator gets its brief, and its JSON starts round 2.
        rig.until(20, |r| r.submitted(100) == 1);
        assert!(fake.typed(100)[0].0.contains("Postgres."));
        fs::write(
            store::moderator_reply_path(&dir, 1),
            r#"{"summary":"Two options."}"#,
        )
        .unwrap();
        rig.until(20, |r| r.session().rounds.len() == 2);
        rig.until(20, |r| r.submitted(101) == 2 && r.submitted(102) == 2);
        assert_eq!(fake.0.borrow().spawned.len(), 3);

        // Everything went to the log: it folds back to the same session.
        // Read-only: the runner still holds the log.
        let stored = store::load(&dir).unwrap();
        assert_eq!(&stored, rig.session());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_down_noa_is_retried_with_backoff_and_panes_are_kept_across_reconnects() {
        let root = temp_root("reconnect");
        let fake = Fake::new();
        fake.w().up = false;
        let mut rig = Rig::new(&fake, &root);
        for _ in 0..20 {
            rig.step();
        }
        assert!(
            rig.updates()
                .iter()
                .any(|u| matches!(u, Update::Link(Link::Down(why)) if why == "Noa is not running"))
        );
        let attempts = fake.0.borrow().attempts.clone();
        assert!(attempts.len() >= 3, "{attempts:?}");
        let gaps: Vec<Millis> = attempts.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps[0] >= 500 && gaps[1] >= 1_000, "{gaps:?}");
        assert!(fake.0.borrow().spawned.is_empty());
        // Nothing is started (or sent) before Noa is there.
        assert!(rig.session().rounds.is_empty());

        fake.w().up = true;
        rig.until(60, |r| r.runner.conn.is_some());
        assert!(rig.updates().contains(&Update::Link(Link::Live)));
        assert_eq!(fake.0.borrow().spawned.len(), 3);

        // The link drops before the briefs are typed, then comes back.
        fake.w().up = false;
        rig.step();
        assert!(rig.runner.conn.is_none());
        fake.w().up = true;
        rig.until(60, |r| r.submitted(101) == 1 && r.submitted(102) == 1);
        assert_eq!(fake.0.borrow().spawned.len(), 3, "panes were respawned");
        // Each brief went once.
        for _ in 0..20 {
            rig.step();
        }
        assert_eq!(rig.submitted(101), 1);
        assert_eq!(rig.submitted(102), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_closed_pane_is_respawned_and_its_brief_sent_again() {
        let root = temp_root("closed");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.until(40, |r| r.submitted(101) == 1 && r.submitted(102) == 1);
        fake.close(101);
        rig.until(40, |r| r.submitted(103) == 1);
        let spawned = fake.0.borrow().spawned.clone();
        assert_eq!(spawned.len(), 4);
        assert_eq!(spawned[3].0, Placement::Below(102));
        assert!(spawned[3].1.label.starts_with("claude · "));
        assert_eq!(rig.session().panelists[0].pane_id, Some(103));
        assert_eq!(rig.receipt(1, "claude").retries, 1);
        assert_eq!(fake.typed(103)[0].0, fake.typed(101)[0].0);
        assert!(
            rig.updates()
                .iter()
                .any(|u| matches!(u, Update::Notice(n) if n.contains("claude")))
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_agent_error_is_acted_on_once_however_often_the_pane_is_listed() {
        let root = temp_root("error");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.until(40, |r| r.submitted(101) == 1);
        fake.report(101, "error");
        rig.until(10, |r| r.submitted(101) == 2);
        assert_eq!(rig.receipt(1, "claude").retries, 1);
        // Lost notifications make the runner list every pane again: the
        // same report must not count twice.
        for _ in 0..3 {
            fake.w().events.push_back(Changes {
                dropped: true,
                ..Changes::default()
            });
            rig.step();
        }
        assert_eq!(rig.receipt(1, "claude").retries, 1);
        assert_eq!(rig.submitted(101), 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_agent_that_exits_at_once_is_not_respawned_forever() {
        let root = temp_root("doomed");
        let fake = Fake::new();
        fake.w().doomed = Some("claude · ".into());
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        rig.until(200, |r| r.receipt(1, "claude").failed.is_some());
        let claude_spawns = |fake: &Fake| {
            let w = fake.0.borrow();
            w.spawned
                .iter()
                .filter(|s| s.1.label.starts_with("claude · "))
                .count()
        };
        let spawns = claude_spawns(&fake);
        // The first spawn, then MAX_SPAWNS per send attempt (1 + retries).
        assert!(spawns <= 1 + MAX_SPAWNS as usize * 3, "{spawns}");
        for _ in 0..50 {
            rig.step();
        }
        assert_eq!(claude_spawns(&fake), spawns);
        // codex is not held up by it.
        rig.until(40, |r| r.receipt(1, "codex").sent.is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn nothing_is_typed_until_the_pane_takes_a_paste() {
        let root = temp_root("cold");
        let fake = Fake::new();
        fake.w().cold = true;
        let mut rig = Rig::new(&fake, &root);
        for _ in 0..30 {
            rig.step();
        }
        assert!(fake.0.borrow().typed.is_empty());
        assert!(rig.receipt(1, "claude").sent.is_none());
        for pane in fake.w().panes.values_mut() {
            pane.paste = true;
        }
        rig.until(40, |r| r.submitted(101) == 1 && r.submitted(102) == 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_fresh_agent_that_reports_idle_gets_its_brief_without_the_quiet_wait() {
        let root = temp_root("idle");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        fake.report(101, "input");
        rig.step();
        // One step to see the report, one to deliver: well under QUIET_FOR.
        rig.step();
        assert_eq!(rig.submitted(101), 1);
        assert_eq!(rig.submitted(102), 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pane_held_on_a_prompt_gets_no_brief_until_it_is_answered() {
        let root = temp_root("held");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        // A permission dialog the runner does not answer: a pasted brief
        // and its Enter would pick its selected choice.
        fake.report_blocked(101, "permission", "permission", "Bash");
        fake.w().panes.get_mut(&101).unwrap().screen =
            "────────────────────────────────────────\n Allow Bash?\n ❯ 1. Yes\n   2. No".into();
        // Well past READY_LIMIT: still nothing typed, nothing failed.
        for _ in 0..250 {
            rig.step();
        }
        assert_eq!(rig.submitted(101), 0);
        assert!(fake.typed(101).is_empty());
        let receipt = rig.receipt(1, "claude");
        assert_eq!(receipt.failed, None);
        let told: Vec<String> = rig
            .notices()
            .into_iter()
            .filter(|n| n.contains("確認待ち"))
            .collect();
        assert_eq!(told.len(), 1, "told once: {told:?}");
        // The user answers; the agent reports it is at its input box.
        fake.w().panes.get_mut(&101).unwrap().screen = FAKE_IDLE.into();
        fake.report(101, "input");
        rig.until(10, |r| r.submitted(101) == 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn claudes_folder_trust_prompt_is_answered_yes_then_the_brief_goes_in() {
        let root = temp_root("trust");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        fake.report_blocked(101, "permission", "permission", CLAUDE_TRUST_DETAIL);
        let prompt = |selected_yes: bool| {
            let (no, yes) = if selected_yes {
                ("  ", "❯ ")
            } else {
                ("❯ ", "  ")
            };
            format!(
                "────────────────────────────────────────\n Accessing workspace:\n {no}No, exit\n {yes}Yes, I trust this folder\n Enter to confirm · Esc to cancel"
            )
        };
        fake.w().panes.get_mut(&101).unwrap().screen = prompt(false);
        rig.step();
        // Down first: "No, exit" is selected. Never a paste.
        assert_eq!(fake.typed(101), vec![("\x1b[B".to_owned(), false)]);
        fake.w().panes.get_mut(&101).unwrap().screen = prompt(true);
        rig.step();
        assert_eq!(fake.typed(101).last(), Some(&("\r".to_owned(), false)));
        assert_eq!(rig.submitted(101), 1, "the Enter answered the prompt");
        assert_eq!(rig.receipt(1, "claude").sent, None);
        // Claude Code starts and reports it is idle: the brief goes in.
        fake.w().panes.get_mut(&101).unwrap().screen = FAKE_IDLE.into();
        fake.report(101, "input");
        rig.until(10, |r| r.receipt(1, "claude").sent.is_some());
        assert!(
            fake.typed(101)
                .iter()
                .any(|(text, paste)| *paste && text.contains("Pick a database"))
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_trust_prompt_in_a_named_workdir_is_left_to_the_user() {
        let root = temp_root("trust-workdir");
        let workdir = temp_root("trust-workdir-repo");
        fs::create_dir_all(&workdir).unwrap();
        let fake = Fake::new();
        fake.w().now = 1_000;
        let mut rig = Rig::open(
            &fake,
            Start::New {
                topic: "Review this repo".into(),
                workdir: Some(workdir.clone()),
                max_rounds: None,
                moderator: AgentKind::Claude.into(),
                panel: vec![AgentKind::Claude.into(), AgentKind::Codex.into()],
                root: root.clone(),
            },
        );
        rig.step();
        fake.report_blocked(101, "permission", "permission", CLAUDE_TRUST_DETAIL);
        fake.w().panes.get_mut(&101).unwrap().screen =
            " ❯ No, exit\n   Yes, I trust this folder\n".into();
        for _ in 0..20 {
            rig.step();
        }
        assert!(fake.typed(101).is_empty(), "{:?}", fake.typed(101));
        assert!(rig.notices().iter().any(|n| n.contains("確認待ち")));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&workdir);
    }

    #[test]
    fn a_trust_prompt_that_does_not_go_away_is_left_to_the_user() {
        let root = temp_root("trust-stuck");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        fake.report_blocked(101, "permission", "permission", CLAUDE_TRUST_DETAIL);
        fake.w().panes.get_mut(&101).unwrap().screen =
            " ❯ No, exit\n   Yes, I trust this folder\n".into();
        for _ in 0..20 {
            rig.step();
        }
        // One Down; the screen never shows it, so no second key: the user
        // is told instead.
        assert_eq!(fake.typed(101), vec![("\x1b[B".to_owned(), false)]);
        assert!(rig.notices().iter().any(|n| n.contains("確認待ち")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn trust_key_moves_to_yes_then_confirms() {
        assert_eq!(
            trust_key(" ❯ No, exit\n   Yes, I trust this folder"),
            Some("\x1b[B")
        );
        assert_eq!(
            trust_key("   No, exit\n ❯ Yes, I trust this folder"),
            Some("\r")
        );
        assert_eq!(trust_key(" ❯ 1. Yes\n   2. No"), None);
    }

    #[test]
    fn an_agent_that_reports_an_error_while_it_signs_in_gets_its_brief_after_sign_in() {
        let root = temp_root("signin");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        // Antigravity reports `error` ("Sign in required") while it signs
        // in, under an input box that drops what is typed meanwhile.
        fake.report(101, "error");
        for _ in 0..20 {
            rig.step();
        }
        assert_eq!(rig.submitted(101), 0);
        assert_eq!(rig.receipt(1, "claude").sent, None);
        fake.report(101, "finished");
        rig.until(10, |r| r.submitted(101) == 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn agents_run_in_the_named_workdir_which_gets_a_self_ignoring_dot_noa() {
        let root = temp_root("workdir");
        let workdir = temp_root("workdir-repo");
        fs::create_dir_all(&workdir).unwrap();
        let fake = Fake::new();
        fake.w().now = 1_000;
        let mut rig = Rig::open(
            &fake,
            Start::New {
                topic: "Review this repo".into(),
                workdir: Some(workdir.clone()),
                max_rounds: None,
                moderator: AgentKind::Claude.into(),
                panel: vec![AgentKind::Codex.into()],
                root: root.clone(),
            },
        );
        rig.step();
        let spawned = fake.0.borrow().spawned.clone();
        assert!(!spawned.is_empty());
        assert!(spawned.iter().all(|(_, spec, _)| spec.cwd == workdir));
        assert_eq!(
            fs::read_to_string(workdir.join(".noa/.gitignore")).unwrap(),
            "*\n"
        );
        // The session log stays in the discussions directory.
        assert!(rig.dir().starts_with(&root));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&workdir);
    }

    #[test]
    fn a_workdir_that_is_not_a_directory_is_refused() {
        let root = temp_root("no-workdir");
        let fake = Fake::new();
        let (_actions, action_rx) = mpsc::channel();
        let (update_tx, _updates) = mpsc::channel();
        let missing = root.join("missing");
        let opened = Runner::open(
            Start::New {
                topic: "t".into(),
                workdir: Some(missing.clone()),
                max_rounds: None,
                moderator: AgentKind::Claude.into(),
                panel: vec![AgentKind::Codex.into()],
                root: root.clone(),
            },
            fake.clone(),
            fake.clone(),
            CONFIG,
            action_rx,
            update_tx,
            Rc::new(|| {}),
        );
        let Err(why) = opened else {
            panic!("opened with a missing workdir");
        };
        assert!(why.contains("ディレクトリではありません"), "{why}");
        assert!(!root.exists(), "nothing was stored");
    }

    #[test]
    fn a_pane_that_took_a_brief_and_then_waits_on_a_prompt_gets_no_resend() {
        let root = temp_root("held-later");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.until(40, |r| r.submitted(101) == 1);
        // The agent writes a reply that is rejected, then asks the user a
        // question before the correction goes out: the correction waits.
        fake.report_blocked(101, "input", "question", "Which database?");
        rig.step();
        let path = store::reply_path(&rig.dir(), 1, &p("claude"));
        fs::write(&path, "---\nissues: [Z]\n---\nbody").unwrap();
        for _ in 0..20 {
            rig.step();
        }
        assert!(rig.receipt(1, "claude").rejected.is_some());
        assert_eq!(rig.submitted(101), 1, "nothing typed into the dialog");
        assert!(rig.notices().iter().any(|n| n.contains("確認待ち")));
        // Answered: the agent is back at its input box.
        fake.report(101, "input");
        rig.until(10, |r| r.submitted(101) == 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_quiet_screen_without_an_input_line_never_takes_a_brief() {
        let root = temp_root("no-input");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        // No report, and a banner with a rule: once counted as ready after
        // 30 s.
        fake.w().panes.get_mut(&101).unwrap().screen =
            "────────────────────────────────────────\n Loading…".into();
        for _ in 0..250 {
            rig.step();
        }
        assert_eq!(rig.submitted(101), 0);
        assert!(fake.typed(101).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_retry_after_rejected_replies_does_not_take_the_old_file_again() {
        let root = temp_root("retry-rejected");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.until(40, |r| r.submitted(101) == 1);
        let path = store::reply_path(&rig.dir(), 1, &p("claude"));
        // Three invalid replies: two go back as corrections, the third
        // fails the brief and stays on disk.
        for n in 1..=3u8 {
            fs::write(&path, "---\nissues: [Z]\n---\nbody").unwrap();
            rig.until(20, |r| {
                let receipt = r.receipt(1, "claude");
                receipt.retries >= n.min(2) && (n < 3 || receipt.failed.is_some())
            });
        }
        assert!(rig.receipt(1, "claude").failed.is_some());
        assert!(path.exists(), "the last invalid reply is left on disk");
        rig.actions.send(Action::Retry).unwrap();
        for _ in 0..10 {
            rig.step();
        }
        let receipt = rig.receipt(1, "claude");
        assert_eq!(receipt.failed, None);
        assert_eq!(
            receipt.retries, 0,
            "the old file was not taken as a new reply"
        );
        assert_eq!(receipt.rejected, None);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_error_after_a_prompt_on_a_briefed_pane_is_a_failed_turn_and_resent() {
        let root = temp_root("error-after-prompt");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.until(40, |r| r.submitted(101) == 1);
        // A rejected reply's correction is held back while a question
        // dialog is up; then the turn fails after the user answers it: the
        // agent is back at its input box.
        fake.report_blocked(101, "input", "question", "Which database?");
        rig.step();
        let path = store::reply_path(&rig.dir(), 1, &p("claude"));
        fs::write(&path, "---\nissues: [Z]\n---\nbody").unwrap();
        for _ in 0..5 {
            rig.step();
        }
        assert_eq!(rig.submitted(101), 1);
        fake.report(101, "error");
        rig.until(20, |r| r.submitted(101) == 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_resend_waits_while_the_agent_is_still_working() {
        let root = temp_root("running-resend");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.until(40, |r| r.submitted(101) == 1);
        fake.report(101, "running");
        // Still working at the reply deadline: the resend waits.
        fake.w().now += CONFIG.reply_timeout + 1;
        for _ in 0..20 {
            rig.step();
        }
        assert_eq!(rig.submitted(101), 1, "nothing typed into a working agent");
        fake.report(101, "finished");
        rig.until(10, |r| r.submitted(101) == 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn after_noa_restarts_a_reused_pane_id_is_not_taken_for_an_agent() {
        let root = temp_root("reused-id");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.until(40, |r| r.submitted(101) == 1 && r.submitted(102) == 1);
        // Noa restarts while the link is down: every pane is gone, and a
        // shell the user opens gets claude's old pane id.
        fake.w().up = false;
        rig.step();
        {
            let mut w = fake.w();
            w.panes.clear();
            w.panes.insert(
                101,
                FakePane {
                    label: "zsh".into(),
                    paste: true,
                    screen: FAKE_IDLE.into(),
                    state: None,
                    blocked: None,
                    detail: None,
                    seq: 0,
                },
            );
            w.typed.clear();
        }
        fake.w().up = true;
        rig.until(60, |r| r.runner.conn.is_some());
        // claude's brief is due again (no reply in time).
        fake.w().now += CONFIG.reply_timeout + 1;
        for _ in 0..40 {
            rig.step();
        }
        assert!(
            fake.typed(101).is_empty(),
            "typed into the user's shell: {:?}",
            fake.typed(101)
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_agent_waiting_on_a_question_does_not_time_out() {
        let root = temp_root("question-deadline");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.until(40, |r| r.submitted(101) == 1);
        fake.report(101, "running");
        rig.step();
        // A question dialog: Noa reports `input` with `blocked: question`.
        fake.report_blocked(101, "input", "question", "Which database?");
        rig.step();
        fake.w().now += 3 * CONFIG.reply_timeout;
        for _ in 0..5 {
            rig.step();
        }
        let receipt = rig.receipt(1, "claude");
        assert_eq!(receipt.retries, 0, "{receipt:?}");
        assert_eq!(receipt.failed, None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn closing_from_the_ui_ends_the_session_and_stops_sending() {
        let root = temp_root("close");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.until(40, |r| r.submitted(101) == 1);
        rig.actions.send(Action::Close).unwrap();
        rig.step();
        assert!(rig.session().closed);
        // A deadline after the end resends nothing.
        fake.w().now += CONFIG.reply_timeout + 1;
        for _ in 0..10 {
            rig.step();
        }
        assert_eq!(rig.submitted(101), 1);
        let stored = store::load(&rig.dir()).unwrap();
        assert!(stored.closed, "the end is in the log");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn panes_close_together_only_after_the_discussion_ends() {
        let root = temp_root("close-panes");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.until(40, |r| r.submitted(101) == 1);
        let _ = rig.notices();
        rig.actions.send(Action::ClosePanes).unwrap();
        rig.step();
        assert!(fake.0.borrow().closed.is_empty(), "refused while running");
        assert!(
            rig.notices()
                .iter()
                .any(|n| n.contains("議論が終わってから")),
            "the refusal is told"
        );
        rig.actions.send(Action::Close).unwrap();
        rig.step();
        let _ = rig.notices();
        rig.actions.send(Action::ClosePanes).unwrap();
        rig.step();
        let mut closed = fake.0.borrow().closed.clone();
        closed.sort_unstable();
        assert_eq!(closed, vec![100, 101, 102]);
        assert!(fake.0.borrow().panes.is_empty());
        for _ in 0..10 {
            rig.step();
        }
        let notices = rig.notices();
        assert!(
            notices.iter().any(|n| n == "3 個のペインを閉じました"),
            "{notices:?}"
        );
        assert!(
            !notices.iter().any(|n| n.contains("のペインが閉じました")),
            "closing is no agent error: {notices:?}"
        );
        assert_eq!(fake.0.borrow().spawned.len(), 3, "nothing is started again");
        rig.actions.send(Action::ClosePanes).unwrap();
        rig.step();
        assert_eq!(rig.notices(), vec!["閉じるペインはありません".to_owned()]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pane_whose_close_timed_out_closes_later_without_an_agent_error() {
        let root = temp_root("close-panes-unsure");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.until(40, |r| r.submitted(101) == 1);
        rig.actions.send(Action::Close).unwrap();
        rig.step();
        fake.w().close_unsure = true;
        let _ = rig.notices();
        rig.actions.send(Action::ClosePanes).unwrap();
        for _ in 0..10 {
            rig.step();
        }
        let notices = rig.notices();
        assert!(
            !notices.iter().any(|n| n.contains("のペインが閉じました")),
            "its closing is no agent error: {notices:?}"
        );
        assert!(
            rig.runner.panes.is_empty(),
            "the closed panes are forgotten"
        );
        assert_eq!(fake.0.borrow().spawned.len(), 3, "nothing is started again");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn implementing_the_result_starts_an_agent_in_a_new_tab_on_a_task_file() {
        let root = temp_root("implement");
        let workdir = temp_root("implement-repo");
        fs::create_dir_all(&workdir).unwrap();
        let fake = Fake::new();
        fake.w().now = 1_000;
        let mut rig = Rig::open(
            &fake,
            Start::New {
                topic: "Review this repo".into(),
                workdir: Some(workdir.clone()),
                max_rounds: None,
                moderator: AgentKind::Claude.into(),
                panel: vec![AgentKind::Codex.into()],
                root: root.clone(),
            },
        );
        rig.step();
        let before = fake.0.borrow().spawned.len();
        rig.actions
            .send(Action::Implement {
                agent: AgentKind::Codex,
            })
            .unwrap();
        rig.step();
        let spawned = fake.0.borrow().spawned.clone();
        assert_eq!(spawned.len(), before + 1);
        let (placement, spec, _) = spawned.last().unwrap();
        assert_eq!(*placement, Placement::NewTab);
        assert_eq!(spec.cwd, workdir);
        assert!(spec.label.starts_with("implement · "), "{}", spec.label);
        let task = workdir
            .join(".noa/discussions")
            .join(rig.session().id.as_str())
            .join("implementation.md");
        assert_eq!(
            fs::read_to_string(&task).unwrap(),
            discussion::model::export::implementation_task(rig.session(), &task)
        );
        let command = spec.argv.join(" ");
        assert!(command.contains("exec codex \"$@\""), "{command}");
        assert!(
            spec.argv.last().unwrap().contains(task.to_str().unwrap()),
            "{:?}",
            spec.argv
        );
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&workdir);
    }

    #[test]
    fn implementing_without_a_working_directory_is_refused() {
        let root = temp_root("implement-none");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        let before = fake.0.borrow().spawned.len();
        rig.actions
            .send(Action::Implement {
                agent: AgentKind::Claude,
            })
            .unwrap();
        rig.step();
        assert_eq!(fake.0.borrow().spawned.len(), before);
        assert!(rig.notices().iter().any(|n| n.contains("作業ディレクトリ")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn focusing_a_participant_brings_its_pane_forward_once_it_has_one() {
        let root = temp_root("focus");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        // Before the first connection no pane exists: Noa is not asked.
        rig.actions
            .send(Action::Focus { part: p("codex") })
            .unwrap();
        rig.runner.step().unwrap();
        assert!(fake.0.borrow().focused.is_empty());
        assert!(
            rig.notices()
                .iter()
                .any(|n| n.contains("codex のペインはまだありません")),
        );

        rig.step();
        let codex = fake.pane_of("codex");
        let moderator = fake.pane_of("noa");
        rig.actions
            .send(Action::Focus { part: p("codex") })
            .unwrap();
        rig.actions
            .send(Action::Focus {
                part: p(discussion::model::types::MODERATOR_ID),
            })
            .unwrap();
        rig.step();
        assert_eq!(fake.0.borrow().focused, [codex, moderator]);

        // Someone not in the discussion: refused, nothing asked.
        rig.actions
            .send(Action::Focus { part: p("gemini") })
            .unwrap();
        rig.step();
        assert_eq!(fake.0.borrow().focused.len(), 2);
        assert!(
            rig.notices()
                .iter()
                .any(|n| n.contains("参加者ではありません"))
        );

        // With the link down: refused with why.
        fake.w().up = false;
        rig.step();
        rig.updates();
        rig.actions
            .send(Action::Focus { part: p("codex") })
            .unwrap();
        rig.step();
        assert_eq!(fake.0.borrow().focused.len(), 2);
        assert!(
            rig.notices()
                .iter()
                .any(|n| n.contains("Noa に接続していません"))
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn hold_continue_and_extend_reach_the_engine_and_refusals_come_back() {
        let root = temp_root("hold");
        let fake = Fake::new();
        fake.w().now = 1_000;
        let mut rig = Rig::open(
            &fake,
            Start::New {
                topic: "Pick a database".into(),
                workdir: None,
                max_rounds: Some(1),
                moderator: AgentKind::Claude.into(),
                panel: vec![AgentKind::Codex.into()],
                root: root.clone(),
            },
        );
        rig.step();
        rig.actions.send(Action::SetHold { hold: true }).unwrap();
        rig.actions.send(Action::Continue).unwrap();
        rig.actions.send(Action::Extend { by: 2 }).unwrap();
        rig.step();
        assert!(rig.session().hold);
        assert_eq!(rig.session().max_rounds, Some(3));
        assert_eq!(rig.session().user_max_rounds, Some(3));
        assert!(rig.notices().iter().any(|n| n.contains("still running")),);
        // Stored: a resume folds the same.
        let dir = rig.dir();
        rig.actions.send(Action::Stop).unwrap();
        assert!(!rig.runner.step().unwrap());
        drop(rig);
        let (_, session) = Store::open(&dir).unwrap();
        assert!(session.hold);
        assert_eq!(session.max_rounds, Some(3));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn each_agent_gets_its_task_the_way_its_cli_takes_a_first_prompt() {
        let argv = |kind| implement_argv(kind, "do it");
        assert_eq!(
            &argv(AgentKind::Claude)[4..],
            ["exec claude \"$@\"", "noa-implement", "do it"]
        );
        assert_eq!(
            &argv(AgentKind::Codex)[4..],
            ["exec codex \"$@\"", "noa-implement", "do it"]
        );
        assert_eq!(
            &argv(AgentKind::Agy)[4..],
            ["exec agy \"$@\"", "noa-implement", "-i", "do it"]
        );
        // The prompt is an argument of its own, never spliced into the script.
        assert_eq!(
            argv(AgentKind::Claude).last().map(String::as_str),
            Some("do it")
        );
    }

    #[test]
    fn a_brief_still_queued_when_the_discussion_ends_is_never_sent() {
        let root = temp_root("close-queued");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        // claude sits on a question, so its first brief waits in the queue.
        fake.report_blocked(101, "input", "question", "Which database?");
        for _ in 0..5 {
            rig.step();
        }
        assert_eq!(rig.submitted(101), 0);
        rig.actions.send(Action::Close).unwrap();
        rig.step();
        assert!(rig.session().closed);
        // The user answers the dialog: the agent is idle, and still nothing
        // goes in.
        fake.report(101, "input");
        for _ in 0..20 {
            rig.step();
        }
        assert!(fake.typed(101).is_empty(), "{:?}", fake.typed(101));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_reopened_pane_gets_the_whole_brief_not_the_pending_correction() {
        let root = temp_root("reopen-correction");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.until(40, |r| r.submitted(101) == 1);
        // A rejected reply; the correction waits behind a question dialog.
        fake.report_blocked(101, "input", "question", "Which database?");
        rig.step();
        let path = store::reply_path(&rig.dir(), 1, &p("claude"));
        fs::write(&path, "---\nissues: [Z]\n---\nbody").unwrap();
        rig.until(20, |r| r.receipt(1, "claude").rejected.is_some());
        // The pane closes before the correction goes out.
        fake.close(101);
        rig.until(40, |r| {
            r.fake
                .0
                .borrow()
                .spawned
                .iter()
                .filter(|s| s.1.label.starts_with("claude · "))
                .count()
                == 2
        });
        let new_pane = fake
            .0
            .borrow()
            .spawned
            .iter()
            .rev()
            .find(|s| s.1.label.starts_with("claude · "))
            .unwrap()
            .2;
        rig.until(40, |r| r.submitted(new_pane) == 1);
        let pasted: Vec<String> = fake
            .typed(new_pane)
            .into_iter()
            .filter(|(_, paste)| *paste)
            .map(|(text, _)| text)
            .collect();
        assert_eq!(pasted.len(), 1);
        assert!(pasted[0].contains(PANELIST_SECTIONS[0]), "{}", pasted[0]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn input_is_an_input_box_or_codexs_input_line() {
        assert!(has_input(FAKE_IDLE));
        // Claude Code's box, and Antigravity's as read from a real pane.
        assert!(has_input(
            "work\n────────────────────────────────────────\n❯ \n────────────────────────────────────────\n  ? for shortcuts"
        ));
        assert!(has_input(
            "────────────────────────────────────────\n> Auto mode: edits & safe commands auto-approved\n────────────────────────────────────────\n♊ Idle discussions"
        ));
        // Claude Code in a short pane, as read from a real one: no rule
        // under its input line.
        assert!(has_input(
            "  (ctrl+o to expand)\n\n──────────────────────────────────────\n❯ Try \"write a test for <filepath>\"\n"
        ));
        assert!(!has_input(
            "────────────────────────────────────────\n ❯ 1. Yes\n   2. No"
        ));
        // A rule alone, or a dialog under one rule, is not an input box.
        assert!(!has_input(
            "────────────────────────────────────────\n Loading…"
        ));
        assert!(!has_input(
            "────────────────────────────────────────\n Accessing workspace:\n ❯ No, exit\n   Yes, I trust this folder\n Enter to confirm"
        ));
        assert!(!has_input(
            "────────────────────────────────────────\n status\n────────────────────────────────────────"
        ));
        assert!(has_input(
            "work\n› Ask Codex to do anything\n\n  gpt · 100% left\n"
        ));
        assert!(!has_input(
            "Do you trust the files in this folder?\n› 1. Yes, proceed\n  2. No, exit\n"
        ));
        assert!(!has_input("> "));
        assert!(!has_input("Loading…\n"));
    }

    #[test]
    fn a_reply_file_is_read_only_once_it_stops_changing() {
        let root = temp_root("stable");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.until(40, |r| r.submitted(101) == 1 && r.submitted(102) == 1);
        let path = store::reply_path(&rig.dir(), 1, &p("claude"));
        // Read twice, 200 ms apart, while it is still being written.
        fs::write(&path, "---\nissues: [").unwrap();
        rig.step();
        rig.step();
        fs::write(&path, "---\nissues: []\n---\nPostgres, because of JSONB.").unwrap();
        rig.until(10, |r| r.receipt(1, "claude").replied.is_some());
        let receipt = rig.receipt(1, "claude");
        assert_eq!(receipt.rejected, None);
        assert_eq!(receipt.retries, 0);
        let reply = rig.session().messages.last().unwrap();
        assert_eq!(reply.body.trim(), "Postgres, because of JSONB.");
        let _ = fs::remove_dir_all(&root);
    }

    /// Why `start` is refused before the runner opens.
    fn refused(fake: &Fake, start: Start) -> String {
        let (_actions, action_rx) = mpsc::channel();
        let (update_tx, _updates) = mpsc::channel();
        match Runner::open(
            start,
            fake.clone(),
            fake.clone(),
            CONFIG,
            action_rx,
            update_tx,
            Rc::new(|| {}),
        ) {
            Ok(_) => panic!("opened"),
            Err(why) => why,
        }
    }

    /// A discussion stored under `root` that ran its one round: codex
    /// replied, and the moderator concluded `conclusion` early (`round_limit`
    /// 2) or in the last round (1).
    fn ran(root: &Path, id: &str, max_rounds: RoundNo) -> PathBuf {
        use discussion::engine::Setup;
        let dir = store::session_dir(root, &SessionId::new(id));
        let setup = Setup {
            topic: "Pick a database",
            workdir: None,
            max_rounds: Some(max_rounds),
            moderator: AgentKind::Claude.into(),
            panel: &[AgentKind::Codex.into()],
        };
        let (mut engine, out) =
            Engine::create_in(SessionId::new(id), setup, &dir, CONFIG, 1).unwrap();
        let mut log = out.batches;
        let mut feed = |engine: &mut Engine, input| {
            log.extend(engine.handle(input).unwrap().batches);
        };
        feed(&mut engine, Input::Start { now: 2 });
        for (part, content) in [
            ("codex", "SQLite."),
            ("noa", r#"{"summary":"one","conclusion":"SQLite."}"#),
        ] {
            let part = p(part);
            feed(
                &mut engine,
                Input::Sent {
                    round: 1,
                    part: part.clone(),
                    at: 3,
                },
            );
            feed(
                &mut engine,
                Input::AgentStateChanged {
                    part: part.clone(),
                    state: AgentState::Running,
                    at: 3,
                },
            );
            feed(
                &mut engine,
                Input::ReplyFileWritten {
                    round: 1,
                    part,
                    content: content.into(),
                    at: 4,
                },
            );
        }
        assert!(engine.session().closed);
        let mut store = Store::create(&dir, &log[0]).unwrap();
        store.append_all(&log[1..]).unwrap();
        dir
    }

    #[test]
    fn a_discussion_a_runner_holds_is_busy_for_a_second_resume() {
        let root = temp_root("busy");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        let dir = rig.dir();
        let why = refused(&fake, Start::Resume { dir: dir.clone() });
        assert!(why.contains("別のウィンドウで実行中"), "{why}");
        let spawned = fake.0.borrow().spawned.len();
        // Once the first runner has ended, it resumes.
        rig.actions.send(Action::Stop).unwrap();
        assert!(!rig.runner.step().unwrap());
        drop(rig);
        let mut rig = Rig::open(&fake, Start::Resume { dir });
        rig.step();
        assert_eq!(fake.0.borrow().spawned.len(), spawned, "panes taken back");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_resume_refuses_reply_files_outside_the_session() {
        let root = temp_root("foreign");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        let dir = rig.dir();
        let codex = store::reply_path(&dir, 1, &p("codex"));
        rig.actions.send(Action::Stop).unwrap();
        assert!(!rig.runner.step().unwrap());
        drop(rig);

        // The log names a file elsewhere for codex's reply.
        let outside = root.join("precious.txt");
        fs::write(&outside, "keep me").unwrap();
        let events = dir.join(store::EVENTS_FILE);
        let log = fs::read_to_string(&events).unwrap();
        let quoted = |p: &Path| serde_json::to_string(&p.display().to_string()).unwrap();
        fs::write(&events, log.replace(&quoted(&codex), &quoted(&outside))).unwrap();
        let why = refused(&fake, Start::Resume { dir: dir.clone() });
        assert!(why.contains("この議論の場所にありません"), "{why}");
        assert_eq!(fs::read_to_string(&outside).unwrap(), "keep me");
        fs::write(&events, log).unwrap();

        // The round directory is a link to somewhere else.
        let elsewhere = root.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let round = store::round_dir(&dir, 1);
        fs::remove_dir_all(&round).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &round).unwrap();
        let why = refused(&fake, Start::Resume { dir });
        assert!(why.contains("シンボリックリンク"), "{why}");
        assert!(fake.0.borrow().spawned.len() == 3, "no pane started");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_resume_whose_workdir_moved_is_refused_and_makes_nothing_there() {
        let root = temp_root("moved");
        let workdir = root.join("repo");
        fs::create_dir_all(&workdir).unwrap();
        let fake = Fake::new();
        fake.w().now = 1_000;
        let mut rig = Rig::open(
            &fake,
            Start::New {
                topic: "Pick a database".into(),
                workdir: Some(workdir.clone()),
                max_rounds: None,
                moderator: AgentKind::Claude.into(),
                panel: vec![AgentKind::Codex.into()],
                root: root.join("discussions"),
            },
        );
        rig.step();
        let dir = rig.dir();
        rig.actions.send(Action::Stop).unwrap();
        assert!(!rig.runner.step().unwrap());
        drop(rig);
        fs::rename(&workdir, root.join("repo-moved")).unwrap();
        let why = refused(&fake, Start::Resume { dir });
        assert!(why.contains("作業ディレクトリ"), "{why}");
        assert!(!workdir.exists(), "nothing made where the repo was");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_ended_discussion_is_not_resumed_and_an_extension_comes_before_any_pane() {
        let root = temp_root("ended");
        let fake = Fake::new();
        fake.w().now = 1_000;
        let concluded_early = ran(&root, "disc-early", 3);
        let ran_out = ran(&root, "disc-out", 1);
        for dir in [&concluded_early, &ran_out] {
            let why = refused(&fake, Start::Resume { dir: dir.clone() });
            assert!(why.contains("終了しています"), "{why}");
        }
        let why = refused(
            &fake,
            Start::Extend {
                dir: concluded_early,
                by: 1,
            },
        );
        assert!(why.contains("ラウンドを追加できません"), "{why}");
        assert!(fake.0.borrow().spawned.is_empty(), "no pane for a refusal");

        let mut rig = Rig::open(
            &fake,
            Start::Extend {
                dir: ran_out.clone(),
                by: 2,
            },
        );
        // Extended and stored before the first connection.
        assert!(fake.0.borrow().spawned.is_empty());
        assert_eq!(rig.session().max_rounds, Some(3));
        assert_eq!(store::load(&ran_out).unwrap().max_rounds, Some(3));
        rig.until(40, |r| r.fake.0.borrow().spawned.len() == 2);
        let codex = fake.pane_of("codex");
        rig.until(40, |r| r.submitted(codex) == 1);
        assert_eq!(rig.session().rounds.len(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_resumed_session_takes_its_panes_back_and_sends_what_was_left() {
        let root = temp_root("resume");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        // codex's pane never takes a paste before the widget quits.
        fake.w().panes.get_mut(&102).unwrap().paste = false;
        rig.until(40, |r| r.submitted(101) == 1);
        let dir = rig.dir();
        rig.actions.send(Action::Stop).unwrap();
        assert!(!rig.runner.step().unwrap());
        drop(rig);

        fake.w().panes.get_mut(&102).unwrap().paste = true;
        let mut rig = Rig::open(&fake, Start::Resume { dir });
        rig.until(40, |r| r.submitted(102) == 1);
        assert_eq!(fake.0.borrow().spawned.len(), 3, "panes were respawned");
        for _ in 0..10 {
            rig.step();
        }
        assert_eq!(rig.submitted(101), 1, "a sent brief was sent again");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_resumed_session_whose_log_holds_a_bad_model_is_refused() {
        let root = temp_root("resume-bad-setup");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        let dir = rig.dir();
        rig.actions.send(Action::Stop).unwrap();
        assert!(!rig.runner.step().unwrap());
        drop(rig);
        // The model name in the log is written into the login shell's script.
        let log = dir.join(store::EVENTS_FILE);
        let text = fs::read_to_string(&log).unwrap();
        let tampered = text.replacen(
            r#""role":"moderator""#,
            r#""role":"moderator","setup":{"model":"opus; touch pwned"}"#,
            1,
        );
        assert_ne!(tampered, text);
        fs::write(&log, tampered).unwrap();
        // The moderator's pane is gone, so resuming starts it again.
        fake.w().panes.remove(&100);

        let (_actions, action_rx) = mpsc::channel();
        let (update_tx, _updates) = mpsc::channel();
        let opened = Runner::open(
            Start::Resume { dir },
            fake.clone(),
            fake.clone(),
            CONFIG,
            action_rx,
            update_tx,
            Rc::new(|| {}),
        );
        let before = fake.0.borrow().spawned.len();
        match opened {
            Err(why) => assert!(why.contains("opus; touch pwned"), "{why}"),
            Ok(mut runner) => {
                for _ in 0..20 {
                    let _ = runner.step();
                }
                let argv: Vec<_> = fake.0.borrow().spawned[before..]
                    .iter()
                    .map(|(_, spec, _)| spec.argv.last().cloned())
                    .collect();
                panic!("resumed and started {argv:?}");
            }
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pane_with_the_old_id_but_another_label_is_not_taken_back() {
        let root = temp_root("relabel");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        let dir = rig.dir();
        drop(rig);
        // Noa restarted: pane 101 is now someone else's shell.
        fake.w().panes.get_mut(&101).unwrap().label = "zsh".into();
        let mut rig = Rig::open(&fake, Start::Resume { dir });
        rig.step();
        let spawned = fake.0.borrow().spawned.clone();
        assert_eq!(spawned.len(), 4);
        assert!(spawned[3].1.label.starts_with("claude · "));
        assert_eq!(rig.session().panelists[0].pane_id, Some(103));
        assert_eq!(fake.pane_of("zsh"), 101);
        rig.until(40, |r| r.submitted(103) == 1);
        assert!(fake.typed(101).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn user_actions_reach_the_engine_and_refusals_come_back_as_notices() {
        let root = temp_root("actions");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        rig.updates();
        rig.actions
            .send(Action::Intervene {
                to: vec![p("nobody")],
                body: "hi".into(),
            })
            .unwrap();
        rig.actions
            .send(Action::Intervene {
                to: vec![],
                body: "Consider cost.".into(),
            })
            .unwrap();
        rig.step();
        let updates = rig.updates();
        assert!(updates.contains(&Update::Notice("no participant nobody".into())));
        assert!(
            rig.session()
                .messages
                .iter()
                .any(|m| m.body == "Consider cost.")
        );
        assert!(
            updates
                .iter()
                .any(|u| matches!(u, Update::Session(s) if s.messages.iter().any(|m| m.body == "Consider cost.")))
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn stop_ends_the_loop_and_reports_it() {
        let root = temp_root("stop");
        let fake = Fake::new();
        fake.w().now = 5;
        let (actions, action_rx) = mpsc::channel();
        let (update_tx, updates) = mpsc::channel();
        actions.send(Action::Stop).unwrap();
        run_session(
            Start::New {
                topic: "t".into(),
                workdir: None,
                max_rounds: None,
                moderator: AgentKind::Codex.into(),
                panel: vec![AgentKind::Agy.into()],
                root: root.clone(),
            },
            fake.clone(),
            fake.clone(),
            CONFIG,
            action_rx,
            update_tx,
            || {},
        );
        let updates: Vec<Update> = updates.try_iter().collect();
        assert!(matches!(updates.first(), Some(Update::Session(_))));
        assert_eq!(updates.last(), Some(&Update::Ended(Ok(()))));

        // A session that cannot be opened ends at once with why.
        let (_actions, action_rx) = mpsc::channel();
        let (update_tx, updates) = mpsc::channel();
        run_session(
            Start::Resume {
                dir: root.join("missing"),
            },
            fake.clone(),
            fake.clone(),
            CONFIG,
            action_rx,
            update_tx,
            || {},
        );
        let updates: Vec<Update> = updates.try_iter().collect();
        assert!(matches!(updates.as_slice(), [Update::Ended(Err(_))]));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn agents_start_through_the_login_shell() {
        let argv = agent_argv(AgentKind::Agy, &AgentSetup::default());
        assert!(argv[0].starts_with('/'));
        assert_eq!(&argv[1..], ["-l", "-i", "-c", "exec agy"]);
        let setup = AgentSetup {
            model: Some("gpt-6.2-sol".into()),
            effort: Some("high".into()),
        };
        assert_eq!(
            agent_argv(AgentKind::Codex, &setup)[4],
            "exec codex -m gpt-6.2-sol -c model_reasoning_effort=high"
        );
        assert_eq!(
            agent_argv(AgentKind::Claude, &setup)[4],
            "exec claude --model gpt-6.2-sol --effort high"
        );
        assert_eq!(
            &implement_argv(AgentKind::Agy, "go")[4..],
            ["exec agy \"$@\"", "noa-implement", "-i", "go"]
        );
        assert_eq!(
            agent_state(Some("permission")),
            Some(AgentState::Permission)
        );
        assert_eq!(agent_state(Some("weird")), None);
        assert_eq!(agent_state(None), None);
    }

    /// The script an agent is started with, run by a shell that is not
    /// POSIX (csh here; fish has no `$@` either), with `echo` standing in
    /// for the agent: it must still start, with every flag.
    fn run_under_csh(argv: &[String], kind: AgentKind) -> Option<String> {
        let csh = std::path::Path::new("/bin/csh");
        if !csh.exists() {
            return None;
        }
        let script = argv[4].replacen(&format!("exec {}", kind.command()), "exec /bin/echo", 1);
        let out = std::process::Command::new(csh)
            .arg("-f")
            .arg("-c")
            .arg(&script)
            .args(&argv[5..])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{script:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Some(String::from_utf8(out.stdout).unwrap().trim().to_owned())
    }

    #[test]
    fn agents_start_under_a_login_shell_that_is_not_posix() {
        let setup = AgentSetup {
            model: Some("opus".into()),
            effort: Some("high".into()),
        };
        let Some(out) = run_under_csh(&agent_argv(AgentKind::Claude, &setup), AgentKind::Claude)
        else {
            return;
        };
        assert_eq!(out, "--model opus --effort high");
        let codex = run_under_csh(&agent_argv(AgentKind::Codex, &setup), AgentKind::Codex);
        assert_eq!(codex.unwrap(), "-m opus -c model_reasoning_effort=high");
        let plain = agent_argv(AgentKind::Agy, &AgentSetup::default());
        assert_eq!(run_under_csh(&plain, AgentKind::Agy).unwrap(), "");
    }

    #[test]
    #[ignore = "perf probe: counts IPC calls per tick"]
    fn probe_ipc_counts_per_tick() {
        let root = temp_root("ipc-probe");
        let fake = Fake::new();
        let mut rig = Rig::new(&fake, &root);
        rig.step();
        fake.report(101, "input");
        fake.report(102, "input");
        rig.until(10, |r| r.submitted(101) == 1 && r.submitted(102) == 1);
        let (startup_panes, startup_paste, startup_screen, startup_changes) = {
            let w = fake.0.borrow();
            (w.rpc_panes, w.rpc_paste_ready, w.rpc_screen, w.rpc_changes)
        };
        for _ in 0..100 {
            rig.step();
        }
        let (steady_panes, steady_paste, steady_screen, steady_changes) = {
            let w = fake.0.borrow();
            (
                w.rpc_panes - startup_panes,
                w.rpc_paste_ready - startup_paste,
                w.rpc_screen - startup_screen,
                w.rpc_changes - startup_changes,
            )
        };
        println!(
            "discussion runner IPC counts:\n  \
             startup (2 idle panelists): panes={startup_panes} paste_ready={startup_paste} screen={startup_screen} changes={startup_changes}\n  \
             steady-state (100 ticks):   panes={steady_panes} paste_ready={steady_paste} screen={steady_screen} changes={steady_changes}"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
