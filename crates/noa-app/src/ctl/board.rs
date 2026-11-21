//! `Noa +ctl board`: a terminal UI that shows an orchestrator's workers and
//! lets a person act on them (agent-orchestration UI phase). It runs in an
//! ordinary pane and reads the same server `+ctl` does, so it needs no new
//! pane kind: the model, key handling and drawing below are pure, and only
//! [`run`] touches the terminal and the connection.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::time::{Duration, Instant};

use noa_ipc::{Client, ClientError, Panel, TextSource};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{EXIT_OK, Failure, HOLD_BANNER, SUBMIT_DELAY};

/// How often the pane list is read again.
const LIST_EVERY: Duration = Duration::from_millis(500);
/// After a listing fails, the first wait before trying again, the most it
/// grows to, and how long failing without a break ends the board.
const RETRY_FIRST: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(4);
const GIVE_UP_AFTER: Duration = Duration::from_secs(30);
/// How soon a second digit must follow the first to make a two-digit
/// worker number.
const SECOND_DIGIT: Duration = Duration::from_millis(1000);
/// How long a working worker's screen stays the same before the board says
/// it shows no output.
const QUIET_AFTER: Duration = Duration::from_secs(120);
/// How often the workers' screens are read again.
const SCREEN_EVERY: Duration = Duration::from_secs(1);
/// Reports kept per worker.
const HISTORY_CAP: usize = 50;
/// Screen lines kept per worker.
const SCREEN_CAP: usize = 200;
/// From this width the selected worker's preview sits right of the list;
/// from [`STACKED_COLS`], under it; narrower panes show the list alone
/// (D15).
const WIDE_COLS: usize = 110;
const STACKED_COLS: usize = 70;
/// The fewest rows a pane needs to give a preview under the list.
const STACKED_ROWS: usize = 18;
/// The most a parent's name takes before its worker's name in the queue.
const CRUMB_WIDTH: usize = 10;
/// The longest gap between two reads of a screen for a change between them
/// to be dated: past it, when the screen moved is not known.
const MOVED_GAP: Duration = Duration::from_secs(3);
/// Closed workers kept at the end of the list.
const GONE_CAP: usize = 5;
/// Where a row's name starts, which its preview lines line up with.
const ROW_INDENT: usize = 6;
/// How long a lone ESC waits for the rest of a sequence before it counts
/// as the Esc key.
const ESC_WAIT: Duration = Duration::from_millis(50);
/// The `float.toggle-focus` binding, which takes focus from an open worker
/// back to the board (agent-orchestration D10).
const BACK_TO_BOARD: &str = if cfg!(target_os = "macos") {
    "⌘⇧I"
} else {
    "^⇧I"
};
/// JSON-RPC code for a pane whose process ended or that closed.
const RPC_PANE_CLOSED: i64 = -32004;
/// The smallest pane the board draws in.
const MIN_COLS: usize = 24;
const MIN_ROWS: usize = 6;

// ---------------------------------------------------------------------------
// Model

/// The language the board speaks: Japanese where the person's locale is
/// Japanese, English anywhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lang {
    Ja,
    En,
}

impl Lang {
    /// The locale variables in POSIX order (`LC_ALL`, `LC_MESSAGES`,
    /// `LANG`: the first one set wins), else the system's preferred language
    /// (`system`): a Noa started from the Dock has no locale variables.
    pub(crate) fn from_locale(
        var: impl Fn(&str) -> Option<String>,
        system: impl FnOnce() -> Option<String>,
    ) -> Self {
        let tag = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|key| var(key).filter(|value| !value.is_empty()))
            .or_else(system);
        match tag {
            Some(tag) if tag.starts_with("ja") => Self::Ja,
            _ => Self::En,
        }
    }

    pub(crate) fn detect() -> Self {
        Self::from_locale(|key| std::env::var(key).ok(), system_language)
    }

    fn pick<T>(self, ja: T, en: T) -> T {
        match self {
            Self::Ja => ja,
            Self::En => en,
        }
    }
}

/// The first of the person's preferred languages, as a BCP 47 tag (`ja-JP`).
#[cfg(target_os = "macos")]
fn system_language() -> Option<String> {
    objc2_foundation::NSLocale::preferredLanguages()
        .firstObject()
        .map(|tag| tag.to_string())
}

#[cfg(not(target_os = "macos"))]
fn system_language() -> Option<String> {
    None
}

/// What a worker is doing, as far as its panel says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Permission,
    Input,
    Error,
    /// An unread notification, with no blocking report.
    Attention,
    Running,
    Finished,
    Exited(Option<i32>),
    /// Alive, but it never reported a state.
    Quiet,
}

/// What a worker's state asks of a person, in the order the header counts
/// them; the list places workers by [`Zone`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Group {
    Needs,
    Working,
    Done,
    Exited,
}

impl Status {
    pub(crate) fn of(panel: &Panel) -> Self {
        if let Some(exit) = &panel.exit {
            return Self::Exited(exit.code);
        }
        match panel
            .agent
            .as_ref()
            .and_then(|agent| agent.state.as_deref())
        {
            Some("permission") => Self::Permission,
            Some("input") => Self::Input,
            Some("error") => Self::Error,
            // A finished turn usually raises a notification too; it is done,
            // not waiting on anyone.
            Some("finished") => Self::Finished,
            _ if panel.attention => Self::Attention,
            Some("running") => Self::Running,
            _ => Self::Quiet,
        }
    }

    /// A process that exited with a non-zero code: it failed, and someone
    /// should look at it.
    pub(crate) fn failed(self) -> bool {
        matches!(self, Self::Exited(Some(code)) if code != 0)
    }

    pub(crate) fn group(self) -> Group {
        match self {
            Self::Permission | Self::Input | Self::Error | Self::Attention => Group::Needs,
            status if status.failed() => Group::Needs,
            Self::Running | Self::Quiet => Group::Working,
            Self::Finished => Group::Done,
            Self::Exited(_) => Group::Exited,
        }
    }

    fn label(self, lang: Lang) -> String {
        match self {
            Self::Permission => lang.pick("承認待ち", "approval").to_string(),
            Self::Input => lang.pick("入力待ち", "question").to_string(),
            Self::Error => lang.pick("エラー", "error").to_string(),
            Self::Attention => lang.pick("通知あり", "notified").to_string(),
            Self::Running => lang.pick("作業中", "working").to_string(),
            Self::Finished => lang.pick("完了", "done").to_string(),
            Self::Exited(Some(code)) if code != 0 => {
                format!("{} {code}", lang.pick("失敗", "failed"))
            }
            Self::Exited(Some(code)) => format!("{} {code}", lang.pick("終了", "exited")),
            Self::Exited(None) => lang.pick("終了", "exited").to_string(),
            Self::Quiet => lang.pick("報告なし", "no report").to_string(),
        }
    }

    /// The label for a narrow pane: the states that share a colour keep a
    /// word each, so they stay apart without the full label.
    fn short_label(self, lang: Lang) -> String {
        match self {
            Self::Permission => lang.pick("承認", "appr").to_string(),
            Self::Input => lang.pick("入力", "ask").to_string(),
            Self::Attention => lang.pick("通知", "note").to_string(),
            Self::Error => lang.pick("エラー", "error").to_string(),
            status if status.failed() => lang.pick("失敗", "fail").to_string(),
            status => status.label(lang),
        }
    }

    /// The mark drawn before a worker's name, so a state reads without its
    /// colour (D15).
    fn mark(self) -> &'static str {
        match self {
            Self::Permission | Self::Attention => "!",
            Self::Input => "?",
            Self::Error => "×",
            status if status.failed() => "×",
            Self::Running => "●",
            Self::Finished => "✓",
            Self::Exited(_) | Self::Quiet => "·",
        }
    }

    fn color(self) -> Rgb {
        match self {
            Self::Permission | Self::Input | Self::Attention => ORANGE,
            Self::Error => RED,
            status if status.failed() => RED,
            Self::Running => BLUE,
            Self::Finished => GREEN,
            Self::Exited(_) | Self::Quiet => GREY,
        }
    }
}

impl Group {
    const ALL: [Self; 4] = [Self::Needs, Self::Working, Self::Done, Self::Exited];

    fn title(self, lang: Lang) -> &'static str {
        match self {
            Self::Needs => lang.pick("要対応", "needs you"),
            Self::Working => lang.pick("作業中", "working"),
            Self::Done => lang.pick("完了", "done"),
            Self::Exited => lang.pick("終了", "exited"),
        }
    }

    fn mark(self) -> &'static str {
        match self {
            Self::Needs => "!",
            Self::Working => "●",
            Self::Done => "✓",
            Self::Exited => "·",
        }
    }

    fn color(self) -> Rgb {
        match self {
            Self::Needs => ORANGE,
            Self::Working => BLUE,
            Self::Done => GREEN,
            Self::Exited => GREY,
        }
    }
}

/// One report seen on a worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) seq: u64,
    pub(crate) status: Status,
    pub(crate) detail: String,
    /// When the board saw it arrive; `None` for the state found at start,
    /// whose time is unknown.
    pub(crate) at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum View {
    List,
    Detail(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    /// Typing a message to the pane.
    Send(u64, String),
    /// Waiting for `y` to close the pane.
    ConfirmClose(u64),
    /// Waiting for `q` (or ^C) again to end the board, which hands every
    /// worker back to the orchestrator and cannot be undone from here.
    ConfirmQuit,
    /// The worker a send line or close prompt was open for went away (its
    /// name): keys are dropped until Esc, so none meant for it acts on
    /// another worker.
    TargetGone(String),
}

/// What a key asks the outside world to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Focus(u64),
    Send(u64, String),
    Close(u64),
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Char(char),
    Enter,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Backspace,
    CtrlC,
    /// Clear the send line.
    CtrlU,
    /// Delete the last word of the send line.
    CtrlW,
    Paste(String),
}

/// A one-line result shown above the footer; `warn` when it needs a look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) text: String,
    pub(crate) warn: bool,
}

impl Notice {
    fn ok(text: String) -> Self {
        Self { text, warn: false }
    }

    fn warn(text: String) -> Self {
        Self { text, warn: true }
    }
}

/// Where a worker sits in the tree the list draws (agent-orchestration
/// D13): a worker of the orchestrator, or one of that worker's own workers,
/// drawn right under it. Nesting goes one level deep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Branch {
    /// A worker of the orchestrator, with how many workers of its own it
    /// has (0: a plain row).
    Top { kids: usize },
    /// A worker's worker, and whether it is the last of its parent's.
    Kid { parent: u64, last: bool },
}

/// Where the list draws a subtree (D15): what needs someone is lifted into
/// the queue at the top, whatever subtree it is in; below it, the tree of
/// work under way, then the folded section of finished and exited work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Zone {
    /// A worker of the orchestrator that needs someone and has no workers
    /// of its own: it is drawn in the queue only.
    Queue,
    /// A subtree with work under way, or a parent that needs someone over
    /// its own workers.
    Background,
    /// A subtree whose every worker is finished or exited.
    Settled,
}

/// A worker in the tree, in the order the list draws it.
#[derive(Clone, Copy)]
struct Node<'a> {
    panel: &'a Panel,
    branch: Branch,
    /// Where its subtree is drawn (a worker's own workers share it).
    zone: Zone,
}

/// A row of the list: a worker in the queue or in the tree. A worker that
/// needs someone is drawn in both; its tree row is an `echo`, pointing up
/// at the queue, and is never selected.
#[derive(Clone, Copy)]
struct Entry<'a> {
    node: Node<'a>,
    queued: bool,
    echo: bool,
}

/// How urgent a worker is (the least is the most urgent): its section, then within
/// 要対応 what holds work up (an approval, an error, a failed command)
/// before a question or a notification, then whoever has waited longest.
type Urgency = (Group, u8, Option<Instant>);

/// A worker that was closed while the board watched: what it last said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Gone {
    name: String,
    status: Status,
    detail: String,
    at: Instant,
}

pub(crate) struct Board {
    /// The orchestrator whose workers are shown; `None` shows every worker.
    of: Option<u64>,
    /// The board's own pane, never listed.
    me: Option<u64>,
    panels: Vec<Panel>,
    /// Whether a listing was taken yet: reports in the first one predate
    /// the board, later ones arrived while it watched.
    started: bool,
    screens: HashMap<u64, Vec<String>>,
    /// When each worker's screen last changed, and when it was last read.
    output: HashMap<u64, (Instant, Instant)>,
    history: HashMap<u64, Vec<Report>>,
    /// Each worker's status and when the board saw it begin (`None`: it
    /// was already so when the board opened).
    since: HashMap<u64, (Status, Option<Instant>)>,
    /// Each worker's number, given once when it is first listed and kept
    /// while it lives, so `3` means the same worker however the list is
    /// sorted. A closed worker's number goes to the next one that appears.
    numbers: HashMap<u64, usize>,
    /// The digit just typed and when, so a second digit soon after picks
    /// a two-digit number (a board holds up to 20 workers, D12).
    digit: Option<(usize, Instant)>,
    selected: Option<u64>,
    pub(crate) view: View,
    pub(crate) mode: Mode,
    /// A one-line result of the last action.
    pub(crate) message: Option<Notice>,
    /// Since when the pane list could not be read; what is shown is the
    /// last listing that worked.
    pub(crate) offline: Option<Instant>,
    /// Workers closed since the board opened, newest first.
    gone: Vec<Gone>,
    /// The key help is shown over the current view.
    pub(crate) help: bool,
    /// Lines the help is scrolled, and how far it can go (set as it is
    /// drawn, which is when the pane's height is known).
    help_scroll: usize,
    help_reach: Cell<usize>,
    /// Lines the list is scrolled past the selected row: `j` on the last
    /// worker goes on through the closed section, `k` comes back first.
    past: usize,
    /// Workers whose own workers are folded away under their row (`Space`);
    /// every subtree starts open.
    collapsed: HashSet<u64>,
    /// Whether the finished and exited workers are listed, not only counted
    /// (D15); folded at first.
    settled_open: bool,
    /// When each worker's screen was last seen to change, since the board
    /// first read it (the first read says nothing about when it last moved).
    moved: HashMap<u64, Instant>,
    /// When the pane list was last read.
    synced: Option<Instant>,
    /// Whether the orchestrator's pane was in a listing, and whether it is
    /// missing from the latest one since.
    of_seen: bool,
    of_gone: bool,
    /// The language of every label (see [`Lang::detect`]).
    pub(crate) lang: Lang,
}

impl Board {
    pub(crate) fn new(of: Option<u64>, me: Option<u64>) -> Self {
        Self {
            of,
            me,
            panels: Vec::new(),
            started: false,
            screens: HashMap::new(),
            output: HashMap::new(),
            history: HashMap::new(),
            since: HashMap::new(),
            numbers: HashMap::new(),
            digit: None,
            selected: None,
            view: View::List,
            mode: Mode::Normal,
            message: None,
            offline: None,
            gone: Vec::new(),
            help: false,
            help_scroll: 0,
            help_reach: Cell::new(0),
            past: 0,
            collapsed: HashSet::new(),
            settled_open: false,
            moved: HashMap::new(),
            synced: None,
            of_seen: false,
            of_gone: false,
            lang: Lang::Ja,
        }
    }

    /// The last thing `panel` said: for a failed process its last line of
    /// output; else its report's detail, else the newest detail it gave in
    /// the same state (a detail from an earlier state, such as the last
    /// turn's "finished" text on a worker running again, would mislead),
    /// else that its process ended.
    fn last_word(&self, panel: &Panel) -> String {
        let pane = panel.pane_id.0;
        let status = Status::of(panel);
        if status.failed()
            && let Some(line) = self.last_output(pane)
        {
            return line.to_string();
        }
        panel
            .agent
            .as_ref()
            .and_then(|agent| agent.detail.clone())
            .filter(|detail| !detail.is_empty())
            .or_else(|| {
                self.history
                    .get(&pane)?
                    .iter()
                    .take_while(|report| report.status == status)
                    .map(|report| report.detail.clone())
                    .find(|detail| !detail.is_empty())
            })
            .or_else(|| {
                panel.exit.as_ref().map(|_| {
                    self.lang
                        .pick("プロセスが終了しました", "its process exited")
                        .to_string()
                })
            })
            .unwrap_or_default()
    }

    /// The last line of `pane`'s screen with text on it, rules aside.
    fn last_screen_line(&self, pane: u64) -> Option<&str> {
        self.screens
            .get(&pane)?
            .iter()
            .map(|line| line.trim())
            .rfind(|line| !line.is_empty() && !is_rule(line))
    }

    /// The last non-empty line of the work on `pane`'s screen.
    fn last_output(&self, pane: u64) -> Option<&str> {
        content_lines(self.screens.get(&pane)?)
            .iter()
            .map(|line| line.trim())
            .rfind(|line| !line.is_empty())
    }

    /// Take a fresh pane listing. A report count that moved is kept in the
    /// worker's history; the first listing records the state found as is.
    pub(crate) fn update(&mut self, panels: Vec<Panel>, now: Instant) {
        let first = !self.started;
        self.started = true;
        self.synced = Some(now);
        let before: Vec<(u64, Gone)> = self
            .workers()
            .iter()
            .map(|panel| {
                let gone = Gone {
                    name: worker_name(panel),
                    status: Status::of(panel),
                    detail: self.last_word(panel),
                    at: now,
                };
                (panel.pane_id.0, gone)
            })
            .collect();
        let target = match &self.mode {
            Mode::Send(pane, _) | Mode::ConfirmClose(pane) => Some((*pane, self.name(*pane))),
            _ => None,
        };
        self.panels = panels;
        if let Some(of) = self.of {
            let listed = self.panels.iter().any(|panel| panel.pane_id.0 == of);
            self.of_seen |= listed;
            self.of_gone = self.of_seen && !listed;
        }
        let workers: Vec<(u64, Option<u64>, Status, String)> = self
            .workers()
            .iter()
            .map(|panel| {
                let agent = panel.agent.as_ref();
                (
                    panel.pane_id.0,
                    // An agent known only by its process name has never
                    // reported: no history for it.
                    agent
                        .filter(|agent| agent.seq != 0 || agent.state.is_some())
                        .map(|agent| agent.seq),
                    Status::of(panel),
                    agent
                        .and_then(|agent| agent.detail.clone())
                        .unwrap_or_default(),
                )
            })
            .collect();
        for (pane, seq, status, detail) in workers.iter().cloned() {
            match self.since.get(&pane) {
                Some((seen, _)) if *seen == status => {}
                _ => {
                    let at = (!first).then_some(now);
                    self.since.insert(pane, (status, at));
                }
            }
            let Some(seq) = seq else { continue };
            let history = self.history.entry(pane).or_default();
            let newest = history.first().map(|report| report.seq);
            if newest.is_none_or(|newest| seq > newest) {
                let at = (!first).then_some(now);
                history.insert(
                    0,
                    Report {
                        seq,
                        status,
                        detail,
                        at,
                    },
                );
                history.truncate(HISTORY_CAP);
            }
        }
        let mut live: Vec<u64> = self.listed().map(|panel| panel.pane_id.0).collect();
        live.sort_unstable();
        self.numbers.retain(|pane, _| live.contains(pane));
        // New workers in spawn order (pane ids grow), each the lowest free
        // number.
        for &pane in &live {
            if !self.numbers.contains_key(&pane) {
                let taken: std::collections::HashSet<usize> =
                    self.numbers.values().copied().collect();
                let mut number = 1;
                while taken.contains(&number) {
                    number += 1;
                }
                self.numbers.insert(pane, number);
            }
        }
        let live: Vec<u64> = self.workers().iter().map(|panel| panel.pane_id.0).collect();
        for (pane, gone) in before {
            if !live.contains(&pane) {
                self.gone.insert(0, gone);
            }
        }
        self.gone.truncate(GONE_CAP);
        self.history.retain(|pane, _| live.contains(pane));
        self.since.retain(|pane, _| live.contains(pane));
        self.screens.retain(|pane, _| live.contains(pane));
        self.output.retain(|pane, _| live.contains(pane));
        self.moved.retain(|pane, _| live.contains(pane));
        // Only the work under way folds: a subtree that finished is listed
        // whole, and comes back open if it goes back to work.
        let at_work: HashSet<u64> = self
            .tree()
            .iter()
            .filter(|node| node.zone == Zone::Background)
            .map(|node| node.panel.pane_id.0)
            .collect();
        self.collapsed.retain(|pane| at_work.contains(pane));
        if self.selected.is_none_or(|pane| !live.contains(&pane)) {
            self.past = 0;
            self.selected = self.picks().first().copied();
        }
        // A selection that left the queue (it went back to work) is shown
        // where it went: its subtree opens.
        let hidden_parent = self.selected.and_then(|pane| {
            self.tree()
                .iter()
                .find(|node| node.panel.pane_id.0 == pane)
                .filter(|node| self.under_folded(node))
                .filter(|node| Status::of(node.panel).group() != Group::Needs)
                .and_then(|node| match node.branch {
                    Branch::Kid { parent, .. } => Some(parent),
                    Branch::Top { .. } => None,
                })
        });
        if let Some(parent) = hidden_parent {
            self.collapsed.remove(&parent);
        }
        if let View::Detail(pane) = self.view
            && !live.contains(&pane)
        {
            self.view = View::List;
        }
        // The keys typed next were meant for that worker: none may act on
        // the one selected now until the person has seen it went (Esc).
        if let Some((pane, name)) = target
            && !live.contains(&pane)
        {
            self.mode = Mode::TargetGone(name);
        }
    }

    #[cfg(test)]
    pub(crate) fn set_screen(&mut self, pane: u64, text: &str) {
        self.set_screen_at(pane, text, Instant::now());
    }

    /// The last lines of a worker's screen (trailing blank lines dropped),
    /// read at `now`.
    pub(crate) fn set_screen_at(&mut self, pane: u64, text: &str, now: Instant) {
        let mut lines: Vec<String> = text
            .lines()
            .map(|line| line.trim_end().to_string())
            .collect();
        // Noa's own hold banner under an exited command is not its output.
        while lines
            .last()
            .is_some_and(|line| line.is_empty() || line.starts_with(HOLD_BANNER))
        {
            lines.pop();
        }
        let skip = lines.len().saturating_sub(SCREEN_CAP);
        let lines = lines.split_off(skip);
        // Only the work counts: an agent's input box and status line can
        // tick (a clock, a token count) while nothing happens.
        let changed = self
            .screens
            .get(&pane)
            .is_none_or(|old| content_lines(old) != content_lines(&lines));
        // A change is dated only between two reads close together: one
        // after a gap (the detail was open, or the worker was not at work)
        // happened some time in it.
        // ...and both in the current state: one read before it began says
        // nothing about output since.
        let began = self.since.get(&pane).and_then(|(_, at)| *at);
        let read_just_before = self.output.get(&pane).is_some_and(|&(_, checked)| {
            now.saturating_duration_since(checked) <= MOVED_GAP
                && began.is_none_or(|began| checked >= began)
        });
        if changed && self.screens.contains_key(&pane) {
            if read_just_before {
                self.moved.insert(pane, now);
            } else {
                self.moved.remove(&pane);
            }
        }
        let entry = self.output.entry(pane).or_insert((now, now));
        if changed {
            entry.0 = now;
        }
        entry.1 = now;
        self.screens.insert(pane, lines);
    }

    /// How long a working worker's screen has shown the same work, once
    /// that is [`QUIET_AFTER`] or more: as of the last time it was read,
    /// and never from before its current state began.
    fn quiet_for(&self, pane: u64, status: Status) -> Option<Duration> {
        if status.group() != Group::Working {
            return None;
        }
        let &(changed, checked) = self.output.get(&pane)?;
        let from = match self.since.get(&pane) {
            Some((_, Some(began))) => changed.max(*began),
            _ => changed,
        };
        let span = checked.saturating_duration_since(from);
        (span >= QUIET_AFTER).then_some(span)
    }

    /// The workers this board shows, in listing order: the orchestrator's,
    /// and theirs (D13).
    fn listed(&self) -> impl Iterator<Item = &Panel> {
        let children: HashSet<u64> = match self.of {
            Some(of) => self
                .panels
                .iter()
                .filter(|panel| panel.parent_pane_id.is_some_and(|parent| parent.0 == of))
                .map(|panel| panel.pane_id.0)
                .collect(),
            None => HashSet::new(),
        };
        self.panels
            .iter()
            .filter(|panel| Some(panel.pane_id.0) != self.me)
            .filter(move |panel| match (self.of, panel.parent_pane_id) {
                (Some(of), Some(parent)) => parent.0 == of || children.contains(&parent.0),
                (None, Some(_)) => true,
                (_, None) => false,
            })
    }

    /// See [`Urgency`]. Elsewhere than 要対応, only the section counts.
    fn urgency(&self, panel: &Panel) -> Urgency {
        let status = Status::of(panel);
        let group = status.group();
        if group != Group::Needs {
            return (group, 0, None);
        }
        let rank = u8::from(matches!(status, Status::Input | Status::Attention));
        // `None` (already so when the board opened) is the oldest.
        let began = self.since.get(&panel.pane_id.0).and_then(|(_, at)| *at);
        (group, rank, began)
    }

