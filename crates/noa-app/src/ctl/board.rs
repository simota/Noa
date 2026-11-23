//! `Noa +ctl board`: a terminal UI that shows an orchestrator's workers and
//! lets a person act on them (agent-orchestration UI phase). It runs in an
//! ordinary pane and reads the same server `+ctl` does, so it needs no new
//! pane kind: the model, key handling and drawing below are pure, and only
//! [`run`] touches the terminal and the connection.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use noa_ipc::{Client, ClientError, Panel, PanelSpawn, TextSource};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::result::{self, StartError};
use super::{EXIT_OK, Failure, HOLD_BANNER, SUBMIT_DELAY, journal};

mod insight;

use insight::{FileState, Insight, ResultFile, Task};

/// How often the pane list is read again.
const LIST_EVERY: Duration = Duration::from_millis(500);
/// After a listing fails, the first wait before trying again and the most
/// it grows to. Failing never ends the board (see [`Polling`]).
const RETRY_FIRST: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(4);
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
/// The worker numbers a board gives out before it takes freed ones again
/// (U14): the digit keys reach 1-20 (D12).
const NUMBERS: usize = 20;
/// Where a row's name starts, which its preview lines line up with.
const ROW_INDENT: usize = 6;
/// How long a lone ESC waits for the rest of a sequence before it counts
/// as the Esc key.
const ESC_WAIT: Duration = Duration::from_millis(50);
/// The `float.toggle-focus` binding, which folds an opened worker back
/// into its row and takes focus to the board (agent-orchestration D10).
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
    /// A question dialog (`OSC 7501` blocked, `Panel.agent.blocked:
    /// "question"`): Enter would answer it with its first choice.
    Question,
    /// Waiting for input, reported without a dialog (a hook, `OSC 777`).
    Input,
    Error,
    /// An unread notification, with no blocking report.
    Attention,
    Running,
    Finished,
    Exited(Option<i32>),
    /// An agent, alive, that never reported a state.
    Quiet,
    /// A command that is no agent (`spawn -- cargo test`), still running:
    /// it has nothing to report (U8).
    Command,
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
            Some("input")
                if panel
                    .agent
                    .as_ref()
                    .is_some_and(|agent| agent.blocked.as_deref() == Some("question")) =>
            {
                Self::Question
            }
            Some("input") => Self::Input,
            Some("error") => Self::Error,
            // A finished turn usually raises a notification too; it is done,
            // not waiting on anyone.
            Some("finished") => Self::Finished,
            _ if panel.attention => Self::Attention,
            Some("running") => Self::Running,
            _ if panel.agent.is_none() => Self::Command,
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
            Self::Permission | Self::Question | Self::Input | Self::Error | Self::Attention => {
                Group::Needs
            }
            status if status.failed() => Group::Needs,
            Self::Running | Self::Quiet | Self::Command => Group::Working,
            Self::Finished => Group::Done,
            Self::Exited(_) => Group::Exited,
        }
    }

    fn label(self, lang: Lang) -> String {
        match self {
            Self::Permission => lang.pick("承認待ち", "approval").to_string(),
            Self::Question => lang.pick("質問", "question").to_string(),
            Self::Input => lang.pick("入力待ち", "input").to_string(),
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
            Self::Command => lang.pick("実行中", "running").to_string(),
        }
    }

    /// What closing a worker in this state cuts short, as the close prompt
    /// says it before its question; nothing once it is over (U10).
    fn still(self, lang: Lang) -> Option<&'static str> {
        Some(match self {
            Self::Permission => lang.pick("まだ承認待ちです。", "It still awaits approval. "),
            Self::Question => lang.pick(
                "まだ質問の答えを待っています。",
                "It still awaits an answer to its question. ",
            ),
            Self::Input => lang.pick("まだ答えを待っています。", "It still awaits an answer. "),
            Self::Error => lang.pick("エラーを報告しています。", "It reported an error. "),
            Self::Attention => {
                lang.pick("未読の通知があります。", "It has an unread notification. ")
            }
            Self::Running => lang.pick("まだ作業中です。", "It is still at work. "),
            Self::Quiet => lang.pick("まだ動いています。", "It is still at work. "),
            Self::Command => lang.pick("まだ実行中です。", "It is still running. "),
            Self::Finished | Self::Exited(_) => return None,
        })
    }

    /// How a worker closed in this state is listed among the closed (U10).
    fn closed_as(self, lang: Lang) -> String {
        match self {
            Self::Permission => lang.pick("承認待ちのまま閉じた", "closed awaiting approval"),
            Self::Question => lang.pick(
                "質問に答えないまま閉じた",
                "closed with its question unanswered",
            ),
            Self::Input => lang.pick("答えを待ったまま閉じた", "closed awaiting an answer"),
            Self::Error => lang.pick("エラーのまま閉じた", "closed after an error"),
            Self::Attention => lang.pick("通知を残して閉じた", "closed with a notification unread"),
            Self::Running => lang.pick("作業中に閉じた", "closed at work"),
            Self::Quiet => lang.pick("報告がないまま閉じた", "closed before it reported"),
            Self::Command => lang.pick("実行中に閉じた", "closed while running"),
            Self::Finished => lang.pick("完了後に閉じた", "closed when done"),
            Self::Exited(Some(code)) => {
                return lang.pick(
                    format!("終了コード {code} で閉じた"),
                    format!("closed after exiting with {code}"),
                );
            }
            Self::Exited(None) => lang.pick("終了後に閉じた", "closed after it exited"),
        }
        .to_string()
    }

    /// The label for a narrow pane: the states that share a colour keep a
    /// word each, so they stay apart without the full label.
    fn short_label(self, lang: Lang) -> String {
        match self {
            Self::Permission => lang.pick("承認", "appr").to_string(),
            Self::Question => lang.pick("質問", "ask").to_string(),
            Self::Input => lang.pick("入力", "input").to_string(),
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
            Self::Question | Self::Input => "?",
            Self::Error => "×",
            status if status.failed() => "×",
            Self::Running | Self::Command => "●",
            Self::Finished => "✓",
            Self::Exited(_) | Self::Quiet => "·",
        }
    }

    fn color(self) -> Rgb {
        match self {
            Self::Permission | Self::Question | Self::Input | Self::Attention => ORANGE,
            Self::Error => RED,
            status if status.failed() => RED,
            Self::Running | Self::Command => BLUE,
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
    /// Waiting for [`Board::quit_key`] to end the board, which hands every
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
    number: Option<usize>,
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
    /// sorted, and not given to another while the board is open (U14).
    numbers: HashMap<u64, usize>,
    /// The highest number given yet, and the numbers of closed workers,
    /// oldest first: one is given again only once all of 1-[`NUMBERS`]
    /// were.
    issued: usize,
    freed: Vec<usize>,
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
    /// Whether the last reconnect reached another Noa than the one the
    /// board started on, whose panes are not these workers.
    pub(crate) elsewhere: bool,
    /// Workers closed since the board opened, newest first.
    gone: Vec<Gone>,
    /// What was typed on a send line that did not go: cancelled, or refused
    /// (the worker began waiting for an approval meanwhile, D7). `s` to the
    /// same worker starts from it (U5).
    pub(crate) drafts: HashMap<u64, String>,
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
    /// The panes that needed someone at the last listing, so only one that
    /// newly does rings the bell (U2).
    needs_seen: HashSet<u64>,
    /// The language of every label (see [`Lang::detect`]).
    pub(crate) lang: Lang,
    /// The Unix time at the last listing, and when it was read: what
    /// `Panel.spawn.startedAt` is told against (U9).
    wall: Option<(u64, Instant)>,
    /// Each worker's task and result file (D18), from files read at most
    /// every [`insight::FILES_EVERY`].
    insight: Insight,
    /// The detail view shows the result file's text in place of the
    /// screen (`r`), scrolled this many lines, as far as it can go (set as
    /// it is drawn).
    result_shown: bool,
    result_scroll: usize,
    result_reach: Cell<usize>,
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
            issued: 0,
            freed: Vec::new(),
            digit: None,
            selected: None,
            view: View::List,
            mode: Mode::Normal,
            message: None,
            offline: None,
            elsewhere: false,
            gone: Vec::new(),
            drafts: HashMap::new(),
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
            needs_seen: HashSet::new(),
            lang: Lang::Ja,
            wall: None,
            insight: Insight::default(),
            result_shown: false,
            result_scroll: 0,
            result_reach: Cell::new(0),
        }
    }

    /// Look at the workers' journals and result files when it is time, and
    /// read the result shown in the detail view when it changed (D18).
    /// Returns whether the frame may have changed.
    pub(crate) fn refresh_files(&mut self, now: Instant) -> bool {
        let panes: Vec<u64> = self.listed().map(|panel| panel.pane_id.0).collect();
        let show = match self.view {
            View::Detail(pane) if self.result_shown => Some(pane),
            _ => None,
        };
        self.insight.refresh(&self.panels, &panes, show, now)
    }

    /// Whether the detail view shows a result file in place of the screen.
    fn showing_result(&self) -> bool {
        self.result_shown && matches!(self.view, View::Detail(_))
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
    /// Returns whether a worker newly needs someone since the last listing
    /// (never on the first): the board rings the bell then, which Noa marks
    /// on the board's tab and sidebar row while the board is not in focus
    /// (U2).
    pub(crate) fn update(&mut self, panels: Vec<Panel>, now: Instant) -> bool {
        let first = !self.started;
        self.started = true;
        self.synced = Some(now);
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        self.wall = Some((unix, now));
        let before: Vec<(u64, Gone)> = self
            .workers()
            .iter()
            .map(|panel| {
                let gone = Gone {
                    number: self.numbers.get(&panel.pane_id.0).copied(),
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
        // The orchestrator's state is dated like a worker's: the header
        // says it, and the queue while it needs someone (U3).
        let orchestrator = self
            .of
            .and_then(|of| self.panel(of))
            .map(|panel| (panel.pane_id.0, Status::of(panel)));
        for (pane, status) in workers
            .iter()
            .map(|(pane, _, status, _)| (*pane, *status))
            .chain(orchestrator)
        {
            match self.since.get(&pane) {
                Some((seen, _)) if *seen == status => {}
                _ => {
                    let at = (!first).then_some(now);
                    self.since.insert(pane, (status, at));
                }
            }
        }
        for (pane, seq, status, detail) in workers.iter().cloned() {
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
        let mut freed: Vec<usize> = self
            .numbers
            .iter()
            .filter(|(pane, _)| !live.contains(pane))
            .map(|(_, number)| *number)
            .collect();
        freed.sort_unstable();
        self.freed.extend(freed);
        self.numbers.retain(|pane, _| live.contains(pane));
        // New workers in spawn order (pane ids grow), each the next number
        // never given, or once 1-20 all were, the one freed longest ago.
        for &pane in &live {
            if !self.numbers.contains_key(&pane) {
                let number = if self.issued < NUMBERS || self.freed.is_empty() {
                    self.issued += 1;
                    self.issued
                } else {
                    self.freed.remove(0)
                };
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
        let mut live = live;
        live.extend(orchestrator.map(|(pane, _)| pane));
        self.history.retain(|pane, _| live.contains(pane));
        self.since.retain(|pane, _| live.contains(pane));
        self.screens.retain(|pane, _| live.contains(pane));
        self.drafts.retain(|pane, _| live.contains(pane));
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
        // The orchestrator is selectable only while it is in the queue.
        let lead = self.lead().map(|panel| panel.pane_id.0);
        if self.selected.is_none_or(|pane| {
            !live.contains(&pane) || (Some(pane) == self.of && Some(pane) != lead)
        }) {
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
        let needing = self.needing();
        let ring = !first && !needing.is_subset(&self.needs_seen);
        self.needs_seen = needing;
        ring
    }

    /// The panes in the queue: the workers that need someone, and the
    /// orchestrator when it does.
    fn needing(&self) -> HashSet<u64> {
        self.listed()
            .filter(|panel| Status::of(panel).group() == Group::Needs)
            .chain(self.lead())
            .map(|panel| panel.pane_id.0)
            .collect()
    }

    /// The orchestrator while it needs someone (U3): an approval for its own
    /// `+ctl` call holds every worker up, so it heads the queue.
    fn lead(&self) -> Option<&Panel> {
        self.panel(self.of?)
            .filter(|panel| Status::of(panel).group() == Group::Needs)
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
        let rank = u8::from(matches!(
            status,
            Status::Question | Status::Input | Status::Attention
        ));
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
        let lead = self.lead().map(|panel| Node {
            panel,
            branch: Branch::Top { kids: 0 },
            zone: Zone::Queue,
        });
        let mut entries: Vec<Entry> = lead
            .into_iter()
            .chain(queue)
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
    /// last showed output), the orchestrator while it needs someone (it
    /// heads the queue), and the selected one, which is previewed.
    pub(crate) fn wanted_screens(&self) -> Vec<u64> {
        match self.view {
            View::List => self
                .lead()
                .into_iter()
                .chain(self.workers())
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

    /// [`spawn_facts`] of `panel` at `now`; none for a pane no IPC spawn
    /// started with a command, or from a server that predates `spawn`.
    fn spawn_facts(&self, panel: &Panel, now: Instant) -> Vec<String> {
        let Some(spawn) = &panel.spawn else {
            return Vec::new();
        };
        let unix_now = self
            .wall
            .map(|(unix, at)| unix + now.saturating_duration_since(at).as_secs());
        spawn_facts(spawn, unix_now, self.lang)
    }

    fn name(&self, pane: u64) -> String {
        if Some(pane) == self.of {
            return self
                .lang
                .pick("オーケストレーター", "orchestrator")
                .to_string();
        }
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
        if let View::Detail(open) = self.view
            && open != pane
        {
            // Another pane opens on its screen, as with Enter (D18).
            self.view = View::Detail(pane);
            self.result_shown = false;
            self.result_scroll = 0;
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

    /// Whether `pane` waits for an approval or on a question dialog, which
    /// is answered in its own pane, never from the board (spec D7): the
    /// Enter that submits a send line would pick the first choice.
    fn awaits_answer(&self, pane: u64) -> bool {
        self.panel(pane)
            .is_some_and(|panel| answered_in_pane(Status::of(panel)))
    }

    /// Whether `pane`'s process has ended: nothing typed reaches it.
    fn has_exited(&self, pane: u64) -> bool {
        self.panel(pane)
            .is_some_and(|panel| matches!(Status::of(panel), Status::Exited(_)))
    }

    /// The key that ends the board from its prompt (U7): `y` when ending it
    /// closes workers for sure — the workers of workers (D13), or more
    /// workers than the floats an orchestrator's pane holds (REQ-019; some
    /// may be split panes, which stay, so this can ask for `y` where `q`
    /// would do) — else `q` again. `^C`, pressed twice by reflex, never ends
    /// it.
    fn quit_key(&self) -> char {
        let tree = self.tree();
        let kids = tree
            .iter()
            .filter(|node| matches!(node.branch, Branch::Kid { .. }))
            .count();
        if kids > 0 || tree.len() - kids > crate::floating::MAX_PER_ANCHOR {
            'y'
        } else {
            'q'
        }
    }

    /// Keep `text` as `pane`'s draft, or drop its draft when nothing is
    /// left of it (`^U`, then Esc). Returns whether one is kept.
    fn keep_draft(&mut self, pane: u64, text: String) -> bool {
        if text.trim().is_empty() {
            self.drafts.remove(&pane);
            return false;
        }
        self.drafts.insert(pane, text);
        true
    }

    /// Why nothing can be sent to `pane`, if so.
    fn cannot_send(&self, pane: u64) -> Option<String> {
        let name = self.name(pane);
        if let Some(why) = self
            .panel(pane)
            .and_then(|panel| answer_message(Status::of(panel), &name, self.lang))
        {
            Some(why)
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
                            let why = if self.keep_draft(pane, text) {
                                format!("{why} · {}", draft_kept(self.lang))
                            } else {
                                why
                            };
                            self.message = Some(Notice::warn(why));
                        } else if !text.trim().is_empty() {
                            return Some(Action::Send(pane, text));
                        }
                    }
                    Key::Esc | Key::CtrlC => {
                        self.mode = Mode::Normal;
                        if self.keep_draft(pane, text) {
                            self.message = Some(Notice::ok(draft_kept(self.lang).to_string()));
                        }
                    }
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
                (key == Key::Char(self.quit_key())).then_some(Action::Quit)
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
                    // A result shown in the detail view scrolls (D18).
                    Key::Char('j') | Key::Down if self.showing_result() => {
                        self.result_scroll = (self.result_scroll + 1).min(self.result_reach.get());
                    }
                    Key::Char('k') | Key::Up if self.showing_result() => {
                        // From the end the last frame showed: a result
                        // read again may be shorter than the kept scroll.
                        self.result_scroll = self
                            .result_scroll
                            .min(self.result_reach.get())
                            .saturating_sub(1);
                    }
                    Key::Char('j') | Key::Down => self.move_selection(1),
                    Key::Char('k') | Key::Up => self.move_selection(-1),
                    // Only for a worker whose files were looked at (not the
                    // orchestrator heading the queue).
                    Key::Char('r')
                        if matches!(self.view, View::Detail(pane)
                            if self.insight.result(pane).is_some()) =>
                    {
                        self.result_shown = !self.result_shown;
                        self.result_scroll = 0;
                    }
                    Key::Enter | Key::Char('l') | Key::Right => {
                        if let Some(pane) = self.selected {
                            self.view = View::Detail(pane);
                            self.result_shown = false;
                            self.result_scroll = 0;
                        }
                    }
                    Key::Esc | Key::Char('h') | Key::Left => {
                        self.view = View::List;
                        self.result_shown = false;
                        self.result_scroll = 0;
                    }
                    Key::Char('g') => return self.target().map(Action::Focus),
                    Key::Char('s') => {
                        if let Some(pane) = self.target() {
                            match self.cannot_send(pane) {
                                Some(why) => self.message = Some(Notice::warn(why)),
                                None => {
                                    let draft = self.drafts.remove(&pane).unwrap_or_default();
                                    self.mode = Mode::Send(pane, draft);
                                }
                            }
                        }
                    }
                    Key::Char('x') => {
                        match self.target() {
                            // Closing the orchestrator closes every worker.
                            Some(pane) if Some(pane) == self.of => {
                                let text = self.lang.pick(
                                    "オーケストレーターはボードからは閉じません",
                                    "the board does not close the orchestrator",
                                );
                                self.message = Some(Notice::warn(text.to_string()));
                            }
                            Some(pane) => self.mode = Mode::ConfirmClose(pane),
                            None => {}
                        }
                    }
                    _ => {}
                }
                None
            }
        }
    }
}

/// Said when a send line's text is kept as a draft.
fn draft_kept(lang: Lang) -> &'static str {
    lang.pick("下書きを残しました (s で再開)", "draft kept (s resumes it)")
}

// Labels and process names come from other processes: never let their
// control characters reach this terminal.
fn answer_message(status: Status, name: &str, lang: Lang) -> Option<String> {
    Some(match (status, lang) {
        (Status::Permission, Lang::Ja) => {
            format!("{name} は承認待ちです。g でペインへ移動して答えてください")
        }
        (Status::Permission, Lang::En) => {
            format!("{name} awaits approval. g goes to its pane to answer it")
        }
        (Status::Question, Lang::Ja) => format!(
            "{name} は質問の答えを待っています。Enter は最初の選択肢を選ぶので、g でペインへ移動して答えてください"
        ),
        (Status::Question, Lang::En) => format!(
            "{name} asks a question, and Enter would pick its first choice. g goes to its pane to answer it"
        ),
        _ => return None,
    })
}

/// Whether a worker in `status` is answered only in its own pane (D7).
fn answered_in_pane(status: Status) -> bool {
    matches!(status, Status::Permission | Status::Question)
}

/// A full-width rule, as agents draw around their input box.
pub(crate) fn is_rule(line: &str) -> bool {
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

/// `panel`'s git branch, if it has one.
fn branch_of(panel: &Panel) -> Option<String> {
    panel
        .branch
        .as_deref()
        .map(clean)
        .filter(|branch| !branch.is_empty())
}

/// Whether `cwd` is in the worktree `+ctl spawn --worktree` makes for
/// `label`, `<top>-wt/<label>` (see `worktree::worktree_path`).
fn in_label_worktree(cwd: &str, label: &str) -> bool {
    if super::worktree::check_label(label).is_err() {
        return false;
    }
    let parts: Vec<_> = std::path::Path::new(cwd)
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    parts
        .windows(2)
        .any(|pair| pair[0].len() > "-wt".len() && pair[0].ends_with("-wt") && pair[1] == label)
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

/// The model and the effort a worker's argv picks (U9): `--model X` or
/// `-m X` (`--model=X`), and `--effort X` (`--effort=X`) or codex's
/// `-c model_reasoning_effort=X`; the last one given wins, as with the
/// agents' own parsing.
fn model_and_effort(argv: &[String]) -> (Option<String>, Option<String>) {
    let (mut model, mut effort) = (None, None);
    let mut args = argv.iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--model" || arg == "-m" {
            model = args.next().cloned();
        } else if let Some(value) = arg.strip_prefix("--model=") {
            model = Some(value.to_string());
        } else if arg == "--effort" {
            effort = args.next().cloned();
        } else if let Some(value) = arg.strip_prefix("--effort=") {
            effort = Some(value.to_string());
        } else if let Some(value) = arg
            .strip_prefix("--config=")
            .unwrap_or(arg)
            .strip_prefix("model_reasoning_effort=")
        {
            // A TOML value: `"high"` is `high`.
            effort = Some(value.trim_matches('"').to_string());
        }
    }
    let shown = |value: Option<String>| value.map(|value| clean(&value)).filter(|v| !v.is_empty());
    (shown(model), shown(effort))
}

/// What `spawn` says of a worker, as the preview and the detail show it:
/// "opus", "推論量 high", "12 分前に起動", each only when it is known.
fn spawn_facts(spawn: &PanelSpawn, unix_now: Option<u64>, lang: Lang) -> Vec<String> {
    let (model, effort) = model_and_effort(&spawn.argv);
    let effort =
        effort.map(|effort| lang.pick(format!("推論量 {effort}"), format!("effort {effort}")));
    let started = unix_now.map(|unix_now| {
        let span = duration_text(
            Duration::from_secs(unix_now.saturating_sub(spawn.started_at)),
            lang,
        );
        lang.pick(format!("{span}前に起動"), format!("started {span} ago"))
    });
    model.into_iter().chain(effort).chain(started).collect()
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

/// `s` over lines of at most `width` columns, each broken after its last
/// space when that leaves it at least half full, else where it is full.
fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for ch in s.chars() {
        if line.width() + ch.width().unwrap_or(0) > width && !line.is_empty() {
            let rest = match line.rfind(' ') {
                Some(at) if line[..at].width() * 2 >= width => {
                    let rest = line[at + 1..].to_string();
                    line.truncate(at);
                    rest
                }
                _ => String::new(),
            };
            lines.push(std::mem::replace(&mut line, rest));
            if ch == ' ' && line.is_empty() {
                continue;
            }
        }
        line.push(ch);
    }
    lines.push(line);
    lines
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
        name: impl Fn(&Panel) -> String,
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
            .map(|&(panel, indent)| indent + name(panel).width())
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

/// `body` (lines, each dimmed or plain) in a box `cols` wide with
/// `height` rows inside, its borders in `border`; what does not fit is cut.
fn boxed(body: &[(Option<Rgb>, String)], cols: usize, height: usize, border: Rgb) -> Vec<String> {
    let inner = cols - 4;
    let mut out = Vec::with_capacity(height + 2);
    let mut top = Line::default();
    top.fg(border, &format!(" ┌{}┐", "─".repeat(cols - 3)));
    out.push(top.finish(cols));
    for index in 0..height {
        let (color, text) = body
            .get(index)
            .map_or((None, ""), |(color, text)| (*color, text.as_str()));
        let text = fit(text, inner.saturating_sub(1));
        let mut line = Line::default();
        line.fg(border, " │ ");
        match color {
            Some(color) => line.fg(color, &text),
            None => line.plain(&text),
        };
        line.plain(&" ".repeat(inner.saturating_sub(1 + text.width())));
        line.fg(border, " │");
        out.push(line.finish(cols));
    }
    let mut bottom = Line::default();
    bottom.fg(border, &format!(" └{}┘", "─".repeat(cols - 3)));
    out.push(bottom.finish(cols));
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
    span_ago(now.saturating_duration_since(at), lang)
}

/// "10 秒前" ("10s ago") for a span already measured.
fn span_ago(span: Duration, lang: Lang) -> String {
    let span = duration_text(span, lang);
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
        let needs = self.needing().len();
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
                    .and_then(|panel| Status::of(panel).still(self.lang))
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
                // The board only closes the pane: the worktree `spawn
                // --worktree` made for it stays, and its branch (U6).
                let worktree = self
                    .panel(*pane)
                    .and_then(|panel| {
                        let label = panel.label.as_deref()?;
                        in_label_worktree(&panel.cwd, label).then(|| clean(label))
                    })
                    .map(|label| {
                        self.lang.pick(
                            format!("worktree とブランチ orch/{label} は残ります。"),
                            format!("Its worktree and branch orch/{label} stay. "),
                        )
                    })
                    .unwrap_or_default();
                let name = self.name(*pane);
                // The keys are in the prompt: an opened worker covers the
                // footer that lists them (D10).
                let ask = self.lang.pick(
                    format!(
                        "{name} を閉じますか? y で閉じる · ほかのキーで取り消し · {busy}{kids}{worktree}出力は消えます。"
                    ),
                    format!(
                        "Close {name}? y closes · other keys cancel · {busy}{kids}{worktree}Its output is lost."
                    ),
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
                // (D13). The key comes first, as an opened worker covers the
                // footer (D10); the whole prompt fits 80 columns.
                let ask = match (self.lang, self.quit_key(), kids > 0) {
                    (Lang::En, 'y', true) => format!(
                        "y quits · {kids} workers' workers and those the orchestrator can't fit close"
                    ),
                    (Lang::En, key, _) => {
                        let quits = if key == 'y' {
                            "y quits"
                        } else {
                            "q again quits"
                        };
                        format!(
                            "{quits} · workers the orchestrator can't fit close; others go back"
                        )
                    }
                    (Lang::Ja, 'y', true) => format!(
                        "y で終了 · ワーカーの子 {kids} 件と、オーケストレーターに収まらない分は閉じます"
                    ),
                    (Lang::Ja, key, _) => {
                        let quits = if key == 'y' {
                            "y で終了"
                        } else {
                            "再度 q で終了"
                        };
                        format!(
                            "{quits} · オーケストレーターに収まらないワーカーは閉じ、残りは戻ります"
                        )
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
                // The listing failed: what is shown is the last one that
                // worked. The board waits for Noa as long as it takes; only
                // the person ends it (U1).
                (Some(since), _) => {
                    let secs = now.saturating_duration_since(*since).as_secs();
                    let seen = match self.synced {
                        Some(at) => {
                            let ago = ago(Some(at), now, self.lang);
                            self.lang
                                .pick(format!("表示は {ago}のもの"), format!("as of {ago}"))
                        }
                        None => self
                            .lang
                            .pick("一覧はまだ読めていません", "nothing read yet")
                            .to_string(),
                    };
                    let text = match (self.elsewhere, self.lang) {
                        (false, Lang::Ja) => {
                            format!("接続できません {secs} 秒 · {seen} · q で終了")
                        }
                        (false, Lang::En) => {
                            format!("Cannot reach Noa for {secs}s · {seen} · q quits")
                        }
                        (true, Lang::Ja) => format!(
                            "別の Noa が応答 · 元の Noa を待機中 {secs} 秒 · {seen} · q で終了"
                        ),
                        (true, Lang::En) => format!(
                            "Another Noa answered · waiting for ours {secs}s · {seen} · q quits"
                        ),
                    };
                    line.fg(ORANGE, &fit(&text, room));
                }
                (None, Some(notice)) => {
                    let color = if notice.warn { ORANGE } else { DIM };
                    line.fg(color, &fit(&clean(&notice.text), room));
                }
                (None, None) => {
                    let hint = self.lang.pick(
                        format!("{BACK_TO_BOARD} ワーカーをたたんでボードへ  ·  ? キー一覧"),
                        format!("{BACK_TO_BOARD} fold an opened worker, back here  ·  ? keys"),
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
        let keys: [(&str, &str); 16] = [
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
                l(
                    "詳細(タスク・結果ファイル・画面・報告の履歴)",
                    "details (task, result file, screen and reports)",
                ),
            ),
            (
                "r",
                l(
                    "詳細で画面と結果ファイルの中身を切り替える(中身は j/k でスクロール)",
                    "in details: switch between the screen and the result file (j/k scroll it)",
                ),
            ),
            ("Esc h ←", l("一覧に戻る", "back to the list")),
            (
                "g",
                l(
                    "ワーカーをこの下に開く(承認や質問はそこで答える)",
                    "open the worker below (answer approvals and questions there)",
                ),
            ),
            (
                BACK_TO_BOARD,
                l(
                    "開いたワーカーをたたんでボードへ戻る",
                    "fold the opened worker and come back here",
                ),
            ),
            (
                "s",
                l(
                    "ワーカーに送る(承認待ち・質問中・終了したワーカーには送れません)",
                    "send to the worker (not to one awaiting approval, asking a question or exited)",
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
        // A description too long for the pane goes on under itself (U11).
        for (key, what) in keys {
            if what.is_empty() {
                let mut line = Line::default();
                line.plain(" ").bold(key);
                lines.push(line.finish(cols));
                continue;
            }
            for (at, part) in wrap(what, cols.saturating_sub(16)).iter().enumerate() {
                let mut line = Line::default();
                let key = if at == 0 { key } else { "" };
                line.plain("   ").plain(&pad(key, 12)).fg(DIM, part);
                lines.push(line.finish(cols));
            }
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
        // What the orchestrator itself is doing, which can hold every worker
        // up (U3): when it is an agent, or needs someone.
        let state = self.of.and_then(|of| self.panel(of)).and_then(|panel| {
            let status = Status::of(panel);
            (panel.agent.is_some() || status.group() == Group::Needs).then(|| {
                let pane = panel.pane_id.0;
                let state = self.state_text(pane, status, StateForm::Full, now);
                let state = format!("  {} {state}", status.mark());
                (status.color(), state, clean(&self.last_word(panel)))
            })
        });
        let room = cols.saturating_sub(title.width + count.width() + 1);
        // The state before a long name; neither cut to nothing.
        let state = state.filter(|(_, text, _)| text.width() + 1 + 8 <= room);
        let scope_room = room - state.as_ref().map_or(0, |(_, text, _)| text.width() + 1);
        title.fg(DIM, &fit(&scope, scope_room));
        if let Some((color, text, detail)) = state {
            title.fg(color, &text);
            let room = cols.saturating_sub(title.width + count.width() + 3);
            if !detail.is_empty() && room >= 6 {
                title.fg(DIM, &format!("  {}", fit(&detail, room)));
            }
        }
        let pad = cols.saturating_sub(title.width + count.width());
        title.plain(&" ".repeat(pad)).fg(DIM, &count);

        let mut chips = Line::default();
        chips.plain(" ");
        for group in Group::ALL {
            let n = match group {
                Group::Needs => self.needing().len(),
                _ => workers
                    .iter()
                    .filter(|panel| Status::of(panel).group() == group)
                    .count(),
            };
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
        // The orchestrator needing someone is listed (and selectable) with
        // no worker too.
        if self.workers().is_empty() && self.lead().is_none() {
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
        // No worker can come any more once the orchestrator closed; before
        // the first listing none is known not to be there (U12).
        let note = if !self.started {
            self.lang.pick("読み込み中…", "Loading…")
        } else if self.of_gone {
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
        let columns = Columns::of(
            &named,
            tallies,
            cols,
            |panel| self.name(panel.pane_id.0),
            |panel, form| self.state_text(panel.pane_id.0, Status::of(panel), form, now),
        );
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
                let n = self.needing().len();
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
                            self.lang.pick("● 作業中", "● Working").to_string(),
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
        let mut name = self.name(pane);
        if let Some(parent) = self.tree_parent(pane) {
            name = format!("{}{name}", self.crumb(parent));
        }
        let number = self
            .numbers
            .get(&pane)
            .map_or(" ".to_string(), |number| format!(" {number} "));
        out.push(rule(
            cols,
            &[
                (Some(BLUE), self.lang.pick("選択中", "Selected").to_string()),
                (Some(DIM), number),
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
        // That it wrote its result file (D18), where it fits.
        if self.insight.result(pane).is_some_and(ResultFile::written) {
            let written = self.lang.pick("結果あり", "result written");
            if meta.width + 3 + written.width() < cols {
                meta.fg(DIM, " · ").fg(GREEN, written);
            }
        }
        // Its model, effort and start (U9), each whole where it fits.
        for fact in self.spawn_facts(panel, now) {
            if meta.width + 3 + fact.width() < cols {
                meta.fg(DIM, &format!(" · {fact}"));
            }
        }
        // Its branch, whole where it fits (U6).
        if let Some(branch) = branch_of(panel)
            && meta.width + 3 + branch.width() < cols
        {
            meta.fg(DIM, &format!(" · {branch}"));
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
            line.plain("   ").fg(GREY, "✕ ");
            // Its number, which no other worker takes meanwhile (U14).
            if let Some(number) = gone.number {
                line.fg(DIM, &format!("#{number} "));
            }
            line.plain(&fit(&gone.name, 20)).fg(DIM, &{
                let closed = gone.status.closed_as(self.lang);
                format!("  {closed} · {}", ago(Some(gone.at), now, self.lang))
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
        // While the list cannot be read, its states are as of then: dimmed
        // and marked as not known now (U12).
        let (color, mark) = match self.offline {
            Some(_) => (DIM, format!("{}?", status.mark())),
            None => (status.color(), status.mark().to_string()),
        };

        let mut line = Line::default();
        line.plain(" ");
        if selected {
            line.fg(color, "▶");
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
        // The orchestrator has no number (U3).
        let number = match self.numbers.get(&pane) {
            Some(number) => format!("{number:>width$}"),
            None => " ".repeat(width),
        };
        line.fg(DIM, &number).plain(" ").fg(color, &mark).plain(" ");
        let name = pad(&format!("{crumb}{}", self.name(pane)), name_width);
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
            line.plain("  ").fg(color, &state);
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
        // Where it works, whole (U6): its branch and directory, on as many
        // lines as they take.
        let place = match branch_of(panel) {
            Some(branch) => format!("{branch} · {}", clean(&panel.cwd)),
            None => clean(&panel.cwd),
        };
        if !place.is_empty() {
            for part in wrap(&place, cols - 2) {
                let mut line = Line::default();
                line.plain(" ").fg(DIM, &part);
                lines.push(line.finish(cols));
            }
        }
        // What it runs on and since when (U9).
        let facts = self.spawn_facts(panel, now).join(" · ");
        if !facts.is_empty() {
            for part in wrap(&facts, cols - 2) {
                let mut line = Line::default();
                line.plain(" ").fg(DIM, &part);
                lines.push(line.finish(cols));
            }
        }

        // What it was asked to do and what it wrote back (D18).
        lines.extend(self.task_lines(pane, cols, rows));
        lines.extend(self.result_lines(pane, cols, now));

        let history: &[Report] = self.history.get(&pane).map(Vec::as_slice).unwrap_or(&[]);
        let history_rows = history.len().clamp(1, 6);
        // Title + box borders + history title + rows + gap, and the footer
        // below.
        let screen_rows = rows
            .saturating_sub(lines.len() + 4 + 2 + history_rows + 1)
            .max(3);

        let border = if status.group() == Group::Needs {
            status.color()
        } else {
            BORDER
        };
        if self.result_shown {
            // `.max(3)` can give the box rows past the frame, which `render`
            // cuts off: scroll only as far as the rows that will show
            // (under the title and the top border, above the footer).
            let visible = rows
                .saturating_sub(1 + lines.len() + 2)
                .clamp(1, screen_rows);
            lines.extend(self.result_box(pane, cols, screen_rows, visible, now));
        } else {
            let mut title = Line::default();
            title.plain(" ").bold(self.lang.pick("画面", "Screen")).fg(
                DIM,
                self.lang
                    .pick("  ライブ · 読み取り専用", "  live · read-only"),
            );
            lines.push(title.finish(cols));
            let screen = self.screens.get(&pane).map(Vec::as_slice).unwrap_or(&[]);
            let skip = screen.len().saturating_sub(screen_rows);
            let body: Vec<(Option<Rgb>, String)> = screen[skip..]
                .iter()
                .map(|line| (None, clean(line)))
                .collect();
            lines.extend(boxed(&body, cols, screen_rows, border));
        }

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

    /// The detail view's task section (D18): the last task the worker was
    /// sent, on up to three lines (fewer in a short pane), or why there is
    /// none. Nothing before the files were first looked at.
    fn task_lines(&self, pane: u64, cols: usize, rows: usize) -> Vec<String> {
        let Some(task) = self.insight.task(pane) else {
            return Vec::new();
        };
        let l = |ja, en| self.lang.pick(ja, en);
        let mut title = Line::default();
        title.plain(" ").bold(l("タスク", "Task"));
        let why = match task {
            Task::Sent {
                text,
                cut,
                retry_of,
            } => {
                if let Some(old) = retry_of {
                    title.fg(
                        DIM,
                        &self.lang.pick(
                            format!("  ペイン {old} の再試行"),
                            format!("  retry of pane {old}"),
                        ),
                    );
                }
                if *cut {
                    let n = journal::TEXT_CHARS;
                    title.fg(
                        DIM,
                        &self.lang.pick(
                            format!("  先頭 {n} 文字で切れています"),
                            format!("  cut at the first {n} characters"),
                        ),
                    );
                }
                let mut out = vec![title.finish(cols)];
                let flat: String = text
                    .chars()
                    .map(|ch| if ch == '\n' { '⏎' } else { ch })
                    .collect();
                let width = cols.saturating_sub(4);
                let max = match rows {
                    0..24 => 1,
                    24..32 => 2,
                    _ => 3,
                };
                let mut parts = wrap(&clean(&flat), width);
                if parts.len() > max {
                    parts.truncate(max);
                    let last = &mut parts[max - 1];
                    *last = fit(&format!("{last}…"), width);
                }
                for part in parts {
                    let mut line = Line::default();
                    line.plain("   ").plain(&part);
                    out.push(line.finish(cols));
                }
                return out;
            }
            Task::NotSent => l("タスクはまだ送られていません", "no task sent").to_string(),
            Task::NotJournaled => l(
                "ジャーナルにこのワーカーの記録がありません",
                "not in the journal",
            )
            .to_string(),
            Task::NoJournal => l(
                "ジャーナルがありません(親ペインが一覧にありません)",
                "no journal (its parent pane is not listed)",
            )
            .to_string(),
            Task::JournalMissing => {
                l("ジャーナルが見つかりません", "journal not found").to_string()
            }
            Task::JournalUnreadable(err) => self.lang.pick(
                format!("ジャーナルを読めません: {}", clean(err)),
                format!("cannot read the journal: {}", clean(err)),
            ),
        };
        title.plain("  ").fg(DIM, &why);
        vec![title.finish(cols)]
    }

    /// What a result file's state says, and in which colour: written (how
    /// much, how long ago) or why there is nothing to read.
    fn result_state(&self, state: &FileState, now: Instant) -> (Rgb, String) {
        let l = |ja: &str, en: &str| self.lang.pick(ja, en).to_string();
        match state {
            FileState::Written {
                bytes, age, seen, ..
            } => {
                let when = span_ago(*age + now.saturating_duration_since(*seen), self.lang);
                let text = self.lang.pick(
                    format!("{bytes} バイト · {when}に書かれました"),
                    format!("{bytes} bytes · written {when}"),
                );
                (GREEN, text)
            }
            FileState::Missing => (DIM, l("まだ作られていません", "not created")),
            FileState::Empty => (
                DIM,
                l("空です(まだ書かれていません)", "empty (not written yet)"),
            ),
            FileState::NotRegular => (
                DIM,
                l(
                    "通常のファイルではないので読みません",
                    "not a regular file; not read",
                ),
            ),
            FileState::Unreadable(err) => (
                DIM,
                self.lang.pick(
                    format!("読めません: {}", clean(err)),
                    format!("cannot read it: {}", clean(err)),
                ),
            ),
        }
    }

    /// The detail view's result section (D18): the state of the worker's
    /// `$NOA_RESULT` and its path.
    fn result_lines(&self, pane: u64, cols: usize, now: Instant) -> Vec<String> {
        let Some(file) = self.insight.result(pane) else {
            return Vec::new();
        };
        let mut title = Line::default();
        title
            .plain(" ")
            .bold(self.lang.pick("結果", "Result"))
            .plain("  ");
        let ResultFile::Known(path, state) = file else {
            title.fg(
                DIM,
                self.lang.pick(
                    "結果ファイルが分かりません(ジャーナルになく、ラベルもありません)",
                    "no known result file (not journaled, no plain label)",
                ),
            );
            return vec![title.finish(cols)];
        };
        let (color, text) = self.result_state(state, now);
        title.fg(color, &text);
        if file.written() {
            let hint = if self.result_shown {
                self.lang.pick(" · r で画面", " · r: screen")
            } else {
                self.lang.pick(" · r で表示", " · r: show")
            };
            title.fg(DIM, hint);
        }
        let mut out = vec![title.finish(cols)];
        let width = cols.saturating_sub(4);
        let mut parts = wrap(&clean(&path.to_string_lossy()), width);
        if parts.len() > 2 {
            parts.truncate(2);
            parts[1] = fit(&format!("{}…", parts[1]), width);
        }
        for part in parts {
            let mut line = Line::default();
            line.plain("   ").fg(DIM, &part);
            out.push(line.finish(cols));
        }
        out
    }

    /// The result file's text in place of the screen box (`r`, D18),
    /// scrolled by `result_scroll`: a title, then a box of `height` rows, of
    /// which the first `visible` fit in the frame.
    fn result_box(
        &self,
        pane: u64,
        cols: usize,
        height: usize,
        visible: usize,
        now: Instant,
    ) -> Vec<String> {
        let l = |ja, en| self.lang.pick(ja, en);
        let dim = |text: String| (Some(DIM), text);
        let mut body: Vec<(Option<Rgb>, String)> = Vec::new();
        match self.insight.result(pane) {
            None => body.push(dim(l("読んでいます…", "reading…").to_string())),
            Some(ResultFile::Unknown) => {
                body.push(dim(
                    l("結果ファイルが分かりません", "no known result file").to_string()
                ))
            }
            Some(ResultFile::Known(_, state)) if !matches!(state, FileState::Written { .. }) => {
                body.push(dim(self.result_state(state, now).1));
            }
            Some(ResultFile::Known(..)) => {
                match self.insight.shown(pane).map(|shown| &shown.text) {
                    None => body.push(dim(l("読んでいます…", "reading…").to_string())),
                    Some(Err(StartError::NotRegular)) => body.push(dim(l(
                        "通常のファイルではないので読みません",
                        "not a regular file; not read",
                    )
                    .to_string())),
                    Some(Err(StartError::Io(err))) => body.push(dim(self.lang.pick(
                        format!("読めません: {}", clean(err)),
                        format!("cannot read it: {}", clean(err)),
                    ))),
                    Some(Ok((text, cut))) => {
                        let width = cols.saturating_sub(5);
                        for line in text.lines() {
                            let line = clean(&line.replace('\t', "    "));
                            body.extend(wrap(&line, width).into_iter().map(|part| (None, part)));
                        }
                        if text.is_empty() {
                            body.push(dim(l("空です", "empty").to_string()));
                        }
                        if *cut {
                            let kib = result::START_BYTES / 1024;
                            let lines = result::START_LINES;
                            body.push(dim(self.lang.pick(
                                format!("… 先頭だけを表示しています({kib} KiB・{lines} 行まで)"),
                                format!(
                                    "… only the start is shown (up to {kib} KiB, {lines} lines)"
                                ),
                            )));
                        }
                    }
                }
            }
        }
        let reach = body.len().saturating_sub(visible);
        self.result_reach.set(reach);
        let offset = self.result_scroll.min(reach);
        let mut title = Line::default();
        title.plain(" ").bold(l("結果ファイル", "Result file"));
        if body.len() > visible {
            let (from, to, all) = (offset + 1, (offset + visible).min(body.len()), body.len());
            title.fg(
                DIM,
                &self.lang.pick(
                    format!("  {from}–{to} / {all} 行 · j/k でスクロール ·"),
                    format!("  lines {from}–{to} of {all} · j/k scroll ·"),
                ),
            );
        }
        title.fg(DIM, l("  r で画面に戻る", "  r: back to the screen"));
        let mut out = vec![title.finish(cols)];
        out.extend(boxed(&body[offset..], cols, height, BORDER));
        out
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
                (
                    if self.quit_key() == 'y' { "y" } else { "q" },
                    l("終了", "quit"),
                    l("終了", "quit"),
                ),
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
                let approval = self.target().is_some_and(|pane| self.awaits_answer(pane));
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
                    View::Detail(_) if self.result_shown => keys.extend([
                        ("Esc", l("一覧", "list"), l("一覧", "list")),
                        ("r", l("画面", "screen"), l("画面", "screen")),
                        ("j/k", l("スクロール", "scroll"), l("スクロール", "scroll")),
                    ]),
                    View::Detail(_) => keys.extend([
                        ("Esc", l("一覧", "list"), l("一覧", "list")),
                        ("r", l("結果", "result"), l("結果", "result")),
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

    fn write(bytes: &[u8]) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(bytes);
        let _ = out.flush();
    }
}

/// What a frame writes (U13): nothing when it is the one on screen, else
/// the whole frame inside a synchronized update (DEC 2026), so the terminal
/// never paints half of one.
#[derive(Default)]
struct Frames {
    shown: Option<Vec<String>>,
    /// The screen is to be cleared first (the pane was resized).
    clear: bool,
}

impl Frames {
    fn next(&mut self, lines: Vec<String>) -> Option<Vec<u8>> {
        if self.shown.as_ref() == Some(&lines) {
            return None;
        }
        let mut out = b"\x1b[?2026h".to_vec();
        if std::mem::take(&mut self.clear) {
            out.extend_from_slice(b"\x1b[2J");
        }
        out.extend_from_slice(b"\x1b[H");
        out.extend_from_slice(lines.join("\r\n").as_bytes());
        out.extend_from_slice(b"\x1b[?2026l");
        self.shown = Some(lines);
        Some(out)
    }

    /// The pane was resized: the next frame clears and draws whole.
    fn resized(&mut self) {
        self.shown = None;
        self.clear = true;
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
/// a growing wait, up to [`RETRY_MAX`], for as long as it takes: ending the
/// board hands every worker back to the orchestrator, which holds 8 of the
/// 20 it may have, and closes the workers of workers (D10, D12, D13), so a
/// stalled or restarting Noa must never end it; only the person does (U1).
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

    /// Note a failed listing.
    pub(crate) fn failed(&mut self, now: Instant) {
        self.failing_since.get_or_insert(now);
        self.delay = (self.delay * 2).clamp(RETRY_FIRST, RETRY_MAX);
        self.next = Some(now + self.delay);
    }

    pub(crate) fn failing_since(&self) -> Option<Instant> {
        self.failing_since
    }
}

/// Why a listing failed; either way it is tried again.
enum ListFailure {
    /// Noa did not answer, or the connection could not be opened.
    Lost,
    /// A reconnect reached another Noa (see [`same_instance`]).
    Elsewhere,
}

/// Whether a connection opened again reached the Noa the board started on
/// (`first`, its instance id; empty when that server predates the field,
/// and then it cannot be told). Pane ids are only meaningful within one
/// instance: another Noa's pane under the same id is not this worker.
fn same_instance(first: &str, now: &str) -> bool {
    first.is_empty() || first == now
}

/// How the board opens a lost connection again: once per attempt. Its own
/// backoff ([`Polling`]) spaces the attempts; a connect that slept through
/// 3 s of retries froze keys and redraws while Noa was at its connection
/// limit.
const RECONNECT: super::ConnectRetry = super::ConnectRetry::Once;

/// One pane listing. A connection that was lost is opened again first
/// ([`RECONNECT`]), and again on the next try while it reaches another Noa.
fn list(
    client: &mut Client,
    reconnect: &mut bool,
    instance: &str,
) -> Result<Vec<Panel>, ListFailure> {
    if *reconnect {
        let fresh = super::connect_client(RECONNECT)
            .map_err(|_| ListFailure::Lost)?
            .0;
        // Another Noa's connection is dropped here, never kept: an action
        // through it would reach that instance's pane of the same id.
        if !same_instance(instance, fresh.server_instance_id()) {
            return Err(ListFailure::Elsewhere);
        }
        *client = fresh;
        *reconnect = false;
    }
    client.list_panels().map_err(|err| {
        *reconnect = lost(&err);
        ListFailure::Lost
    })
}

/// Take what a listing gave, and whether to ring the bell (see
/// [`Board::update`]). A failure is only noted: the board keeps what it had
/// and tries again (see [`Polling`]).
fn take_listing(
    board: &mut Board,
    polling: &mut Polling,
    listing: Result<Vec<Panel>, ListFailure>,
    now: Instant,
) -> bool {
    let ring = match listing {
        Ok(panels) => {
            let ring = board.update(panels, now);
            board.elsewhere = false;
            polling.succeeded(now);
            ring
        }
        Err(failure) => {
            board.elsewhere = matches!(failure, ListFailure::Elsewhere);
            polling.failed(now);
            false
        }
    };
    board.offline = polling.failing_since();
    ring
}

/// Whether `err` means the connection is gone and must be opened again. A
/// server at its connection limit closes the connection too (1013).
fn lost(err: &ClientError) -> bool {
    matches!(
        err,
        ClientError::Transport(_)
            | ClientError::HandshakeDropped(_)
            | ClientError::Closed(_)
            | ClientError::TooManyConnections(_)
    )
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
pub(crate) static SIGNALED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

pub(crate) extern "C" fn on_signal(signal: libc::c_int) {
    SIGNALED.store(signal, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) const TERMINATING_SIGNALS: [libc::c_int; 3] =
    [libc::SIGTERM, libc::SIGHUP, libc::SIGINT];

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
    let mut frames = Frames::default();
    let mut dirty = true;
    let mut title = String::new();
    let mut ring = false;
    let mut reader = KeyReader::default();
    loop {
        if SIGNALED.load(std::sync::atomic::Ordering::Relaxed) != 0 {
            return Ok(EXIT_OK);
        }
        let now = Instant::now();
        if polling.due(now) {
            let listing = list(client, &mut reconnect, &instance);
            ring |= take_listing(&mut board, &mut polling, listing, now);
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
        // Journals and result files, at most every few seconds (D18).
        dirty |= board.refresh_files(now);
        let current = Screen::size();
        if current != size {
            size = current;
            frames.resized();
            dirty = true;
        }
        if dirty {
            if let Some(frame) = frames.next(board.render(size.0, size.1, now)) {
                Screen::write(&frame);
            }
            let current = board.title();
            if current != title {
                Screen::set_title(&current);
                title = current;
            }
            dirty = false;
        }
        if std::mem::take(&mut ring) {
            Screen::write(b"\x07");
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
            if hold_elsewhere(&mut board, &action) {
                continue;
            }
            let (notice, draft) = perform(client, &board, action);
            board.message = Some(notice);
            board.drafts.extend(draft);
        }
    }
}

/// Why no action is carried out right now: the last reconnect reached
/// another Noa, whose panes share ids with the workers listed here.
fn held_elsewhere(board: &Board) -> Option<Notice> {
    board.elsewhere.then(|| {
        Notice::warn(
            board
                .lang
                .pick(
                    "別の Noa が応答しているため操作できません · 元の Noa を待っています",
                    "Another Noa answered, so nothing is done · waiting for ours",
                )
                .to_string(),
        )
    })
}

/// Hold `action` while another Noa answers, saying so on the board; a
/// send's text stays as the worker's draft. Returns whether it was held.
fn hold_elsewhere(board: &mut Board, action: &Action) -> bool {
    let Some(mut notice) = held_elsewhere(board) else {
        return false;
    };
    if let Action::Send(pane, text) = action
        && board.keep_draft(*pane, text.clone())
    {
        notice.text = format!("{} · {}", notice.text, draft_kept(board.lang));
    }
    board.message = Some(notice);
    true
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

/// Carry out an action and describe the result in one line; a message none
/// of which was typed comes back to be kept as its worker's draft (U5).
fn perform(
    server: &mut impl Server,
    board: &Board,
    action: Action,
) -> (Notice, Option<(u64, String)>) {
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
    let kept = |notice: Notice, pane: u64, text: &str| {
        let notice = Notice {
            text: format!("{} · {}", notice.text, draft_kept(board.lang)),
            ..notice
        };
        (notice, Some((pane, text.to_string())))
    };
    // The board's listing can be up to `LIST_EVERY` old: look again right
    // before typing, so a worker that started waiting for an approval or on
    // a question since is never answered from here (spec D7).
    if let Action::Send(pane, text) = &action {
        match server.panels() {
            Ok(panels) => {
                if let Some(why) = panels
                    .iter()
                    .find(|panel| panel.pane_id.0 == *pane)
                    .and_then(|panel| {
                        answer_message(Status::of(panel), &board.name(*pane), board.lang)
                    })
                {
                    return kept(Notice::warn(why), *pane, text);
                }
            }
            Err(err) => return kept(failed(err), *pane, text),
        }
    }
    let result = match &action {
        Action::Focus(pane) => server.focus(*pane),
        Action::Send(pane, text) => {
            if let Err(err) = server.type_text(*pane, text, true) {
                return kept(failed(err), *pane, text);
            }
            std::thread::sleep(SUBMIT_DELAY);
            // The Enter could answer a prompt that appeared in the pause.
            let asking = server.panels().map(|panels| {
                panels
                    .iter()
                    .find(|panel| panel.pane_id.0 == *pane)
                    .map(Status::of)
                    .filter(|status| answered_in_pane(*status))
            });
            match asking {
                Ok(Some(status)) => {
                    // The text is in the worker's input box already: no
                    // draft, or sending it again would type it twice.
                    let name = board.name(*pane);
                    let why = match status {
                        Status::Question => board.lang.pick(
                            format!(
                                "{name} が質問を出したので Enter は送っていません (最初の選択肢を選ぶため)。g でペインへ移動して答えてください"
                            ),
                            format!(
                                "{name} now asks a question, so Enter was not sent (it would pick the first choice). g goes to its pane to answer it"
                            ),
                        ),
                        _ => board.lang.pick(
                            format!(
                                "{name} が承認待ちになったので Enter は送っていません。g でペインへ移動して答えてください"
                            ),
                            format!(
                                "{name} now awaits approval, so Enter was not sent. g goes to its pane to answer it"
                            ),
                        ),
                    };
                    return (Notice::warn(why), None);
                }
                Ok(None) => server.type_text(*pane, "\r", false),
                Err(err) => Err(err),
            }
        }
        Action::Close(pane) => server.close(*pane),
        Action::Quit => Ok(()),
    };
    let name = |pane: &u64| board.name(*pane);
    let notice = match (action, result) {
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
    };
    (notice, None)
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

    // U8: a long command run in a worker looked stopped: grey "· 報告なし",
    // as an exited one.
    #[test]
    fn a_command_that_is_no_agent_is_running() {
        let mut command = worker(2, 1, "tests", None, 0);
        command.agent = None;
        assert_eq!(Status::of(&command), Status::Command);
        assert_eq!(Status::Command.group(), Group::Working);
        let mut board = Board::new(Some(1), None);
        board.update(vec![command.clone()], Instant::now());
        let all = rendered(&board, 80, 30, Instant::now());
        assert!(
            line_with(&all, "tests").contains("● tests  実行中"),
            "{all}"
        );
        // An agent that never reported stays "no report".
        let mut silent = worker(2, 1, "tests", None, 0);
        silent.agent.as_mut().unwrap().detail = None;
        assert_eq!(Status::of(&silent), Status::Quiet);
        // A command that rang the bell needs someone.
        command.attention = true;
        assert_eq!(Status::of(&command), Status::Attention);
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

        let (message, draft) = perform(&mut server, &board, Action::Send(2, "y".to_string()));

        assert_eq!(server.typed, Vec::new());
        assert!(message.text.contains("承認待ち"), "{message:?}");
        assert!(message.warn);
        // U5: nothing was typed, so the message is kept to send later.
        assert!(message.text.contains("下書きを残しました"), "{message:?}");
        assert_eq!(draft, Some((2, "y".to_string())));
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

        let (message, draft) = perform(&mut server, &board, Action::Send(2, "run it".to_string()));

        assert_eq!(server.typed, vec![(2, "run it".to_string())]);
        assert!(message.text.contains("承認待ち"), "{message:?}");
        assert!(message.warn);
        // It is in the worker's input box: no draft to type it twice.
        assert_eq!(draft, None);
    }

    // U5: a message being typed was lost when the worker began waiting for
    // an approval before Enter, and on Esc.
    #[test]
    fn a_message_that_did_not_go_is_kept_as_a_draft() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), t0);
        board.select(2);
        board.key(Key::Char('s'));
        board.key(Key::Paste("look at the tests".to_string()));
        let mut waiting = sample();
        waiting[0].agent.as_mut().unwrap().state = Some("permission".to_string());
        board.update(waiting, t0);
        assert_eq!(board.key(Key::Enter), None);
        let message = board.message.clone().unwrap();
        assert!(
            message.warn && message.text.ends_with("下書きを残しました (s で再開)"),
            "{message:?}"
        );
        // Not while it still waits; once it works again, `s` starts from it.
        board.key(Key::Char('s'));
        assert_eq!(board.mode, Mode::Normal);
        board.update(sample(), t0);
        board.key(Key::Char('s'));
        assert_eq!(board.mode, Mode::Send(2, "look at the tests".to_string()));
        // Esc keeps it too; a line cleared first drops it.
        board.key(Key::Esc);
        assert_eq!(
            board.drafts.get(&2).map(String::as_str),
            Some("look at the tests")
        );
        board.key(Key::Char('s'));
        board.key(Key::CtrlU);
        board.key(Key::Esc);
        assert!(board.drafts.is_empty());
        // A draft goes with its worker.
        board.drafts.insert(2, "later".to_string());
        let without: Vec<Panel> = sample().into_iter().filter(|p| p.pane_id.0 != 2).collect();
        board.update(without, t0);
        assert!(board.drafts.is_empty());
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

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    // U9: the argv the tiers pass (ctl/worktree.rs), and the joined forms.
    #[test]
    fn the_model_and_effort_are_read_from_the_argv() {
        let read = |args: &[&str]| model_and_effort(&argv(args));
        let some = |model: &str, effort: Option<&str>| {
            (Some(model.to_string()), effort.map(str::to_string))
        };
        assert_eq!(
            read(&["claude", "--model", "opus", "--effort", "high"]),
            some("opus", Some("high"))
        );
        assert_eq!(
            read(&[
                "codex",
                "-m",
                "gpt-6-luna",
                "-c",
                "model_reasoning_effort=low"
            ]),
            some("gpt-6-luna", Some("low"))
        );
        assert_eq!(
            read(&[
                "codex",
                "--config=model_reasoning_effort=\"medium\"",
                "--model=o"
            ]),
            some("o", Some("medium"))
        );
        assert_eq!(
            read(&["agy", "--model", "gemini-4-neon-high"]),
            some("gemini-4-neon-high", None)
        );
        assert_eq!(
            read(&["claude", "--effort=low"]),
            (None, Some("low".to_string()))
        );
        assert_eq!(read(&["cargo", "test"]), (None, None));
        // The program itself is no flag, and a flag at the end has no value.
        assert_eq!(read(&["-m", "--model"]), (None, None));
    }

    #[test]
    fn the_spawn_facts_say_model_effort_and_start_in_both_languages() {
        let spawn = PanelSpawn {
            argv: argv(&["claude", "--model", "opus", "--effort", "high"]),
            started_at: 1_000,
        };
        assert_eq!(
            spawn_facts(&spawn, Some(1_720), Lang::Ja),
            vec!["opus", "推論量 high", "12 分前に起動"]
        );
        assert_eq!(
            spawn_facts(&spawn, Some(1_720), Lang::En),
            vec!["opus", "effort high", "started 12m ago"]
        );
        let bare = PanelSpawn {
            argv: argv(&["cargo", "test"]),
            started_at: 1_000,
        };
        assert_eq!(
            spawn_facts(&bare, Some(1_030), Lang::En),
            vec!["started 30s ago"]
        );
        assert!(spawn_facts(&bare, None, Lang::En).is_empty());
    }

    // U9: the preview's last line and the detail say what a worker runs on
    // and since when, and every line keeps the pane's width.
    #[test]
    fn the_preview_and_the_detail_say_the_model_and_since_when() {
        let t0 = Instant::now();
        let mut panel = worker(2, 1, "w2", Some("running"), 1);
        panel.spawn = Some(PanelSpawn {
            argv: argv(&["claude", "--model", "opus", "--effort", "high"]),
            started_at: 1_000,
        });
        // A pane from an older server, with no spawn.
        let plain = worker(3, 1, "w3", Some("running"), 1);
        for lang in [Lang::Ja, Lang::En] {
            let mut board = Board::new(Some(1), None);
            board.lang = lang;
            board.update(vec![panel.clone(), plain.clone()], t0);
            board.wall = Some((1_000 + 12 * 60, t0));
            let facts = lang.pick(
                "opus · 推論量 high · 13 分前に起動",
                "opus · effort high · started 13m ago",
            );
            let at = t0 + Duration::from_secs(60);
            board.select(2);
            let all = rendered(&board, 200, 30, at);
            assert!(
                line_with(&all, lang.pick("報告 1", "report 1")).contains(facts),
                "{all}"
            );
            board.view = View::Detail(2);
            let all = rendered(&board, 80, 30, at);
            assert!(all.contains(facts), "{all}");
            board.view = View::List;
            board.select(3);
            let all = rendered(&board, 120, 30, at);
            assert!(!all.contains("opus"), "{all}");

            board.select(2);
            for cols in [24, 40, 41, 60, 80, 120] {
                for view in [View::List, View::Detail(2)] {
                    board.view = view;
                    for line in board.render(cols, 30, at) {
                        let line = visible(&line);
                        assert_eq!(line.width(), cols, "{view:?} {cols}: {line:?}");
                    }
                }
            }
            board.view = View::List;
        }
    }

    /// `panel` asking a question dialog, as an `OSC 7501` blocked report
    /// lists it (AU-01).
    fn asking(mut panel: Panel) -> Panel {
        let agent = panel.agent.as_mut().unwrap();
        agent.state = Some("input".to_string());
        agent.blocked = Some("question".to_string());
        panel
    }

    // A question dialog is answered in its pane like an approval: the Enter
    // that submits a send line would pick its first choice. An input
    // reported without a dialog (a hook) still takes a message.
    #[test]
    fn a_question_dialog_reads_as_a_question_and_is_answered_in_its_pane() {
        let t0 = Instant::now();
        let question = asking(worker(2, 1, "ask", Some("running"), 2));
        assert_eq!(Status::of(&question), Status::Question);
        let hook = worker(3, 1, "hook", Some("input"), 2);
        assert_eq!(Status::of(&hook), Status::Input);
        for lang in [Lang::Ja, Lang::En] {
            let mut board = Board::new(Some(1), None);
            board.lang = lang;
            board.update(vec![question.clone(), hook.clone()], t0);
            let all = rendered(&board, 80, 30, t0);
            let row = line_with(&all, "? ask");
            assert!(row.contains(lang.pick("質問", "question")), "{all}");
            let row = line_with(&all, "? hook");
            assert!(row.contains(lang.pick("入力待ち", "input")), "{all}");
            assert!(!row.contains("question"), "{all}");

            board.select(2);
            assert_eq!(board.key(Key::Char('s')), None);
            assert_eq!(board.mode, Mode::Normal);
            let notice = board.message.clone().unwrap();
            assert!(notice.warn);
            assert!(
                notice
                    .text
                    .contains(lang.pick("最初の選択肢", "first choice")),
                "{notice:?}"
            );
            let footer = visible(&board.render(120, 20, t0).pop().unwrap());
            assert!(
                footer.contains(lang.pick("移動して答える", "go and answer")),
                "{footer}"
            );
            board.view = View::Detail(2);
            let detail = rendered(&board, 80, 30, t0);
            assert!(
                line_with(&detail, "ask").contains(lang.pick("質問", "question")),
                "{detail}"
            );
            board.view = View::List;

            board.select(3);
            board.key(Key::Char('s'));
            assert!(matches!(board.mode, Mode::Send(3, _)));
            // It opened a dialog while the message was being typed.
            board.mode = Mode::Send(2, "yes".to_string());
            assert_eq!(board.key(Key::Enter), None);
            assert_eq!(board.drafts.get(&2).map(String::as_str), Some("yes"));
        }
    }

    // The listing right before typing, and the one after the pause before
    // Enter, refuse a question dialog as they do an approval.
    #[test]
    fn sending_never_answers_a_question_dialog_that_opened_since() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        let mut now_asking = sample();
        now_asking[0] = asking(now_asking[0].clone());
        let mut server = FakeServer {
            listings: vec![now_asking.clone()],
            typed: Vec::new(),
        };
        let (message, draft) = perform(&mut server, &board, Action::Send(2, "go".to_string()));
        assert!(server.typed.is_empty());
        assert!(
            message.warn && message.text.contains("最初の選択肢"),
            "{message:?}"
        );
        assert_eq!(draft, Some((2, "go".to_string())));

        let mut server = FakeServer {
            listings: vec![sample(), now_asking],
            typed: Vec::new(),
        };
        board.lang = Lang::En;
        let (message, draft) = perform(&mut server, &board, Action::Send(2, "go".to_string()));
        assert_eq!(server.typed, vec![(2, "go".to_string())]);
        assert!(
            message.warn
                && message
                    .text
                    .contains("asks a question, so Enter was not sent"),
            "{message:?}"
        );
        assert_eq!(draft, None);
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
        assert_eq!(board.mode, Mode::ConfirmQuit);
        assert_eq!(board.key(Key::Char('q')), Some(Action::Quit));
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
            all.contains("✕ #3 docs  完了後に閉じた · 1 分前  docs detail"),
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
        // U4: the keys are said where an opened worker does not cover them.
        assert!(
            visible(&lines[2]).contains("api を閉じますか? y で閉じる · ほかのキーで取り消し"),
            "{}",
            visible(&lines[2])
        );
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

    // U3: the orchestrator waiting for an approval of its own `+ctl` call
    // held every worker up, and the board did not say so.
    #[test]
    fn an_orchestrator_needing_someone_heads_the_queue() {
        let t0 = Instant::now();
        let orchestrator = |state: &str| {
            let mut me = worker(1, 0, "", Some(state), 3);
            me.parent_pane_id = None;
            me.label = None;
            me.name = "fix the flaky suite".to_string();
            me.agent.as_mut().unwrap().detail = Some("Bash: +ctl wait".to_string());
            me
        };
        let with = |state: &str| {
            let mut panels = sample();
            panels.push(orchestrator(state));
            panels
        };
        let mut board = Board::new(Some(1), Some(8));
        board.update(with("running"), t0);
        // At work: said in the header, not queued.
        let header = visible(&board.render(120, 30, t0)[0]);
        assert!(
            header.contains("fix the flaky suite · claude  ● 作業中  Bash: +ctl wait"),
            "{header}"
        );
        assert_eq!(queue(&board), vec![3]);
        assert_eq!(board.title(), "ボード · 要対応 1");

        assert!(board.update(with("permission"), t0 + Duration::from_secs(1)));
        let at = t0 + Duration::from_secs(6);
        let all = rendered(&board, 120, 30, at);
        assert!(all.contains("! 承認待ち 5 秒  Bash: +ctl wait"), "{all}");
        assert_eq!(queue(&board), vec![1, 3]);
        assert!(line_with(&all, "! 人の対応が必要").contains(" 2 "), "{all}");
        assert!(
            line_with(&all, "! オーケストレーター").contains("承認待ち"),
            "{all}"
        );
        assert_eq!(board.title(), "ボード · 要対応 2");
        // Selected and gone to like a worker, but never closed from here.
        board.key(Key::Char('n'));
        assert_eq!(board.selected(), Some(1));
        assert_eq!(board.key(Key::Char('g')), Some(Action::Focus(1)));
        assert_eq!(board.key(Key::Char('x')), None);
        assert_eq!(board.mode, Mode::Normal);
        assert!(board.message.clone().unwrap().warn);
        for cols in [24, 40, 60, 80, 120] {
            for line in board.render(cols, 30, at) {
                assert_eq!(visible(&line).width(), cols, "{cols}: {:?}", visible(&line));
            }
        }
        // Back at work it leaves the queue, and the selection moves on.
        assert!(!board.update(with("running"), at));
        assert_eq!(queue(&board), vec![3]);
        assert_eq!(board.selected(), Some(3));
    }

    /// Orchestrator 1 waiting for an approval, with no worker yet.
    fn lone_orchestrator() -> Board {
        let mut me = worker(1, 0, "", Some("permission"), 3);
        me.parent_pane_id = None;
        me.label = None;
        me.name = "fix the flaky suite".to_string();
        let mut board = Board::new(Some(1), Some(8));
        board.update(vec![me], Instant::now());
        board
    }

    // Review finding: with no worker, the orchestrator needing someone was
    // counted in the header and selected, but the list drew only the
    // empty note: neither its queue row nor its preview.
    #[test]
    fn an_orchestrator_needing_someone_is_drawn_with_no_workers() {
        let mut board = lone_orchestrator();
        board.set_screen(1, "Allow Bash: +ctl wait?");
        assert_eq!(board.title(), "ボード · 要対応 1");
        assert_eq!(board.selected(), Some(1));
        let all = rendered(&board, 120, 30, Instant::now());
        assert!(
            line_with(&all, "▶  ! オーケストレーター").contains("承認待ち"),
            "{all}"
        );
        assert!(all.contains("Allow Bash: +ctl wait?"), "{all}");
        assert!(!all.contains("ワーカーはまだいません"), "{all}");
    }

    // Review finding: the list view fetched only the workers' screens, so
    // the orchestrator's preview stayed at "reading the screen…".
    #[test]
    fn the_list_fetches_the_orchestrators_screen_while_it_needs_someone() {
        let board = lone_orchestrator();
        assert_eq!(board.wanted_screens(), vec![1]);
        let mut panels = sample();
        panels.extend(lone_orchestrator().panels);
        let mut board = Board::new(Some(1), Some(8));
        board.update(panels, Instant::now());
        assert!(board.wanted_screens().contains(&1));
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
        assert!(all.contains("✕ #4 w5"), "{all}");
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
        assert!(!view(&board).contains("w3"), "{}", view(&board));

        for _ in 0..10 {
            board.key(Key::Char('j'));
        }
        let all = view(&board);
        for pane in 3..8 {
            assert!(
                all.contains(&format!("✕ #{} w{pane}", pane - 1)),
                "w{pane}:\n{all}"
            );
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
                if all.contains(&format!("✕ #{} w{pane}", pane - 1)) {
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

        // A worker that starts while the section is scrolled is in view,
        // with a number none of the closed ones had (U14).
        for _ in 0..10 {
            board.key(Key::Char('j'));
        }
        board.update(
            vec![worker(9, 1, "w9", Some("running"), 1)],
            start + Duration::from_secs(3),
        );
        let all = view(&board);
        assert!(all.contains("▶6 ● w9"), "{all}");
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
        let (notice, _) = perform(&mut Closed, &board, Action::Focus(5));
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

        // A closed worker's number is not the next one's (U14).
        panels.remove(0);
        panels.push(worker(7, 1, "w7", Some("running"), 1));
        board.update(panels, start + Duration::from_secs(2));
        board.key(Key::Char('4'));
        assert_eq!(board.selected(), Some(7));
        board.key(Key::Char('1'));
        assert_eq!(board.selected(), Some(7), "1 is w2's, which closed");
        board.key(Key::Char('3'));
        assert_eq!(board.selected(), Some(4));
    }

    // U14: "3 awaits approval" must not come to mean another worker while
    // the board is open; numbers come back only once 1-20 were all given.
    #[test]
    fn a_closed_workers_number_is_given_again_only_after_twenty() {
        let start = Instant::now();
        let mut board = Board::new(Some(1), None);
        let mut panels: Vec<Panel> = (2..22)
            .map(|pane| worker(pane, 1, &format!("w{pane}"), Some("running"), 1))
            .collect();
        board.update(panels.clone(), start);
        assert_eq!(board.number(21), 20);
        // w5 (#4) closes, then w3 (#2).
        panels.retain(|panel| panel.pane_id.0 != 5);
        board.update(panels.clone(), start + Duration::from_secs(1));
        panels.retain(|panel| panel.pane_id.0 != 3);
        board.update(panels.clone(), start + Duration::from_secs(2));
        // All of 1-20 were given: the next takes the number freed first.
        panels.push(worker(30, 1, "w30", Some("running"), 1));
        board.update(panels.clone(), start + Duration::from_secs(3));
        assert_eq!(board.number(30), 4);
        panels.push(worker(31, 1, "w31", Some("running"), 1));
        board.update(panels.clone(), start + Duration::from_secs(4));
        assert_eq!(board.number(31), 2);
        // None freed is left: the numbers go on past 20.
        panels.push(worker(32, 1, "w32", Some("running"), 1));
        board.update(panels, start + Duration::from_secs(5));
        assert_eq!(board.number(32), 21);
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
        // U7: ^C asks too, and a second ^C takes it back.
        assert_eq!(board.key(Key::CtrlC), None);
        assert_eq!(board.mode, Mode::ConfirmQuit);
        assert_eq!(board.key(Key::CtrlC), None);
        assert_eq!(board.mode, Mode::Normal);
        board.key(Key::CtrlC);
        assert_eq!(board.key(Key::Char('q')), Some(Action::Quit));

        let mut empty = Board::new(Some(1), Some(8));
        empty.update(Vec::new(), Instant::now());
        assert_eq!(empty.key(Key::Char('q')), Some(Action::Quit));
    }

    // U2: a worker newly needing someone showed only on the board's rows,
    // which are out of sight while the person works elsewhere.
    #[test]
    fn a_worker_newly_needing_someone_rings_once() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        // ui awaits approval already: not on the first listing.
        assert!(!board.update(sample(), t0));
        assert!(!board.update(sample(), t0));
        let mut asks = sample();
        asks[0].agent.as_mut().unwrap().state = Some("input".to_string());
        assert!(board.update(asks.clone(), t0), "api asks");
        // Not again while it stays in the queue, whatever it waits for.
        assert!(!board.update(asks.clone(), t0));
        asks[0].agent.as_mut().unwrap().state = Some("permission".to_string());
        assert!(!board.update(asks, t0));
        // Back to work, then waiting again: a new request.
        assert!(!board.update(sample(), t0));
        let mut fails = sample();
        fails.push(failed_worker(6, 2));
        assert!(board.update(fails, t0), "a command failed");
        // Leaving the queue rings nothing.
        assert!(!board.update(Vec::new(), t0));
    }

    // Review B02: one failed listing ended the board. U1: so did 30 seconds
    // of them, which closed every worker the orchestrator had no room for.
    #[test]
    fn failed_listings_are_retried_with_backoff_for_as_long_as_it_takes() {
        let t0 = Instant::now();
        let mut polling = Polling::default();
        assert!(polling.due(t0));
        polling.succeeded(t0);
        assert!(!polling.due(t0 + Duration::from_millis(400)));
        assert!(polling.due(t0 + LIST_EVERY));

        let mut at = t0 + LIST_EVERY;
        let mut waits = Vec::new();
        while at < t0 + Duration::from_secs(600) {
            polling.failed(at);
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
        assert_eq!(waits.last(), Some(&RETRY_MAX));

        // One listing that works starts the count again.
        let mut polling = Polling::default();
        polling.failed(t0);
        polling.succeeded(t0 + Duration::from_secs(20));
        assert_eq!(polling.failing_since(), None);
        polling.failed(t0 + Duration::from_secs(40));
        assert_eq!(polling.failing_since(), Some(t0 + Duration::from_secs(40)));
    }

    // U12: before the first listing the board said there were no workers,
    // and while offline its rows looked as current as ever.
    #[test]
    fn what_is_not_known_now_does_not_look_known() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        assert!(rendered(&board, 80, 20, t0).contains("読み込み中…"));
        board.update(Vec::new(), t0);
        assert!(rendered(&board, 80, 20, t0).contains("ワーカーはまだいません"));
        board.update(sample(), t0);
        let row = |board: &Board| {
            board
                .render(80, 30, t0)
                .into_iter()
                .find(|line| visible(line).contains(" api "))
                .unwrap()
        };
        let Rgb(r, g, b) = BLUE;
        assert!(
            row(&board).contains(&format!("38;2;{r};{g};{b}m●")),
            "{:?}",
            row(&board)
        );
        board.offline = Some(t0);
        let Rgb(r, g, b) = DIM;
        assert!(
            row(&board).contains(&format!("38;2;{r};{g};{b}m●?")),
            "{:?}",
            row(&board)
        );
        assert!(
            row(&board).contains(&format!("38;2;{r};{g};{b}m作業中")),
            "{:?}",
            row(&board)
        );
    }

    #[test]
    fn the_board_says_it_cannot_connect_and_keeps_what_it_had() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        let mut polling = Polling::default();
        take_listing(&mut board, &mut polling, Ok(sample()), t0);
        take_listing(&mut board, &mut polling, Err(ListFailure::Lost), t0);
        let all = rendered(&board, 100, 30, t0 + Duration::from_secs(7));
        assert!(
            all.contains("接続できません 7 秒 · 表示は 7 秒前のもの · q で終了"),
            "{all}"
        );
        assert!(all.contains("ui") && all.contains("api"), "{all}");
        // Another Noa answering is said, and tried again too.
        take_listing(&mut board, &mut polling, Err(ListFailure::Elsewhere), t0);
        let all = rendered(&board, 120, 30, t0 + Duration::from_secs(7));
        assert!(
            all.contains("別の Noa が応答 · 元の Noa を待機中 7 秒"),
            "{all}"
        );
        assert!(polling.next.is_some());
        // The Noa the board started on answering again puts it back.
        take_listing(
            &mut board,
            &mut polling,
            Ok(sample()),
            t0 + Duration::from_secs(8),
        );
        assert_eq!((board.offline, board.elsewhere), (None, false));
        // Q still asks first while the listing fails.
        take_listing(&mut board, &mut polling, Err(ListFailure::Lost), t0);
        assert_eq!(board.key(Key::Char('q')), None);
        assert_eq!(board.mode, Mode::ConfirmQuit);
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
        assert!(all.contains("─ ● 作業中 3 "), "{all}");
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

    // U11: help descriptions were cut at 80 columns, and at 60 several more.
    #[test]
    fn the_help_wraps_what_does_not_fit() {
        for lang in [Lang::Ja, Lang::En] {
            let mut board = Board::new(Some(1), Some(8));
            board.lang = lang;
            board.update(sample(), Instant::now());
            board.key(Key::Char('?'));
            for cols in [60, 80] {
                let lines = board.render(cols, 60, Instant::now());
                let text: String = lines
                    .iter()
                    .map(|line| visible(line).replace(' ', ""))
                    .collect();
                let space = lang.pick(
                    "作業中のワーカーの子をたたむ / 開く(それ以外では完了・終了を開く / たたむ)",
                    "fold / open a subtree at work (elsewhere: list / fold done and exited)",
                );
                assert!(text.contains(&space.replace(' ', "")), "{cols}: {text}");
                assert!(!text.contains('…'), "{cols}: {text}");
                for line in &lines {
                    assert_eq!(visible(line).width(), cols);
                }
            }
        }
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

    // U7: with workers that close for sure, `q` again is not enough.
    #[test]
    fn quitting_says_the_workers_of_workers_close() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(nested(), Instant::now());
        board.key(Key::Char('q'));
        let lines = board.render(80, 20, Instant::now());
        let top = visible(&lines[2]);
        assert!(
            top.contains(
                "y で終了 · ワーカーの子 2 件と、オーケストレーターに収まらない分は閉じます"
            ),
            "{top}"
        );
        assert!(
            visible(&lines[19]).contains("y 終了"),
            "{}",
            visible(&lines[19])
        );
        assert_eq!(board.key(Key::Char('q')), None);
        assert_eq!(board.mode, Mode::Normal);
        board.key(Key::Char('q'));
        assert_eq!(board.key(Key::Char('y')), Some(Action::Quit));
    }

    // Review finding: the prompt promised that every worker goes back to
    // the orchestrator, which holds 8 floats with those it already has, and
    // at 80 columns in English its warning about the rest was cut off.
    // U11: the key goes first, and the whole prompt fits 80 columns, with
    // workers of workers too. More workers than an orchestrator's pane holds
    // ask for `y` (U7).
    #[test]
    fn quitting_warns_first_that_workers_that_do_not_fit_close() {
        let panels = |n: u64| -> Vec<Panel> {
            (20..20 + n)
                .map(|pane| worker(pane, 1, &format!("w{pane}"), Some("running"), 1))
                .collect()
        };
        for lang in [Lang::En, Lang::Ja] {
            for (n, quit) in [
                (8, lang.pick("再度 q で終了", "q again quits")),
                (9, lang.pick("y で終了", "y quits")),
            ] {
                let mut board = Board::new(Some(1), None);
                board.lang = lang;
                board.update(panels(n), Instant::now());
                board.key(Key::Char('q'));
                let top = visible(&board.render(80, 20, Instant::now())[2]);
                let warning = lang.pick(
                    "オーケストレーターに収まらないワーカーは閉じ、残りは戻ります",
                    "workers the orchestrator can't fit close; others go back",
                );
                assert!(top.trim_start().starts_with(quit), "{top}");
                assert!(top.trim_end().ends_with(warning), "whole at 80: {top}");
                assert!(!top.contains('9') && !top.contains('8'), "no count: {top}");
            }
            let mut board = Board::new(Some(1), Some(8));
            board.lang = lang;
            let mut deep = nested();
            deep.extend((30..40).map(|pane| worker(pane, 2, "k", Some("running"), 1)));
            board.update(deep, Instant::now());
            board.key(Key::Char('q'));
            let top = visible(&board.render(80, 20, Instant::now())[2]);
            let end = lang.pick("収まらない分は閉じます", "can't fit close");
            assert!(top.trim_end().ends_with(end), "whole at 80: {top}");
            assert!(top.contains("12"), "{top}");
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
    // U6: which branch a worker is on was nowhere, nor its whole directory,
    // nor that closing it leaves its worktree and branch.
    #[test]
    fn the_branch_and_the_worktree_that_stays_are_said() {
        let t0 = Instant::now();
        let mut panels = sample();
        let path = "/Users/me/src/project/noa-wt/api/crates/noa-app/src/ctl";
        panels[0].cwd = path.to_string();
        panels[0].branch = Some("orch/api".to_string());
        let mut board = Board::new(Some(1), Some(8));
        board.update(panels, t0);
        board.select(2);
        let all = rendered(&board, 200, 30, t0);
        assert!(
            line_with(&all, "報告 4").contains("報告 4 · orch/api · /Users/me"),
            "{all}"
        );
        board.key(Key::Enter);
        for cols in [24, 40, 60, 80] {
            let lines = board.render(cols, 30, t0);
            let place: String = lines[2..]
                .iter()
                .map(|line| visible(line))
                .take_while(|line| !line.contains("画面"))
                .map(|line| line.replace(' ', ""))
                .collect();
            assert_eq!(place, format!("orch/api·{path}"), "{cols}");
            for line in &lines {
                assert_eq!(visible(line).width(), cols, "{cols}: {:?}", visible(line));
            }
        }
        board.key(Key::Char('x'));
        let top = visible(&board.render(200, 30, t0)[1]);
        assert!(
            top.contains("worktree とブランチ orch/api は残ります。"),
            "{top}"
        );
        // Only a worker in its label's worktree.
        assert!(!in_label_worktree("/src/noa-wt/ui", "api"));
        assert!(!in_label_worktree("/src/-wt/api", "api"));
        assert!(!in_label_worktree("/src/noa/api", "api"));
        assert!(in_label_worktree("/src/noa-wt/api", "api"));
    }

    // U11: help lines wrapped, a path broken where it is full.
    #[test]
    fn wrap_breaks_at_a_space_or_where_full() {
        assert_eq!(
            wrap("fold / open a subtree", 12),
            ["fold / open", "a subtree"]
        );
        assert_eq!(wrap("/a/b/c/d", 3), ["/a/", "b/c", "/d"]);
        assert_eq!(wrap("作業中のワーカー", 6), ["作業中", "のワー", "カー"]);
        assert_eq!(wrap("short", 10), ["short"]);
    }

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

    // U10: labels put into sentences read "It is still approval." and
    // "closed while failed 1".
    #[test]
    fn states_put_into_sentences_read_as_sentences() {
        let still = |status: Status| (status.still(Lang::Ja), status.still(Lang::En));
        assert_eq!(
            still(Status::Permission),
            (
                Some("まだ承認待ちです。"),
                Some("It still awaits approval. ")
            )
        );
        assert_eq!(
            still(Status::Input),
            (
                Some("まだ答えを待っています。"),
                Some("It still awaits an answer. ")
            )
        );
        assert_eq!(
            still(Status::Attention),
            (
                Some("未読の通知があります。"),
                Some("It has an unread notification. ")
            )
        );
        assert_eq!(
            still(Status::Quiet),
            (Some("まだ動いています。"), Some("It is still at work. "))
        );
        assert_eq!(still(Status::Finished), (None, None));
        assert_eq!(still(Status::Exited(Some(1))), (None, None));
        let closed = |status: Status| (status.closed_as(Lang::Ja), status.closed_as(Lang::En));
        assert_eq!(
            closed(Status::Exited(Some(1))),
            (
                "終了コード 1 で閉じた".to_string(),
                "closed after exiting with 1".to_string()
            )
        );
        assert_eq!(
            closed(Status::Permission),
            (
                "承認待ちのまま閉じた".to_string(),
                "closed awaiting approval".to_string()
            )
        );
        assert_eq!(
            closed(Status::Quiet),
            (
                "報告がないまま閉じた".to_string(),
                "closed before it reported".to_string()
            )
        );
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
        // U10: a state put into a sentence reads as one.
        let all = rendered(&board, 200, 30, t0);
        for label in [
            "approval",
            "question",
            "notified",
            "no report",
            "failed",
            "exited",
        ] {
            assert!(!all.contains(&format!("still {label}")), "{all}");
            assert!(!all.contains(&format!("while {label}")), "{all}");
        }
        board.key(Key::Esc);
        board.key(Key::Char('q'));
        check(&board, "quit");
        board.key(Key::Esc);
        board.key(Key::Enter);
        check(&board, "detail");
        board.offline = Some(t0);
        check(&board, "offline");
        assert!(!japanese(&board.title()), "{}", board.title());
        // A notice from an action, and from a key with nothing to do: a
        // number no live worker has.
        let unused = (1..10)
            .find(|n| !board.numbers.values().any(|v| v == n))
            .unwrap();
        board.key(Key::Char(char::from_digit(unused as u32, 10).unwrap()));
        assert!(!japanese(&board.message.clone().unwrap().text));
        let mut server = FakeServer {
            listings: vec![sample()],
            typed: Vec::new(),
        };
        let (notice, _) = perform(&mut server, &board, Action::Focus(2));
        assert!(!japanese(&notice.text), "{}", notice.text);
    }

    // U13: every tick wrote the whole screen, unchanged or not, outside a
    // synchronized update.
    #[test]
    fn a_frame_is_written_once_and_whole() {
        let mut frames = Frames::default();
        let frame = |text: &str| vec![text.to_string(), "two".to_string()];
        let first = frames.next(frame("one")).expect("drawn");
        assert_eq!(first, b"\x1b[?2026h\x1b[Hone\r\ntwo\x1b[?2026l");
        assert_eq!(frames.next(frame("one")), None);
        assert!(frames.next(frame("1")).is_some());
        // After a resize the same frame is drawn again, on a cleared screen.
        frames.resized();
        let again = frames.next(frame("1")).expect("drawn");
        assert!(again.starts_with(b"\x1b[?2026h\x1b[2J\x1b[H"), "{again:?}");
        assert_eq!(frames.next(frame("1")), None);
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

    // Review finding #9: each reconnect slept through up to 3 s of connect
    // retries while Noa was at its connection limit, freezing keys (`q`
    // included), redraws and the offline counter. It connects once.
    // PR #210 review: after a reconnect reached another Noa the board kept
    // running, and g / x / send went to that instance's pane of the same id.
    #[test]
    fn nothing_is_done_while_another_noa_answers() {
        let mut board = Board::new(Some(1), Some(8));
        assert!(held_elsewhere(&board).is_none());
        board.elsewhere = true;
        let notice = held_elsewhere(&board).expect("actions are held");
        assert!(notice.warn);
    }

    // Review finding: while another Noa answered, `s` still opened the send
    // line and Enter's text was dropped: neither sent nor kept as a draft
    // (`s` had taken the old draft out).
    #[test]
    fn a_send_held_while_another_noa_answers_keeps_its_text() {
        let t0 = Instant::now();
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), t0);
        board.select(2);
        board.elsewhere = true;
        board.key(Key::Char('s'));
        board.key(Key::Paste("look at the tests".to_string()));
        let action = board.key(Key::Enter).expect("Enter sends");
        assert!(hold_elsewhere(&mut board, &action));
        assert_eq!(
            board.drafts.get(&2).map(String::as_str),
            Some("look at the tests")
        );
        let message = board.message.clone().unwrap();
        assert!(
            message.warn && message.text.ends_with("下書きを残しました (s で再開)"),
            "{message:?}"
        );
        // Anything else is held without a draft.
        board.drafts.clear();
        assert!(hold_elsewhere(&mut board, &Action::Focus(2)));
        assert!(board.drafts.is_empty());
    }

    #[test]
    fn a_reconnect_connects_once_without_sleeping() {
        let (mut tries, mut slept) = (0, Vec::new());
        let result: Result<(), _> = super::super::connect_with(
            RECONNECT,
            || {
                tries += 1;
                Err(ClientError::TooManyConnections("1013".to_string()))
            },
            |pause| slept.push(pause),
        );
        assert!(matches!(result, Err(ClientError::TooManyConnections(_))));
        assert_eq!(tries, 1);
        assert!(slept.is_empty());
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

    // Audit finding (REL-10): a server at its connection limit closes the
    // connection (1013); the board opens it again like a lost one.
    #[test]
    fn a_full_server_is_a_lost_connection() {
        assert!(lost(&ClientError::TooManyConnections(
            "1013 too many connections".to_string()
        )));
        assert!(!lost(&ClientError::Timeout("noa.listPanels".to_string())));
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

    // D18: the board shows each worker's task (its parent's journal) and
    // its result file ($NOA_RESULT).

    fn insight_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("noa-board-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo").join(".noa")).unwrap();
        dir.canonicalize().unwrap()
    }

    /// Orchestrator 1 (surface `orch`) and its worker 2 (`api`) working in
    /// `<dir>/repo`, on a board whose journals are under `dir`.
    fn insight_board(dir: &std::path::Path) -> Board {
        let mut orchestrator = worker(1, 0, "orch", None, 0);
        orchestrator.parent_pane_id = None;
        orchestrator.agent = None;
        orchestrator.surface_id = Some("orch".to_string());
        let mut api = worker(2, 1, "api", Some("running"), 3);
        api.cwd = dir.join("repo").to_string_lossy().into_owned();
        let mut board = Board::new(Some(1), Some(8));
        board.insight.root = Some(dir.to_path_buf());
        board.update(vec![orchestrator, api], Instant::now());
        board
    }

    fn journal_at(dir: &std::path::Path) -> std::path::PathBuf {
        let path = noa_config::ctl_journal_path_in(dir, "orch");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    }

    fn write_journal(dir: &std::path::Path, events: &[serde_json::Value]) -> std::path::PathBuf {
        let path = journal_at(dir);
        let text: String = events.iter().map(|event| format!("{event}\n")).collect();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn result_at(dir: &std::path::Path) -> std::path::PathBuf {
        dir.join("repo").join(".noa").join("result-api.md")
    }

    fn spawn_and_send(dir: &std::path::Path, task: &str) -> std::path::PathBuf {
        let result = result_at(dir);
        write_journal(
            dir,
            &[
                serde_json::json!({"ev": "spawn", "ts": 1, "pane": 2, "label": "api",
                    "result": result.to_string_lossy()}),
                serde_json::json!({"ev": "send", "ts": 2, "pane": 2, "text": task,
                    "seqBefore": 0}),
            ],
        )
    }

    fn assert_widths(board: &Board, cols: usize, rows: usize, now: Instant) {
        for line in board.render(cols, rows, now) {
            assert_eq!(visible(&line).width(), cols, "{cols}×{rows}: {line:?}");
        }
    }

    #[test]
    fn a_retried_worker_shows_its_whole_task_without_the_preface() {
        let dir = insight_dir("retry");
        let result = result_at(&dir);
        let task = format!("Fix the login bug {}", "x".repeat(300));
        let sent = crate::ctl::retry::compose(
            &crate::ctl::retry::preface(7, "claude, tier light", Some("finished"), None),
            &task,
        );
        write_journal(
            &dir,
            &[
                serde_json::json!({"ev": "spawn", "ts": 1, "pane": 2, "label": "api",
                    "result": result.to_string_lossy(), "retryOf": "7"}),
                serde_json::json!({"ev": "send", "ts": 2, "pane": 2,
                    "text": journal::clip(&sent), "full": sent,
                    "bytes": sent.len(), "raw": false, "seqBefore": 0}),
            ],
        );
        let mut board = insight_board(&dir);
        board.refresh_files(Instant::now());
        assert_eq!(
            board.insight.task(2).cloned(),
            Some(Task::Sent {
                text: task,
                cut: false,
                retry_of: Some(7),
            })
        );
        board.lang = Lang::En;
        board.view = View::Detail(2);
        let all = rendered(&board, 80, 30, Instant::now());
        assert!(all.contains("retry of pane 7"), "{all}");
        assert!(all.contains("Fix the login bug"), "{all}");
        assert!(!all.contains("Retry: a previous worker"), "{all}");
        assert!(!all.contains("cut at the first"), "{all}");
        assert_widths(&board, 80, 30, Instant::now());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_detail_shows_the_task_and_the_result_file() {
        let dir = insight_dir("detail");
        spawn_and_send(&dir, "Fix the login bug in the api crate");
        let result = result_at(&dir);
        std::fs::write(&result, "## Done\nAll tests pass.\n").unwrap();
        let mut board = insight_board(&dir);
        let t0 = Instant::now();
        assert!(board.refresh_files(t0));
        board.key(Key::Enter);
        assert_eq!(board.view, View::Detail(2));
        let all = rendered(&board, 80, 30, t0);
        assert!(all.contains("タスク"), "{all}");
        assert!(all.contains("Fix the login bug in the api crate"), "{all}");
        assert!(
            all.contains("結果  24 バイト · 0 秒前に書かれました · r で表示"),
            "{all}"
        );
        // The path wraps where it is longer than the pane is wide (the temp
        // dir's length varies): join the wrapped lines before looking.
        let joined: String = all.lines().map(str::trim).collect();
        assert!(
            joined.contains(&result.to_string_lossy().to_string()),
            "{all}"
        );
        // The screen is still there below.
        assert!(all.contains("画面"), "{all}");
        for (cols, rows) in [(80, 30), (80, 20), (40, 16), (MIN_COLS, MIN_ROWS)] {
            assert_widths(&board, cols, rows, t0);
        }
        board.lang = Lang::En;
        let all = rendered(&board, 80, 30, t0 + Duration::from_secs(90));
        assert!(all.contains("Task"), "{all}");
        assert!(
            all.contains("Result  24 bytes · written 1m ago · r: show"),
            "{all}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_task_cut_by_the_journal_says_so_and_wraps_to_a_few_lines() {
        let dir = insight_dir("cut");
        // As long as the journal keeps (`journal::clip`).
        let long = "word ".repeat(40);
        assert_eq!(long.chars().count(), journal::TEXT_CHARS);
        spawn_and_send(&dir, &long);
        let mut board = insight_board(&dir);
        let t0 = Instant::now();
        board.refresh_files(t0);
        board.key(Key::Enter);
        let all = rendered(&board, 80, 40, t0);
        assert!(all.contains("先頭 200 文字で切れています"), "{all}");
        let task_lines = all
            .lines()
            .filter(|line| line.starts_with("   word"))
            .count();
        assert_eq!(task_lines, 3, "{all}");
        // A short pane keeps one line of it.
        let all = rendered(&board, 80, 20, t0);
        let task_lines = all
            .lines()
            .filter(|line| line.starts_with("   word"))
            .count();
        assert_eq!(task_lines, 1, "{all}");
        assert!(all.contains("…"), "{all}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_and_unreadable_files_are_said_briefly() {
        let dir = insight_dir("reasons");
        let mut board = insight_board(&dir);
        let t0 = Instant::now();
        board.key(Key::Enter);
        // No journal yet; the result file is found by its label, missing.
        board.refresh_files(t0);
        let all = rendered(&board, 80, 30, t0);
        assert!(all.contains("ジャーナルが見つかりません"), "{all}");
        assert!(all.contains("まだ作られていません"), "{all}");
        // Spawned, nothing sent, the result reserved but empty.
        let result = result_at(&dir);
        std::fs::write(&result, "").unwrap();
        write_journal(
            &dir,
            &[serde_json::json!({"ev": "spawn", "ts": 1, "pane": 2,
                "result": result.to_string_lossy()})],
        );
        board.refresh_files(t0 + Duration::from_secs(3));
        let all = rendered(&board, 80, 30, t0);
        assert!(all.contains("タスクはまだ送られていません"), "{all}");
        assert!(all.contains("空です(まだ書かれていません)"), "{all}");
        board.lang = Lang::En;
        let all = rendered(&board, 80, 30, t0);
        assert!(all.contains("no task sent"), "{all}");
        assert!(all.contains("empty (not written yet)"), "{all}");
        // A FIFO in its place is never opened (it would block the board).
        std::fs::remove_file(&result).unwrap();
        let c_path = std::ffi::CString::new(result.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: mkfifo reads the NUL-terminated path only.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        board.refresh_files(t0 + Duration::from_secs(6));
        board.key(Key::Char('r'));
        board.refresh_files(t0 + Duration::from_secs(6));
        let all = rendered(&board, 80, 30, t0);
        assert!(all.contains("not a regular file; not read"), "{all}");
        // A worker of a parent the board does not list has no journal.
        let mut lone = Board::new(None, None);
        lone.insight.root = Some(dir.clone());
        lone.update(vec![worker(5, 4, "x", Some("running"), 1)], t0);
        lone.refresh_files(t0);
        lone.key(Key::Enter);
        let all = rendered(&lone, 80, 30, t0);
        assert!(all.contains("ジャーナルがありません"), "{all}");
        // Panes whose files the board never looked at take no `r`.
        let mut fresh = Board::new(Some(1), Some(8));
        fresh.update(sample(), t0);
        fresh.key(Key::Enter);
        fresh.key(Key::Char('r'));
        assert!(!fresh.result_shown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Review finding: moving to another pane in the detail view (a digit,
    // `n`, a listing) kept the result shown and its scroll.
    #[test]
    fn another_pane_in_the_detail_view_opens_on_its_screen() {
        let dir = insight_dir("move");
        spawn_and_send(&dir, "write the report");
        let text: String = (1..=60).map(|n| format!("result line {n}\n")).collect();
        std::fs::write(result_at(&dir), text).unwrap();
        let mut board = insight_board(&dir);
        let t0 = Instant::now();
        board.refresh_files(t0);
        board.select(2);
        board.key(Key::Enter);
        board.key(Key::Char('r'));
        board.refresh_files(t0 + Duration::from_millis(100));
        rendered(&board, 80, 30, t0);
        board.key(Key::Char('j'));
        assert!(board.result_shown);
        assert_eq!(board.result_scroll, 1);

        board.select(1);
        assert_eq!(board.view, View::Detail(1));
        assert!(!board.result_shown);
        assert_eq!(board.result_scroll, 0);
        // Selecting the pane already open keeps its result.
        board.select(1);
        board.view = View::Detail(2);
        board.result_shown = true;
        board.select(2);
        assert!(board.result_shown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Review finding: after a result got shorter, `k` did not move until the
    // kept scroll came under the new end.
    #[test]
    fn k_scrolls_up_at_once_after_the_result_got_shorter() {
        let dir = insight_dir("shorter");
        spawn_and_send(&dir, "write the report");
        let text: String = (1..=60).map(|n| format!("result line {n}\n")).collect();
        std::fs::write(result_at(&dir), text).unwrap();
        let mut board = insight_board(&dir);
        let t0 = Instant::now();
        board.refresh_files(t0);
        board.select(2);
        board.key(Key::Enter);
        board.key(Key::Char('r'));
        board.refresh_files(t0 + Duration::from_millis(100));
        rendered(&board, 80, 30, t0);
        for _ in 0..100 {
            board.key(Key::Char('j'));
        }
        let long_reach = board.result_reach.get();

        let text: String = (1..=30).map(|n| format!("result line {n}\n")).collect();
        std::fs::write(result_at(&dir), text).unwrap();
        board.refresh_files(t0 + Duration::from_secs(3));
        rendered(&board, 80, 30, t0);
        let reach = board.result_reach.get();
        assert!(reach > 0 && reach < long_reach, "{reach} {long_reach}");
        board.key(Key::Char('k'));
        assert_eq!(board.result_scroll, reach - 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_short_frame_still_scrolls_the_result_to_its_last_line() {
        let dir = insight_dir("short");
        spawn_and_send(&dir, "write the report");
        let text: String = (1..=60).map(|n| format!("result line {n:02}\n")).collect();
        std::fs::write(result_at(&dir), text).unwrap();
        let mut board = insight_board(&dir);
        let t0 = Instant::now();
        board.refresh_files(t0);
        board.key(Key::Enter);
        board.key(Key::Char('r'));
        board.refresh_files(t0 + Duration::from_millis(100));
        let mut checked = 0;
        for rows in MIN_ROWS..=30 {
            board.result_scroll = 0;
            let first = rendered(&board, 80, rows, t0);
            if !first.contains("result line 01") {
                continue; // The box does not reach the frame at all.
            }
            for _ in 0..100 {
                board.key(Key::Char('j'));
            }
            let all = rendered(&board, 80, rows, t0);
            assert!(all.contains("result line 60"), "rows {rows}:\n{all}");
            checked += 1;
        }
        assert!(checked > 10, "{checked}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn r_shows_the_result_and_j_k_scroll_it() {
        let dir = insight_dir("toggle");
        spawn_and_send(&dir, "write the report");
        let text: String = (1..=60).map(|n| format!("result line {n}\n")).collect();
        std::fs::write(result_at(&dir), text).unwrap();
        let mut board = insight_board(&dir);
        board.set_screen(2, "screen text here\n");
        let t0 = Instant::now();
        board.refresh_files(t0);
        // `r` does nothing in the list.
        board.key(Key::Char('r'));
        assert!(!board.result_shown);
        board.key(Key::Enter);
        let all = rendered(&board, 80, 30, t0);
        assert!(all.contains("screen text here"), "{all}");
        assert!(all.lines().last().unwrap().contains("r 結果"), "{all}");

        board.key(Key::Char('r'));
        // Read only once shown, by the next refresh, before the frame.
        assert!(board.insight.shown(2).is_none());
        assert!(board.refresh_files(t0 + Duration::from_millis(100)));
        let all = rendered(&board, 80, 30, t0);
        assert!(all.contains("結果ファイル"), "{all}");
        assert!(all.contains("result line 1 "), "{all}");
        assert!(!all.contains("screen text here"), "{all}");
        assert!(
            all.lines().last().unwrap().contains("j/k スクロール"),
            "{all}"
        );
        for (cols, rows) in [(80, 30), (40, 16), (MIN_COLS, MIN_ROWS)] {
            assert_widths(&board, cols, rows, t0);
        }
        rendered(&board, 80, 30, t0);
        board.key(Key::Char('j'));
        board.key(Key::Down);
        let all = rendered(&board, 80, 30, t0);
        assert!(!all.contains("result line 2 "), "{all}");
        assert!(all.contains("result line 3 "), "{all}");
        // j/k scroll, not step to another worker.
        assert_eq!(board.view, View::Detail(2));
        board.key(Key::Char('k'));
        board.key(Key::Up);
        board.key(Key::Up);
        assert_eq!(board.result_scroll, 0);
        // No further than the end.
        for _ in 0..200 {
            board.key(Key::Char('j'));
        }
        let all = rendered(&board, 80, 30, t0);
        assert!(all.contains("result line 60"), "{all}");
        assert_eq!(board.result_scroll, board.result_reach.get());

        // A rewritten result is read again; an unchanged one is not.
        std::fs::write(result_at(&dir), "short now\n").unwrap();
        board.refresh_files(t0 + Duration::from_secs(3));
        let all = rendered(&board, 80, 30, t0);
        assert!(all.contains("short now"), "{all}");

        board.key(Key::Char('r'));
        let all = rendered(&board, 80, 30, t0);
        assert!(all.contains("screen text here"), "{all}");
        board.key(Key::Char('r'));
        board.key(Key::Esc);
        assert_eq!(board.view, View::List);
        assert!(!board.result_shown);
        board.refresh_files(t0 + Duration::from_secs(3));
        assert!(board.insight.shown(2).is_none());
        board.lang = Lang::En;
        board.key(Key::Enter);
        let footer = rendered(&board, 80, 30, t0);
        assert!(
            footer.lines().last().unwrap().contains("r result"),
            "{footer}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_preview_says_when_a_result_was_written() {
        let dir = insight_dir("preview");
        spawn_and_send(&dir, "task");
        let result = result_at(&dir);
        std::fs::write(&result, "").unwrap();
        let mut board = insight_board(&dir);
        let t0 = Instant::now();
        board.refresh_files(t0);
        assert!(!rendered(&board, 80, 24, t0).contains("結果あり"));
        std::fs::write(&result, "done\n").unwrap();
        // Looked at again only after a while.
        board.refresh_files(t0 + Duration::from_millis(500));
        assert!(!rendered(&board, 80, 24, t0).contains("結果あり"));
        board.refresh_files(t0 + Duration::from_secs(3));
        let all = rendered(&board, 80, 24, t0);
        let meta = line_with(&all, "結果あり");
        assert!(meta.contains("報告 3 · 結果あり"), "{meta}");
        board.lang = Lang::En;
        assert!(rendered(&board, 80, 24, t0).contains("report 3 · result written"));
        for cols in [120, 80, 60, 40, 30, MIN_COLS] {
            assert_widths(&board, cols, 24, t0);
            assert_widths(&board, cols, 10, t0);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unchanged_journal_is_not_read_again() {
        let dir = insight_dir("cache");
        let path = spawn_and_send(&dir, "first task");
        let mut board = insight_board(&dir);
        let t0 = Instant::now();
        board.refresh_files(t0);
        let task = |board: &Board| board.insight.task(2).cloned();
        let sent = |text: &str| {
            Some(Task::Sent {
                text: text.to_string(),
                cut: false,
                retry_of: None,
            })
        };
        assert_eq!(task(&board), sent("first task"));
        // Same length and time: taken as unchanged, so not read.
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("first task", "other task")).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        board.refresh_files(t0 + Duration::from_secs(3));
        assert_eq!(task(&board), sent("first task"));
        // A new line changes its length: read again.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({"ev": "send", "ts": 3, "pane": 2, "text": "second task"})
        )
        .unwrap();
        board.refresh_files(t0 + Duration::from_secs(4));
        // Not yet: files are looked at every two seconds.
        assert_eq!(task(&board), sent("first task"));
        board.refresh_files(t0 + Duration::from_secs(6));
        assert_eq!(task(&board), sent("second task"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_help_says_what_r_does_in_both_languages() {
        let mut board = Board::new(Some(1), Some(8));
        board.update(sample(), Instant::now());
        board.key(Key::Char('?'));
        let all = rendered(&board, 100, 40, Instant::now());
        assert!(
            all.contains("詳細で画面と結果ファイルの中身を切り替える"),
            "{all}"
        );
        board.lang = Lang::En;
        let all = rendered(&board, 100, 40, Instant::now());
        assert!(
            all.contains("in details: switch between the screen and the result file"),
            "{all}"
        );
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
