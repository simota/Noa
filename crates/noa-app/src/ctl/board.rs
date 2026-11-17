//! `Noa +ctl board`: a terminal UI that shows an orchestrator's workers and
//! lets a person act on them (agent-orchestration UI phase). It runs in an
//! ordinary pane and reads the same server `+ctl` does, so it needs no new
//! pane kind: the model, key handling and drawing below are pure, and only
//! [`run`] touches the terminal and the connection.

use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

use noa_ipc::{Client, Panel, TextSource};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{EXIT_OK, Failure, SUBMIT_DELAY};

/// How often the pane list is read again.
const LIST_EVERY: Duration = Duration::from_millis(500);
/// How often the workers' screens are read again.
const SCREEN_EVERY: Duration = Duration::from_secs(1);
/// Reports kept per worker.
const HISTORY_CAP: usize = 50;
/// Screen lines kept per worker.
const SCREEN_CAP: usize = 200;
/// Screen lines a card shows.
const CARD_PREVIEW: usize = 2;
/// How long a lone ESC waits for the rest of a sequence before it counts
/// as the Esc key.
const ESC_WAIT: Duration = Duration::from_millis(50);
/// The smallest pane the board draws in.
const MIN_COLS: usize = 24;
const MIN_ROWS: usize = 6;

// ---------------------------------------------------------------------------
// Model

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

/// The sections of the list, in the order they are shown.
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

    pub(crate) fn group(self) -> Group {
        match self {
            Self::Permission | Self::Input | Self::Error | Self::Attention => Group::Needs,
            Self::Running | Self::Quiet => Group::Working,
            Self::Finished => Group::Done,
            Self::Exited(_) => Group::Exited,
        }
    }

    fn label(self) -> String {
        match self {
            Self::Permission => "承認待ち".to_string(),
            Self::Input => "入力待ち".to_string(),
            Self::Error => "エラー".to_string(),
            Self::Attention => "通知あり".to_string(),
            Self::Running => "作業中".to_string(),
            Self::Finished => "完了".to_string(),
            Self::Exited(Some(code)) => format!("終了 {code}"),
            Self::Exited(None) => "終了".to_string(),
            Self::Quiet => "報告なし".to_string(),
        }
    }

    fn color(self) -> Rgb {
        match self {
            Self::Permission | Self::Input | Self::Attention => ORANGE,
            Self::Error => RED,
            Self::Running => BLUE,
            Self::Finished => GREEN,
            Self::Exited(_) | Self::Quiet => GREY,
        }
    }
}

impl Group {
    const ALL: [Self; 4] = [Self::Needs, Self::Working, Self::Done, Self::Exited];

    fn title(self) -> &'static str {
        match self {
            Self::Needs => "要対応",
            Self::Working => "作業中",
            Self::Done => "完了",
            Self::Exited => "終了",
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
    Paste(String),
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
    history: HashMap<u64, Vec<Report>>,
    selected: Option<u64>,
    pub(crate) view: View,
    pub(crate) mode: Mode,
    /// A one-line result of the last action.
    pub(crate) message: Option<String>,
}

impl Board {
    pub(crate) fn new(of: Option<u64>, me: Option<u64>) -> Self {
        Self {
            of,
            me,
            panels: Vec::new(),
            started: false,
            screens: HashMap::new(),
            history: HashMap::new(),
            selected: None,
            view: View::List,
            mode: Mode::Normal,
            message: None,
        }
    }