    /// Every worker shown, as a tree (D13): the orchestrator's workers by
    /// where their subtree is drawn (D15), then by its most urgent worker,
    /// each followed by its own workers by their urgency; ties go by number.
    fn tree(&self) -> Vec<Node<'_>> {
        let listed: Vec<&Panel> = self.listed().collect();
        let ids: HashSet<u64> = listed.iter().map(|panel| panel.pane_id.0).collect();
        let listed_parent = |panel: &Panel| {
            panel
                .parent_pane_id
                .map(|parent| parent.0)
                .filter(|parent| ids.contains(parent))
        };
        let parents: HashMap<u64, Option<u64>> = listed
            .iter()
            .map(|panel| (panel.pane_id.0, listed_parent(panel)))
            .collect();
        let mut tops: Vec<&Panel> = Vec::new();
        let mut kids: HashMap<u64, Vec<&Panel>> = HashMap::new();
        for panel in listed {
            // Under a listed worker that is itself under none: one level.
            match listed_parent(panel).filter(|parent| parents[parent].is_none()) {
                Some(parent) => kids.entry(parent).or_default().push(panel),
                None => tops.push(panel),
            }
        }
        let order = |panel: &Panel| {
            let pane = panel.pane_id.0;
            (self.urgency(panel), self.number(pane), pane)
        };
        for list in kids.values_mut() {
            list.sort_by_key(|panel| order(panel));
        }
        let subtree = |top: &Panel| {
            let own = self.urgency(top);
            kids.get(&top.pane_id.0)
                .into_iter()
                .flatten()
                .map(|kid| self.urgency(kid))
                .fold(own, std::cmp::min)
        };
        let zone = |top: &Panel| {
            let mine = kids.get(&top.pane_id.0).map(Vec::as_slice).unwrap_or(&[]);
            let group = |panel: &Panel| Status::of(panel).group();
            if mine.is_empty() && group(top) == Group::Needs {
                Zone::Queue
            } else if std::iter::once(&top)
                .chain(mine)
                .all(|panel| matches!(group(panel), Group::Done | Group::Exited))
            {
                Zone::Settled
            } else {
                Zone::Background
            }
        };
        tops.sort_by_key(|top| {
            let pane = top.pane_id.0;
            (zone(top), subtree(top), self.number(pane), pane)
        });
        let mut tree = Vec::new();
        for top in tops {
            let zone = zone(top);
            let mine = kids.get(&top.pane_id.0).map(Vec::as_slice).unwrap_or(&[]);
            tree.push(Node {
                panel: top,
                branch: Branch::Top { kids: mine.len() },
                zone,
            });
            for (at, kid) in mine.iter().enumerate() {
                tree.push(Node {
                    panel: kid,
                    branch: Branch::Kid {
                        parent: top.pane_id.0,
                        last: at + 1 == mine.len(),
                    },
                    zone,
                });
            }
        }
        tree
    }

    /// Every row of the list in order, folded away or not (D15): the queue
    /// of workers needing someone, most urgent first, then the tree of the
    /// rest, in which a worker needing someone is an echo of its queue row.
    fn layout(&self) -> Vec<Entry<'_>> {
        let tree = self.tree();
        let needs = |node: &Node| Status::of(node.panel).group() == Group::Needs;
        let mut queue: Vec<Node> = tree.iter().copied().filter(needs).collect();
        queue.sort_by_key(|node| {
            let pane = node.panel.pane_id.0;
            (self.urgency(node.panel), self.number(pane), pane)
        });
        let mut entries: Vec<Entry> = queue
            .into_iter()
            .map(|node| Entry {
                node,
                queued: true,
                echo: false,
            })
            .collect();
        entries.extend(
            tree.iter()
                .filter(|node| node.zone != Zone::Queue)
                .map(|node| Entry {
                    node: *node,
                    queued: false,
                    echo: needs(node),
                }),
        );
        entries
    }

    /// Whether `entry` is folded away: under a folded parent, or among the
    /// finished and exited workers while they are only counted.
    fn folded(&self, entry: &Entry) -> bool {
        if entry.queued {
            return false;
        }
        self.under_folded(&entry.node) || (entry.node.zone == Zone::Settled && !self.settled_open)
    }

    /// Whether `node` is one of a parent's workers folded away under it.
    /// Only the work under way folds: a finished subtree is listed whole.
    fn under_folded(&self, node: &Node) -> bool {
        node.zone == Zone::Background
            && matches!(
                node.branch,
                Branch::Kid { parent, .. } if self.collapsed.contains(&parent)
            )
    }

    /// The rows drawn.
    fn rows(&self) -> Vec<Entry<'_>> {
        let mut rows = self.layout();
        rows.retain(|entry| !self.folded(entry));
        rows
    }

    /// The workers `j`/`k` step through, in list order: every row but the
    /// echoes and the folded subtrees. The finished and exited ones count
    /// while folded, so `j` past the last row opens them.
    fn picks(&self) -> Vec<u64> {
        self.layout()
            .iter()
            .filter(|entry| !entry.echo)
            .filter(|entry| entry.queued || !self.under_folded(&entry.node))
            .map(|entry| entry.node.panel.pane_id.0)
            .collect()
    }

    /// Every worker shown, folded away or not, in list order (see
    /// [`Board::tree`]).
    pub(crate) fn workers(&self) -> Vec<&Panel> {
        self.tree().into_iter().map(|node| node.panel).collect()
    }

    /// The worker whose own workers `pane` is among, if it is one.
    fn tree_parent(&self, pane: u64) -> Option<u64> {
        self.tree()
            .iter()
            .find(|node| node.panel.pane_id.0 == pane)
            .and_then(|node| match node.branch {
                Branch::Kid { parent, .. } => Some(parent),
                Branch::Top { .. } => None,
            })
    }

    /// What `Space` acts on (D15): the subtree of the work under way the
    /// selected worker is in (as a parent or one of its workers), by its
    /// parent; else the finished and exited workers (`None`).
    fn space_target(&self) -> Option<u64> {
        let pane = self.selected?;
        let tree = self.tree();
        let node = tree.iter().find(|node| node.panel.pane_id.0 == pane)?;
        if node.zone != Zone::Background {
            return None;
        }
        // A worker's worker needing someone is selected in the queue, not
        // in its parent's subtree; a parent needing someone still holds
        // its own workers.
        let needs = Status::of(node.panel).group() == Group::Needs;
        match node.branch {
            Branch::Kid { .. } if needs => None,
            Branch::Kid { parent, .. } => Some(parent),
            Branch::Top { kids } if kids > 0 => Some(pane),
            Branch::Top { .. } => None,
        }
    }

    /// Whether what `Space` acts on is folded; `None` when there is nothing
    /// for it to fold.
    fn space_folded(&self) -> Option<bool> {
        // With no worker left, the closed ones are always listed.
        if self.workers().is_empty() {
            return None;
        }
        match self.space_target() {
            Some(top) => Some(self.collapsed.contains(&top)),
            None if self.tree().iter().any(|node| node.zone == Zone::Settled)
                || !self.gone.is_empty() =>
            {
                Some(!self.settled_open)
            }
            None => None,
        }
    }

    /// `Space`: fold the subtree the selected worker is in away under its
    /// parent's row, or open it again; outside one, list the finished and
    /// exited workers, or only count them again.
    fn space(&mut self) {
        let Some(top) = self.space_target() else {
            if self.space_folded().is_none() {
                let text = self.lang.pick("たたむものはありません", "nothing to fold");
                self.message = Some(Notice::ok(text.to_string()));
                return;
            }
            self.settled_open = !self.settled_open;
            self.past = 0;
            // A selection folded away moves to the last row left.
            if !self.settled_open
                && let Some(pane) = self.selected
                && self
                    .tree()
                    .iter()
                    .any(|node| node.panel.pane_id.0 == pane && node.zone == Zone::Settled)
            {
                let picks = self.picks();
                let settled: HashSet<u64> = self
                    .tree()
                    .iter()
                    .filter(|node| node.zone == Zone::Settled)
                    .map(|node| node.panel.pane_id.0)
                    .collect();
                self.selected = picks
                    .iter()
                    .rev()
                    .find(|pane| !settled.contains(pane))
                    .or(picks.first())
                    .copied();
            }
            return;
        };
        if !self.collapsed.remove(&top) {
            self.collapsed.insert(top);
            // The selection stays in view, on the row it was folded under.
            self.past = 0;
            self.selected = Some(top);
        }
    }

    /// `pane`'s number (see [`Board::numbers`]).
    fn number(&self, pane: u64) -> usize {
        self.numbers.get(&pane).copied().unwrap_or(usize::MAX)
    }

    /// The panes whose screens the current view needs: in the list, the
    /// workers that need someone or are at work (whose rows say when they
    /// last showed output), and the selected one, which is previewed.
    pub(crate) fn wanted_screens(&self) -> Vec<u64> {
        match self.view {
            View::List => self
                .workers()
                .iter()
                .map(|panel| panel.pane_id.0)
                .filter(|&pane| {
                    Some(pane) == self.selected
                        || self.panel(pane).is_some_and(|panel| {
                            matches!(Status::of(panel).group(), Group::Needs | Group::Working)
                        })
                })
                .collect(),
            View::Detail(pane) => vec![pane],
        }
    }

    #[cfg(test)]
    pub(crate) fn selected(&self) -> Option<u64> {
        self.selected
    }

    /// The pane the action keys act on.
    fn target(&self) -> Option<u64> {
        match self.view {
            View::Detail(pane) => Some(pane),
            View::List => self.selected,
        }
    }

    fn panel(&self, pane: u64) -> Option<&Panel> {
        self.panels.iter().find(|panel| panel.pane_id.0 == pane)
    }

    fn name(&self, pane: u64) -> String {
        self.panel(pane)
            .map(worker_name)
            .unwrap_or_else(|| format!("pane {pane}"))
    }

    fn move_selection(&mut self, step: isize) {
        let workers = self.picks();
        if workers.is_empty() {
            // Nothing to select: j/k scroll the closed section, as far as
            // its title and entries go.
            if self.view == View::List {
                let reach = 1 + self.gone.len();
                self.past = self.past.saturating_add_signed(step).min(reach);
            }
            return;
        }
        let at = self
            .selected
            .and_then(|pane| workers.iter().position(|&p| p == pane))
            .unwrap_or(0) as isize;
        let last = at as usize + 1 == workers.len();
        if self.view == View::List {
            // As far as the closed section's title and entries go, once
            // the finished and exited workers are listed.
            let reach = 1 + self.gone.len();
            if step > 0 && last && !self.gone.is_empty() {
                self.settled_open = true;
                self.past = (self.past + 1).min(reach);
                return;
            }
            if step < 0 && self.past > 0 {
                self.past -= 1;
                return;
            }
        }
        let next = (at + step).clamp(0, workers.len() as isize - 1) as usize;
        self.select(workers[next]);
    }

    /// Select `pane`, opening the subtree or section it is folded away in
    /// (one that needs someone is selected in the queue, never folded).
    fn select(&mut self, pane: u64) {
        let place = self
            .tree()
            .iter()
            .find(|node| node.panel.pane_id.0 == pane)
            .filter(|node| Status::of(node.panel).group() != Group::Needs)
            .map(|node| (node.branch, node.zone));
        if let Some((branch, zone)) = place {
            if let Branch::Kid { parent, .. } = branch {
                self.collapsed.remove(&parent);
            }
            if zone == Zone::Settled {
                self.settled_open = true;
            }
        }
        self.past = 0;
        self.selected = Some(pane);
        if let View::Detail(_) = self.view {
            self.view = View::Detail(pane);
        }
    }

    /// Select the worker numbered `number`.
    /// A digit key: the worker with that number, or, typed within
    /// [`SECOND_DIGIT`] of a first digit, the two-digit number they make
    /// when such a worker exists (`1` then `2` is 12). `0` only completes.
    fn type_digit(&mut self, digit: usize, now: Instant) {
        let paired = self
            .digit
            .take()
            .filter(|(_, at)| now.saturating_duration_since(*at) < SECOND_DIGIT)
            .map(|(first, _)| first * 10 + digit)
            .filter(|number| self.numbers.values().any(|n| n == number));
        match paired {
            Some(number) => self.select_number(number),
            None if digit == 0 => {}
            None => {
                self.select_number(digit);
                self.digit = Some((digit, now));
            }
        }
    }

    fn select_number(&mut self, number: usize) {
        let pane = self
            .listed()
            .map(|panel| panel.pane_id.0)
            .find(|&pane| self.number(pane) == number);
        match pane {
            Some(pane) => self.select(pane),
            None => {
                let text = self.lang.pick(
                    format!("{number} 番のワーカーはいません"),
                    format!("no worker {number}"),
                );
                self.message = Some(Notice::warn(text));
            }
        }
    }

    /// Select the next worker that needs someone, after the selected one,
    /// in the queue's order.
    fn select_next_needing(&mut self) {
        let needing: Vec<u64> = self
            .layout()
            .iter()
            .filter(|entry| entry.queued)
            .map(|entry| entry.node.panel.pane_id.0)
            .collect();
        if needing.is_empty() {
            let text = self
                .lang
                .pick("要対応のワーカーはいません", "no worker needs you");
            self.message = Some(Notice::ok(text.to_string()));
            return;
        }
        let next = self
            .selected
            .and_then(|pane| needing.iter().position(|&p| p == pane))
            .map_or(0, |at| (at + 1) % needing.len());
        self.select(needing[next]);
    }

    /// Whether `pane` waits for an approval, which is answered in its own
    /// pane, never from the board (spec D7).
    fn awaits_approval(&self, pane: u64) -> bool {
        self.panel(pane)
            .is_some_and(|panel| Status::of(panel) == Status::Permission)
    }

    /// Whether `pane`'s process has ended: nothing typed reaches it.
    fn has_exited(&self, pane: u64) -> bool {
        self.panel(pane)
            .is_some_and(|panel| matches!(Status::of(panel), Status::Exited(_)))
    }

    /// Why nothing can be sent to `pane`, if so.
    fn cannot_send(&self, pane: u64) -> Option<String> {
        let name = self.name(pane);
        if self.awaits_approval(pane) {
            Some(approval_message(&name, self.lang))
        } else if self.has_exited(pane) {
            Some(self.lang.pick(
                format!("{name} は終了しています。x で閉じる"),
                format!("{name} has exited. x closes it"),
            ))
        } else {
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn key(&mut self, key: Key) -> Option<Action> {
        self.key_at(key, Instant::now())
    }

    /// A key that arrived at `at`.
    pub(crate) fn key_at(&mut self, key: Key, at: Instant) -> Option<Action> {
        // Only a digit right after a digit makes a two-digit number.
        if !matches!(key, Key::Char('0'..='9')) {
            self.digit = None;
        }
        match self.mode.clone() {
            Mode::Send(pane, mut text) => {
                match key {
                    Key::Char(ch) => {
                        text.push(ch);
                        self.mode = Mode::Send(pane, text);
                    }
                    Key::Paste(paste) => {
                        text.push_str(&paste);
                        self.mode = Mode::Send(pane, text);
                    }
                    Key::Backspace => {
                        text.pop();
                        self.mode = Mode::Send(pane, text);
                    }
                    Key::CtrlU => self.mode = Mode::Send(pane, String::new()),
                    Key::CtrlW => {
                        let kept = text.trim_end().len();
                        let word = text[..kept]
                            .char_indices()
                            .rfind(|(_, ch)| ch.is_whitespace())
                            .map_or(0, |(at, ch)| at + ch.len_utf8());
                        text.truncate(word);
                        self.mode = Mode::Send(pane, text);
                    }
                    Key::Enter => {
                        self.mode = Mode::Normal;
                        if let Some(why) = self.cannot_send(pane) {
                            self.message = Some(Notice::warn(why));
                        } else if !text.trim().is_empty() {
                            return Some(Action::Send(pane, text));
                        }
                    }
                    Key::Esc | Key::CtrlC => self.mode = Mode::Normal,
                    _ => {}
                }
                None
            }
            Mode::ConfirmClose(pane) => {
                self.mode = Mode::Normal;
                (key == Key::Char('y')).then_some(Action::Close(pane))
            }
            Mode::ConfirmQuit => {
                self.mode = Mode::Normal;
                matches!(key, Key::Char('q') | Key::CtrlC).then_some(Action::Quit)
            }
            Mode::TargetGone(_) => {
                if key == Key::Esc {
                    self.mode = Mode::Normal;
                }
                None
            }
            Mode::Normal => {
                self.message = None;
                // j/k scroll the help, any other key closes it; Ctrl-C
                // still quits.
                if self.help {
                    match key {
                        Key::Char('j') | Key::Down => {
                            self.help_scroll = (self.help_scroll + 1).min(self.help_reach.get());
                            return None;
                        }
                        Key::Char('k') | Key::Up => {
                            self.help_scroll = self.help_scroll.saturating_sub(1);
                            return None;
                        }
                        Key::CtrlC => {}
                        _ => {
                            self.help = false;
                            return None;
                        }
                    }
                }
                match key {
                    Key::Char('q') | Key::CtrlC => {
                        // Only a listing that worked can say there is no
                        // worker to hand back.
                        if self.started && self.offline.is_none() && self.workers().is_empty() {
                            return Some(Action::Quit);
                        }
                        self.mode = Mode::ConfirmQuit;
                    }
                    Key::Char('?') => {
                        self.help = true;
                        self.help_scroll = 0;
                    }
                    Key::Char('n') => self.select_next_needing(),
                    Key::Char(' ') if self.view == View::List => self.space(),
                    Key::Char(digit @ '0'..='9') => {
                        self.type_digit(digit as usize - '0' as usize, at)
                    }
                    Key::Char('j') | Key::Down => self.move_selection(1),
                    Key::Char('k') | Key::Up => self.move_selection(-1),
                    Key::Enter | Key::Char('l') | Key::Right => {
                        if let Some(pane) = self.selected {
                            self.view = View::Detail(pane);
                        }
                    }
                    Key::Esc | Key::Char('h') | Key::Left => self.view = View::List,
                    Key::Char('g') => return self.target().map(Action::Focus),
                    Key::Char('s') => {
                        if let Some(pane) = self.target() {
                            match self.cannot_send(pane) {
                                Some(why) => self.message = Some(Notice::warn(why)),
                                None => self.mode = Mode::Send(pane, String::new()),
                            }
                        }
                    }
                    Key::Char('x') => {
                        if let Some(pane) = self.target() {
                            self.mode = Mode::ConfirmClose(pane);
                        }
                    }
                    _ => {}
                }
                None
            }
        }
    }
}

// Labels and process names come from other processes: never let their
// control characters reach this terminal.
fn approval_message(name: &str, lang: Lang) -> String {
    match lang {
        Lang::Ja => format!("{name} は承認待ちです。g でペインへ移動して答えてください"),
        Lang::En => format!("{name} awaits approval. g goes to its pane to answer it"),
    }
}

/// A full-width rule, as agents draw around their input box.
fn is_rule(line: &str) -> bool {
    let line = line.trim();
    line.chars().count() >= 20 && line.chars().all(|ch| matches!(ch, '─' | '━' | '═'))
}

/// The part of a screen that shows work: above the agent's input box (the
/// rules near the bottom and what lies between and below them, where its
/// prompt and status line live). A screen without one is all work.
pub(crate) fn content_lines(lines: &[String]) -> &[String] {
    const BOX_REACH: usize = 12;
    const RULE_GAP: usize = 4;
    let bottom = lines.len().saturating_sub(BOX_REACH);
    let Some(mut first) = (bottom..lines.len()).rev().find(|&i| is_rule(&lines[i])) else {
        return lines;
    };
    while let Some(above) = (first.saturating_sub(RULE_GAP)..first)
        .rev()
        .find(|&i| is_rule(&lines[i]))
    {
        first = above;
    }
    &lines[..first]
}

fn worker_name(panel: &Panel) -> String {
    panel
        .label
        .as_deref()
        .map(clean)
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| format!("pane {}", panel.pane_id.0))
}

fn worker_kind(panel: &Panel) -> String {
    panel
        .agent
        .as_ref()
        .and_then(|agent| agent.kind.clone())
        .or_else(|| panel.process.clone())
        .map(|kind| clean(&kind))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Keys

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Turns what the terminal sends into keys. A read can end inside a UTF-8
/// character, an escape sequence or a bracketed paste, so the unfinished
/// part waits for the next read; a paste arrives whole as [`Key::Paste`]
/// and is never read as keys.
#[derive(Default)]
pub(crate) struct KeyReader {
    pending: Vec<u8>,
    paste: Option<Vec<u8>>,
    /// When the held-over escape start arrived.
    pending_since: Option<Instant>,
}

impl KeyReader {
    /// Keys in `bytes`, which arrived at `at`. An escape start held over
    /// from a read [`ESC_WAIT`] or more before is settled first: keys
    /// pressed apart never join into one sequence, however late the loop
    /// gets to them.
    pub(crate) fn feed_at(&mut self, bytes: &[u8], at: Instant) -> Vec<Key> {
        let mut keys = Vec::new();
        if self
            .pending_since
            .is_some_and(|since| at.saturating_duration_since(since) >= ESC_WAIT)
        {
            keys.extend(self.flush());
        }
        keys.extend(self.feed(bytes));
        self.pending_since = self.waiting_on_esc().then_some(at);
        keys
    }

    /// Whether the last read ended inside an escape start: a lone ESC (the
    /// Esc key, or a sequence still on its way) or an unfinished `ESC [` /
    /// `ESC O`.
    pub(crate) fn waiting_on_esc(&self) -> bool {
        self.paste.is_none() && self.pending.first() == Some(&0x1b)
    }

    /// Nothing completed the escape start in time: a lone ESC was the Esc
    /// key, and an unfinished `ESC [` or `ESC O` was Option/Meta with `[` or
    /// `O`, which does nothing here.
    pub(crate) fn flush(&mut self) -> Vec<Key> {
        if !self.waiting_on_esc() {
            return Vec::new();
        }
        let lone = self.pending == [0x1b];
        self.pending.clear();
        self.pending_since = None;
        if lone { vec![Key::Esc] } else { Vec::new() }
    }
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Vec<Key> {
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(bytes);
        let mut keys = Vec::new();
        let mut i = 0;
        loop {
            if let Some(paste) = self.paste.as_mut() {
                let rest = &buf[i..];
                match find(rest, PASTE_END) {
                    Some(at) => {
                        paste.extend_from_slice(&rest[..at]);
                        keys.push(Key::Paste(String::from_utf8_lossy(paste).into_owned()));
                        self.paste = None;
                        i += at + PASTE_END.len();
                        continue;
                    }
                    None => {
                        // Keep what could be the start of the end marker.
                        let keep = (1..PASTE_END.len().min(rest.len() + 1))
                            .rev()
                            .find(|&n| rest.ends_with(&PASTE_END[..n]))
                            .unwrap_or(0);
                        paste.extend_from_slice(&rest[..rest.len() - keep]);
                        self.pending = rest[rest.len() - keep..].to_vec();
                        return keys;
                    }
                }
            }
            let rest = &buf[i..];
            let Some(&first) = rest.first() else {
                return keys;
            };
            if first == 0x1b {
                if rest.starts_with(PASTE_START) {
                    self.paste = Some(Vec::new());
                    i += PASTE_START.len();
                    continue;
                }
                if rest.len() == 1 {
                    // Esc, or a sequence cut right after its ESC: wait for
                    // the next read, or `flush` when none comes.
                    self.pending = rest.to_vec();
                    return keys;
                }
                if rest[1] == b'[' || rest[1] == b'O' {
                    match rest[2..]
                        .iter()
                        .position(|b| b.is_ascii_alphabetic() || *b == b'~')
                    {
                        Some(at) => {
                            match rest[2 + at] {
                                b'A' => keys.push(Key::Up),
                                b'B' => keys.push(Key::Down),
                                b'C' => keys.push(Key::Right),
                                b'D' => keys.push(Key::Left),
                                _ => {}
                            }
                            i += 3 + at;
                        }
                        // Unfinished: wait for the rest; a runaway one is dropped.
                        None if rest.len() < 32 => {
                            self.pending = rest.to_vec();
                            return keys;
                        }
                        None => i = buf.len(),
                    }
                    continue;
                }
                if rest[1] == 0x1b {
                    keys.push(Key::Esc);
                    i += 1;
                    continue;
                }
                // ESC and a key in one read is Option/Meta with that key: no
                // command here, and never Esc plus a stray key.
                let len = utf8_len(rest[1]);
                if rest.len() < 1 + len {
                    self.pending = rest.to_vec();
                    return keys;
                }
                i += 1 + len;
                continue;
            }
            let len = utf8_len(first);
            if rest.len() < len {
                self.pending = rest.to_vec();
                return keys;
            }
            match std::str::from_utf8(&rest[..len]) {
                Ok(text) => {
                    keys.extend(text.chars().filter_map(char_key));
                    i += len;
                }
                Err(_) => {
                    keys.push(Key::Char(char::REPLACEMENT_CHARACTER));
                    i += 1;
                }
            }
        }
    }
}

/// The length of the UTF-8 character that starts with `first`.
fn utf8_len(first: u8) -> usize {
    match first {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn char_key(ch: char) -> Option<Key> {
    match ch {
        '\r' | '\n' => Some(Key::Enter),
        '\u{7f}' | '\u{8}' => Some(Key::Backspace),
        '\u{3}' => Some(Key::CtrlC),
        '\u{15}' => Some(Key::CtrlU),
        '\u{17}' => Some(Key::CtrlW),
        ch if ch.is_control() => None,
        ch => Some(Key::Char(ch)),
    }
}

// ---------------------------------------------------------------------------
// Drawing

#[derive(Clone, Copy)]
struct Rgb(u8, u8, u8);

const ORANGE: Rgb = Rgb(240, 163, 94);
const RED: Rgb = Rgb(255, 122, 122);
const BLUE: Rgb = Rgb(106, 168, 255);
const GREEN: Rgb = Rgb(95, 196, 160);
const GREY: Rgb = Rgb(139, 144, 160);
const DIM: Rgb = Rgb(158, 162, 177);
const BORDER: Rgb = Rgb(70, 74, 90);

/// A line built from styled pieces, tracking its display width.
#[derive(Default)]
struct Line {
    text: String,
    width: usize,
}

impl Line {
    fn plain(&mut self, s: &str) -> &mut Self {
        self.text.push_str(s);
        self.width += s.width();
        self
    }

    fn fg(&mut self, color: Rgb, s: &str) -> &mut Self {
        let Rgb(r, g, b) = color;
        self.text
            .push_str(&format!("\x1b[38;2;{r};{g};{b}m{s}\x1b[39m"));
        self.width += s.width();
        self
    }

    fn bold(&mut self, s: &str) -> &mut Self {
        self.text.push_str(&format!("\x1b[1m{s}\x1b[22m"));
        self.width += s.width();
        self
    }

    /// Pad with spaces (or cut) to exactly `width` columns.
    fn finish(mut self, width: usize) -> String {
        if self.width > width {
            return cut_styled(&self.text, width);
        }
        self.text.push_str(&" ".repeat(width - self.width));
        self.text.push_str("\x1b[0m");
        self.text
    }
}

/// `s` cut to at most `width` columns, ending in `…` when it was cut.
fn fit(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > width - 1 {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// `s` fitted to exactly `width` columns: cut with `…`, or padded.
fn pad(s: &str, width: usize) -> String {
    let s = fit(s, width);
    let room = width.saturating_sub(s.width());
    s + &" ".repeat(room)
}

/// What a scrolled list hides: (workers, of them needing someone) above and
/// below, and closed entries below.
struct Hidden {
    above: (usize, usize),
    below: (usize, usize),
    gone: usize,
}

/// "↓ あと 3 件 · うち要対応 1 · 閉じた 2 件": what a scrolled list hides.
/// Only parts with something in them are said.
fn more_line(
    arrow: &str,
    (n, needs): (usize, usize),
    gone: usize,
    cols: usize,
    lang: Lang,
) -> String {
    let mut line = Line::default();
    line.plain("   ").fg(DIM, arrow);
    if n > 0 {
        let more = lang.pick(format!(" あと {n} 件"), format!(" {n} more"));
        line.fg(DIM, &more);
        if needs > 0 {
            let needs = lang.pick(format!(" うち要対応 {needs}"), format!(" {needs} need you"));
            line.fg(DIM, " ·").fg(ORANGE, &needs);
        }
    }
    if gone > 0 {
        let sep = if n > 0 { " ·" } else { "" };
        let gone = lang.pick(
            format!("{sep} 閉じた {gone} 件"),
            format!("{sep} {gone} closed"),
        );
        line.fg(DIM, &gone);
    }
    line.finish(cols)
}

/// A section's title rule, `─ title ─────── hint ─`, exactly `cols` wide;
/// the hint goes first when it does not fit. A piece without a colour is
/// bold.
fn rule(cols: usize, title: &[(Option<Rgb>, String)], hint: Option<&str>) -> String {
    let mut line = Line::default();
    line.fg(BORDER, "─ ");
    for (color, text) in title {
        match color {
            Some(color) => line.fg(*color, text),
            None => line.bold(text),
        };
    }
    line.plain(" ");
    let hint = hint.map(|hint| format!(" {hint} ")).unwrap_or_default();
    let hint = if line.width + hint.width() + 2 <= cols {
        hint
    } else {
        String::new()
    };
    let tail = if hint.is_empty() { 0 } else { hint.width() + 1 };
    let fill = cols.saturating_sub(line.width + tail);
    line.fg(BORDER, &"─".repeat(fill));
    if !hint.is_empty() {
        line.fg(DIM, &hint).fg(BORDER, "─");
    }
    line.finish(cols)
}

/// How much of a worker's state its column says, from most to least.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StateForm {
    /// "承認待ち 5 秒"
    Full,
    /// "承認 5 秒"
    Short,
    /// "承認"
    Bare,
}

/// The widths of a list's columns, shared by every row so they line up. A
/// column that does not fit the pane is left out (0).
struct Columns {
    /// Up to where the state starts: a worker's worker, drawn further in,
    /// gets this less its indent.
    name: usize,
    /// Shown only when the workers run different programs.
    kind: usize,
    state: usize,
    form: StateForm,
    /// The tally of a worker's own workers (see [`Tally`]), when any
    /// worker has some.
    tally: usize,
    tally_form: StateForm,
}

impl Columns {
    /// `rows` are the workers with how far in each name starts; `tallies`
    /// the widest tally in each [`StateForm`] (0: no worker has workers).
    fn of(
        rows: &[(&Panel, usize)],
        tallies: [usize; 3],
        cols: usize,
        state: impl Fn(&Panel, StateForm) -> String,
    ) -> Self {
        let widest = |text: &dyn Fn(&Panel) -> String| {
            rows.iter()
                .map(|(panel, _)| text(panel).width())
                .max()
                .unwrap_or(0)
        };
        let room = cols.saturating_sub(ROW_INDENT + 1);
        let indent = rows.iter().map(|&(_, indent)| indent).max().unwrap_or(0);
        let mut name = rows
            .iter()
            .map(|&(panel, indent)| indent + worker_name(panel).width())
            .max()
            .unwrap_or(0)
            .min(24 + indent)
            .min((room / 3).max(4) + indent);
        let kinds: HashSet<String> = rows
            .iter()
            .map(|(panel, _)| worker_kind(panel))
            .filter(|kind| !kind.is_empty())
            .collect();
        let mut kind = if kinds.len() > 1 {
            widest(&|panel| worker_kind(panel)).min(10)
        } else {
            0
        };
        let width = |form: StateForm| widest(&|panel| state(panel, form));
        let tally = |form: StateForm| match tallies[form as usize] {
            0 => 0,
            w => 2 + w,
        };
        // Keep about 10 columns for the detail (what a worker asks is what
        // a narrow pane must not lose): give up the kind first, then say
        // the state and the tally more briefly, and drop each only when
        // even its short form does not fit (the tally keeps its ▾/▸ the
        // longest, so a folded subtree still shows).
        if name + 2 + kind + 2 + width(StateForm::Full) + tally(StateForm::Full) + 10 > room {
            kind = 0;
        }
        let forms = [StateForm::Full, StateForm::Short, StateForm::Bare];
        let (form, mut state_width) = forms
            .iter()
            .map(|&form| (form, width(form)))
            .find(|&(form, w)| name + 2 + w + tally(form) + 10 <= room)
            .unwrap_or((StateForm::Bare, width(StateForm::Bare)));
        let tally_form = forms
            .iter()
            .copied()
            .skip(form as usize)
            .find(|&tally_form| name + 2 + state_width + tally(tally_form) <= room)
            .unwrap_or(StateForm::Bare);
        let mut tally_width = tallies[tally_form as usize];
        if name + 2 + state_width + tally(tally_form) > room {
            // The ▾/▸ alone, paid for by the name, before nothing.
            let short = (name + 2 + state_width + tally(StateForm::Bare)).saturating_sub(room);
            if tallies[0] > 0 && name >= short + 4 {
                name -= short;
                tally_width = tallies[StateForm::Bare as usize];
            } else {
                tally_width = 0;
            }
        }
        if name + 2 + state_width > room {
            state_width = 0;
        }
        Self {
            name,
            kind,
            state: state_width,
            form,
            tally: tally_width,
            tally_form,
        }
    }
}

/// What a worker with workers of its own says about them on its row: how
/// many, how many of them need someone, and whether they are folded away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tally {
    kids: usize,
    needs: usize,
    collapsed: bool,
}

/// A tally as styled pieces: "▾ 子 3 · 要対応 1", "▾3 要対応1", or "▾" alone.
/// Only parts with something in them are said.
fn tally_pieces(tally: Tally, form: StateForm, lang: Lang) -> Vec<(Rgb, String)> {
    let mark = if tally.collapsed { "▸" } else { "▾" };
    let Tally { kids, needs, .. } = tally;
    let mut pieces = Vec::new();
    match form {
        StateForm::Full => {
            let kids = lang.pick(
                format!("{mark} 子 {kids}"),
                format!("{mark} {kids} workers"),
            );
            pieces.push((DIM, kids));
            if needs > 0 {
                pieces.push((DIM, " · ".to_string()));
                let needs = lang.pick(format!("要対応 {needs}"), format!("{needs} need you"));
                pieces.push((ORANGE, needs));
            }
        }
        StateForm::Short => {
            pieces.push((DIM, format!("{mark}{kids}")));
            if needs > 0 {
                let needs = lang.pick(format!(" 要対応{needs}"), format!(" !{needs}"));
                pieces.push((ORANGE, needs));
            }
        }
        StateForm::Bare => pieces.push((DIM, mark.to_string())),
    }
    pieces
}

fn tally_width(tally: Tally, form: StateForm, lang: Lang) -> usize {
    tally_pieces(tally, form, lang)
        .iter()
        .map(|(_, text)| text.width())
        .sum()
}

/// A styled line cut at `width` columns, keeping its escape sequences.
fn cut_styled(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            out.push(ch);
            for next in chars.by_ref() {
                out.push(next);
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        let w = ch.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push_str(&" ".repeat(width - used));
    out.push_str("\x1b[0m");
    out
}

/// Strip control characters a screen line may carry.
fn clean(s: &str) -> String {
    s.chars().filter(|ch| !ch.is_control()).collect()
}

/// "10 秒", "2 分", "3 時間" (in English "10s", "2m", "3h").
fn duration_text(span: Duration, lang: Lang) -> String {
    let secs = span.as_secs();
    let (n, ja, en) = match secs {
        0..60 => (secs, "秒", "s"),
        60..3600 => (secs / 60, "分", "m"),
        _ => (secs / 3600, "時間", "h"),
    };
    lang.pick(format!("{n} {ja}"), format!("{n}{en}"))
}

/// "10 秒前" ("10s ago"), or "開始時" for a time before the board opened.
fn ago(at: Option<Instant>, now: Instant, lang: Lang) -> String {
    let Some(at) = at else {
        return lang.pick("開始時", "at start").to_string();
    };
    let span = duration_text(now.saturating_duration_since(at), lang);
    lang.pick(format!("{span}前"), format!("{span} ago"))
}

impl Board {
    /// The screen as `rows` lines of exactly `cols` columns each.
    pub(crate) fn render(&self, cols: usize, rows: usize, now: Instant) -> Vec<String> {
        // Drawing wider than the pane would wrap every line; say so instead.
        if cols < MIN_COLS || rows < MIN_ROWS {
            let note = self.lang.pick(
                format!("{MIN_COLS}×{MIN_ROWS} 以上に広げてください"),
                format!("Widen to {MIN_COLS}×{MIN_ROWS} or more"),
            );
            return (0..rows)
                .map(|row| {
                    let mut line = Line::default();
                    if row == 0 {
                        line.fg(DIM, &fit(&note, cols));
                    }
                    line.finish(cols)
                })
                .collect();
        }
        // An opened worker covers the lower part of the board (D10), so
        // what the person must see — the result of the last action, a
        // prompt, the send line — is in the top band, under the header;
        // only the key list is at the bottom.
        let body_rows = rows - 1;
        let mut help_cut = false;
        let mut lines = match (self.help, self.view) {
            (true, _) => {
                let (lines, cut) = self.render_help(cols, body_rows);
                help_cut = cut;
                lines
            }
            (false, View::List) => self.render_list(cols, rows, now),
            (false, View::Detail(pane)) => self.render_detail(pane, cols, rows, now),
        };
        lines.truncate(body_rows);
        while lines.len() < body_rows {
            lines.push(Line::default().finish(cols));
        }
        lines.push(self.footer(cols, help_cut));
        lines
    }

    /// The pane title: what needs someone, so it shows on a collapsed or
    /// background pane too.
    pub(crate) fn title(&self) -> String {
        let workers = self.workers();
        let count = |group: Group| {
            workers
                .iter()
                .filter(|panel| Status::of(panel).group() == group)
                .count()
        };
        let needs = count(Group::Needs);
        let working = count(Group::Working);
        if needs > 0 {
            self.lang.pick(
                format!("ボード · 要対応 {needs}"),
                format!("Board · {needs} need you"),
            )
        } else if working > 0 {
            self.lang.pick(
                format!("ボード · 作業中 {working}"),
                format!("Board · {working} working"),
            )
        } else {
            self.lang.pick("ボード", "Board").to_string()
        }
    }

    /// The line under the header: the send line while typing, a prompt
    /// waiting for its key, else the result of the last action, else how to
    /// get back here from an opened worker.
    fn notice_line(&self, cols: usize, now: Instant) -> String {
        let mut line = Line::default();
        line.plain(" ");
        let room = cols.saturating_sub(1);
        match &self.mode {
            Mode::Send(pane, text) => {
                line.fg(
                    BLUE,
                    &fit(
                        &self.lang.pick(
                            format!("{} に送る", self.name(*pane)),
                            format!("to {}", self.name(*pane)),
                        ),
                        room / 2,
                    ),
                )
                .fg(BLUE, " › ");
                let room = cols.saturating_sub(line.width + 1);
                // Show the end of a long message, where the typing happens,
                // and line breaks without printing them.
                let text: String = text
                    .chars()
                    .map(|ch| if ch == '\n' { '⏎' } else { ch })
                    .filter(|ch| !ch.is_control())
                    .collect();
                let mut tail = String::new();
                let mut used = 0;
                for ch in text.chars().rev() {
                    let w = ch.width().unwrap_or(0);
                    if used + w > room {
                        break;
                    }
                    tail.insert(0, ch);
                    used += w;
                }
                line.plain(&tail).plain("█");
            }
            Mode::ConfirmClose(pane) => {
                // Closing a worker mid-task loses its work, not only its
                // screen: say so.
                let busy = self
                    .panel(*pane)
                    .map(Status::of)
                    .filter(|status| {
                        !matches!(status, Status::Exited(_))
                            && matches!(status.group(), Group::Needs | Group::Working)
                    })
                    .map(|status| {
                        let label = status.label(self.lang);
                        self.lang.pick(
                            format!("まだ{label}です。"),
                            format!("It is still {label}. "),
                        )
                    })
                    .unwrap_or_default();
                // D13: its own workers close with it.
                let kids = self
                    .listed()
                    .filter(|panel| panel.parent_pane_id.is_some_and(|p| p.0 == *pane))
                    .count();
                let kids = if kids > 0 {
                    self.lang.pick(
                        format!("ワーカーの子 {kids} 件も閉じます。"),
                        format!("Its {kids} workers close too. "),
                    )
                } else {
                    String::new()
                };
                let name = self.name(*pane);
                let ask = self.lang.pick(
                    format!("{name} を閉じますか? {busy}{kids}出力は消えます。"),
                    format!("Close {name}? {busy}{kids}Its output is lost."),
                );
                line.fg(RED, &fit(&ask, room));
            }
            Mode::ConfirmQuit => {
                let kids = self
                    .tree()
                    .iter()
                    .filter(|node| matches!(node.branch, Branch::Kid { .. }))
                    .count();
                // Back on the orchestrator they are ordinary floats, which
                // a pane holds 8 of, those it already has included
                // (floating-panes REQ-019): how many fit is not known here.
                // A worker's own workers close: their parent is a float then
                // (D13). What closes is said first, so a narrow pane cuts
                // the rest.
                let ask = match (self.lang, kids > 0) {
                    (Lang::En, true) => format!(
                        "{kids} workers' workers, and workers that don't fit on the orchestrator, close; others go back. q again quits"
                    ),
                    (Lang::En, false) => {
                        "Workers that don't fit on the orchestrator close; others go back. q again quits"
                            .to_string()
                    }
                    (Lang::Ja, true) => format!(
                        "ワーカーの子 {kids} 件と、オーケストレーターに収まらないワーカーは閉じ、残りは戻ります。再度 q で終了"
                    ),
                    (Lang::Ja, false) => {
                        "オーケストレーターに収まらないワーカーは閉じ、残りは戻ります。再度 q で終了"
                            .to_string()
                    }
                };
                line.fg(ORANGE, &fit(&ask, room));
            }
            Mode::TargetGone(name) => {
                let text = self.lang.pick(
                    format!("{name} は閉じられました。Esc で続けます"),
                    format!("{name} was closed. Esc goes on"),
                );
                line.fg(ORANGE, &fit(&text, room));
            }
            Mode::Normal => match (&self.offline, &self.message) {
                // The listing failed: what is shown is as of `since`.
                (Some(since), _) => {
                    let secs = now.saturating_duration_since(*since).as_secs();
                    let text = self.lang.pick(
                        format!("接続できません · 再試行中 {secs} 秒"),
                        format!("Cannot connect · retrying for {secs}s"),
                    );
                    line.fg(ORANGE, &fit(&text, room));
                }
                (None, Some(notice)) => {
                    let color = if notice.warn { ORANGE } else { DIM };
                    line.fg(color, &fit(&clean(&notice.text), room));
                }
                (None, None) => {
                    let hint = self.lang.pick(
                        format!("{BACK_TO_BOARD} 開いたワーカーからボードへ  ·  ? キー一覧"),
                        format!("{BACK_TO_BOARD} back here from an opened worker  ·  ? keys"),
                    );
                    line.fg(DIM, &fit(&hint, room));
                }
            },
        }
        line.finish(cols)
    }

    /// The key help in `rows` lines, and whether it was cut: then it
    /// scrolls by `help_scroll`, and its last line says there is more.
    fn render_help(&self, cols: usize, rows: usize) -> (Vec<String>, bool) {
        let mut title = Line::default();
        title
            .plain(" ")
            .bold(self.lang.pick("キー操作", "Keys"))
            .fg(
                DIM,
                self.lang.pick(
                    "  j/k 以外のキーで閉じます",
                    "  any key but j/k closes this",
                ),
            );
        let mut lines = vec![Line::default().finish(cols)];
        let l = |ja, en| self.lang.pick(ja, en);
        let keys: [(&str, &str); 15] = [
            ("j k ↑ ↓", l("ワーカーを選ぶ", "select a worker")),
            (
                "1–20",
                l(
                    "番号のワーカーを選ぶ(10 以上は 2 桁続けて)",
                    "select by number (two digits in a row for 10 and up)",
                ),
            ),
            (
                "n",
                l(
                    "キューの次の要対応のワーカーへ",
                    "the next worker in the queue",
                ),
            ),
            (
                "Space",
                l(
                    "作業中のワーカーの子をたたむ / 開く(それ以外では完了・終了を開く / たたむ)",
                    "fold / open a subtree at work (elsewhere: list / fold done and exited)",
                ),
            ),
            (
                "Enter l →",
                l("詳細(画面と報告の履歴)", "details (screen and reports)"),
            ),
            ("Esc h ←", l("一覧に戻る", "back to the list")),
            (
                "g",
                l(
                    "ワーカーをこの下に開く(承認はそこで答える)",
                    "open the worker below (answer approvals there)",
                ),
            ),
            (
                BACK_TO_BOARD,
                l(
                    "開いたワーカーからボードへ戻る",
                    "back to the board from an opened worker",
                ),
            ),
            (
                "s",
                l(
                    "ワーカーに送る(承認待ち・終了したワーカーには送れません)",
                    "send to the worker (not to one awaiting approval or exited)",
                ),
            ),
            (
                "x",
                l(
                    "ワーカーを閉じる(確認あり。出力は消えます)",
                    "close the worker (asks first; its output is lost)",
                ),
            ),
            (
                "q ^C",
                l(
                    "ボードを終える(ワーカーはオーケストレーターへ戻る)",
                    "end the board (workers go back to the orchestrator)",
                ),
            ),
            ("", ""),
            (l("送信欄", "Send line"), ""),
            ("Enter / Esc", l("送る / 取り消す", "send / cancel")),
            ("^U / ^W", l("全部消す / 1 語消す", "clear / delete a word")),
        ];
        for (key, what) in keys {
            let mut line = Line::default();
            if what.is_empty() {
                line.plain(" ").bold(key);
            } else {
                line.plain("   ")
                    .plain(&pad(key, 12))
                    .fg(DIM, &fit(what, cols.saturating_sub(16)));
            }
            lines.push(line.finish(cols));
        }
        let room = rows.saturating_sub(1);
        let mut out = vec![title.finish(cols)];
        if lines.len() <= room {
            self.help_reach.set(0);
            out.extend(lines);
            return (out, false);
        }
        // One line for the marker; the rest scroll.
        let view = room.saturating_sub(1).max(1);
        let reach = lines.len() - view;
        self.help_reach.set(reach);
        let offset = self.help_scroll.min(reach);
        out.extend(lines.into_iter().skip(offset).take(view));
        let mut marker = Line::default();
        let text = if offset < reach {
            self.lang
                .pick("↓ 続き  j/k でスクロール", "↓ more  j/k to scroll")
        } else {
            self.lang.pick("↑ j/k でスクロール", "↑ j/k to scroll")
        };
        marker
            .plain("   ")
            .fg(DIM, &fit(text, cols.saturating_sub(3)));
        out.push(marker.finish(cols));
        (out, true)
    }

    fn header(&self, cols: usize, now: Instant) -> Vec<String> {
        let workers = self.workers();
        // When the list was last read: a stale one says how stale (an
        // unreachable server is said on the notice line too).
        let synced = match self.synced {
            Some(at) if now.saturating_duration_since(at) < Duration::from_secs(2) => {
                self.lang.pick("いま", "now").to_string()
            }
            Some(at) => ago(Some(at), now, self.lang),
            None => self.lang.pick("まだ", "never").to_string(),
        };
        let n = workers.len();
        let count = self
            .lang
            .pick(format!("ワーカー {n} "), format!("workers {n} "));
        let long_count = self.lang.pick(
            format!("ワーカー {n} · 同期 {synced} "),
            format!("workers {n} · synced {synced} "),
        );
        // A narrow pane gets the short title before the count is cut.
        const LONG: &str = "NOA / CONTROL";
        const SCOPE_ROOM: usize = 8;
        let name = if 1 + LONG.width() + 2 + SCOPE_ROOM + count.width() <= cols {
            LONG
        } else {
            self.lang.pick("ボード", "Board")
        };
        let count = if 1 + name.width() + 2 + SCOPE_ROOM + long_count.width() <= cols {
            long_count
        } else {
            count
        };
        let mut title = Line::default();
        title.plain(" ").bold(name).plain("  ");
        // The orchestrator by its pane's name (its session title), which a
        // person recognises; the pane id only when it has none.
        let scope = match self.of {
            Some(of) => {
                let panel = self.panel(of);
                let name = panel
                    .map(|panel| clean(&panel.name))
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| format!("pane {of}"));
                match panel.map(worker_kind).filter(|kind| !kind.is_empty()) {
                    Some(kind) => format!("{name} · {kind}"),
                    None => name,
                }
            }
            None => self
                .lang
                .pick("すべてのワーカー", "all workers")
                .to_string(),
        };
        let room = cols.saturating_sub(title.width + count.width() + 1);
        title.fg(DIM, &fit(&scope, room));
        let pad = cols.saturating_sub(title.width + count.width());
        title.plain(&" ".repeat(pad)).fg(DIM, &count);

        let mut chips = Line::default();
        chips.plain(" ");
        for group in Group::ALL {
            let n = workers
                .iter()
                .filter(|panel| Status::of(panel).group() == group)
                .count();
            let text = format!(" {} {n}", group.title(self.lang));
            // An empty section is dimmed, so the ones with workers stand out.
            if n == 0 {
                chips.fg(BORDER, group.mark()).fg(BORDER, &text);
            } else if group == Group::Needs {
                chips
                    .fg(group.color(), group.mark())
                    .fg(group.color(), &text);
            } else {
                chips.fg(group.color(), group.mark()).plain(&text);
            }
            chips.plain("   ");
        }
        vec![
            title.finish(cols),
            chips.finish(cols),
            self.notice_line(cols, now),
        ]
    }

    /// The list (D15): the queue of workers needing someone, the tree of
    /// the work under way, and the finished and exited workers; with the
    /// selected worker's screen beside it in a wide pane, under it in a
    /// tall enough one, and not at all in a narrow one (`Enter` shows it).
    fn render_list(&self, cols: usize, rows: usize, now: Instant) -> Vec<String> {
        let mut lines = self.header(cols, now);
        let room = rows.saturating_sub(lines.len() + 1);
        if self.workers().is_empty() {
            lines.extend(self.empty_lines(cols, room, now));
            return lines;
        }
        let preview = self.selected.is_some();
        if preview && cols >= WIDE_COLS {
            let right = cols * 2 / 5;
            let left = cols - right - 1;
            let list = self.list_lines(left, room, now);
            let side = self.preview_lines(right, room, now);
            let mut bar = Line::default();
            bar.fg(BORDER, "│");
            let bar = bar.finish(1);
            for (list, side) in list.into_iter().zip(side) {
                lines.push(format!("{list}{bar}{side}"));
            }
        } else if preview && cols >= STACKED_COLS && rows >= STACKED_ROWS {
            let height = (room / 3).clamp(3, 8);
            lines.extend(self.list_lines(cols, room - height, now));
            lines.extend(self.preview_lines(cols, height, now));
        } else {
            lines.extend(self.list_lines(cols, room, now));
        }
        lines
    }

    /// With no worker left: a note, and the closed section, which `j`/`k`
    /// scroll by `past`.
    fn empty_lines(&self, cols: usize, room: usize, now: Instant) -> Vec<String> {
        let mut empty = Line::default();
        // No worker can come any more once the orchestrator closed.
        let note = if self.of_gone {
            self.lang.pick(
                "オーケストレーターのペインは閉じられました。q でボードを終えます。",
                "The orchestrator pane closed. q ends the board.",
            )
        } else {
            self.lang.pick(
                "ワーカーはまだいません。オーケストレーターが `+ctl spawn` すると、ここに並びます。",
                "No workers yet. They appear here once the orchestrator runs `+ctl spawn`.",
            )
        };
        empty.plain(" ").fg(DIM, &fit(note, cols - 1));
        let mut body = vec![empty.finish(cols)];
        body.extend(self.gone_lines(cols, now));
        // The closed section scrolls by `past`; its entries start after
        // the note and the section title.
        let marks = room >= 3;
        let (mut above, mut below) = (false, false);
        let mut offset = 0;
        let gone_hidden = |offset: usize, avail: usize| {
            let lines = 2..body.len();
            let above = lines.clone().filter(|&line| line < offset).count();
            let below = lines.filter(|&line| line >= offset + avail).count();
            (above, below)
        };
        for _ in 0..3 {
            let avail = room.saturating_sub(above as usize + below as usize).max(1);
            offset = self.past.min(body.len().saturating_sub(avail));
            let (up, down) = gone_hidden(offset, avail);
            let now = (marks && up > 0, marks && down > 0);
            if now == (above, below) {
                break;
            }
            (above, below) = now;
        }
        let avail = room.saturating_sub(above as usize + below as usize).max(1);
        let (up, down) = gone_hidden(offset, avail);
        let mut lines = Vec::new();
        if above {
            lines.push(more_line("↑", (0, 0), up, cols, self.lang));
        }
        lines.extend(body.iter().skip(offset).take(avail).cloned());
        if below {
            lines.push(more_line("↓", (0, 0), down, cols, self.lang));
        }
        lines
    }

    /// The rows of the list in exactly `room` lines of `cols` columns,
    /// scrolled to keep the selected worker in view.
    fn list_lines(&self, cols: usize, room: usize, now: Instant) -> Vec<String> {
        let tree = self.tree();
        let shown = self.rows();
        let kid_indent = self.kid_indent();
        let tally = |entry: &Entry| {
            (!entry.queued)
                .then(|| self.tally(&tree, &entry.node))
                .flatten()
        };
        let widest_tally = |form: StateForm| {
            shown
                .iter()
                .filter_map(&tally)
                .map(|tally| tally_width(tally, form, self.lang))
                .max()
                .unwrap_or(0)
        };
        // A worker's worker in the queue is named after its parent too.
        let named: Vec<(&Panel, usize)> = shown
            .iter()
            .map(|entry| {
                let panel = entry.node.panel;
                match (entry.queued, entry.node.branch) {
                    (_, Branch::Top { .. }) => (panel, 0),
                    (true, Branch::Kid { parent, .. }) => (panel, self.crumb(parent).width()),
                    (false, Branch::Kid { .. }) => (panel, kid_indent),
                }
            })
            .collect();
        let tallies = [
            widest_tally(StateForm::Full),
            widest_tally(StateForm::Short),
            widest_tally(StateForm::Bare),
        ];
        let columns = Columns::of(&named, tallies, cols, |panel, form| {
            self.state_text(panel.pane_id.0, Status::of(panel), form, now)
        });
        let space_on_subtree = self.space_target().is_some();
        let mut body: Vec<String> = Vec::new();
        // Each selectable row's line in `body`, and its worker's group.
        let mut spans: Vec<(usize, Group)> = Vec::new();
        let mut selected_line = None;
        let mut last_section = None;
        for entry in &shown {
            let section = if entry.queued {
                Zone::Queue
            } else {
                entry.node.zone
            };
            if section == Zone::Settled {
                break;
            }
            if last_section != Some(section) {
                last_section = Some(section);
                body.push(self.section_rule(section, &tree, space_on_subtree, cols));
            }
            if !entry.echo {
                let pane = entry.node.panel.pane_id.0;
                spans.push((body.len(), Status::of(entry.node.panel).group()));
                if Some(pane) == self.selected {
                    selected_line = Some(body.len());
                }
            }
            body.push(self.row(entry, tally(entry), &columns, cols, now));
        }
        // The finished and exited workers: counted on one line, listed
        // under it once opened, with the workers closed since the board
        // opened.
        let settled_line = body.len();
        let settled = tree.iter().any(|node| node.zone == Zone::Settled);
        if settled || !self.gone.is_empty() {
            body.push(self.section_rule(Zone::Settled, &tree, space_on_subtree, cols));
        }
        let mut gone_start = None;
        if self.settled_open {
            for entry in shown
                .iter()
                .filter(|entry| !entry.queued && entry.node.zone == Zone::Settled)
            {
                let pane = entry.node.panel.pane_id.0;
                spans.push((body.len(), Status::of(entry.node.panel).group()));
                if Some(pane) == self.selected {
                    selected_line = Some(body.len());
                }
                body.push(self.row(entry, tally(entry), &columns, cols, now));
            }
            let gone = self.gone_lines(cols, now);
            if !gone.is_empty() {
                gone_start = Some(body.len() + 1);
                body.extend(gone);
            }
        }
        // A selection folded away is at the line that counts it.
        let selected_line = selected_line.unwrap_or(if self.selected.is_some() {
            settled_line.min(body.len().saturating_sub(1))
        } else {
            0
        });

        // Scroll so the selected worker is in view, `past` lines further
        // once `j` went on past the last one. A cut end says how many
        // workers it hides, and how many of them need someone, on a line of
        // its own.
        let last_pick = spans.last().map(|span| span.0);
        let sel_start = selected_line;
        let sel_end = if Some(selected_line) == last_pick {
            body.len()
        } else {
            selected_line + 1
        };
        let hidden = |offset: usize, avail: usize| {
            let end = offset + avail;
            let above: Vec<_> = spans.iter().filter(|span| span.0 < offset).collect();
            let below: Vec<_> = spans.iter().filter(|span| span.0 >= end).collect();
            let needs = |list: &[&(usize, Group)]| {
                list.iter().filter(|span| span.1 == Group::Needs).count()
            };
            let gone = gone_start.map_or(0, |start| {
                (start..body.len()).filter(|&line| line >= end).count()
            });
            Hidden {
                above: (above.len(), needs(&above)),
                below: (below.len(), needs(&below)),
                gone,
            }
        };
        let (mut above, mut below) = (false, false);
        let mut offset = 0;
        // A pane too short for the markers spends its rows on the selection.
        let marks = room >= 3;
        for _ in 0..3 {
            let avail = room.saturating_sub(above as usize + below as usize).max(1);
            offset = sel_end.saturating_sub(avail).min(sel_start + self.past);
            let seen = hidden(offset, avail);
            let now_above = marks && seen.above.0 > 0;
            let now_below = marks && (seen.below.0 > 0 || seen.gone > 0);
            if (now_above, now_below) == (above, below) {
                break;
            }
            (above, below) = (now_above, now_below);
        }
        let avail = room.saturating_sub(above as usize + below as usize).max(1);
        let seen = hidden(offset, avail);
        let mut lines = Vec::new();
        if above {
            lines.push(more_line("↑", seen.above, 0, cols, self.lang));
        }
        lines.extend(body.iter().skip(offset).take(avail).cloned());
        if below {
            lines.push(more_line("↓", seen.below, seen.gone, cols, self.lang));
        }
        lines.truncate(room);
        while lines.len() < room {
            lines.push(Line::default().finish(cols));
        }
        lines
    }

    /// "planner › ": the parent a worker's worker is named after in the
    /// queue and the preview. The parent's name is cut short, so a long one never crowds out the
    /// worker's own.
    fn crumb(&self, parent: u64) -> String {
        format!("{} › ", fit(&self.name(parent), CRUMB_WIDTH))
    }

    /// A section's title rule, with how many it holds and, where a key
    /// acts on it, that key.
    fn section_rule(
        &self,
        zone: Zone,
        tree: &[Node],
        space_on_subtree: bool,
        cols: usize,
    ) -> String {
        let count = |group: Group| {
            tree.iter()
                .filter(|node| node.zone == zone && Status::of(node.panel).group() == group)
                .count()
        };
        match zone {
            Zone::Queue => {
                let n = tree
                    .iter()
                    .filter(|node| Status::of(node.panel).group() == Group::Needs)
                    .count();
                rule(
                    cols,
                    &[
                        (
                            Some(ORANGE),
                            self.lang
                                .pick("! 人の対応が必要", "! Needs you")
                                .to_string(),
                        ),
                        (Some(DIM), format!(" {n}")),
                    ],
                    Some(self.lang.pick("n 次の要対応", "n next")),
                )
            }
            Zone::Background => {
                let n = tree
                    .iter()
                    .filter(|node| node.zone == Zone::Background)
                    .filter(|node| Status::of(node.panel).group() != Group::Needs)
                    .count();
                let hint = match (space_on_subtree, self.space_folded()) {
                    (true, Some(true)) => Some(self.lang.pick("Space 開く", "Space open")),
                    (true, _) => Some(self.lang.pick("Space たたむ", "Space fold")),
                    (false, _) => None,
                };
                rule(
                    cols,
                    &[
                        (
                            Some(BLUE),
                            self.lang
                                .pick("● バックグラウンド", "● Background")
                                .to_string(),
                        ),
                        (Some(DIM), format!(" {n}")),
                    ],
                    hint,
                )
            }
            Zone::Settled => {
                // A selection folded away in here is marked on this line.
                let hidden_selection = !self.settled_open
                    && self.selected.is_some_and(|pane| {
                        tree.iter()
                            .any(|node| node.panel.pane_id.0 == pane && node.zone == Zone::Settled)
                    });
                let mut title = Vec::new();
                if hidden_selection {
                    title.push((Some(GREEN), "▶ ".to_string()));
                }
                let l = |ja, en| self.lang.pick(ja, en);
                let done = count(Group::Done);
                let exited = count(Group::Exited);
                title.push((Some(GREEN), format!("✓ {} {done}", l("完了", "done"))));
                title.push((Some(GREY), format!("   · {} {exited}", l("終了", "exited"))));
                if !self.gone.is_empty() {
                    let gone = self.gone.len();
                    title.push((Some(DIM), format!("   {} {gone}", l("閉じた", "closed"))));
                }
                let hint = match (space_on_subtree, self.settled_open) {
                    (true, _) => None,
                    (false, true) => Some(l("Space たたむ", "Space fold")),
                    (false, false) => Some(l("Space 開く", "Space open")),
                };
                rule(cols, &title, hint)
            }
        }
    }

    /// The selected worker's screen in exactly `height` lines of `cols`
    /// columns: a title, the end of its screen, and what it is.
    fn preview_lines(&self, cols: usize, height: usize, now: Instant) -> Vec<String> {
        let mut out = Vec::new();
        let blank = || Line::default().finish(cols);
        let Some((pane, panel)) = self
            .selected
            .and_then(|pane| self.panel(pane).map(|panel| (pane, panel)))
        else {
            return vec![blank(); height];
        };
        let status = Status::of(panel);
        let mut name = worker_name(panel);
        if let Some(parent) = self.tree_parent(pane) {
            name = format!("{}{name}", self.crumb(parent));
        }
        out.push(rule(
            cols,
            &[
                (Some(BLUE), self.lang.pick("選択中", "Selected").to_string()),
                (Some(DIM), format!(" {} ", self.number(pane))),
                (Some(status.color()), format!("{} ", status.mark())),
                (None, name),
            ],
            Some(self.lang.pick("プレビュー", "preview")),
        ));
        // A worker waiting on someone asks at the bottom of its screen, in
        // the agent's input box or a dialog in its place; one at work shows
        // the work above that box.
        let screen: Vec<&str> = self
            .screens
            .get(&pane)
            .map(|lines| {
                let lines = if status.group() == Group::Needs {
                    &lines[..]
                } else {
                    content_lines(lines)
                };
                lines
                    .iter()
                    .map(|line| line.trim_end())
                    .filter(|line| !line.trim().is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let room = height.saturating_sub(2);
        let mut text: Vec<String> = screen
            .iter()
            .skip(screen.len().saturating_sub(room))
            .map(|line| {
                let mut out = Line::default();
                out.plain(" ")
                    .plain(&fit(&clean(line), cols.saturating_sub(2)));
                out.finish(cols)
            })
            .collect();
        if text.is_empty() && room > 0 {
            let note = if self.screens.contains_key(&pane) {
                self.lang
                    .pick("画面に何も出ていません", "nothing on its screen")
            } else {
                self.lang.pick("画面を読んでいます…", "reading its screen…")
            };
            let mut line = Line::default();
            line.plain(" ").fg(DIM, note);
            text.push(line.finish(cols));
        }
        out.extend(text);
        while out.len() + 1 < height {
            out.push(blank());
        }
        // What it is: its program, state, report count and directory.
        let mut meta = Line::default();
        meta.plain(" ");
        let kind = worker_kind(panel);
        if !kind.is_empty() {
            meta.fg(DIM, &format!("{kind} · "));
        }
        meta.fg(
            status.color(),
            &self.state_text(pane, status, StateForm::Full, now),
        );
        if let Some(agent) = &panel.agent {
            let report = self.lang.pick("報告", "report");
            meta.fg(DIM, &format!(" · {report} {}", agent.seq));
        }
        // The directory only where a few of its columns fit: a lone `…`
        // or separator says nothing.
        let cwd = clean(&panel.cwd);
        let room = cols.saturating_sub(meta.width + 4);
        if !cwd.is_empty() && room >= 4 {
            meta.fg(DIM, &format!(" · {}", fit(&cwd, room)));
        }
        if height > 1 {
            out.push(meta.finish(cols));
        }
        out.truncate(height);
        out
    }

    /// The state column: "承認待ち 5 秒", "完了 3 分前", or shorter.
    fn state_text(&self, pane: u64, status: Status, form: StateForm, now: Instant) -> String {
        match self.state_parts(pane, status, form, now) {
            (state, Some(quiet)) => format!("{state} {quiet}"),
            (state, None) => state,
        }
    }

    /// The state column in its two styles: the state and its age, and for a
    /// working worker when it last showed output, "8 秒前に出力", or once
    /// its screen has not moved for a while, "出力なし 3 分" (drawn dimmed,
    /// in place of the age).
    fn state_parts(
        &self,
        pane: u64,
        status: Status,
        form: StateForm,
        now: Instant,
    ) -> (String, Option<String>) {
        let label = match form {
            StateForm::Full => status.label(self.lang),
            StateForm::Short | StateForm::Bare => status.short_label(self.lang),
        };
        if form == StateForm::Bare {
            return (label, None);
        }
        if let Some(span) = self.quiet_for(pane, status) {
            let span = duration_text(span, self.lang);
            let quiet = self
                .lang
                .pick(format!("出力なし {span}"), format!("quiet {span}"));
            return (label, Some(quiet));
        }
        // At work: how fresh its output is, not how long it has worked
        // (D15), once its screen was seen to move in its current state.
        if status.group() == Group::Working {
            let began = self.since.get(&pane).and_then(|(_, at)| *at);
            return match self.moved.get(&pane) {
                Some(&at) if began.is_none_or(|began| at >= began) => {
                    let ago = ago(Some(at), now, self.lang);
                    let text = match (form, self.lang) {
                        (StateForm::Full, Lang::Ja) => format!("{label} {ago}に出力"),
                        (StateForm::Full, Lang::En) => format!("{label}, output {ago}"),
                        _ => format!("{label} {ago}"),
                    };
                    (text, None)
                }
                _ => (label, None),
            };
        }
        match self.state_age(pane, status, now) {
            Some(age) => (format!("{label} {age}"), None),
            None => (label, None),
        }
    }

    /// Workers closed while the board watched, and what they last said.
    fn gone_lines(&self, cols: usize, now: Instant) -> Vec<String> {
        if self.gone.is_empty() {
            return Vec::new();
        }
        let mut title = Line::default();
        title
            .plain(" ")
            .fg(GREY, self.lang.pick("閉じた", "Closed"))
            .fg(
                DIM,
                self.lang
                    .pick("  このボードを開いてから", "  since this board opened"),
            );
        let mut lines = vec![title.finish(cols)];
        for gone in &self.gone {
            let mut line = Line::default();
            line.plain("   ")
                .fg(GREY, "✕ ")
                .plain(&fit(&gone.name, 20))
                .fg(DIM, &{
                    let label = gone.status.label(self.lang);
                    let ago = ago(Some(gone.at), now, self.lang);
                    self.lang.pick(
                        format!("  {label}で閉じた · {ago}"),
                        format!("  closed while {label} · {ago}"),
                    )
                });
            if !gone.detail.is_empty() {
                let room = cols.saturating_sub(line.width + 3);
                line.plain("  ").fg(DIM, &fit(&clean(&gone.detail), room));
            }
            lines.push(line.finish(cols));
        }
        lines
    }

    /// How long `pane` has been in `status`: "2 分" while it lasts, "3 分前"
    /// once it is done; nothing when it predates the board, whose start
    /// says nothing about it.
    fn state_age(&self, pane: u64, status: Status, now: Instant) -> Option<String> {
        let Some((_, Some(at))) = self.since.get(&pane) else {
            return None;
        };
        // A failed process is listed with what needs someone, but it is
        // over: say when, like any other ended state.
        let over = matches!(status, Status::Exited(_)) || status.group() == Group::Done;
        Some(if over {
            ago(Some(*at), now, self.lang)
        } else {
            duration_text(now.saturating_duration_since(*at), self.lang)
        })
    }

    /// Two columns for numbers once any worker's has two digits.
    fn number_width(&self) -> usize {
        if self.numbers.values().any(|n| *n >= 10) {
            2
        } else {
            1
        }
    }

    /// How much further in a worker's worker's name starts than its
    /// parent's: its tree line hangs under the parent's dot.
    fn kid_indent(&self) -> usize {
        3 + self.number_width()
    }

    /// The tally on `node`'s row, when it has workers of its own.
    fn tally(&self, tree: &[Node], node: &Node) -> Option<Tally> {
        let Branch::Top { kids } = node.branch else {
            return None;
        };
        if kids == 0 {
            return None;
        }
        let pane = node.panel.pane_id.0;
        let needs = tree
            .iter()
            .filter(|kid| matches!(kid.branch, Branch::Kid { parent, .. } if parent == pane))
            .filter(|kid| Status::of(kid.panel).group() == Group::Needs)
            .count();
        Some(Tally {
            kids,
            needs,
            collapsed: self.collapsed.contains(&pane),
        })
    }

    /// A worker's row, `▶ 1 ! name  kind  state age  tally  detail`. A
    /// worker's worker is drawn further in on a tree line (`├`, `└`) that
    /// hangs from its parent's mark, or in the queue after its parent's
    /// name. An echo of a queue row is dimmed and points up at it.
    fn row(
        &self,
        entry: &Entry,
        tally: Option<Tally>,
        columns: &Columns,
        cols: usize,
        now: Instant,
    ) -> String {
        let panel = entry.node.panel;
        let pane = panel.pane_id.0;
        let status = Status::of(panel);
        let selected = Some(pane) == self.selected && !entry.echo;
        let width = self.number_width();

        let mut line = Line::default();
        line.plain(" ");
        if selected {
            line.fg(status.color(), "▶");
        } else {
            line.plain(" ");
        }
        let (name_width, crumb) = match (entry.queued, entry.node.branch) {
            (_, Branch::Top { .. }) => (columns.name, String::new()),
            (true, Branch::Kid { parent, .. }) => (columns.name, self.crumb(parent)),
            (false, Branch::Kid { last, .. }) => {
                line.plain(&" ".repeat(width + 1))
                    .fg(BORDER, if last { "└" } else { "├" })
                    .plain(" ");
                let indent = self.kid_indent();
                let name = columns.name.saturating_sub(indent).max(columns.name.min(4));
                (name, String::new())
            }
        };
        let number = format!("{:>width$}", self.number(pane));
        line.fg(DIM, &number)
            .plain(" ")
            .fg(status.color(), status.mark())
            .plain(" ");
        let name = pad(&format!("{crumb}{}", worker_name(panel)), name_width);
        if selected {
            line.bold(&name);
        } else if entry.echo {
            line.fg(DIM, &name);
        } else {
            line.plain(&name);
        }
        if columns.kind > 0 {
            line.plain("  ")
                .fg(DIM, &pad(&worker_kind(panel), columns.kind));
        }
        if columns.state > 0 {
            let (state, quiet) = self.state_parts(pane, status, columns.form, now);
            line.plain("  ").fg(status.color(), &state);
            let mut used = state.width();
            if let Some(quiet) = quiet {
                line.fg(DIM, &format!(" {quiet}"));
                used += 1 + quiet.width();
            }
            line.plain(&" ".repeat(columns.state.saturating_sub(used)));
        }
        if columns.tally > 0 {
            line.plain("  ");
            let mut used = 0;
            for (color, text) in tally
                .map(|tally| tally_pieces(tally, columns.tally_form, self.lang))
                .unwrap_or_default()
            {
                line.fg(color, &text);
                used += text.width();
            }
            line.plain(&" ".repeat(columns.tally.saturating_sub(used)));
        }
        let room = cols.saturating_sub(line.width + 2);
        if entry.echo {
            let echo = self.lang.pick("↑ 要対応に表示", "↑ in the queue above");
            line.plain("  ").fg(DIM, &fit(echo, room));
        } else {
            // What it said, else, while it needs someone, the last line of
            // its screen (its question is in the input box, under the work),
            // and while it works, the last line of its work.
            let mut detail = self.last_word(panel);
            if detail.is_empty() {
                let line = match status.group() {
                    Group::Needs => self.last_screen_line(pane),
                    Group::Working => self.last_output(pane),
                    Group::Done | Group::Exited => None,
                };
                detail = line.unwrap_or_default().to_string();
            }
            if !detail.is_empty() {
                line.plain("  ").fg(DIM, &fit(&clean(&detail), room));
            }
        }
        line.finish(cols)
    }

    fn render_detail(&self, pane: u64, cols: usize, rows: usize, now: Instant) -> Vec<String> {
        let mut lines = Vec::new();
        let Some(panel) = self.panel(pane) else {
            return lines;
        };
        let status = Status::of(panel);
        let mut head = Line::default();
        head.plain(" ")
            .fg(
                DIM,
                self.lang.pick("← 一覧 (Esc)  /  ", "← list (Esc)  /  "),
            )
            .bold(&fit(&worker_name(panel), cols / 3));
        let kind = worker_kind(panel);
        if !kind.is_empty() {
            head.plain("  ").fg(DIM, &kind);
        }
        head.plain("  ")
            .fg(status.color(), &status.label(self.lang));
        let right = match &panel.agent {
            Some(agent) => format!(
                "pane {pane} · {} {} ",
                self.lang.pick("報告", "report"),
                agent.seq
            ),
            None => format!("pane {pane} "),
        };
        if head.width + right.width() < cols {
            let pad = cols - head.width - right.width();
            head.plain(&" ".repeat(pad)).fg(DIM, &right);
        }
        lines.push(head.finish(cols));
        lines.push(self.notice_line(cols, now));

        let history: &[Report] = self.history.get(&pane).map(Vec::as_slice).unwrap_or(&[]);
        let history_rows = history.len().clamp(1, 6);
        // Title + box borders + history title + rows + gap, and the footer
        // below.
        let screen_rows = rows
            .saturating_sub(lines.len() + 4 + 2 + history_rows + 1)
            .max(3);

        let mut title = Line::default();
        title.plain(" ").bold(self.lang.pick("画面", "Screen")).fg(
            DIM,
            self.lang
                .pick("  ライブ · 読み取り専用", "  live · read-only"),
        );
        lines.push(title.finish(cols));
        let border = if status.group() == Group::Needs {
            status.color()
        } else {
            BORDER
        };
        let inner = cols - 4;
        let mut top = Line::default();
        top.fg(border, &format!(" ┌{}┐", "─".repeat(cols - 3)));
        lines.push(top.finish(cols));
        let screen = self.screens.get(&pane).cloned().unwrap_or_default();
        let skip = screen.len().saturating_sub(screen_rows);
        for index in 0..screen_rows {
            let text = screen
                .get(skip + index)
                .map(|s| clean(s))
                .unwrap_or_default();
            let text = fit(&text, inner.saturating_sub(1));
            let mut line = Line::default();
            line.fg(border, " │ ").plain(&text);
            line.plain(&" ".repeat(inner.saturating_sub(1 + text.width())));
            line.fg(border, " │");
            lines.push(line.finish(cols));
        }
        let mut bottom = Line::default();
        bottom.fg(border, &format!(" └{}┘", "─".repeat(cols - 3)));
        lines.push(bottom.finish(cols));

        let mut title = Line::default();
        title
            .plain(" ")
            .bold(self.lang.pick("報告の履歴", "Reports"))
            .fg(
                DIM,
                self.lang
                    .pick("  このボードを開いてから", "  since this board opened"),
            );
        lines.push(title.finish(cols));
        if history.is_empty() {
            let mut line = Line::default();
            line.plain("   ").fg(
                DIM,
                self.lang.pick("報告はまだありません", "no reports yet"),
            );
            lines.push(line.finish(cols));
        }
        for report in history.iter().take(history_rows) {
            let mut line = Line::default();
            let when = ago(report.at, now, self.lang);
            line.plain("   ")
                .fg(
                    DIM,
                    &format!("{} {:<4}", self.lang.pick("報告", "report"), report.seq),
                )
                .plain(" ")
                .fg(
                    report.status.color(),
                    &pad(&report.status.label(self.lang), 8),
                );
            let room = cols.saturating_sub(line.width + when.width() + 3);
            let detail = fit(&clean(&report.detail), room);
            line.plain(" ").plain(&detail);
            let pad = cols.saturating_sub(line.width + when.width() + 1);
            line.plain(&" ".repeat(pad)).fg(DIM, &when);
            lines.push(line.finish(cols));
        }
        lines
    }

    fn footer(&self, cols: usize, help_cut: bool) -> String {
        let mut line = Line::default();
        let l = |ja, en| self.lang.pick(ja, en);
        // (key, what, what when the line is tight), most important first.
        let keys: Vec<(&str, &str, &str)> = match &self.mode {
            Mode::Send(..) => vec![
                ("Enter", l("送信", "send"), l("送信", "send")),
                ("Esc", l("取り消し", "cancel"), l("取消", "cancel")),
                ("^U", l("全部消す", "clear"), l("全消", "clear")),
                ("^W", l("1 語消す", "delete word"), l("語消", "word")),
            ],
            Mode::ConfirmClose(_) => vec![
                ("y", l("閉じる", "close"), l("閉じる", "close")),
                (
                    l("ほかのキー", "other keys"),
                    l("取り消し", "cancel"),
                    l("取消", "cancel"),
                ),
            ],
            Mode::TargetGone(_) => vec![("Esc", l("続ける", "go on"), l("続ける", "go on"))],
            Mode::ConfirmQuit => vec![
                ("q", l("終了", "quit"), l("終了", "quit")),
                (
                    l("ほかのキー", "other keys"),
                    l("取り消し", "cancel"),
                    l("取消", "cancel"),
                ),
            ],
            Mode::Normal if self.help => {
                let text = if help_cut {
                    l(
                        " j/k スクロール   ほかのキーで閉じます",
                        " j/k scroll   other keys close",
                    )
                } else {
                    l(" どのキーでもヘルプを閉じます", " any key closes the help")
                };
                line.fg(DIM, text);
                return line.finish(cols);
            }
            Mode::Normal => {
                let approval = self.target().is_some_and(|pane| self.awaits_approval(pane));
                let go = if approval {
                    l("移動して答える", "go and answer")
                } else {
                    l("移動", "go")
                };
                // Nothing can be sent to an exited worker.
                let exited = self.target().is_some_and(|pane| self.has_exited(pane));
                let mut keys = vec![("g", go, l("移動", "go"))];
                if !exited {
                    keys.push(("s", l("送る", "send"), l("送る", "send")));
                }
                keys.extend([
                    ("x", l("閉じる", "close"), l("閉じる", "close")),
                    ("q", l("終了", "quit"), l("終了", "quit")),
                ]);
                match self.view {
                    View::List => {
                        keys.extend([
                            ("Enter", l("詳細", "details"), l("詳細", "details")),
                            ("n", l("次の要対応", "next"), l("次", "next")),
                            ("j/k", l("選択", "select"), l("選択", "select")),
                        ]);
                        // What Space does: to the subtree the selection is
                        // in, else to the finished and exited workers.
                        if let Some(folded) = self.space_folded() {
                            let what = match (self.space_target().is_some(), folded) {
                                (true, true) => l("開く", "open"),
                                (true, false) => l("たたむ", "fold"),
                                (false, true) => l("完了を開く", "open done"),
                                (false, false) => l("完了をたたむ", "fold done"),
                            };
                            keys.push(("Space", what, what));
                        }
                    }
                    View::Detail(_) => keys.extend([
                        ("Esc", l("一覧", "list"), l("一覧", "list")),
                        ("j/k", l("前後", "prev/next"), l("前後", "prev/next")),
                    ]),
                }
                keys
            }
        };
        let help = matches!(self.mode, Mode::Normal);
        let help_word = self.lang.pick(" ヘルプ", " help");
        let more = if help { 1 + help_word.width() } else { 0 };
        // Every key in full, else every key short, else the most important
        // ones that fit (short).
        let width = |n: usize, short: bool| {
            let gap = if short { 2 } else { 3 };
            1 + keys[..n]
                .iter()
                .map(|(key, long, brief)| {
                    key.width() + 1 + if short { brief } else { long }.width() + gap
                })
                .sum::<usize>()
                + more
        };
        let (n, short) = if width(keys.len(), false) <= cols {
            (keys.len(), false)
        } else {
            let n = (0..=keys.len())
                .rev()
                .find(|&n| width(n, true) <= cols)
                .unwrap_or(0);
            (n, true)
        };
        let gap = if short { "  " } else { "   " };
        line.plain(" ");
        for (key, long, brief) in &keys[..n] {
            let what = if short { brief } else { long };
            line.plain(key).fg(DIM, &format!(" {what}{gap}"));
        }
        if help {
            line.plain("?").fg(DIM, help_word);
        }
        line.finish(cols)
    }
}

// ---------------------------------------------------------------------------
// Terminal and connection

/// Raw mode on the alternate screen, put back when dropped.
struct Screen {
    saved: Option<libc::termios>,
}

impl Screen {
    fn enter() -> Self {
        // SAFETY: tcgetattr/tcsetattr on stdin with a zeroed termios they fill.
        let saved = unsafe {
            let mut term: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut term) == 0 {
                let saved = term;
                term.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN);
                term.c_iflag &= !(libc::IXON | libc::ICRNL);
                term.c_cc[libc::VMIN] = 1;
                term.c_cc[libc::VTIME] = 0;
                libc::tcsetattr(0, libc::TCSANOW, &term);
                Some(saved)
            } else {
                None
            }
        };
        let mut out = std::io::stdout().lock();
        // Bracketed paste, so pasted text is never read as keys; the pane
        // title is saved (CSI 22 t) for `set_title`, and put back on drop.
        let _ = out.write_all(b"\x1b[22;0t\x1b[?1049h\x1b[?25l\x1b[?2004h\x1b[2J");
        let _ = out.flush();
        Self { saved }
    }

    fn size() -> (usize, usize) {
        // SAFETY: TIOCGWINSZ fills a zeroed winsize.
        unsafe {
            let mut size: libc::winsize = std::mem::zeroed();
            if libc::ioctl(1, libc::TIOCGWINSZ, &mut size) == 0 && size.ws_col > 0 {
                (size.ws_col as usize, size.ws_row as usize)
            } else {
                (80, 24)
            }
        }
    }

    /// The pane's title (OSC 2), from text the board built itself.
    fn set_title(title: &str) {
        let mut out = std::io::stdout().lock();
        let _ = write!(out, "\x1b]2;{}\x07", clean(title));
        let _ = out.flush();
    }

    fn draw(lines: &[String]) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"\x1b[H");
        let _ = out.write_all(lines.join("\r\n").as_bytes());
        let _ = out.flush();
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"\x1b[0m\x1b[?2004l\x1b[?25h\x1b[?1049l\x1b[23;0t");
        let _ = out.flush();
        if let Some(saved) = self.saved {
            // SAFETY: restores the termios read in `enter`.
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, &saved);
            }
        }
    }
}