    /// Take a fresh pane listing. A report count that moved is kept in the
    /// worker's history; the first listing records the state found as is.
    pub(crate) fn update(&mut self, panels: Vec<Panel>, now: Instant) {
        let first = !self.started;
        self.started = true;
        self.panels = panels;
        let workers: Vec<(u64, Option<u64>, Status, String)> = self
            .workers()
            .iter()
            .map(|panel| {
                let agent = panel.agent.as_ref();
                (
                    panel.pane_id.0,
                    agent.map(|agent| agent.seq),
                    Status::of(panel),
                    agent
                        .and_then(|agent| agent.detail.clone())
                        .unwrap_or_default(),
                )
            })
            .collect();
        for (pane, seq, status, detail) in workers {
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
        let live: Vec<u64> = self.workers().iter().map(|panel| panel.pane_id.0).collect();
        self.history.retain(|pane, _| live.contains(pane));
        self.screens.retain(|pane, _| live.contains(pane));
        if self.selected.is_none_or(|pane| !live.contains(&pane)) {
            self.selected = live.first().copied();
        }
        if let View::Detail(pane) = self.view
            && !live.contains(&pane)
        {
            self.view = View::List;
        }
        if let Mode::Send(pane, _) | Mode::ConfirmClose(pane) = self.mode
            && !live.contains(&pane)
        {
            self.mode = Mode::Normal;
        }
    }

    /// The last lines of a worker's screen (trailing blank lines dropped).
    pub(crate) fn set_screen(&mut self, pane: u64, text: &str) {
        let mut lines: Vec<String> = text
            .lines()
            .map(|line| line.trim_end().to_string())
            .collect();
        while lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        let skip = lines.len().saturating_sub(SCREEN_CAP);
        self.screens.insert(pane, lines.split_off(skip));
    }

    /// The workers shown, most urgent section first.
    pub(crate) fn workers(&self) -> Vec<&Panel> {
        let mut workers: Vec<&Panel> = self
            .panels
            .iter()
            .filter(|panel| Some(panel.pane_id.0) != self.me)
            .filter(|panel| match (self.of, panel.parent_pane_id) {
                (Some(of), Some(parent)) => parent.0 == of,
                (None, Some(_)) => true,
                (_, None) => false,
            })
            .collect();
        workers.sort_by_key(|panel| (Status::of(panel).group(), panel.pane_id.0));
        workers
    }

    /// The panes whose screens the current view shows.
    pub(crate) fn wanted_screens(&self) -> Vec<u64> {
        match self.view {
            View::List => self.workers().iter().map(|panel| panel.pane_id.0).collect(),
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
        let workers: Vec<u64> = self.workers().iter().map(|panel| panel.pane_id.0).collect();
        if workers.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|pane| workers.iter().position(|&p| p == pane))
            .unwrap_or(0) as isize;
        let next = (at + step).clamp(0, workers.len() as isize - 1) as usize;
        self.selected = Some(workers[next]);
        if let View::Detail(_) = self.view {
            self.view = View::Detail(workers[next]);
        }
    }

    /// Whether `pane` waits for an approval, which is answered in its own
    /// pane, never from the board (spec D7).
    fn awaits_approval(&self, pane: u64) -> bool {
        self.panel(pane)
            .is_some_and(|panel| Status::of(panel) == Status::Permission)
    }

    fn refuse_send(&mut self, pane: u64) {
        self.mode = Mode::Normal;
        self.message = Some(approval_message(&self.name(pane)));
    }

    pub(crate) fn key(&mut self, key: Key) -> Option<Action> {
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
                    Key::Enter => {
                        self.mode = Mode::Normal;
                        if self.awaits_approval(pane) {
                            self.refuse_send(pane);
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
            Mode::Normal => {
                self.message = None;
                match key {
                    Key::Char('q') | Key::CtrlC => return Some(Action::Quit),
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
                            if self.awaits_approval(pane) {
                                self.refuse_send(pane);
                            } else {
                                self.mode = Mode::Send(pane, String::new());
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
fn approval_message(name: &str) -> String {
    format!("{name} は承認待ちです。g でペインへ移動して答えてください")
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
}

impl KeyReader {
    /// Whether the last read ended in a lone ESC: the Esc key, or the start
    /// of a sequence (a paste's start marker included) still on its way.
    pub(crate) fn waiting_on_esc(&self) -> bool {
        self.paste.is_none() && self.pending == [0x1b]
    }

    /// Nothing followed a lone ESC in time: it was the Esc key.
    pub(crate) fn flush(&mut self) -> Vec<Key> {
        if self.waiting_on_esc() {
            self.pending.clear();
            vec![Key::Esc]
        } else {
            Vec::new()
        }
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
                keys.push(Key::Esc);
                i += 1;
                continue;
            }
            let len = match first {
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => 1,
            };
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

fn ago(at: Option<Instant>, now: Instant) -> String {
    let Some(at) = at else {
        return "開始時".to_string();
    };
    let secs = now.saturating_duration_since(at).as_secs();
    match secs {
        0..60 => format!("{secs} 秒前"),
        60..3600 => format!("{} 分前", secs / 60),
        _ => format!("{} 時間前", secs / 3600),
    }
}

impl Board {
    /// The screen as `rows` lines of exactly `cols` columns each.
    pub(crate) fn render(&self, cols: usize, rows: usize, now: Instant) -> Vec<String> {
        // Drawing wider than the pane would wrap every line; say so instead.
        if cols < MIN_COLS || rows < MIN_ROWS {
            let note = format!("{MIN_COLS}×{MIN_ROWS} 以上に広げてください");
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
        let mut lines = match self.view {
            View::List => self.render_list(cols, rows),
            View::Detail(pane) => self.render_detail(pane, cols, rows, now),
        };
        lines.truncate(rows - 2);
        while lines.len() < rows - 2 {
            lines.push(Line::default().finish(cols));
        }
        let mut status = Line::default();
        if let Some(message) = &self.message {
            status.fg(DIM, &fit(&clean(message), cols));
        }
        lines.push(status.finish(cols));
        lines.push(self.footer(cols));
        lines
    }

    fn header(&self, cols: usize) -> Vec<String> {
        let workers = self.workers();
        let mut title = Line::default();
        title.plain(" ").bold("オーケストレーション").plain("  ");
        let scope = match self.of {
            Some(of) => self
                .panel(of)
                .map(|panel| {
                    let kind = worker_kind(panel);
                    if kind.is_empty() {
                        format!("親 pane {of}")
                    } else {
                        format!("親 pane {of} · {kind}")
                    }
                })
                .unwrap_or_else(|| format!("親 pane {of}")),
            None => "すべてのワーカー".to_string(),
        };
        let count = format!("ワーカー {} ", workers.len());
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
            chips
                .fg(group.color(), "●")
                .plain(&format!(" {} {n}   ", group.title()));
        }
        vec![
            title.finish(cols),
            chips.finish(cols),
            Line::default().finish(cols),
        ]
    }

    fn render_list(&self, cols: usize, rows: usize) -> Vec<String> {
        let mut lines = self.header(cols);
        let workers = self.workers();
        if workers.is_empty() {
            let mut empty = Line::default();
            empty.plain(" ").fg(
                DIM,
                &fit(
                    "ワーカーはまだいません。`+ctl spawn` で起動するとここに出ます。",
                    cols - 1,
                ),
            );
            lines.push(empty.finish(cols));
            return lines;
        }
        let mut body: Vec<String> = Vec::new();
        let mut selected_span = (0, 0);
        let mut last_group = None;
        for panel in workers {
            let status = Status::of(panel);
            if last_group != Some(status.group()) {
                last_group = Some(status.group());
                let mut title = Line::default();
                title
                    .plain(" ")
                    .fg(status.group().color(), status.group().title());
                body.push(title.finish(cols));
            }
            let start = body.len();
            body.extend(self.card(panel, cols));
            if Some(panel.pane_id.0) == self.selected {
                selected_span = (start, body.len());
            }
        }
        let room = rows.saturating_sub(lines.len() + 2);
        // Scroll so the selected card is in view; when it is taller than the
        // room, keep its first line (name, state, pane) rather than its end.
        let offset = selected_span.1.saturating_sub(room).min(selected_span.0);
        lines.extend(body.into_iter().skip(offset));
        lines
    }

    fn card(&self, panel: &Panel, cols: usize) -> Vec<String> {
        let pane = panel.pane_id.0;
        let status = Status::of(panel);
        let selected = Some(pane) == self.selected;
        let border = if selected { status.color() } else { BORDER };
        let inner = cols - 4;

        let mut top = Line::default();
        top.fg(border, " ┌ ").fg(status.color(), "●").plain(" ");
        let right = match (self.of, panel.parent_pane_id) {
            (None, Some(parent)) => format!(" pane {pane} ← {} ", parent.0),
            _ => format!(" pane {pane} "),
        };
        let name = worker_name(panel);
        let kind = worker_kind(panel);
        let state = status.label();
        let budget = cols.saturating_sub(top.width + right.width() + 2);
        let name = fit(&name, budget);
        top.bold(&name);
        let budget = budget.saturating_sub(name.width());
        if !kind.is_empty() && budget > kind.width() + 2 {
            top.plain("  ").fg(DIM, &kind);
        }
        let budget = cols.saturating_sub(top.width + right.width() + 2);
        if budget > state.width() + 2 {
            top.plain("  ").fg(status.color(), &state);
        }
        let fill = cols.saturating_sub(top.width + right.width() + 2);
        top.fg(border, &format!(" {}", "─".repeat(fill.saturating_sub(1))))
            .fg(DIM, &right)
            .fg(border, "┐");

        let detail = panel
            .agent
            .as_ref()
            .and_then(|agent| agent.detail.clone())
            .filter(|detail| !detail.is_empty())
            .or_else(|| {
                panel
                    .exit
                    .as_ref()
                    .map(|_| "プロセスが終了しました".to_string())
            })
            .unwrap_or_default();
        let mut out = vec![top.finish(cols)];
        let mut row = |text: &str, color: Rgb| {
            let mut line = Line::default();
            line.fg(border, " │ ");
            let text = fit(&clean(text), inner.saturating_sub(1));
            line.fg(color, &text);
            line.plain(&" ".repeat(inner.saturating_sub(1 + text.width())));
            line.fg(border, " │");
            out.push(line.finish(cols));
        };
        row(&detail, Rgb(201, 204, 214));
        let screen = self.screens.get(&pane);
        let preview: Vec<&String> = screen
            .map(|lines| {
                lines
                    .iter()
                    .filter(|line| !line.trim().is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let skip = preview.len().saturating_sub(CARD_PREVIEW);
        for index in 0..CARD_PREVIEW {
            let text = preview.get(skip + index).map(|s| s.as_str()).unwrap_or("");
            row(text, DIM);
        }

        let mut bottom = Line::default();
        bottom.fg(border, " └ ");
        if let Some(agent) = &panel.agent {
            bottom.fg(DIM, &format!("ターン {} ", agent.seq));
        }
        let fill = cols.saturating_sub(bottom.width + 2);
        bottom.fg(border, &"─".repeat(fill)).fg(border, "┘");
        out.push(bottom.finish(cols));
        out
    }

    fn render_detail(&self, pane: u64, cols: usize, rows: usize, now: Instant) -> Vec<String> {
        let mut lines = Vec::new();
        let Some(panel) = self.panel(pane) else {
            return lines;
        };
        let status = Status::of(panel);
        let mut head = Line::default();
        head.plain(" ")
            .fg(DIM, "← 一覧 (Esc)  /  ")
            .bold(&fit(&worker_name(panel), cols / 3));
        let kind = worker_kind(panel);
        if !kind.is_empty() {
            head.plain("  ").fg(DIM, &kind);
        }
        head.plain("  ").fg(status.color(), &status.label());
        let right = match &panel.agent {
            Some(agent) => format!("pane {pane} · ターン {} ", agent.seq),
            None => format!("pane {pane} "),
        };
        if head.width + right.width() < cols {
            let pad = cols - head.width - right.width();
            head.plain(&" ".repeat(pad)).fg(DIM, &right);
        }
        lines.push(head.finish(cols));
        lines.push(Line::default().finish(cols));

        let history: &[Report] = self.history.get(&pane).map(Vec::as_slice).unwrap_or(&[]);
        let history_rows = history.len().clamp(1, 6);
        // Title + box borders + history title + rows + gap, and the two
        // status/footer rows below.
        let screen_rows = rows
            .saturating_sub(lines.len() + 4 + 2 + history_rows + 2)
            .max(3);

        let mut title = Line::default();
        title
            .plain(" ")
            .bold("画面")
            .fg(DIM, "  ライブ · 読み取り専用");
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
            .bold("報告の履歴")
            .fg(DIM, "  このボードを開いてから");
        lines.push(title.finish(cols));
        if history.is_empty() {
            let mut line = Line::default();
            line.plain("   ").fg(DIM, "報告はまだありません");
            lines.push(line.finish(cols));
        }
        for report in history.iter().take(history_rows) {
            let mut line = Line::default();
            let when = ago(report.at, now);
            line.plain("   ")
                .fg(DIM, &format!("ターン {:<4}", report.seq))
                .plain(" ")
                .fg(
                    report.status.color(),
                    &format!("{:<8}", report.status.label()),
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

    fn footer(&self, cols: usize) -> String {
        let mut line = Line::default();
        match &self.mode {
            Mode::Send(pane, text) => {
                line.plain(" ")
                    .fg(BLUE, &format!("{} に送る › ", self.name(*pane)));
                let room = cols.saturating_sub(line.width + 2);
                // Show the end of a long message, where the typing happens.
                let shown: String = {
                    let mut tail = String::new();
                    let mut used = 0;
                    // Show line breaks without printing them.
                    let text: String = text
                        .chars()
                        .map(|ch| if ch == '\n' { '⏎' } else { ch })
                        .filter(|ch| !ch.is_control())
                        .collect();
                    for ch in text.chars().rev() {
                        let w = ch.width().unwrap_or(0);
                        if used + w > room {
                            break;
                        }
                        tail.insert(0, ch);
                        used += w;
                    }
                    tail
                };
                line.plain(&shown).plain("█");
            }
            Mode::ConfirmClose(pane) => {
                line.plain(" ")
                    .fg(
                        RED,
                        &format!("{} を閉じますか? 出力は消えます。", self.name(*pane)),
                    )
                    .fg(DIM, "  y 閉じる / ほかのキーで取り消し");
            }
            Mode::Normal => {
                let help = match self.view {
                    View::List => " j/k 選択   Enter 詳細   g 移動   s 送る   x 閉じる   q 終了",
                    View::Detail(_) => {
                        " g 移動して答える   s 送る   x 閉じる   j/k 前後   Esc 一覧   q 終了"
                    }
                };
                line.fg(DIM, help);
            }
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
        // Bracketed paste, so pasted text is never read as keys.
        let _ = out.write_all(b"\x1b[?1049h\x1b[?25l\x1b[?2004h\x1b[2J");
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
        let _ = out.write_all(b"\x1b[0m\x1b[?2004l\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
        if let Some(saved) = self.saved {
            // SAFETY: restores the termios read in `enter`.
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, &saved);
            }
        }
    }
}

/// Run the board until `q`. The terminal is restored before returning.
pub(crate) fn run(client: &mut Client, of: Option<u64>, me: Option<u64>) -> Result<i32, Failure> {
    let mut board = Board::new(of, me);
    board.update(client.list_panels()?, Instant::now());

    let (keys_tx, keys_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        loop {
            // SAFETY: reads into a stack buffer of the given length.
            let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 || keys_tx.send(buf[..n as usize].to_vec()).is_err() {
                break;
            }
        }
    });

    let _screen = Screen::enter();
    let mut listed_at = Instant::now();
    let mut screens_at: Option<Instant> = None;
    let mut size = (0, 0);
    let mut dirty = true;
    let mut reader = KeyReader::default();
    loop {
        let now = Instant::now();
        if now.duration_since(listed_at) >= LIST_EVERY {
            board.update(client.list_panels()?, now);
            listed_at = now;
            dirty = true;
        }
        if screens_at.is_none_or(|at| now.duration_since(at) >= SCREEN_EVERY) {
            for pane in board.wanted_screens() {
                // A pane that closed meanwhile is dropped at the next listing.
                if let Ok(text) = client.get_text(pane, TextSource::Screen, None) {
                    board.set_screen(pane, &text.text);
                }
            }
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
            dirty = false;
        }
        // A lone ESC waits briefly for the rest of a sequence.
        let wait = if reader.waiting_on_esc() {
            ESC_WAIT
        } else {
            Duration::from_millis(100)
        };
        let keys = match keys_rx.recv_timeout(wait) {
            Ok(bytes) => reader.feed(&bytes),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => reader.flush(),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(EXIT_OK),
        };
        for key in keys {
            dirty = true;
            let Some(action) = board.key(key) else {
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
fn perform(server: &mut impl Server, board: &Board, action: Action) -> String {
    // The board's listing can be up to `LIST_EVERY` old: look again right
    // before typing, so a worker that started waiting for an approval since
    // is never answered from here (spec D7).
    if let Action::Send(pane, _) = &action {
        match server.panels() {
            Ok(panels) => {
                if panels.iter().any(|panel| {
                    panel.pane_id.0 == *pane && Status::of(panel) == Status::Permission
                }) {
                    return approval_message(&board.name(*pane));
                }
            }
            Err(err) => return format!("できませんでした: {err}"),
        }
    }
    let result = match &action {
        Action::Focus(pane) => server.focus(*pane),
        Action::Send(pane, text) => server.type_text(*pane, text, true).and_then(|()| {
            std::thread::sleep(SUBMIT_DELAY);
            server.type_text(*pane, "\r", false)
        }),
        Action::Close(pane) => server.close(*pane),
        Action::Quit => Ok(()),
    };
    let name = |pane: &u64| board.name(*pane);
    match (action, result) {
        (Action::Focus(pane), Ok(())) => format!("{} へ移動しました", name(&pane)),
        (Action::Send(pane, _), Ok(())) => format!("{} に送りました", name(&pane)),
        (Action::Close(pane), Ok(())) => format!("{} を閉じました", name(&pane)),
        (_, Err(err)) => format!("できませんでした: {err}"),
        (Action::Quit, Ok(())) => String::new(),
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
        exited.exit = Some(PanelExit { code: Some(101) });
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
            "older line\nAllow command? cargo test\n› 1. Yes   2. No\n",
        );
        let text: Vec<String> = board
            .render(80, 40, Instant::now())
            .iter()
            .map(|l| visible(l))
            .collect();
        let all = text.join("\n");

        assert!(all.contains("要対応"));
        assert!(all.contains("ui") && all.contains("承認待ち") && all.contains("pane 3"));
        assert!(all.contains("ui detail"));
        assert!(all.contains("› 1. Yes   2. No"));
        assert!(!all.contains("older line"));
        assert!(all.contains("終了 101"));
        assert!(!all.contains("board"));
    }

    /// A server that answers with fixed panels and records what was typed.
    struct FakeServer {
        panels: Vec<Panel>,
        typed: Vec<(u64, String)>,
    }

    impl Server for FakeServer {
        fn panels(&mut self) -> Result<Vec<Panel>, noa_ipc::ClientError> {
            Ok(self.panels.clone())
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
            panels: now_waiting,
            typed: Vec::new(),
        };

        let message = perform(&mut server, &board, Action::Send(2, "y".to_string()));

        assert_eq!(server.typed, Vec::new());
        assert!(message.contains("承認待ち"), "{message}");
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
        let footer = board.render(60, 20, Instant::now()).pop().unwrap();
        assert!(!footer.contains('\n'), "{footer:?}");
        assert!(visible(&footer).contains("hello⏎xy"));
        assert_eq!(visible(&footer).width(), 60);
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
        assert!(board.message.as_deref().is_some_and(|m| m.contains("g")));

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
                all.contains("api") && all.contains("pane 2"),
                "{rows} rows: {all}"
            );
        }
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
        assert!(all.contains("ターン 5"));
        assert!(all.contains("開始時"));
    }
}