/// When to read the pane list next. A failed listing is tried again after
/// a growing wait, and only a run of failures [`GIVE_UP_AFTER`] long ends
/// the board: one timeout or a server restart must not close it, which
/// would hand every worker back to the orchestrator (D10).
#[derive(Default)]
pub(crate) struct Polling {
    next: Option<Instant>,
    failing_since: Option<Instant>,
    delay: Duration,
}

impl Polling {
    pub(crate) fn due(&self, now: Instant) -> bool {
        self.next.is_none_or(|next| now >= next)
    }

    pub(crate) fn succeeded(&mut self, now: Instant) {
        *self = Self {
            next: Some(now + LIST_EVERY),
            ..Self::default()
        };
    }

    /// Note a failed listing; `false` once they have failed for
    /// [`GIVE_UP_AFTER`] without a break.
    pub(crate) fn failed(&mut self, now: Instant) -> bool {
        let since = *self.failing_since.get_or_insert(now);
        self.delay = (self.delay * 2).clamp(RETRY_FIRST, RETRY_MAX);
        self.next = Some(now + self.delay);
        now.saturating_duration_since(since) < GIVE_UP_AFTER
    }

    pub(crate) fn failing_since(&self) -> Option<Instant> {
        self.failing_since
    }
}

/// Why a listing failed: worth trying again, or the board must end.
enum ListFailure {
    Retry(Failure),
    End(Failure),
}

/// Whether a connection opened again reached the Noa the board started on
/// (`first`, its instance id; empty when that server predates the field,
/// and then it cannot be told). Pane ids are only meaningful within one
/// instance: another Noa's pane under the same id is not this worker.
fn same_instance(first: &str, now: &str) -> bool {
    first.is_empty() || first == now
}

/// One pane listing. A connection that was lost is opened again first.
fn list(
    client: &mut Client,
    reconnect: &mut bool,
    instance: &str,
) -> Result<Vec<Panel>, ListFailure> {
    if *reconnect {
        *client = super::Session::connect()
            .map_err(ListFailure::Retry)?
            .client;
        *reconnect = false;
        if !same_instance(instance, client.server_instance_id()) {
            return Err(ListFailure::End(Failure::error(
                "the Noa that answered after a reconnect is not the one the board started on; its panes are not these workers",
            )));
        }
    }
    client.list_panels().map_err(|err| {
        *reconnect = lost(&err);
        ListFailure::Retry(Failure::from(err))
    })
}

/// Whether `err` means the connection is gone and must be opened again.
fn lost(err: &ClientError) -> bool {
    matches!(err, ClientError::Transport(_) | ClientError::Closed(_))
}

/// Read the screens the board wants through `get`. The first lost
/// connection ends the pass: every later read would wait out its own
/// timeout. Returns whether the connection was lost.
fn fetch_screens(
    board: &mut Board,
    now: Instant,
    mut get: impl FnMut(u64) -> Result<String, ClientError>,
) -> bool {
    for pane in board.wanted_screens() {
        match get(pane) {
            Ok(text) => board.set_screen_at(pane, &text, now),
            Err(err) if lost(&err) => return true,
            // Noa is slow but connected: the other reads would each wait
            // out the deadline too. The next pass tries again.
            Err(ClientError::Timeout(_)) => return false,
            // A pane that closed meanwhile is dropped at the next listing.
            Err(_) => {}
        }
    }
    false
}

/// Why the board cannot run here, if so: it reads keys from stdin and
/// draws on stdout, and in a pipe it would write escape sequences into it
/// and wait for keys no one sees prompts for.
fn terminal_problem(stdin_tty: bool, stdout_tty: bool) -> Option<Failure> {
    (!(stdin_tty && stdout_tty)).then(|| {
        Failure::error(
            "board needs a terminal on stdin and stdout; from a script use `board --open`",
        )
    })
}

/// The terminating signal [`run`] caught, or 0.
static SIGNALED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

extern "C" fn on_signal(signal: libc::c_int) {
    SIGNALED.store(signal, std::sync::atomic::Ordering::Relaxed);
}

const TERMINATING_SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT];

/// Run the board until `q`. The terminal is restored before returning; a
/// terminating signal restores it too, then ends the process as that
/// signal would.
pub(crate) fn run(client: &mut Client, of: Option<u64>, me: Option<u64>) -> Result<i32, Failure> {
    // SAFETY: isatty only inspects the descriptors.
    let ttys = unsafe { (libc::isatty(0) == 1, libc::isatty(1) == 1) };
    if let Some(failure) = terminal_problem(ttys.0, ttys.1) {
        return Err(failure);
    }
    for signal in TERMINATING_SIGNALS {
        // SAFETY: the handler only stores to an atomic.
        unsafe {
            libc::signal(
                signal,
                on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
            )
        };
    }
    let result = run_until_quit(client, of, me);
    let signal = SIGNALED.load(std::sync::atomic::Ordering::Relaxed);
    for signal in TERMINATING_SIGNALS {
        // SAFETY: back to the default disposition.
        unsafe { libc::signal(signal, libc::SIG_DFL) };
    }
    if signal != 0 {
        // SAFETY: re-raises the caught signal with its default action.
        unsafe { libc::raise(signal) };
    }
    result
}

fn run_until_quit(client: &mut Client, of: Option<u64>, me: Option<u64>) -> Result<i32, Failure> {
    let mut board = Board::new(of, me);
    board.lang = Lang::detect();
    let mut polling = Polling::default();
    let mut reconnect = false;
    let instance = client.server_instance_id().to_string();

    let (keys_tx, keys_rx) = std::sync::mpsc::channel::<(Instant, Vec<u8>)>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        loop {
            // SAFETY: reads into a stack buffer of the given length.
            let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
            // Stamped on arrival: the loop may get to it later.
            if n <= 0
                || keys_tx
                    .send((Instant::now(), buf[..n as usize].to_vec()))
                    .is_err()
            {
                break;
            }
        }
    });

    let _screen = Screen::enter();
    let mut screens_at: Option<Instant> = None;
    let mut size = (0, 0);
    let mut dirty = true;
    let mut title = String::new();
    let mut reader = KeyReader::default();
    loop {
        if SIGNALED.load(std::sync::atomic::Ordering::Relaxed) != 0 {
            return Ok(EXIT_OK);
        }
        let now = Instant::now();
        if polling.due(now) {
            match list(client, &mut reconnect, &instance) {
                Ok(panels) => {
                    board.update(panels, now);
                    polling.succeeded(now);
                }
                Err(ListFailure::End(failure)) => return Err(failure),
                Err(ListFailure::Retry(failure)) if !polling.failed(now) => return Err(failure),
                Err(ListFailure::Retry(_)) => {}
            }
            board.offline = polling.failing_since();
            dirty = true;
        }
        // While offline the notice counts the seconds.
        dirty |= board.offline.is_some();
        if board.offline.is_none()
            && !reconnect
            && screens_at.is_none_or(|at| now.duration_since(at) >= SCREEN_EVERY)
        {
            // A lost connection is opened again by the next listing, on
            // its usual schedule.
            reconnect = fetch_screens(&mut board, now, |pane| {
                client
                    .get_text(pane, TextSource::Screen, None)
                    .map(|text| text.text)
            });
            screens_at = Some(now);
            dirty = true;
        }
        let current = Screen::size();
        if current != size {
            size = current;
            let _ = std::io::stdout().lock().write_all(b"\x1b[2J");
            dirty = true;
        }
        if dirty {
            Screen::draw(&board.render(size.0, size.1, now));
            let current = board.title();
            if current != title {
                Screen::set_title(&current);
                title = current;
            }
            dirty = false;
        }
        // A lone ESC waits briefly for the rest of a sequence.
        let wait = if reader.waiting_on_esc() {
            ESC_WAIT
        } else {
            Duration::from_millis(100)
        };
        let (at, keys) = match keys_rx.recv_timeout(wait) {
            Ok((at, bytes)) => (at, reader.feed_at(&bytes, at)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => (Instant::now(), reader.flush()),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(EXIT_OK),
        };
        for key in keys {
            dirty = true;
            let Some(action) = board.key_at(key, at) else {
                continue;
            };
            if action == Action::Quit {
                return Ok(EXIT_OK);
            }
            board.message = Some(perform(client, &board, action));
        }
    }
}

/// The server calls an action makes, so they can be faked in tests.
pub(crate) trait Server {
    fn panels(&mut self) -> Result<Vec<Panel>, noa_ipc::ClientError>;
    fn focus(&mut self, pane: u64) -> Result<(), noa_ipc::ClientError>;
    fn type_text(&mut self, pane: u64, text: &str, paste: bool)
    -> Result<(), noa_ipc::ClientError>;
    fn close(&mut self, pane: u64) -> Result<(), noa_ipc::ClientError>;
}

impl Server for Client {
    fn panels(&mut self) -> Result<Vec<Panel>, noa_ipc::ClientError> {
        self.list_panels()
    }
    fn focus(&mut self, pane: u64) -> Result<(), noa_ipc::ClientError> {
        self.focus_pane(pane)
    }
    fn type_text(
        &mut self,
        pane: u64,
        text: &str,
        paste: bool,
    ) -> Result<(), noa_ipc::ClientError> {
        self.send_text(pane, text, paste)
    }
    fn close(&mut self, pane: u64) -> Result<(), noa_ipc::ClientError> {
        self.close_pane(pane)
    }
}

/// Carry out an action and describe the result in one line.
fn perform(server: &mut impl Server, board: &Board, action: Action) -> Notice {
    let failed = |err: noa_ipc::ClientError| {
        let why = match err {
            noa_ipc::ClientError::Rpc {
                code: RPC_PANE_CLOSED,
                ..
            } => board
                .lang
                .pick(
                    "そのワーカーは終了したか、閉じられています",
                    "that worker exited or was closed",
                )
                .to_string(),
            err => err.to_string(),
        };
        Notice::warn(board.lang.pick(
            format!("できませんでした: {why}"),
            format!("could not: {why}"),
        ))
    };
    // The board's listing can be up to `LIST_EVERY` old: look again right
    // before typing, so a worker that started waiting for an approval since
    // is never answered from here (spec D7).
    if let Action::Send(pane, _) = &action {
        match server.panels() {
            Ok(panels) => {
                if panels.iter().any(|panel| {
                    panel.pane_id.0 == *pane && Status::of(panel) == Status::Permission
                }) {
                    return Notice::warn(approval_message(&board.name(*pane), board.lang));
                }
            }
            Err(err) => return failed(err),
        }
    }
    let result = match &action {
        Action::Focus(pane) => server.focus(*pane),
        Action::Send(pane, text) => {
            if let Err(err) = server.type_text(*pane, text, true) {
                return failed(err);
            }
            std::thread::sleep(SUBMIT_DELAY);
            // The Enter could answer a prompt that appeared in the pause.
            match server.panels() {
                Ok(panels)
                    if panels.iter().any(|panel| {
                        panel.pane_id.0 == *pane && Status::of(panel) == Status::Permission
                    }) =>
                {
                    let name = board.name(*pane);
                    return Notice::warn(board.lang.pick(
                        format!(
                            "{name} が承認待ちになったので Enter は送っていません。g でペインへ移動して答えてください"
                        ),
                        format!(
                            "{name} now awaits approval, so Enter was not sent. g goes to its pane to answer it"
                        ),
                    ));
                }
                Ok(_) => server.type_text(*pane, "\r", false),
                Err(err) => Err(err),
            }
        }
        Action::Close(pane) => server.close(*pane),
        Action::Quit => Ok(()),
    };
    let name = |pane: &u64| board.name(*pane);
    match (action, result) {
        (Action::Focus(pane), Ok(())) => {
            let name = name(&pane);
            Notice::ok(
                board
                    .lang
                    .pick(format!("{name} へ移動しました"), format!("went to {name}")),
            )
        }
        (Action::Send(pane, _), Ok(())) => {
            let name = name(&pane);
            Notice::ok(
                board
                    .lang
                    .pick(format!("{name} に送りました"), format!("sent to {name}")),
            )
        }
        (Action::Close(pane), Ok(())) => {
            let name = name(&pane);
            Notice::ok(
                board
                    .lang
                    .pick(format!("{name} を閉じました"), format!("closed {name}")),
            )
        }
        (_, Err(err)) => failed(err),
        (Action::Quit, Ok(())) => Notice::ok(String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_ipc::{PanelAgent, PanelExit, WireId};

    fn worker(pane: u64, parent: u64, label: &str, state: Option<&str>, seq: u64) -> Panel {
        let mut panel: Panel = serde_json::from_value(serde_json::json!({
            "windowGroupId": "1", "windowId": "1", "paneId": pane.to_string(),
            "name": "", "cwd": "/repo", "busy": false, "attention": false, "preview": [],
        }))
        .unwrap();
        panel.parent_pane_id = Some(WireId(parent));
        panel.label = Some(label.to_string());
        panel.agent = Some(PanelAgent {
            kind: Some("claude".to_string()),
            state: state.map(str::to_string),
            detail: Some(format!("{label} detail")),
            seq,
            blocked: None,
        });
        panel
    }

    fn visible(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars();
        while let Some(ch) = chars.next() {
            if ch == '\u{1b}' {
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    fn sample() -> Vec<Panel> {
        let mut exited = worker(5, 1, "tests", None, 0);
        exited.agent = None;
        exited.exit = Some(PanelExit { code: Some(0) });
        let mut other = worker(9, 7, "elsewhere", Some("running"), 1);
        other.label = Some("elsewhere".to_string());
        vec![
            worker(2, 1, "api", Some("running"), 4),
            worker(3, 1, "ui", Some("permission"), 5),
            worker(4, 1, "docs", Some("finished"), 2),
            exited,
            other,
            worker(8, 1, "board", None, 0),
        ]
    }

    #[test]
    fn workers_are_this_orchestrators_most_urgent_first_without_the_board() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());

        let order: Vec<u64> = board.workers().iter().map(|p| p.pane_id.0).collect();
        assert_eq!(order, vec![3, 2, 4, 5]);
        assert_eq!(board.selected(), Some(3));

        let mut all = Board::new(None, Some(8));
        all.update(sample(), Instant::now());
        assert!(all.workers().iter().any(|p| p.pane_id.0 == 9));
    }

    #[test]
    fn a_notification_without_a_blocking_report_needs_attention() {
        let mut panel = worker(2, 1, "api", Some("running"), 3);
        panel.attention = true;
        assert_eq!(Status::of(&panel), Status::Attention);
        assert_eq!(Status::of(&panel).group(), Group::Needs);
        // Seen in a real Noa: a finished report comes with a notification.
        panel.agent.as_mut().unwrap().state = Some("finished".to_string());
        assert_eq!(Status::of(&panel), Status::Finished);
        panel.agent.as_mut().unwrap().state = Some("permission".to_string());
        assert_eq!(Status::of(&panel), Status::Permission);
    }

    #[test]
    fn history_keeps_each_new_report_and_marks_the_starting_one() {
        let start = Instant::now();
        let mut board = Board::new(Some(1), None);
        board.update(sample(), start);
        let mut next = sample();
        next[0].agent.as_mut().unwrap().seq = 6;
        next[0].agent.as_mut().unwrap().state = Some("finished".to_string());
        let later = start + Duration::from_secs(5);
        board.update(next.clone(), later);
        board.update(next, later + Duration::from_secs(1));

        let history = &board.history[&2];
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].seq, 6);
        assert_eq!(history[0].status, Status::Finished);
        assert_eq!(history[0].at, Some(later));
        assert_eq!(history[1].at, None);
    }

    #[test]
    fn keys_are_read_from_bytes() {
        assert_eq!(
            KeyReader::default().feed(b"j\x1b[A\x1bOB\r\x7f\x03\x1b"),
            vec![
                Key::Char('j'),
                Key::Up,
                Key::Down,
                Key::Enter,
                Key::Backspace,
                Key::CtrlC,
            ]
        );
        assert_eq!(
            KeyReader::default().feed("確認".as_bytes()),
            vec![Key::Char('確'), Key::Char('認')]
        );
    }

    #[test]
    fn keys_drive_selection_detail_send_and_close() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());

        assert_eq!(board.key(Key::Char('j')), None);
        assert_eq!(board.selected(), Some(2));
        board.key(Key::Enter);
        assert_eq!(board.view, View::Detail(2));
        assert_eq!(board.key(Key::Char('g')), Some(Action::Focus(2)));

        board.key(Key::Char('s'));
        for ch in "run it".chars() {
            board.key(Key::Char(ch));
        }
        board.key(Key::Backspace);
        assert_eq!(
            board.key(Key::Enter),
            Some(Action::Send(2, "run i".to_string()))
        );
        assert_eq!(board.mode, Mode::Normal);

        board.key(Key::Char('x'));
        assert_eq!(board.key(Key::Char('n')), None);
        board.key(Key::Char('x'));
        assert_eq!(board.key(Key::Char('y')), Some(Action::Close(2)));

        board.key(Key::Esc);
        assert_eq!(board.view, View::List);
        assert_eq!(board.key(Key::Char('q')), None);
        assert_eq!(board.key(Key::Char('q')), Some(Action::Quit));
    }

    #[test]
    fn every_drawn_line_fills_the_width_exactly() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        board.set_screen(
            3,
            "Allow command? cargo test -p noa-app\n› 1. Yes   2. No — 承認してください\n\n",
        );
        for cols in [24, 41, 80, 120] {
            for view in [View::List, View::Detail(3)] {
                board.view = view;
                let lines = board.render(cols, 30, Instant::now());
                assert_eq!(lines.len(), 30);
                for line in &lines {
                    assert_eq!(
                        visible(line).width(),
                        cols,
                        "{cols} cols: {:?}",
                        visible(line)
                    );
                }
            }
        }
    }

    // Seen in a real Noa: a 21-column pane got 24-column lines, which wrapped
    // and left the board blank.
    #[test]
    fn a_pane_too_small_gets_a_note_that_fits() {
        let mut board = Board::new(Some(1), None);
        board.update(sample(), Instant::now());
        for (cols, rows) in [(21, 30), (80, 4), (1, 1)] {
            let lines = board.render(cols, rows, Instant::now());
            assert_eq!(lines.len(), rows);
            for line in &lines {
                assert_eq!(visible(line).width(), cols);
            }
        }
    }

    #[test]
    fn the_list_shows_cards_with_state_detail_and_screen_tail() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        board.set_screen(
            3,
            "older line\nreading src\nAllow command? cargo test\n› 1. Yes   2. No\n",
        );
        let text: Vec<String> = board
            .render(80, 40, Instant::now())
            .iter()
            .map(|l| visible(l))
            .collect();
        let all = text.join("\n");

        assert!(all.contains("要対応"));
        assert!(all.contains("ui") && all.contains("承認待ち"));
        // Pane ids are for +ctl, not for a person: only the detail shows one.
        assert!(!all.contains("pane 3"), "{all}");
        assert!(all.contains("ui detail"));
        // The selected worker's screen is previewed under the list (D15).
        assert!(all.contains("› 1. Yes   2. No"));
        // Finished and exited workers are counted until opened.
        assert!(all.contains("✓ 完了 1   · 終了 1"), "{all}");
        assert!(!all.contains("終了 0"), "{all}");
        board.key(Key::Char(' '));
        let all = rendered(&board, 80, 40, Instant::now());
        assert!(all.contains("終了 0"), "{all}");
        assert!(!all.contains("board"));
    }

    /// A server that answers with fixed panels and records what was typed.
    struct FakeServer {
        /// Listings answered in order; the last one repeats.
        listings: Vec<Vec<Panel>>,
        typed: Vec<(u64, String)>,
    }

    impl Server for FakeServer {
        fn panels(&mut self) -> Result<Vec<Panel>, noa_ipc::ClientError> {
            if self.listings.len() > 1 {
                Ok(self.listings.remove(0))
            } else {
                Ok(self.listings[0].clone())
            }
        }
        fn focus(&mut self, _pane: u64) -> Result<(), noa_ipc::ClientError> {
            Ok(())
        }
        fn type_text(
            &mut self,
            pane: u64,
            text: &str,
            _paste: bool,
        ) -> Result<(), noa_ipc::ClientError> {
            self.typed.push((pane, text.to_string()));
            Ok(())
        }
        fn close(&mut self, _pane: u64) -> Result<(), noa_ipc::ClientError> {
            Ok(())
        }
    }

    // Review finding: the board decides from a listing up to 500 ms old, so
    // a worker that started waiting for an approval since then was sent the
    // typed answer.
    #[test]
    fn sending_checks_the_worker_again_right_before_typing() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        let mut now_waiting = sample();
        now_waiting[0].agent.as_mut().unwrap().state = Some("permission".to_string());
        let mut server = FakeServer {
            listings: vec![now_waiting],
            typed: Vec::new(),
        };

        let message = perform(&mut server, &board, Action::Send(2, "y".to_string()));

        assert_eq!(server.typed, Vec::new());
        assert!(message.text.contains("承認待ち"), "{message:?}");
        assert!(message.warn);
    }

    // Review finding: the Enter that submits goes 150 ms after the text, and
    // a worker that started waiting for an approval in that pause took the
    // Enter as its answer.
    #[test]
    fn the_submitting_enter_is_not_sent_to_a_worker_now_waiting() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        let mut now_waiting = sample();
        now_waiting[0].agent.as_mut().unwrap().state = Some("permission".to_string());
        let mut server = FakeServer {
            listings: vec![sample(), now_waiting],
            typed: Vec::new(),
        };

        let message = perform(&mut server, &board, Action::Send(2, "run it".to_string()));

        assert_eq!(server.typed, vec![(2, "run it".to_string())]);
        assert!(message.text.contains("承認待ち"), "{message:?}");
        assert!(message.warn);
    }

    // Review finding: ESC and the key after it in one read (Option/Meta
    // with a key) became Esc plus that key: the send line closed and the key
    // ran as a command.
    #[test]
    fn a_meta_key_is_neither_esc_nor_its_key() {
        let mut reader = KeyReader::default();
        assert_eq!(reader.feed(b"\x1bx"), Vec::new());
        assert_eq!(reader.feed(b"\x1b\x7f"), Vec::new());
        assert_eq!(reader.feed("\x1bあj".as_bytes()), vec![Key::Char('j')]);
        // Two ESCs: the first is the Esc key.
        assert_eq!(reader.feed(b"\x1b\x1b[A"), vec![Key::Esc, Key::Up]);
    }

    // Review finding: Option+[ or Option+Shift+O (`ESC [`, `ESC O`) began a
    // sequence that swallowed everything typed after it up to a letter.
    #[test]
    fn a_meta_bracket_does_not_swallow_what_is_typed_after() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_millis(200);
        let mut reader = KeyReader::default();
        assert_eq!(reader.feed_at(b"\x1b[", t0), Vec::new());
        assert_eq!(
            reader.feed_at(b"123 a", later),
            "123 a".chars().map(Key::Char).collect::<Vec<_>>()
        );
        let mut reader = KeyReader::default();
        assert_eq!(reader.feed_at(b"\x1bO", t0), Vec::new());
        assert_eq!(reader.feed_at(b"x", later), vec![Key::Char('x')]);
        // A sequence that really was split arrives within the wait.
        let mut reader = KeyReader::default();
        assert_eq!(reader.feed_at(b"\x1b[", t0), Vec::new());
        assert_eq!(
            reader.feed_at(b"A", t0 + Duration::from_millis(5)),
            vec![Key::Up]
        );
    }

    // Review finding: an Esc and a key pressed well apart were joined into
    // one Option/Meta key, and both lost, when the loop handled them late.
    #[test]
    fn keys_that_arrived_apart_stay_apart_when_handled_late() {
        let t0 = Instant::now();
        let mut reader = KeyReader::default();
        assert_eq!(reader.feed_at(b"\x1b", t0), Vec::new());
        assert_eq!(
            reader.feed_at(b"j", t0 + Duration::from_millis(100)),
            vec![Key::Esc, Key::Char('j')]
        );
        // Arriving together, they are one Option/Meta key.
        let mut reader = KeyReader::default();
        assert_eq!(reader.feed_at(b"\x1b", t0), Vec::new());
        assert_eq!(
            reader.feed_at(b"j", t0 + Duration::from_millis(5)),
            Vec::new()
        );
    }

    // Review finding: a read that ended right after the ESC of a paste's
    // start marker made it the Esc key, and the paste then ran as keys.
    #[test]
    fn an_esc_at_the_end_of_a_read_waits_for_what_follows() {
        let mut reader = KeyReader::default();
        assert_eq!(reader.feed(b"\x1b"), Vec::new());
        assert_eq!(
            reader.feed(b"[200~hello\nxy\x1b[201~"),
            vec![Key::Paste("hello\nxy".to_string())]
        );
        // Nothing follows: it was the Esc key.
        assert_eq!(reader.feed(b"\x1b"), Vec::new());
        assert!(reader.waiting_on_esc());
        assert_eq!(reader.flush(), vec![Key::Esc]);
        assert!(!reader.waiting_on_esc());
    }

    // Seen in a real Noa after the paste fix: the send line printed the
    // pasted newline as is, breaking the footer.
    #[test]
    fn the_send_line_shows_newlines_without_printing_them() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        board.mode = Mode::Send(2, "hello\nxy".to_string());
        let send = board.render(60, 20, Instant::now())[2].clone();
        assert!(!send.contains('\n'), "{send:?}");
        assert!(visible(&send).contains("hello⏎xy"));
        assert_eq!(visible(&send).width(), 60);
    }

    // Review finding: a pasted newline was read as Enter, so `hello\nxy`
    // sent "hello" and then ran `x` and `y`, closing the worker.
    #[test]
    fn a_paste_is_typed_whole_and_never_runs_as_keys() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        board.key(Key::Char('j'));
        board.key(Key::Char('s'));
        let mut reader = KeyReader::default();
        let mut actions = Vec::new();
        for key in reader.feed(b"\x1b[200~hello\nxy\x1b[201~") {
            actions.extend(board.key(key));
        }
        assert_eq!(actions, Vec::new());
        assert_eq!(board.mode, Mode::Send(2, "hello\nxy".to_string()));

        // A paste split across reads, and one outside the send line.
        let mut reader = KeyReader::default();
        assert_eq!(reader.feed(b"\x1b[200~par"), Vec::new());
        assert_eq!(reader.feed(b"t\x1b[20"), Vec::new());
        assert_eq!(
            reader.feed(b"1~q"),
            vec![Key::Paste("part".to_string()), Key::Char('q')]
        );
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        assert_eq!(board.key(Key::Paste("x\ny".to_string())), None);
        assert_eq!(board.mode, Mode::Normal);
    }

    // Review finding: each read was decoded on its own, so a character split
    // between two reads became two replacement characters.
    #[test]
    fn a_character_split_between_reads_is_kept() {
        let mut reader = KeyReader::default();
        let bytes = "あ".as_bytes();
        assert_eq!(reader.feed(&bytes[..1]), Vec::new());
        assert_eq!(reader.feed(&bytes[1..]), vec![Key::Char('あ')]);
        assert_eq!(reader.feed(b"\x1b["), Vec::new());
        assert_eq!(reader.feed(b"A"), vec![Key::Up]);
    }

    // Review finding: `s` opened the send line on a worker waiting for an
    // approval, so typing `y` and Enter approved it from the board.
    #[test]
    fn a_worker_waiting_for_approval_is_answered_in_its_pane() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        assert_eq!(board.selected(), Some(3));
        assert_eq!(board.key(Key::Char('s')), None);
        assert_eq!(board.mode, Mode::Normal);
        assert!(
            board
                .message
                .as_ref()
                .is_some_and(|m| m.warn && m.text.contains("g"))
        );

        // It started waiting while the message was being typed.
        board.key(Key::Char('j'));
        board.key(Key::Char('s'));
        board.key(Key::Char('y'));
        let mut panels = sample();
        panels[0].agent.as_mut().unwrap().state = Some("permission".to_string());
        board.update(panels, Instant::now());
        assert_eq!(board.key(Key::Enter), None);
    }

    // Review finding: a label is not cleaned like screen text, so an escape
    // sequence in it ran on the board's terminal at every redraw.
    #[test]
    fn control_characters_in_names_never_reach_the_terminal() {
        let mut panels = sample();
        panels[1].label = Some("review\u{1b}[2J\n".to_string());
        panels[1].agent.as_mut().unwrap().kind = Some("x\u{1b}]0;t\u{7}".to_string());
        let mut board = Board::new(Some(1), Some(8));
        board.update(panels, Instant::now());
        board.key(Key::Char('x'));
        for view in [View::List, View::Detail(3)] {
            board.view = view;
            for line in board.render(80, 30, Instant::now()) {
                assert!(
                    !line.contains("\u{1b}[2J") && !line.contains("\u{1b}]0;"),
                    "{line:?}"
                );
                assert!(!line.contains('\n') && !line.contains('\u{7}'));
            }
        }
    }

    // Review finding: a worker that appeared after the board opened had its
    // first report shown as the state found at start.
    #[test]
    fn a_report_from_a_worker_that_came_later_has_its_time() {
        let start = Instant::now();
        let mut board = Board::new(Some(1), None);
        board.update(Vec::new(), start);
        let later = start + Duration::from_secs(3);
        board.update(sample(), later);
        assert_eq!(board.history[&2][0].at, Some(later));
    }

    // Review finding: in a pane 6 to 9 rows tall, keeping the selected
    // card's bottom in view pushed its top, with the name and pane, out.
    #[test]
    fn a_short_pane_still_names_the_selected_worker() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        board.key(Key::Char('j'));
        assert_eq!(board.selected(), Some(2));
        for rows in MIN_ROWS..=12 {
            let all: String = board
                .render(60, rows, Instant::now())
                .iter()
                .map(|l| visible(l))
                .collect();
            assert!(
                all.contains("▶1 ● api") && all.contains("作業中"),
                "{rows} rows: {all}"
            );
        }
    }

    fn rendered(board: &Board, cols: usize, rows: usize, now: Instant) -> String {
        board
            .render(cols, rows, now)
            .iter()
            .map(|l| visible(l) + "\n")
            .collect()
    }

    // Owner request 1 (from a screenshot): every card showed the agent's
    // input box and status line, the bottom of its screen, not its work.
    #[test]
    fn a_card_previews_the_work_above_the_agents_input_box() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        // Selected, it previews a few lines (others show one).
        board.key(Key::Char('j'));
        let rule = "─".repeat(40);
        board.set_screen(
            2,
            &format!(
                "⏺ Wrote store.js\n  ⎿ 42 lines\n\n{rule}\n› commit this\n{rule}\n  todo-sample main\n  ◆◆ 6% (~31t left)\n  -- INSERT -- ▸▸ auto mode on\n"
            ),
        );
        let all = rendered(&board, 80, 40, Instant::now());
        assert!(
            all.contains("⏺ Wrote store.js") && all.contains("⎿ 42 lines"),
            "{all}"
        );
        assert!(
            !all.contains("INSERT") && !all.contains("commit this"),
            "{all}"
        );

        // A screen without such a box (a plain command) is shown as before.
        assert_eq!(content_lines(&["a".to_string(), "b".to_string()]).len(), 2);
    }

    // Owner request 2: a report without a detail left the card's first
    // line blank. It says what the worker last said in the same state.
    #[test]
    fn a_card_without_a_detail_says_what_it_last_reported_in_that_state() {
        let start = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), start);
        let mut next = sample();
        let api = next[0].agent.as_mut().unwrap();
        api.seq = 5;
        api.detail = Some(String::new());
        board.update(next, start + Duration::from_secs(1));
        let all = rendered(&board, 80, 40, start + Duration::from_secs(2));
        assert!(all.contains("api detail"), "{all}");

        let mut quiet = Board::new(Some(1), Some(8));
        let mut panels = sample();
        panels[2].agent.as_mut().unwrap().detail = None;
        quiet.update(panels, start);
        quiet.key(Key::Char(' '));
        let all = rendered(&quiet, 80, 40, start);
        assert!(all.contains("✓ docs   完了  "), "{all}");
        assert!(!all.contains("docs detail"), "{all}");
    }

    // Review B03: a worker running a new turn showed the last turn's
    // "finished" text, so it looked done.
    #[test]
    fn a_detail_from_an_earlier_state_is_not_shown() {
        let start = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = sample();
        let api = panels[0].agent.as_mut().unwrap();
        api.state = Some("finished".to_string());
        api.detail = Some("wrote 3 files".to_string());
        board.update(panels.clone(), start);
        let api = panels[0].agent.as_mut().unwrap();
        api.seq += 1;
        api.state = Some("running".to_string());
        api.detail = None;
        board.update(panels, start + Duration::from_secs(1));
        let all = rendered(&board, 80, 40, start + Duration::from_secs(2));
        assert!(!all.contains("wrote 3 files"), "{all}");
        // At work, it says when it last showed output, not how long it
        // has worked (D15): nothing yet.
        assert!(all.contains("● api  作業中  "), "{all}");
        assert!(!line_with(&all, "● api").contains("1 秒"), "{all}");
    }

    // Owner request 4: how long a worker has been in its state. Review
    // B09: a state found at start has no known age, and says none.
    #[test]
    fn a_card_tells_how_long_the_worker_has_been_in_its_state() {
        let start = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), start);
        let all = rendered(&board, 80, 40, start);
        assert!(all.contains("● api  作業中    api detail"), "{all}");
        assert!(!all.contains("開始時"), "{all}");

        let mut next = sample();
        next[0].agent.as_mut().unwrap().state = Some("finished".to_string());
        next[0].agent.as_mut().unwrap().seq = 5;
        let changed = start + Duration::from_secs(10);
        board.update(next.clone(), changed);
        board.update(next, changed + Duration::from_secs(30));
        board.key(Key::Char(' '));
        let all = rendered(&board, 80, 40, changed + Duration::from_secs(180));
        assert!(all.contains("完了 3 分前"), "{all}");
    }

    #[test]
    fn the_list_scrolls_to_keep_the_selection_in_view() {
        let mut board = Board::new(Some(1), None);
        let panels: Vec<Panel> = (2..12)
            .map(|pane| worker(pane, 1, &format!("w{pane}"), Some("running"), 1))
            .collect();
        board.update(panels, Instant::now());
        for _ in 0..9 {
            board.key(Key::Char('j'));
        }
        let all: String = board
            .render(60, 16, Instant::now())
            .iter()
            .map(|l| visible(l))
            .collect();
        assert!(all.contains("w11"));
    }

    #[test]
    fn the_detail_shows_the_screen_and_the_history() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        board.set_screen(
            3,
            "line one\nWould you like to run the following command?\n",
        );
        board.key(Key::Enter);
        let all: String = board
            .render(80, 30, Instant::now())
            .iter()
            .map(|l| visible(l) + "\n")
            .collect();
        assert!(all.contains("Would you like to run the following command?"));
        assert!(all.contains("報告の履歴"));
        // Owner request 3: `seq` counts reports, not turns.
        assert!(all.contains("報告 5"));
        assert!(!all.contains("ターン"));
        assert!(all.contains("開始時"));
    }

    // UI pass: eight workers, the most one pane holds, fit a 30-row pane
    // without scrolling (a boxed card took 5 rows, so 3 fit).
    #[test]
    fn eight_workers_fit_a_thirty_row_pane() {
        let mut board = Board::new(Some(1), None);
        let panels: Vec<Panel> = (2..10)
            .map(|pane| worker(pane, 1, &format!("w{pane}"), Some("running"), 1))
            .collect();
        board.update(panels, Instant::now());
        for pane in 2..10 {
            board.set_screen(pane, &format!("work of w{pane}\n"));
        }
        let all = rendered(&board, 80, 30, Instant::now());
        for pane in 2..10 {
            assert!(all.contains(&format!("w{pane}")), "{all}");
        }
        assert!(!all.contains("あと"), "{all}");
    }

    #[test]
    fn number_keys_and_n_pick_workers_without_stepping() {
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = sample();
        panels[2].agent.as_mut().unwrap().state = Some("input".to_string());
        board.update(panels, Instant::now());
        // Order: ui (permission), docs (input), api, tests; numbered by
        // spawn: api 1, ui 2, docs 3, tests 4.
        assert_eq!(board.selected(), Some(3));
        board.key(Key::Char('1'));
        assert_eq!(board.selected(), Some(2));
        board.key(Key::Char('n'));
        assert_eq!(board.selected(), Some(3));
        board.key(Key::Char('n'));
        assert_eq!(board.selected(), Some(4));
        board.key(Key::Char('n'));
        assert_eq!(board.selected(), Some(3), "n wraps round");

        board.key(Key::Char('9'));
        assert_eq!(board.selected(), Some(3));
        assert!(board.message.as_ref().is_some_and(|m| m.warn));

        // In the detail, a number switches the worker shown.
        board.key(Key::Enter);
        board.key(Key::Char('4'));
        assert_eq!(board.view, View::Detail(5));
    }

    // D12: with up to 20 workers, numbers take two columns and two digits
    // typed in a row pick a two-digit number.
    #[test]
    fn two_digits_in_a_row_pick_a_two_digit_worker() {
        let mut board = Board::new(Some(1), None);
        let panels: Vec<Panel> = (0..12)
            .map(|i| worker(10 + i, 1, &format!("w{}", i + 1), Some("running"), 1))
            .collect();
        board.update(panels, Instant::now());
        board.key(Key::Char('1'));
        assert_eq!(board.selected(), Some(10));
        board.key(Key::Char('2'));
        assert_eq!(board.selected(), Some(21), "1 then 2 is worker 12");
        board.key(Key::Char('3'));
        assert_eq!(board.selected(), Some(12), "32 does not exist: 3 alone");
        board.key(Key::Char('0'));
        assert_eq!(board.selected(), Some(12), "0 only completes a number");
        let all = rendered(&board, 80, 40, Instant::now());
        assert!(
            all.contains(" 1 ● w1 ") && all.contains("12 ● w12"),
            "{all}"
        );
    }

    #[test]
    fn n_says_so_when_no_worker_needs_anyone() {
        let mut board = Board::new(Some(1), None);
        board.update(
            vec![worker(2, 1, "api", Some("running"), 1)],
            Instant::now(),
        );
        board.key(Key::Char('n'));
        assert_eq!(board.selected(), Some(2));
        assert!(board.message.as_ref().is_some_and(|m| !m.warn));
    }

    #[test]
    fn help_opens_with_a_question_mark_and_any_key_closes_it() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        board.key(Key::Char('?'));
        assert!(board.help);
        let all = rendered(&board, 80, 30, Instant::now());
        assert!(
            all.contains("キー操作") && all.contains("次の要対応"),
            "{all}"
        );
        // q closes the help rather than the board.
        assert_eq!(board.key(Key::Char('q')), None);
        assert!(!board.help);
        board.key(Key::Char('?'));
        assert_eq!(board.key(Key::CtrlC), None);
        assert_eq!(board.key(Key::CtrlC), Some(Action::Quit));
    }

    #[test]
    fn the_footer_fits_and_always_points_at_the_help() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        for cols in [24, 40, 80] {
            let footer = visible(&board.render(cols, 20, Instant::now()).pop().unwrap());
            assert!(footer.contains("? ヘルプ"), "{cols}: {footer}");
            assert!(!footer.contains('…'), "{cols}: {footer}");
        }
        // The selected worker waits for an approval: g is where it is answered.
        let footer = visible(&board.render(120, 20, Instant::now()).pop().unwrap());
        assert!(footer.contains("g 移動して答える"), "{footer}");
    }

    #[test]
    fn a_closed_worker_stays_listed_with_its_last_word() {
        let start = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), start);
        let without_docs: Vec<Panel> = sample().into_iter().filter(|p| p.pane_id.0 != 4).collect();
        board.update(without_docs, start + Duration::from_secs(1));
        // Counted with the finished and exited workers, listed once opened.
        let all = rendered(&board, 80, 40, start + Duration::from_secs(120));
        assert!(all.contains("閉じた 1"), "{all}");
        board.key(Key::Char(' '));
        let all = rendered(&board, 80, 40, start + Duration::from_secs(120));
        assert!(
            all.contains("✕ docs  完了で閉じた · 1 分前  docs detail"),
            "{all}"
        );
        // The first listing is the state found, not a closing.
        let mut fresh = Board::new(Some(1), Some(8));
        fresh.update(Vec::new(), start);
        assert!(fresh.gone.is_empty());
    }

    #[test]
    fn a_scrolled_list_says_what_it_hides_and_how_many_need_someone() {
        let mut board = Board::new(Some(1), None);
        let mut panels: Vec<Panel> = (2..12)
            .map(|pane| worker(pane, 1, &format!("w{pane}"), Some("running"), 1))
            .collect();
        panels[9].agent.as_mut().unwrap().state = Some("error".to_string());
        board.update(panels, Instant::now());
        // w11 errored, so it is listed first and selected.
        let all = rendered(&board, 60, 12, Instant::now());
        assert!(all.contains("w11") && all.contains("↓ あと"), "{all}");
        assert!(!all.contains("↑"), "{all}");
        for _ in 0..9 {
            board.key(Key::Char('j'));
        }
        let all = rendered(&board, 60, 12, Instant::now());
        assert!(
            all.contains("↑ あと") && all.contains("うち要対応 1"),
            "{all}"
        );
        assert!(all.contains("▶"), "{all}");
    }

    #[test]
    fn the_send_line_clears_and_deletes_a_word() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        board.key(Key::Char('j'));
        board.key(Key::Char('s'));
        board.key(Key::Paste("fix the　テスト ".to_string()));
        board.key(Key::CtrlW);
        assert_eq!(board.mode, Mode::Send(2, "fix the　".to_string()));
        board.key(Key::CtrlW);
        assert_eq!(board.mode, Mode::Send(2, "fix ".to_string()));
        board.key(Key::CtrlU);
        assert_eq!(board.mode, Mode::Send(2, String::new()));
        assert_eq!(
            KeyReader::default().feed(b"\x15\x17"),
            vec![Key::CtrlU, Key::CtrlW]
        );
        let lines = board.render(80, 20, Instant::now());
        let send = visible(&lines[2]);
        assert!(send.contains("api に送る"), "{send}");
        let footer = visible(&lines[19]);
        assert!(footer.contains("Esc 取り消し"), "{footer}");
    }

    #[test]
    fn closing_a_busy_worker_says_it_is_still_at_work() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        board.key(Key::Char('j'));
        board.key(Key::Char('x'));
        let lines = board.render(100, 20, Instant::now());
        assert!(
            visible(&lines[2]).contains("まだ作業中です。"),
            "{}",
            visible(&lines[2])
        );
        assert!(
            visible(&lines[19]).contains("y 閉じる"),
            "{}",
            visible(&lines[19])
        );
    }

    #[test]
    fn the_header_and_title_name_the_orchestrator_and_what_needs_someone() {
        let mut panels = sample();
        let mut me = worker(1, 0, "", Some("running"), 1);
        me.parent_pane_id = None;
        me.label = None;
        me.name = "fix the flaky suite".to_string();
        panels.push(me);
        let mut board = Board::new(Some(1), Some(8));
        board.update(panels, Instant::now());
        let all = rendered(&board, 80, 30, Instant::now());
        assert!(all.contains("fix the flaky suite · claude"), "{all}");
        assert_eq!(board.title(), "ボード · 要対応 1");

        let mut calm = Board::new(Some(1), None);
        calm.update(
            vec![worker(2, 1, "api", Some("finished"), 1)],
            Instant::now(),
        );
        assert_eq!(calm.title(), "ボード");
    }

    /// The number in a "あと N 件" marker line with `arrow`, if drawn.
    fn marker(all: &str, arrow: &str) -> Option<usize> {
        let at = all.find(&format!("{arrow} あと "))?;
        let rest = &all[at + arrow.len() + " あと ".len()..];
        rest.split(' ').next()?.parse().ok()
    }

    /// Workers w2.. running, each with a few lines of work on screen.
    fn tall_board(n: u64) -> Board {
        let mut board = Board::new(Some(1), None);
        let panels: Vec<Panel> = (2..2 + n)
            .map(|pane| worker(pane, 1, &format!("w{pane}"), Some("running"), 1))
            .collect();
        board.update(panels, Instant::now());
        for pane in 2..2 + n {
            board.set_screen(pane, "one\ntwo\nthree\nfour\n");
        }
        board
    }

    // Review finding (codex): with more lines than room, the closed section
    // came after the last worker's end, which was as far as the list
    // scrolled: selecting the last worker never brought it into view.
    #[test]
    fn the_closed_section_is_reachable_by_selecting_the_last_worker() {
        let start = Instant::now();
        let mut board = tall_board(4);
        let panels: Vec<Panel> = (2..5)
            .map(|pane| worker(pane, 1, &format!("w{pane}"), Some("running"), 1))
            .collect();
        board.update(panels, start + Duration::from_secs(1));
        for _ in 0..3 {
            board.key(Key::Char('j'));
        }
        assert_eq!(board.selected(), Some(4));
        let all = rendered(&board, 60, 12, start + Duration::from_secs(2));
        assert!(all.contains("✕ w5"), "{all}");
        assert!(all.contains("▶3 ● w4"), "{all}");
    }

    // Review findings (agy, claude): a marker said "あと 0 件" when only a
    // section title or the closed section was cut, and counted a worker
    // whose row was in view but whose preview was cut.
    #[test]
    fn scroll_markers_count_exactly_the_workers_whose_row_is_out_of_view() {
        let start = Instant::now();
        for with_gone in [false, true] {
            // Shorter panes draw no markers (they spend the rows on the selection).
            for rows in 8..=24 {
                for pick in 0..4 {
                    let mut board = tall_board(if with_gone { 5 } else { 4 });
                    if with_gone {
                        let panels: Vec<Panel> = (2..6)
                            .map(|p| worker(p, 1, &format!("w{p}"), Some("running"), 1))
                            .collect();
                        board.update(panels, start);
                    }
                    for _ in 0..pick {
                        board.key(Key::Char('j'));
                    }
                    let all = rendered(&board, 60, rows, start);
                    let shown = (2..6).filter(|p| all.contains(&format!("● w{p} "))).count();
                    let above = marker(&all, "↑").unwrap_or(0);
                    let below = marker(&all, "↓").unwrap_or(0);
                    let ctx = format!("rows {rows}, pick {pick}, gone {with_gone}:\n{all}");
                    assert!(!all.contains("あと 0 件"), "{ctx}");
                    assert_eq!(shown + above + below, 4, "{ctx}");
                }
            }
        }
    }

    #[test]
    fn a_short_pane_keeps_the_selected_row_and_drops_the_preview() {
        let mut board = Board::new(Some(1), None);
        let mut panels = vec![worker(2, 1, "w2", Some("permission"), 1)];
        panels.push(worker(3, 1, "w3", Some("running"), 1));
        board.update(panels, Instant::now());
        board.set_screen(2, "one\ntwo\nthree\n");
        // Too short for a preview under the list: the rows keep the room.
        let all = rendered(&board, 80, 9, Instant::now());
        assert!(all.contains("▶1 ! w2"), "{all}");
        assert!(!all.contains("three") && !all.contains("選択中"), "{all}");
        assert!(!all.contains("うち要対応"), "{all}");
    }

    // Review finding (codex): in a short pane the view stopped at the last
    // worker's row, so most of a long closed section stayed out of reach.
    #[test]
    fn j_past_the_last_worker_scrolls_through_the_closed_section() {
        let start = Instant::now();
        let mut board = tall_board(6);
        board.update(
            vec![worker(2, 1, "w2", Some("running"), 1)],
            start + Duration::from_secs(1),
        );
        assert_eq!(board.gone.len(), 5);
        let view = |board: &Board| rendered(board, 60, 10, start + Duration::from_secs(2));
        // Counted on the line of the finished and exited workers.
        assert!(view(&board).contains("閉じた 5"), "{}", view(&board));
        assert!(!view(&board).contains("✕ w3"), "{}", view(&board));

        for _ in 0..10 {
            board.key(Key::Char('j'));
        }
        let all = view(&board);
        for pane in 3..8 {
            assert!(all.contains(&format!("✕ w{pane}")), "w{pane}:\n{all}");
        }
        assert!(!all.contains("↓"), "{all}");
        // The selected row scrolled out of view, and the marker says so.
        assert!(
            !all.contains("▶1 ● w2") && all.contains("↑ あと 1 件"),
            "{all}"
        );
        assert_eq!(board.selected(), Some(2));

        // k scrolls back before it moves the selection.
        for _ in 0..10 {
            board.key(Key::Char('k'));
        }
        let all = view(&board);
        assert!(all.contains("▶1 ● w2") && !all.contains("↑"), "{all}");
    }

    // Review finding (codex, PR #194): once every worker had closed, the
    // list skipped its scrolling and cut the closed section at the pane's
    // height, and j/k did nothing without a live worker.
    #[test]
    fn the_closed_section_scrolls_when_no_worker_is_left() {
        let start = Instant::now();
        let mut board = tall_board(5);
        board.update(Vec::new(), start + Duration::from_secs(1));
        assert_eq!(board.gone.len(), 5);
        let view = |board: &Board| rendered(board, 80, 8, start + Duration::from_secs(2));
        assert!(view(&board).contains("↓ 閉じた"), "{}", view(&board));

        let mut seen = std::collections::HashSet::new();
        for _ in 0..10 {
            let all = view(&board);
            assert!(!all.contains("あと 0 件"), "{all}");
            for pane in 2..7 {
                if all.contains(&format!("✕ w{pane}")) {
                    seen.insert(pane);
                }
            }
            board.key(Key::Char('j'));
        }
        assert_eq!(seen.len(), 5, "{seen:?}");
        let all = view(&board);
        assert!(all.contains("↑ 閉じた") && !all.contains("↓"), "{all}");

        for _ in 0..10 {
            board.key(Key::Char('k'));
        }
        let all = view(&board);
        assert!(
            all.contains("ワーカーはまだいません") && !all.contains("↑"),
            "{all}"
        );

        // A worker that starts while the section is scrolled is in view.
        for _ in 0..10 {
            board.key(Key::Char('j'));
        }
        board.update(
            vec![worker(9, 1, "w9", Some("running"), 1)],
            start + Duration::from_secs(3),
        );
        let all = view(&board);
        assert!(all.contains("▶1 ● w9"), "{all}");
    }

    // Review B09: at 30 columns the state column was dropped, so 承認待ち,
    // 入力待ち and 通知あり (one colour) looked the same.
    #[test]
    fn a_narrow_pane_shortens_the_state_before_dropping_it() {
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = sample();
        panels[0].agent.as_mut().unwrap().state = Some("input".to_string());
        board.update(panels, Instant::now());
        let all = rendered(&board, 26, 20, Instant::now());
        assert!(all.contains("承認 ") && all.contains("入力 "), "{all}");
        assert!(!all.contains("承認待ち"), "{all}");
        let wide = rendered(&board, 80, 20, Instant::now());
        assert!(
            wide.contains("承認待ち") && wide.contains("入力待ち"),
            "{wide}"
        );
    }

    fn failed_worker(pane: u64, code: i32) -> Panel {
        let mut panel = worker(pane, 1, "failer", None, 0);
        panel.agent = None;
        panel.exit = Some(PanelExit { code: Some(code) });
        panel
    }

    // Review B06: a command that exited 3 was grey, listed last as "終了 3",
    // and said only that its process ended.
    #[test]
    fn a_failed_command_needs_someone_and_says_its_last_line() {
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = sample();
        panels.push(failed_worker(6, 3));
        board.update(panels, Instant::now());
        assert_eq!(Status::of(board.panel(6).unwrap()).group(), Group::Needs);
        board.set_screen(
            6,
            "about to fail\nboom\n\n[Process exited. Press any key to close the terminal.]\n",
        );
        let all = rendered(&board, 80, 40, Instant::now());
        let row = all.lines().find(|l| l.contains("failer")).unwrap();
        assert!(row.contains("失敗 3") && row.contains("boom"), "{all}");
        // Listed with what needs someone, before the working ones.
        assert!(
            all.find("failer").unwrap() < all.find("● api").unwrap(),
            "{all}"
        );
        assert!(!all.contains("[Process exited"), "{all}");
        // Red, like an error.
        let lines = board.render(80, 40, Instant::now());
        let line = lines
            .iter()
            .find(|l| visible(l).contains("failer"))
            .unwrap();
        let Rgb(r, g, b) = RED;
        assert!(
            line.contains(&format!("38;2;{r};{g};{b}m失敗 3")),
            "{line:?}"
        );
        // A clean exit stays where it was.
        assert_eq!(Status::Exited(Some(0)).group(), Group::Exited);
        assert_eq!(Status::Exited(None).group(), Group::Exited);
    }

    // Review B05: `s` on an exited worker opened the send line, and the
    // send failed with "rpc error -32004: pane closed".
    #[test]
    fn nothing_is_sent_to_an_exited_worker() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        board.select(5);
        assert_eq!(board.key(Key::Char('s')), None);
        assert_eq!(board.mode, Mode::Normal);
        let message = board.message.clone().unwrap();
        assert!(message.warn, "{message:?}");
        assert_eq!(message.text, "tests は終了しています。x で閉じる");
        for view in [View::List, View::Detail(5)] {
            board.view = view;
            let footer = visible(&board.render(120, 20, Instant::now()).pop().unwrap());
            assert!(!footer.contains("s 送る"), "{footer}");
            assert!(footer.contains("x 閉じる"), "{footer}");
        }

        struct Closed;
        impl Server for Closed {
            fn panels(&mut self) -> Result<Vec<Panel>, noa_ipc::ClientError> {
                Ok(Vec::new())
            }
            fn focus(&mut self, _pane: u64) -> Result<(), noa_ipc::ClientError> {
                Err(noa_ipc::ClientError::Rpc {
                    code: -32004,
                    message: "pane closed".to_string(),
                })
            }
            fn type_text(
                &mut self,
                _pane: u64,
                _text: &str,
                _paste: bool,
            ) -> Result<(), noa_ipc::ClientError> {
                Ok(())
            }
            fn close(&mut self, _pane: u64) -> Result<(), noa_ipc::ClientError> {
                Ok(())
            }
        }
        let notice = perform(&mut Closed, &board, Action::Focus(5));
        assert!(!notice.text.contains("-32004"), "{notice:?}");
        assert!(
            notice.text.contains("終了したか、閉じられています"),
            "{notice:?}"
        );
    }

    // Review B04: a number was the row's position, so it changed whenever
    // the list was sorted again, and `3` could pick another worker.
    #[test]
    fn a_worker_keeps_its_number_however_the_list_is_sorted() {
        let start = Instant::now();
        let mut board = Board::new(Some(1), None);
        let mut panels: Vec<Panel> = (2..5)
            .map(|pane| worker(pane, 1, &format!("w{pane}"), Some("running"), 1))
            .collect();
        board.update(panels.clone(), start);
        // w4 starts waiting and moves to the top; numbers stay.
        panels[2].agent.as_mut().unwrap().state = Some("permission".to_string());
        panels[2].agent.as_mut().unwrap().seq = 2;
        board.update(panels.clone(), start + Duration::from_secs(1));
        let order: Vec<u64> = board.workers().iter().map(|p| p.pane_id.0).collect();
        assert_eq!(order, vec![4, 2, 3]);
        let all = rendered(&board, 80, 30, start + Duration::from_secs(1));
        assert!(all.contains("3 ! w4") && all.contains("1 ● w2"), "{all}");
        board.key(Key::Char('2'));
        assert_eq!(board.selected(), Some(3));

        // A closed worker's number goes to the next one spawned.
        panels.remove(0);
        panels.push(worker(7, 1, "w7", Some("running"), 1));
        board.update(panels, start + Duration::from_secs(2));
        board.key(Key::Char('1'));
        assert_eq!(board.selected(), Some(7));
        board.key(Key::Char('3'));
        assert_eq!(board.selected(), Some(4));
    }

    // Review B08: within 要対応 workers were in pane order, so an approval
    // holding up a turn could sit below a fresh notification.
    #[test]
    fn needing_workers_are_ordered_by_what_blocks_and_by_how_long() {
        let start = Instant::now();
        let mut board = Board::new(Some(1), None);
        let mut panels = vec![
            worker(2, 1, "asks", Some("input"), 1),
            worker(3, 1, "late", Some("running"), 1),
            worker(4, 1, "early", Some("running"), 1),
            worker(5, 1, "calm", Some("running"), 1),
        ];
        panels[3].attention = true;
        board.update(panels.clone(), start);
        panels[2].agent.as_mut().unwrap().state = Some("permission".to_string());
        panels[2].agent.as_mut().unwrap().seq = 2;
        board.update(panels.clone(), start + Duration::from_secs(1));
        panels[1].agent.as_mut().unwrap().state = Some("error".to_string());
        panels[1].agent.as_mut().unwrap().seq = 2;
        board.update(panels, start + Duration::from_secs(5));
        let order: Vec<u64> = board.workers().iter().map(|p| p.pane_id.0).collect();
        // Blocking first, oldest first (early since 1 s, late since 5 s);
        // then input and attention, both found at start, in spawn order.
        assert_eq!(order, vec![4, 3, 2, 5]);
    }

    // Review B01: an opened worker covers the board below its top 35%, and
    // the result of `g`, every later warning and the send line were in the
    // bottom rows.
    #[test]
    fn results_prompts_and_the_send_line_sit_under_the_header() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        let top = |board: &Board| visible(&board.render(80, 30, Instant::now())[2]);
        assert!(top(&board).contains("? キー一覧"));
        board.message = Some(Notice::ok("ui へ移動しました".to_string()));
        assert!(top(&board).contains("ui へ移動しました"));
        board.view = View::Detail(3);
        let detail = board.render(80, 30, Instant::now());
        assert!(visible(&detail[1]).contains("ui へ移動しました"));
        board.view = View::List;
        board.key(Key::Char('j'));
        board.key(Key::Char('s'));
        board.key(Key::Char('h'));
        assert!(top(&board).contains("api に送る › h█"));
        // Only the keys are at the bottom.
        let lines = board.render(80, 30, Instant::now());
        assert!(visible(&lines[29]).contains("Enter 送信"));
        assert!(
            !lines[..29]
                .iter()
                .skip(3)
                .any(|l| visible(l).contains("に送る"))
        );
    }

    // Review B10: at about 80 columns the footer lost `x` and `q`, the help
    // was cut without notice, and a narrow header cut its count.
    #[test]
    fn the_footer_keeps_close_and_quit_and_the_help_scrolls() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        // ui waits for an approval: "g 移動して答える" is the longest label.
        for cols in [60, 72, 80] {
            let footer = visible(&board.render(cols, 20, Instant::now()).pop().unwrap());
            assert!(
                footer.contains("x 閉じる")
                    && footer.contains("q 終了")
                    && footer.contains("? ヘルプ"),
                "{cols}: {footer}"
            );
        }
        let footer = visible(&board.render(80, 20, Instant::now()).pop().unwrap());
        assert!(footer.contains("j/k 選択"), "{footer}");

        board.key(Key::Char('?'));
        let all = rendered(&board, 60, 14, Instant::now());
        assert!(all.contains("↓ 続き"), "{all}");
        assert!(!all.contains("^U / ^W"), "{all}");
        for _ in 0..20 {
            assert_eq!(board.key(Key::Char('j')), None);
        }
        assert!(board.help);
        let all = rendered(&board, 60, 14, Instant::now());
        assert!(all.contains("^U / ^W") && !all.contains("↓ 続き"), "{all}");
        // k comes straight back, however far j was pressed.
        board.key(Key::Char('k'));
        assert!(rendered(&board, 60, 14, Instant::now()).contains("↓ 続き"));
        board.key(Key::Char('x'));
        assert!(!board.help);

        let header = visible(&board.render(30, 12, Instant::now())[0]);
        assert!(
            header.starts_with(" ボード") && header.contains("ワーカー 4"),
            "{header}"
        );
    }

    // Review B07: one stray `q` ended the board and handed every worker
    // back to the orchestrator, which the board cannot undo.
    #[test]
    fn quitting_with_workers_asks_first() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        assert_eq!(board.key(Key::Char('q')), None);
        assert_eq!(board.mode, Mode::ConfirmQuit);
        let top = visible(&board.render(100, 20, Instant::now())[2]);
        assert!(
            top.contains("収まらないワーカーは閉じ") && top.contains("再度 q で終了"),
            "{top}"
        );
        // Any other key takes it back.
        assert_eq!(board.key(Key::Char('j')), None);
        assert_eq!(board.mode, Mode::Normal);
        assert_eq!(board.key(Key::CtrlC), None);
        assert_eq!(board.key(Key::CtrlC), Some(Action::Quit));

        let mut empty = Board::new(Some(1), Some(8));
        empty.update(Vec::new(), Instant::now());
        assert_eq!(empty.key(Key::Char('q')), Some(Action::Quit));
    }

    // Review B02: one failed listing ended the board.
    #[test]
    fn failed_listings_are_retried_with_backoff_for_thirty_seconds() {
        let t0 = Instant::now();
        let mut polling = Polling::default();
        assert!(polling.due(t0));
        polling.succeeded(t0);
        assert!(!polling.due(t0 + Duration::from_millis(400)));
        assert!(polling.due(t0 + LIST_EVERY));

        let mut at = t0 + LIST_EVERY;
        let mut waits = Vec::new();
        while polling.failed(at) {
            assert_eq!(polling.failing_since(), Some(t0 + LIST_EVERY));
            let next = polling.next.unwrap();
            waits.push(next - at);
            assert!(!polling.due(next - Duration::from_millis(1)));
            at = next;
        }
        assert_eq!(
            waits[..5],
            [500, 1000, 2000, 4000, 4000].map(Duration::from_millis)
        );
        let failed_for = at - (t0 + LIST_EVERY);
        assert!(failed_for >= GIVE_UP_AFTER && failed_for < GIVE_UP_AFTER + RETRY_MAX);

        // One listing that works starts the count again.
        let mut polling = Polling::default();
        assert!(polling.failed(t0));
        polling.succeeded(t0 + Duration::from_secs(20));
        assert_eq!(polling.failing_since(), None);
        assert!(polling.failed(t0 + Duration::from_secs(40)));
    }

    #[test]
    fn the_board_says_it_cannot_connect_and_keeps_what_it_had() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), t0);
        board.offline = Some(t0);
        let all = rendered(&board, 80, 30, t0 + Duration::from_secs(7));
        assert!(all.contains("接続できません · 再試行中 7 秒"), "{all}");
        assert!(all.contains("ui") && all.contains("api"), "{all}");
    }

    // Review B11: "作業中 12 分" looked the same whether the worker was
    // streaming output or stuck.
    #[test]
    fn a_working_worker_whose_screen_stays_the_same_says_so() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), t0);
        let rule = "─".repeat(40);
        let screen = |clock: u32| format!("⏺ Reading src\n{rule}\n› \n{rule}\n  {clock}s\n");
        board.set_screen_at(2, &screen(1), t0);
        // The status line ticks; the work does not.
        board.set_screen_at(2, &screen(2), t0 + Duration::from_secs(60));
        let at = t0 + Duration::from_secs(181);
        board.set_screen_at(2, &screen(3), at);
        let all = rendered(&board, 80, 40, at);
        assert!(all.contains("● api  作業中 出力なし 3 分"), "{all}");
        let lines = board.render(80, 40, at);
        let row = lines.iter().find(|l| visible(l).contains("● api")).unwrap();
        let Rgb(r, g, b) = DIM;
        assert!(
            row.contains(&format!("38;2;{r};{g};{b}m 出力なし 3 分")),
            "{row:?}"
        );

        // New work clears it.
        board.set_screen_at(
            2,
            "⏺ Reading src\n⏺ Wrote lib.rs\n",
            at + Duration::from_secs(1),
        );
        let all = rendered(&board, 80, 40, at + Duration::from_secs(9));
        assert!(!all.contains("出力なし"), "{all}");
        // It says when it last showed output instead (D15).
        assert!(all.contains("● api  作業中 8 秒前に出力"), "{all}");

        // Only working workers, and not before the current state began.
        board.set_screen_at(4, "done\n", t0);
        board.set_screen_at(4, "done\n", at);
        assert_eq!(board.quiet_for(4, Status::Finished), None);
        let mut next = sample();
        next[0].agent.as_mut().unwrap().seq = 9;
        next[0].agent.as_mut().unwrap().state = Some("permission".to_string());
        board.update(next.clone(), at + Duration::from_secs(149));
        next[0].agent.as_mut().unwrap().state = Some("running".to_string());
        board.update(next, at + Duration::from_secs(150));
        board.set_screen_at(
            2,
            "⏺ Reading src\n⏺ Wrote lib.rs\n",
            at + Duration::from_secs(200),
        );
        assert_eq!(board.quiet_for(2, Status::Running), None);
    }

    // Review B12: the list read the screens of finished and exited workers
    // whose rows show no preview.
    #[test]
    fn the_list_reads_only_the_screens_it_shows() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        // ui (permission) is selected; api runs; docs finished; tests exited.
        assert_eq!(board.wanted_screens(), vec![3, 2]);
        board.select(4);
        assert_eq!(board.wanted_screens(), vec![3, 2, 4]);
        board.view = View::Detail(5);
        assert_eq!(board.wanted_screens(), vec![5]);
    }

    /// Orchestrator 1's workers lead (2, numbered 1) and solo (3, numbered
    /// 2); lead's own workers sub-a (10, 3) and sub-b (11, 4); another
    /// orchestrator's worker (4) with one of its own (12); the board (8).
    fn nested() -> Vec<Panel> {
        vec![
            worker(2, 1, "lead", Some("running"), 1),
            worker(3, 1, "solo", Some("running"), 1),
            worker(10, 2, "sub-a", Some("running"), 1),
            worker(11, 2, "sub-b", Some("running"), 1),
            worker(4, 7, "other", Some("running"), 1),
            worker(12, 4, "other-sub", Some("running"), 1),
            worker(8, 1, "board", None, 0),
        ]
    }

    fn order(board: &Board) -> Vec<u64> {
        board.workers().iter().map(|p| p.pane_id.0).collect()
    }

    /// The drawn line that contains `text`.
    fn line_with(all: &str, text: &str) -> String {
        all.lines()
            .find(|line| line.contains(text))
            .unwrap_or_else(|| panic!("no line with {text:?} in\n{all}"))
            .to_string()
    }

    // D13: a worker's workers are listed right under it, further in, on a
    // tree line that runs down past their work.
    #[test]
    fn workers_of_workers_are_drawn_under_their_parent_on_a_tree_line() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(nested(), Instant::now());
        assert_eq!(order(&board), vec![2, 10, 11, 3]);
        let all = rendered(&board, 80, 30, Instant::now());
        assert!(!all.contains("other"), "{all}");
        assert!(line_with(&all, "lead ").starts_with(" ▶1 ● lead"), "{all}");
        assert!(
            line_with(&all, "sub-a").starts_with("    ├ 3 ● sub-a"),
            "{all}"
        );
        assert!(
            line_with(&all, "sub-b").starts_with("    └ 4 ● sub-b"),
            "{all}"
        );
        assert!(line_with(&all, "solo").starts_with("  2 ● solo"), "{all}");
        // A board of every worker draws the same tree.
        let mut every = Board::new(None, Some(8));
        every.update(nested(), Instant::now());
        let order = order(&every);
        let at = order.iter().position(|&pane| pane == 4).unwrap();
        assert_eq!(order[at + 1], 12, "{order:?}");
    }

    // D13: a subtree is placed by its most urgent worker, and a worker's
    // workers are ordered under it by D11's rules.
    #[test]
    fn a_worker_needing_someone_is_lifted_into_the_queue() {
        let start = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = nested();
        panels.push(worker(5, 1, "asks", Some("input"), 1));
        board.update(panels.clone(), start);
        assert_eq!(order(&board), vec![5, 2, 10, 11, 3]);
        assert_eq!(queue(&board), vec![5]);
        // sub-b waits for an approval: it holds work up, so it goes before
        // a question in the queue, named after its parent, while its row
        // in the tree stays under lead and points up.
        panels[3].agent.as_mut().unwrap().state = Some("permission".to_string());
        panels[3].agent.as_mut().unwrap().seq = 2;
        board.update(panels.clone(), start + Duration::from_secs(1));
        assert_eq!(queue(&board), vec![11, 5]);
        assert_eq!(order(&board), vec![5, 2, 11, 10, 3]);
        let all = rendered(&board, 80, 30, start + Duration::from_secs(1));
        assert!(all.contains("─ ! 人の対応が必要 2 "), "{all}");
        assert!(all.contains("5 ! lead › sub-b"), "{all}");
        assert!(
            line_with(&all, "├ 5 ! sub-b").contains("↑ 要対応に表示"),
            "{all}"
        );
        // lead and its other worker are at work, solo too; sub-b is not
        // counted twice.
        assert!(all.contains("─ ● バックグラウンド 3 "), "{all}");
        // An older approval (sub-b's, since 1 s) goes before a newer error.
        panels[1].agent.as_mut().unwrap().state = Some("error".to_string());
        panels[1].agent.as_mut().unwrap().seq = 2;
        board.update(panels, start + Duration::from_secs(5));
        assert_eq!(queue(&board), vec![11, 3, 5]);
        // The echo is never selected: j steps from the queue into the tree.
        board.key(Key::Char('n'));
        assert_eq!(board.selected(), Some(11));
        for want in [3, 5, 2, 10] {
            board.key(Key::Char('j'));
            assert_eq!(board.selected(), Some(want));
        }
    }

    fn queue(board: &Board) -> Vec<u64> {
        board
            .layout()
            .iter()
            .filter(|entry| entry.queued)
            .map(|entry| entry.node.panel.pane_id.0)
            .collect()
    }

    // D13: a parent's row tallies its workers and shows whether they are
    // open; Space folds them away and opens them again.
    #[test]
    fn a_parent_tallies_its_workers_and_space_folds_them() {
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = nested();
        board.update(panels.clone(), Instant::now());
        let all = rendered(&board, 100, 30, Instant::now());
        let lead = line_with(&all, "● lead");
        assert!(
            lead.contains("▾ 子 2 ") && !lead.contains("要対応"),
            "{all}"
        );
        assert!(!line_with(&all, "solo").contains("子"), "{all}");

        panels[3].agent.as_mut().unwrap().state = Some("permission".to_string());
        board.update(panels, Instant::now());
        let all = rendered(&board, 100, 30, Instant::now());
        assert!(
            line_with(&all, "● lead").contains("▾ 子 2 · 要対応 1"),
            "{all}"
        );
        let footer = visible(&board.render(120, 30, Instant::now()).pop().unwrap());
        assert!(footer.contains("Space たたむ"), "{footer}");

        // Space on the parent folds; the selection stays on it.
        board.key(Key::Char('1'));
        board.key(Key::Char(' '));
        let all = rendered(&board, 100, 30, Instant::now());
        assert!(
            line_with(&all, "● lead").contains("▸ 子 2 · 要対応 1"),
            "{all}"
        );
        assert!(!all.contains("sub-a") && !all.contains("├"), "{all}");
        let footer = visible(&board.render(120, 30, Instant::now()).pop().unwrap());
        assert!(footer.contains("Space 開く"), "{footer}");
        // j steps over what is folded away.
        board.key(Key::Char('j'));
        assert_eq!(board.selected(), Some(3));
        board.key(Key::Char('k'));
        board.key(Key::Char(' '));
        assert!(rendered(&board, 100, 30, Instant::now()).contains("sub-a"));

        // Space on one of its workers folds its parent's, and selects it.
        board.key(Key::Char('3'));
        assert_eq!(board.selected(), Some(10));
        board.key(Key::Char(' '));
        assert_eq!(board.selected(), Some(2));
        assert!(!rendered(&board, 100, 30, Instant::now()).contains("sub-a"));

        // A worker without workers has nothing to fold, and says so.
        board.key(Key::Char('2'));
        board.key(Key::Char(' '));
        assert!(board.message.as_ref().is_some_and(|m| !m.warn));
        assert!(board.collapsed.contains(&2));
        let footer = visible(&board.render(120, 30, Instant::now()).pop().unwrap());
        assert!(!footer.contains("Space"), "{footer}");
    }

    // D13: numbers, n, the action keys and every count take a worker's
    // workers in; a number or n into a folded subtree opens it.
    #[test]
    fn workers_of_workers_are_numbered_counted_and_acted_on() {
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = nested();
        panels[3].agent.as_mut().unwrap().state = Some("permission".to_string());
        board.update(panels, Instant::now());
        let all = rendered(&board, 80, 30, Instant::now());
        assert!(
            line_with(&all, "NOA / CONTROL").contains("ワーカー 4"),
            "{all}"
        );
        assert!(all.contains("! 要対応 1   ● 作業中 3"), "{all}");
        assert_eq!(board.title(), "ボード · 要対応 1");

        // sub-b needs someone: it is selected in the queue, and its
        // parent's subtree stays folded (D15).
        board.collapsed.insert(2);
        board.key(Key::Char('4'));
        assert_eq!(board.selected(), Some(11));
        assert!(board.collapsed.contains(&2));
        board.key(Key::Char('2'));
        board.key(Key::Char('n'));
        assert_eq!(board.selected(), Some(11));
        // A number opens the subtree a worker at work is folded away in.
        board.key(Key::Char('3'));
        assert_eq!(board.selected(), Some(10));
        assert!(!board.collapsed.contains(&2), "a number opens the subtree");
        // Folded or not, the screens of workers at work or needing someone
        // are read: their rows say when they last showed output.
        board.collapsed.insert(2);
        board.key(Key::Char('2'));
        assert!(board.wanted_screens().contains(&11));
        assert!(board.wanted_screens().contains(&10));

        board.key(Key::Char('3'));
        assert_eq!(board.key(Key::Char('g')), Some(Action::Focus(10)));
        board.key(Key::Char('s'));
        board.key(Key::Char('y'));
        assert_eq!(board.key(Key::Enter), Some(Action::Send(10, "y".into())));
        board.key(Key::Char('x'));
        assert_eq!(board.key(Key::Char('y')), Some(Action::Close(10)));
        // sub-b waits for an approval: answered in its pane, as any worker.
        board.key(Key::Char('4'));
        board.key(Key::Char('s'));
        assert_eq!(board.mode, Mode::Normal);
    }

    // D13 with D11: a narrow pane says the tally more briefly before it
    // leaves it out, and keeps the ▾/▸ the longest.
    #[test]
    fn a_narrow_pane_shortens_the_tally_and_keeps_its_mark() {
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = nested();
        panels[3].agent.as_mut().unwrap().state = Some("permission".to_string());
        board.update(panels, Instant::now());
        for pane in [2, 3, 10, 11] {
            board.set_screen(pane, "some work on screen\n");
        }
        board.key(Key::Char('4'));
        for cols in [24, 30, 41, 50, 60, 80, 120] {
            for folded in [false, true] {
                if folded {
                    board.collapsed.insert(2);
                } else {
                    board.collapsed.remove(&2);
                }
                let lines = board.render(cols, 30, Instant::now());
                for line in &lines {
                    assert_eq!(visible(line).width(), cols, "{cols}: {:?}", visible(line));
                }
                let all: String = lines.iter().map(|l| visible(l) + "\n").collect();
                let mark = if folded { "▸" } else { "▾" };
                assert!(line_with(&all, "● lead").contains(mark), "{cols}: {all}");
            }
        }
        board.collapsed.clear();
        let full = rendered(&board, 100, 30, Instant::now());
        assert!(
            line_with(&full, "● lead").contains("▾ 子 2 · 要対応 1"),
            "{full}"
        );
        let short = rendered(&board, 50, 30, Instant::now());
        assert!(
            line_with(&short, "● lead").contains("▾2 要対応1"),
            "{short}"
        );
    }

    #[test]
    fn the_help_tells_what_space_does() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(nested(), Instant::now());
        board.key(Key::Char('?'));
        let all = rendered(&board, 80, 40, Instant::now());
        assert!(
            line_with(&all, "Space").contains("ワーカーの子をたたむ / 開く"),
            "{all}"
        );
    }

    // D13 with D12: ending the board closes the workers of workers, whose
    // parent is a float once it is back on the orchestrator.
    #[test]
    fn closing_a_worker_says_its_own_workers_close_with_it() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(nested(), Instant::now());
        board.key(Key::Char('1'));
        assert_eq!(board.selected(), Some(2));
        board.key(Key::Char('x'));
        let top = visible(&board.render(200, 20, Instant::now())[2]);
        assert!(top.contains("ワーカーの子 2 件も閉じます"), "{top}");
    }

    #[test]
    fn quitting_says_the_workers_of_workers_close() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(nested(), Instant::now());
        board.key(Key::Char('q'));
        let top = visible(&board.render(200, 20, Instant::now())[2]);
        assert!(
            top.contains(
                "ワーカーの子 2 件と、オーケストレーターに収まらないワーカーは閉じ、残りは戻ります"
            ),
            "{top}"
        );
    }

    // Review finding: the prompt promised that every worker goes back to
    // the orchestrator, which holds 8 floats with those it already has, and
    // at 80 columns in English its warning about the rest was cut off.
    #[test]
    fn quitting_warns_first_that_workers_that_do_not_fit_close() {
        let panels: Vec<Panel> = (20..29)
            .map(|pane| worker(pane, 1, &format!("w{pane}"), Some("running"), 1))
            .collect();
        for lang in [Lang::En, Lang::Ja] {
            let mut board = Board::new(Some(1), None);
            board.lang = lang;
            board.update(panels.clone(), Instant::now());
            board.key(Key::Char('q'));
            let top = visible(&board.render(80, 20, Instant::now())[2]);
            let (warning, quit) = lang.pick(
                (
                    "オーケストレーターに収まらないワーカーは閉じ",
                    "再度 q で終了",
                ),
                (
                    "Workers that don't fit on the orchestrator close",
                    "q again quits",
                ),
            );
            assert!(top.trim_start().starts_with(warning), "{top}");
            assert!(top.contains(quit), "whole at 80 columns: {top}");
            assert!(!top.contains('9') && !top.contains('8'), "no count: {top}");
        }
    }
    // D15: the selected worker's screen sits beside the list in a wide
    // pane, under it in a pane about 80 wide, and nowhere in a narrow one.
    #[test]
    fn the_preview_sits_beside_under_or_nowhere_by_width() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        board.set_screen(3, "reading src\nAllow command? cargo test\n");
        for cols in [24, 44, 45, 69, 70, 80, 109, 110, 140] {
            let lines = board.render(cols, 24, Instant::now());
            assert_eq!(lines.len(), 24);
            for line in &lines {
                assert_eq!(visible(line).width(), cols, "{cols}: {:?}", visible(line));
            }
            let all: String = lines.iter().map(|l| visible(l) + "\n").collect();
            let title = all.lines().position(|line| line.contains("選択中"));
            let queue = all.lines().position(|line| line.contains("人の対応が必要"));
            match cols {
                ..70 => assert_eq!(title, None, "{cols}: {all}"),
                70..110 => assert!(title > queue, "{cols}: {all}"),
                _ => {
                    assert_eq!(title, queue, "{cols}: {all}");
                    assert!(line_with(&all, "▶").contains("│"), "{cols}: {all}");
                }
            }
            if title.is_some() {
                assert!(all.contains("Allow command? cargo test"), "{cols}: {all}");
                assert!(
                    all.contains("claude · 承認待ち · 報告 5 · /repo"),
                    "{cols}: {all}"
                );
            }
        }
        // Too short for it under the list: the rows keep the room.
        let all = rendered(&board, 80, 17, Instant::now());
        assert!(!all.contains("選択中"), "{all}");
    }

    // D15: a worker waiting on someone asks at the bottom of its screen,
    // in the agent's input box; one at work shows the work above that box.
    #[test]
    fn the_preview_shows_a_question_whole_and_work_above_the_input_box() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        let rule = "─".repeat(40);
        let screen = format!(
            "⏺ Read src\n{rule}\n> formatter を選んでください\n{rule}\n  ? for shortcuts\n"
        );
        board.set_screen(3, &screen);
        board.set_screen(2, &screen);
        let all = rendered(&board, 80, 30, Instant::now());
        assert!(all.contains("> formatter を選んでください"), "{all}");
        board.key(Key::Char('j'));
        assert_eq!(board.selected(), Some(2));
        let all = rendered(&board, 80, 30, Instant::now());
        assert!(all.contains("⏺ Read src"), "{all}");
        assert!(!all.contains("formatter"), "{all}");
    }

    // D15: Space folds the subtree the selection is in; elsewhere it lists
    // the finished and exited workers, which are only counted at first.
    #[test]
    fn space_lists_the_finished_and_exited_workers_outside_a_subtree() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        assert_eq!(board.selected(), Some(3));
        let all = rendered(&board, 80, 30, Instant::now());
        assert!(!all.contains("docs") && all.contains("Space 開く"), "{all}");
        let footer = visible(&board.render(140, 30, Instant::now()).pop().unwrap());
        assert!(footer.contains("Space 完了を開く"), "{footer}");
        board.key(Key::Char(' '));
        let all = rendered(&board, 80, 30, Instant::now());
        assert!(all.contains("✓ docs") && all.contains("· tests"), "{all}");
        assert!(all.contains("Space たたむ"), "{all}");

        // A selection folded away moves to the last row left.
        board.key(Key::Char('j'));
        board.key(Key::Char('j'));
        assert_eq!(board.selected(), Some(4));
        board.key(Key::Char(' '));
        assert_eq!(board.selected(), Some(2));
        assert!(!rendered(&board, 80, 30, Instant::now()).contains("docs"));
        // j past the last row opens them again.
        board.key(Key::Char('j'));
        assert_eq!(board.selected(), Some(4));
        assert!(board.settled_open);
    }

    // D15: a working worker's row says how fresh its output is, from the
    // second read on: the first read says nothing about when it moved.
    #[test]
    fn a_working_worker_says_when_it_last_showed_output() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), t0);
        board.set_screen_at(2, "one\n", t0);
        let all = rendered(&board, 80, 30, t0 + Duration::from_secs(5));
        assert!(!all.contains("前に出力"), "{all}");
        board.set_screen_at(2, "one\ntwo\n", t0 + Duration::from_secs(1));
        board.set_screen_at(2, "one\ntwo\n", t0 + Duration::from_secs(4));
        let all = rendered(&board, 80, 30, t0 + Duration::from_secs(9));
        assert!(all.contains("● api  作業中 8 秒前に出力"), "{all}");
    }

    // Review 1: a subtree folded while at work, then finished, stayed
    // folded in the finished section, where Space cannot open it.
    #[test]
    fn a_subtree_folded_at_work_is_listed_whole_once_finished() {
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = nested();
        board.update(panels.clone(), Instant::now());
        board.key(Key::Char('1'));
        board.key(Key::Char(' '));
        assert!(board.collapsed.contains(&2));
        for at in [0, 2, 3] {
            panels[at].agent.as_mut().unwrap().state = Some("finished".to_string());
        }
        board.update(panels, Instant::now());
        board.key(Key::Char(' '));
        assert!(board.settled_open);
        let all = rendered(&board, 80, 30, Instant::now());
        assert!(all.contains("sub-a") && all.contains("sub-b"), "{all}");
        assert!(board.picks().contains(&10), "{:?}", board.picks());
    }

    // Review 2/4: a parent needing someone folded with Space could not be
    // opened with Space again, nor its workers reached with j/k.
    #[test]
    fn space_opens_again_a_subtree_whose_parent_needs_someone() {
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = nested();
        panels[0].agent.as_mut().unwrap().state = Some("permission".to_string());
        board.update(panels, Instant::now());
        board.key(Key::Char('3'));
        assert_eq!(board.selected(), Some(10));
        board.key(Key::Char(' '));
        assert!(board.collapsed.contains(&2));
        board.key(Key::Char(' '));
        assert!(!board.collapsed.contains(&2), "Space opens it again");
        assert!(!board.settled_open);
    }

    // Review 3: a worker selected in the queue that went back to work was
    // left selected inside its folded parent, with no row marked.
    #[test]
    fn a_selection_leaving_the_queue_opens_the_subtree_it_goes_back_to() {
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = nested();
        panels[3].agent.as_mut().unwrap().state = Some("permission".to_string());
        board.update(panels.clone(), Instant::now());
        board.collapsed.insert(2);
        board.key(Key::Char('4'));
        assert_eq!(board.selected(), Some(11));
        panels[3].agent.as_mut().unwrap().state = Some("running".to_string());
        panels[3].agent.as_mut().unwrap().seq = 2;
        board.update(panels, Instant::now());
        assert_eq!(board.selected(), Some(11));
        let all = rendered(&board, 80, 30, Instant::now());
        assert!(line_with(&all, "sub-b").contains("▶"), "{all}");
    }

    // Review 5: a long parent name widened the queue's name column until
    // the state column was dropped from every row.
    #[test]
    fn a_long_parent_name_in_the_queue_keeps_the_state_column() {
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = nested();
        panels[0].label = Some("a-very-long-parent-worker-name".to_string());
        panels[3].agent.as_mut().unwrap().state = Some("permission".to_string());
        board.update(panels, Instant::now());
        let all = rendered(&board, 40, 20, Instant::now());
        // The queue row: the parent's name, then the worker's own.
        let row = line_with(&all, "▶");
        assert!(row.contains("sub-b") && row.contains("承認"), "{all}");
        assert!(line_with(&all, "● solo").contains("作業"), "{all}");
    }

    // Review 6: a screen not read for a while (the detail was open, or the
    // worker was not at work) that changed meanwhile said "0 秒前に出力".
    #[test]
    fn a_change_seen_after_a_gap_in_reading_has_no_time() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), t0);
        board.set_screen_at(2, "one\n", t0);
        board.set_screen_at(2, "one\ntwo\n", t0 + Duration::from_secs(60));
        let all = rendered(&board, 80, 30, t0 + Duration::from_secs(61));
        assert!(!all.contains("前に出力"), "{all}");
    }

    // Review 7: a working worker whose screen was not yet seen to move
    // showed how long it had worked instead of nothing.
    #[test]
    fn a_working_worker_without_seen_output_shows_no_time() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = sample();
        panels[0].agent.as_mut().unwrap().state = Some("finished".to_string());
        board.update(panels.clone(), t0);
        panels[0].agent.as_mut().unwrap().state = Some("running".to_string());
        panels[0].agent.as_mut().unwrap().seq = 5;
        board.update(panels, t0 + Duration::from_secs(1));
        let all = rendered(&board, 80, 30, t0 + Duration::from_secs(5));
        assert!(line_with(&all, "● api").contains("作業中  "), "{all}");
        assert!(!line_with(&all, "● api").contains("秒"), "{all}");
    }

    // Review 8: output from a turn before the current one was said as the
    // current turn's.
    #[test]
    fn output_from_an_earlier_turn_is_not_the_current_turns() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = sample();
        board.update(panels.clone(), t0);
        board.set_screen_at(2, "one\n", t0);
        board.set_screen_at(2, "one\ntwo\n", t0 + Duration::from_secs(1));
        panels[0].agent.as_mut().unwrap().state = Some("finished".to_string());
        panels[0].agent.as_mut().unwrap().seq = 5;
        board.update(panels.clone(), t0 + Duration::from_secs(10));
        panels[0].agent.as_mut().unwrap().state = Some("running".to_string());
        panels[0].agent.as_mut().unwrap().seq = 6;
        board.update(panels, t0 + Duration::from_secs(300));
        let all = rendered(&board, 80, 30, t0 + Duration::from_secs(301));
        assert!(!all.contains("前に出力"), "{all}");
    }

    // Review 2-1: a subtree folded at work kept its folded mark once
    // finished, while its workers were listed, and came back folded.
    #[test]
    fn a_finished_subtree_drops_its_folded_mark() {
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = nested();
        board.update(panels.clone(), Instant::now());
        board.key(Key::Char('1'));
        board.key(Key::Char(' '));
        for at in [0, 2, 3] {
            panels[at].agent.as_mut().unwrap().state = Some("finished".to_string());
        }
        board.update(panels.clone(), Instant::now());
        board.key(Key::Char(' '));
        let all = rendered(&board, 100, 30, Instant::now());
        assert!(line_with(&all, "✓ lead").contains("▾"), "{all}");
        // Back at work, it is open.
        panels[0].agent.as_mut().unwrap().state = Some("running".to_string());
        board.update(panels, Instant::now());
        assert!(!board.collapsed.contains(&2));
    }

    // Review 2-2: a change read across a change of state was dated as
    // output in the new state.
    #[test]
    fn a_change_read_across_a_change_of_state_has_no_time() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = sample();
        board.update(panels.clone(), t0);
        panels[0].agent.as_mut().unwrap().state = Some("permission".to_string());
        panels[0].agent.as_mut().unwrap().seq = 5;
        board.update(panels.clone(), t0 + Duration::from_secs(1));
        board.set_screen_at(2, "one\n", t0 + Duration::from_secs(2));
        panels[0].agent.as_mut().unwrap().state = Some("running".to_string());
        panels[0].agent.as_mut().unwrap().seq = 6;
        board.update(panels, t0 + Duration::from_secs(3));
        board.set_screen_at(2, "one\ntwo\n", t0 + Duration::from_secs(4));
        let all = rendered(&board, 80, 30, t0 + Duration::from_secs(5));
        assert!(!all.contains("前に出力"), "{all}");
    }

    // Review 2-3: the help said n opens a folded subtree, which it no
    // longer does: a worker needing someone is selected in the queue.
    #[test]
    fn the_help_says_what_n_does() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        board.key(Key::Char('?'));
        let all = rendered(&board, 100, 40, Instant::now());
        assert!(line_with(&all, "次の要対応").contains("キュー"), "{all}");
        assert!(!all.contains("たたんだ子も開いて"), "{all}");
    }

    // Review 2-4: before any listing was read, the header said it synced
    // at the start.
    #[test]
    fn a_board_never_synced_says_so() {
        let mut board = Board::new(Some(1), Some(8));
        board.offline = Some(Instant::now());
        let all = rendered(&board, 80, 20, Instant::now());
        assert!(!all.contains("開始時"), "{all}");
        assert!(all.contains("同期 まだ"), "{all}");
    }

    // Review 2-5/2-6: with no worker left, the closed ones are always
    // listed; Space offered to fold them and did nothing.
    #[test]
    fn with_no_worker_left_space_is_not_offered() {
        let start = Instant::now();
        let mut board = tall_board(3);
        board.update(Vec::new(), start + Duration::from_secs(1));
        assert!(!board.gone.is_empty());
        let footer = visible(&board.render(140, 20, start).pop().unwrap());
        assert!(!footer.contains("Space"), "{footer}");
        // Space changes nothing but saying there is nothing to fold.
        let list = |board: &Board| board.render(80, 20, start)[3..].to_vec();
        let before = list(&board);
        board.key(Key::Char(' '));
        assert_eq!(list(&board), before);
        assert!(!board.settled_open);
    }

    // A narrow pane says how fresh a working worker's output is briefly.
    #[test]
    fn a_narrow_pane_says_when_output_was_shown_briefly() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), t0);
        board.set_screen_at(2, "one\n", t0);
        board.set_screen_at(2, "one\ntwo\n", t0 + Duration::from_secs(1));
        let all = rendered(&board, 40, 20, t0 + Duration::from_secs(9));
        assert!(line_with(&all, "● api").contains("作業中 8 秒前"), "{all}");
        assert!(!all.contains("に出力"), "{all}");
    }

    // Review 3-1: a worker needing someone without a detail showed old
    // work from above its input box, not the question in it.
    #[test]
    fn a_row_without_a_detail_shows_the_question_at_the_bottom() {
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = sample();
        panels[1].agent.as_mut().unwrap().detail = None;
        board.update(panels, Instant::now());
        let rule = "─".repeat(40);
        board.set_screen(
            3,
            &format!("old work\n{rule}\n> formatter を選んでください\n{rule}\n"),
        );
        let all = rendered(&board, 60, 20, Instant::now());
        let row = line_with(&all, "! ui");
        assert!(row.contains("> formatter"), "{all}");
        assert!(!row.contains("old work"), "{all}");
    }

    // Review 3-2: a preview too narrow for the directory still drew its
    // separator at the end of the line.
    #[test]
    fn a_preview_without_room_for_the_directory_has_no_dangling_separator() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        let mut panels = sample();
        panels[0].agent.as_mut().unwrap().seq = 12;
        board.update(panels, t0);
        board.set_screen_at(2, "one\n", t0);
        board.set_screen_at(2, "one\ntwo\n", t0 + Duration::from_secs(1));
        board.key(Key::Char('j'));
        assert_eq!(board.selected(), Some(2));
        let all = rendered(&board, 110, 24, t0 + Duration::from_secs(11));
        let meta = line_with(&all, "報告 12");
        assert!(!meta.trim_end().ends_with('·'), "{all}");
    }

    // The board speaks Japanese where the locale is Japanese, English
    // anywhere else: the locale variables in POSIX order, else the system's
    // preferred language.
    #[test]
    fn the_language_follows_the_locale() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            }
        };
        let none = || None;
        let ja = || Some("ja-JP".to_string());
        assert_eq!(
            Lang::from_locale(env(&[("LANG", "ja_JP.UTF-8")]), none),
            Lang::Ja
        );
        assert_eq!(
            Lang::from_locale(env(&[("LANG", "en_US.UTF-8")]), ja),
            Lang::En
        );
        assert_eq!(
            Lang::from_locale(env(&[("LC_ALL", "C"), ("LANG", "ja_JP.UTF-8")]), none),
            Lang::En
        );
        assert_eq!(
            Lang::from_locale(env(&[("LC_MESSAGES", "ja_JP"), ("LANG", "en_US")]), none),
            Lang::Ja
        );
        // An empty variable is not set.
        assert_eq!(
            Lang::from_locale(env(&[("LC_ALL", ""), ("LANG", "ja_JP.UTF-8")]), none),
            Lang::Ja
        );
        // No locale variables (a Noa started from the Dock): the system's.
        assert_eq!(Lang::from_locale(env(&[]), ja), Lang::Ja);
        assert_eq!(
            Lang::from_locale(env(&[]), || Some("en-GB".to_string())),
            Lang::En
        );
        assert_eq!(Lang::from_locale(env(&[]), none), Lang::En);
    }

    fn japanese(text: &str) -> bool {
        text.chars()
            .any(|ch| matches!(ch, '\u{3040}'..='\u{30ff}' | '\u{4e00}'..='\u{9fff}'))
    }

    // In English, no label is Japanese, in any view or prompt, at any width.
    #[test]
    fn an_english_board_has_no_japanese_label() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        board.lang = Lang::En;
        let mut panels = sample();
        panels.extend(
            nested()
                .into_iter()
                .filter(|p| p.pane_id.0 >= 10 && p.pane_id.0 < 12),
        );
        panels.push(failed_worker(6, 2));
        board.update(panels.clone(), t0);
        panels[0].agent.as_mut().unwrap().seq = 9;
        board.update(panels[1..].to_vec(), t0 + Duration::from_secs(1));
        board.update(panels, t0 + Duration::from_secs(2));
        board.set_screen_at(2, "one\n", t0 + Duration::from_secs(3));
        board.set_screen_at(2, "one\ntwo\n", t0 + Duration::from_secs(4));
        let check = |board: &Board, what: &str| {
            for cols in [24, 40, 60, 80, 120] {
                let lines = board.render(cols, 30, t0 + Duration::from_secs(200));
                for line in &lines {
                    let line = visible(line);
                    assert_eq!(line.width(), cols, "{what} {cols}: {line:?}");
                    assert!(!japanese(&line), "{what} {cols}: {line:?}");
                }
            }
        };
        check(&board, "list");
        let all = rendered(&board, 120, 30, t0 + Duration::from_secs(8));
        assert!(
            all.contains("! Needs you") && all.contains("approval"),
            "{all}"
        );
        assert!(all.contains("working, output 4s ago"), "{all}");
        board.key(Key::Char(' '));
        check(&board, "settled");
        board.key(Key::Char('?'));
        check(&board, "help");
        board.key(Key::Char('?'));
        board.key(Key::Char('n'));
        board.key(Key::Char('s'));
        check(&board, "send");
        board.key(Key::Esc);
        board.key(Key::Char('x'));
        check(&board, "close");
        board.key(Key::Esc);
        board.key(Key::Char('q'));
        check(&board, "quit");
        board.key(Key::Esc);
        board.key(Key::Enter);
        check(&board, "detail");
        board.offline = Some(t0);
        check(&board, "offline");
        assert!(!japanese(&board.title()), "{}", board.title());
        // A notice from an action, and from a key with nothing to do.
        board.key(Key::Char('9'));
        assert!(!japanese(&board.message.clone().unwrap().text));
        let mut server = FakeServer {
            listings: vec![sample()],
            typed: Vec::new(),
        };
        let notice = perform(&mut server, &board, Action::Focus(2));
        assert!(!japanese(&notice.text), "{}", notice.text);
    }

    // Review finding: run from a tool shell the board wrote escape
    // sequences into a pipe and waited for keys; with stdout elsewhere it
    // took confirmations no one saw.
    #[test]
    fn the_board_needs_a_terminal_on_stdin_and_stdout() {
        assert!(terminal_problem(true, true).is_none());
        for (stdin, stdout) in [(false, true), (true, false), (false, false)] {
            let failure = terminal_problem(stdin, stdout).expect("refused");
            assert_eq!(failure.code, super::super::EXIT_ERROR);
            assert!(failure.message.contains("board --open"), "{failure:?}");
        }
    }

    // Review finding: a reconnect adopted whatever Noa answered, whose
    // panes can share ids with this one's workers.
    #[test]
    fn a_reconnect_must_reach_the_same_noa() {
        assert!(same_instance("a1", "a1"));
        assert!(!same_instance("a1", "b2"));
        assert!(!same_instance("a1", ""));
        // A server from before the id cannot be told apart.
        assert!(same_instance("", "b2"));
    }

    // Review finding: a worker closing under the send line or the close
    // prompt dropped back to normal keys, so the `x` and `y` typed next
    // closed the worker selected in its place.
    #[test]
    fn keys_meant_for_a_worker_that_went_are_dropped_until_esc() {
        let t0 = Instant::now();
        for open in ['s', 'x'] {
            let mut board = Board::new(Some(1), Some(8));
            board.update(sample(), t0);
            board.key(Key::Char('j'));
            assert_eq!(board.selected(), Some(2));
            board.key(Key::Char(open));
            let without: Vec<Panel> = sample()
                .into_iter()
                .filter(|panel| panel.pane_id.0 != 2)
                .collect();
            board.update(without, t0);
            assert_eq!(board.mode, Mode::TargetGone("api".to_string()));
            let top = visible(&board.render(100, 20, t0)[2]);
            assert!(top.contains("api は閉じられました"), "{top}");
            for key in [Key::Char('x'), Key::Char('y'), Key::Enter, Key::Char('q')] {
                assert_eq!(board.key(key), None);
            }
            assert!(matches!(board.mode, Mode::TargetGone(_)));
            board.key(Key::Esc);
            assert_eq!(board.mode, Mode::Normal);
            board.key(Key::Char('x'));
            assert_eq!(board.key(Key::Char('y')), Some(Action::Close(3)));
        }
    }

    // Review finding: with the listing failing, or before the first one,
    // the empty list was trusted and `q` quit at once.
    #[test]
    fn q_asks_first_unless_a_listing_that_worked_showed_no_worker() {
        let t0 = Instant::now();
        let mut unread = Board::new(Some(1), Some(8));
        assert_eq!(unread.key(Key::Char('q')), None);
        assert_eq!(unread.mode, Mode::ConfirmQuit);

        let mut offline = Board::new(Some(1), Some(8));
        offline.update(Vec::new(), t0);
        offline.offline = Some(t0);
        assert_eq!(offline.key(Key::Char('q')), None);
        assert_eq!(offline.mode, Mode::ConfirmQuit);
        assert_eq!(offline.key(Key::Char('q')), Some(Action::Quit));

        let mut empty = Board::new(Some(1), Some(8));
        empty.update(Vec::new(), t0);
        assert_eq!(empty.key(Key::Char('q')), Some(Action::Quit));
    }

    // Review finding: with the server stopped, each screen read waited out
    // its timeout, one worker after another.
    #[test]
    fn a_lost_connection_ends_the_screen_pass() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), t0);
        assert!(board.wanted_screens().len() > 1);
        let mut reads = 0;
        let lost = fetch_screens(&mut board, t0, |_| {
            reads += 1;
            Err(ClientError::Transport("deadline exceeded".to_string()))
        });
        assert!(lost);
        assert_eq!(reads, 1);

        // A read Noa did not answer in time ends the pass too, on the
        // connection it has.
        let mut reads = 0;
        let lost = fetch_screens(&mut board, t0, |_| {
            reads += 1;
            Err(ClientError::Timeout("noa.getText".to_string()))
        });
        assert!(!lost);
        assert_eq!(reads, 1);

        // A pane that closed is skipped; the pass goes on.
        let mut reads = 0;
        let lost = fetch_screens(&mut board, t0, |_| {
            reads += 1;
            Err(ClientError::Rpc {
                code: RPC_PANE_CLOSED,
                message: "closed".to_string(),
            })
        });
        assert!(!lost);
        assert_eq!(reads, board.wanted_screens().len());
    }

    // Review finding: once the orchestrator's pane closed, the board went
    // on saying its workers had not started yet.
    #[test]
    fn a_closed_orchestrator_is_said_and_q_quits() {
        let t0 = Instant::now();
        let mut orchestrator = worker(1, 1, "orchestrator", None, 0);
        orchestrator.parent_pane_id = None;
        orchestrator.agent = None;
        for lang in [Lang::Ja, Lang::En] {
            let mut board = Board::new(Some(1), Some(8));
            board.lang = lang;
            board.update(vec![orchestrator.clone()], t0);
            let all = rendered(&board, 100, 20, t0);
            assert!(
                all.contains(lang.pick("ワーカーはまだいません", "No workers yet")),
                "{all}"
            );
            board.update(Vec::new(), t0);
            let all = rendered(&board, 100, 20, t0);
            assert!(
                all.contains(lang.pick(
                    "オーケストレーターのペインは閉じられました",
                    "The orchestrator pane closed"
                )) && !all.contains(lang.pick("ワーカーはまだいません", "No workers yet")),
                "{all}"
            );
            assert_eq!(board.key(Key::Char('q')), Some(Action::Quit));
        }
        // Never seen (a wrong `--of`, or not listed yet): still waiting.
        let mut board = Board::new(Some(1), Some(8));
        board.update(Vec::new(), t0);
        assert!(rendered(&board, 100, 20, t0).contains("ワーカーはまだいません"));
    }

    // Review finding: `1`, `j`, `2` within a second picked worker 12, and
    // the second was timed by when the loop got to the key.
    #[test]
    fn only_a_digit_soon_after_a_digit_makes_two() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), None);
        let panels: Vec<Panel> = (0..12)
            .map(|i| worker(10 + i, 1, &format!("w{}", i + 1), Some("running"), 1))
            .collect();
        board.update(panels, t0);
        board.key_at(Key::Char('1'), t0);
        board.key_at(Key::Char('j'), t0);
        board.key_at(Key::Char('2'), t0);
        assert_eq!(board.selected(), Some(11), "2 alone");

        // Arrival times decide, however late they are handled.
        board.key_at(Key::Char('1'), t0);
        board.key_at(Key::Char('2'), t0 + SECOND_DIGIT + Duration::from_millis(1));
        assert_eq!(board.selected(), Some(11), "too late for 12");
        board.key_at(Key::Char('1'), t0);
        board.key_at(Key::Char('2'), t0 + Duration::from_millis(900));
        assert_eq!(board.selected(), Some(21), "12");
    }

    // Review finding: an agent known only by its process name (no state,
    // report count 0) got a bogus history row.
    #[test]
    fn an_agent_that_never_reported_has_no_history() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), None);
        let mut panel = worker(2, 1, "api", None, 0);
        panel.agent.as_mut().unwrap().detail = None;
        board.update(vec![panel.clone()], t0);
        board.update(vec![panel], t0 + Duration::from_secs(1));
        assert!(!board.history.contains_key(&2), "{:?}", board.history);
        board.key(Key::Enter);
        assert!(rendered(&board, 100, 30, t0).contains("報告はまだありません"));

        // Its first real report is kept.
        board.update(
            vec![worker(2, 1, "api", Some("running"), 1)],
            t0 + Duration::from_secs(2),
        );
        assert_eq!(board.history[&2].len(), 1);
    }
}

#[cfg(test)]
mod perf {
    use super::*;
    use noa_ipc::{PanelAgent, WireId};

    /// Twenty workers under orchestrator 1, as `+ctl board` lists them.
    fn workers(count: u64, seq: u64) -> Vec<Panel> {
        let states = ["running", "permission", "finished", "running", "idle"];
        (2..2 + count)
            .map(|pane| {
                let mut panel: Panel = serde_json::from_value(serde_json::json!({
                    "windowGroupId": "1", "windowId": "1", "paneId": pane.to_string(),
                    "name": format!("claude · w{pane}"), "cwd": "/repo",
                    "busy": true, "attention": false, "preview": [],
                }))
                .unwrap();
                panel.parent_pane_id = Some(WireId(1));
                panel.label = Some(format!("worker-{pane}"));
                panel.agent = Some(PanelAgent {
                    kind: Some("claude".to_string()),
                    state: Some(states[pane as usize % states.len()].to_string()),
                    detail: Some(format!("step {seq} of the work on worker {pane}")),
                    seq,
                    blocked: None,
                });
                panel
            })
            .collect()
    }

    fn screen(pane: u64) -> String {
        (0..40)
            .map(|row| format!("{row:>3} worker {pane} output line with some text in it\n"))
            .collect()
    }

    /// What one idle board tick costs with twenty workers: a listing, the
    /// screens it reads, and a full frame. Run with
    /// `cargo test --release -p noa-app board::perf -- --ignored --nocapture`.
    #[test]
    #[ignore = "release-mode measurement of the board's idle tick"]
    fn idle_tick_cost_with_twenty_workers() {
        let iters = 2000u32;
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(100));
        board.lang = Lang::En;
        board.update(workers(20, 1), t0);
        let screens: Vec<(u64, String)> = (2..22).map(|pane| (pane, screen(pane))).collect();
        let mut update = Duration::ZERO;
        let mut set = Duration::ZERO;
        let mut render = Duration::ZERO;
        let mut bytes = 0usize;
        let mut same = 0u32;
        let mut last: Vec<String> = Vec::new();
        for i in 0..iters {
            let now = t0 + Duration::from_millis(500) * (i + 1);
            let panels = workers(20, 1);
            let at = Instant::now();
            board.update(panels, now);
            update += at.elapsed();
            let at = Instant::now();
            for pane in board.wanted_screens() {
                let text = &screens.iter().find(|(p, _)| *p == pane).unwrap().1;
                board.set_screen_at(pane, text, now);
            }
            set += at.elapsed();
            let at = Instant::now();
            let lines = board.render(120, 40, now);
            render += at.elapsed();
            bytes += lines.join("\r\n").len() + 3;
            same += u32::from(lines == last);
            last = lines;
        }
        let per = |d: Duration| d.as_secs_f64() * 1e6 / f64::from(iters);
        println!(
            "board tick: update {:.1}µs  screens {:.1}µs ({} read)  render {:.1}µs  frame {} B  identical frames {same}/{iters}",
            per(update),
            per(set),
            board.wanted_screens().len(),
            per(render),
            bytes / iters as usize,
        );
    }
}
