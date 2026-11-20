//! `Noa +ctl`: the shell-callable orchestration client
//! (`docs/specs/agent-orchestration.md` D3/D4/D6). An orchestrator running
//! in one pane spawns worker panes, sends them input, waits for their agent
//! state, reads their output, and closes them — each as one command that
//! prints one JSON object per line.
//!
//! Argument parsing and the wait condition are pure functions so they are
//! unit-testable without a server; [`run`] is the thin I/O shell around them.

mod board;
mod worktree;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use noa_ipc::{
    Client, ClientError, EventKind, Notification, Panel, ScopeSet, SpawnConfig, SpawnParams,
    SplitPlacement, TextSource, WireId,
};

/// Exit status: the request succeeded (or the wait condition held).
pub(crate) const EXIT_OK: i32 = 0;
/// Exit status: bad arguments, an unknown pane, or any other failure.
pub(crate) const EXIT_ERROR: i32 = 1;
/// Exit status: `wait` timed out.
pub(crate) const EXIT_TIMEOUT: i32 = 2;
/// Exit status: the server is disabled, unreachable, or lacks a scope.
pub(crate) const EXIT_UNAVAILABLE: i32 = 3;

/// Default cap on live workers one orchestrator may hold (D4, D12): as many
/// as its board holds.
const DEFAULT_MAX_CHILDREN: usize = MOST_CHILDREN;
/// The most `--max-children` accepts: every worker is a float stowed on the
/// orchestrator's board, which holds this many (D12).
const MOST_CHILDREN: usize = crate::floating::MAX_STOWED_PER_BOARD;
/// Pause between a pasted prompt and the Enter that submits it, so a TUI
/// that debounces paste sees them as two events.
const SUBMIT_DELAY: Duration = Duration::from_millis(150);
/// How often `wait` re-lists panes while idle: `noa.stateChanged` has no
/// removal event, so a closed pane is only noticed by listing.
const WAIT_RELIST_INTERVAL: Duration = Duration::from_secs(1);
/// `OSC 777;noa-agent` detail bound (the terminal applies the same one).
const MAX_REPORT_DETAIL_CHARS: usize = 160;

const SURFACE_ID_ENV: &str = "NOA_SURFACE_ID";
/// How long a command that needs its own pane waits for it to be listed:
/// 30 listings 100 ms apart.
const OWN_WAIT_LOOKS: usize = 30;
const OWN_WAIT_PAUSE: Duration = Duration::from_millis(100);

pub(crate) const USAGE: &str = "\
Usage: Noa +ctl <command> [options]

Commands:
  self                          Print this pane's panel.
  ls [--children]               Print every panel (or only this pane's workers).
  spawn [options] [-- COMMAND...]
                                Start a worker in a floating pane stowed on
                                this pane's board (opened by the first one),
                                where its row shows it until someone opens
                                it; prints {\"paneId\":...}. With --no-board,
                                or no board, it floats over this pane,
                                collapsed to its title strip. A worker's
                                workers go on its orchestrator's board (one
                                level deep; no --no-board).
      --cwd DIR  --env KEY=VALUE  --label TEXT  --input TEXT
      --no-wait-after  --focus  --no-board
      --max-children N (default and most 20: the board holds 20 workers;
                         with --no-board this pane holds 8 floats)
      --worktree        Run the worker in its own git worktree
                        <repo>-wt/LABEL on a new branch orch/LABEL (from
                        HEAD; needs --label, refuses --cwd). Prints
                        \"worktree\" and \"branch\" too.
      --tier light|standard|deep
                        Pick the model tier of a claude, codex or agy
                        COMMAND (refused when it already picks a model
                        or effort). Prints \"tier\" too.
  send PANE [--enter] [--raw] [--replace] [--wait [wait options]] [--answer]
       [--any-pane] [--] TEXT|-  Type TEXT (or stdin) into PANE; prints the
                                report count to pass to `wait --after`. Refused
                                while PANE waits for an approval, unless --answer.
                                --replace first clears what the input line holds
                                (Ctrl-E, then Ctrl-U per line, up to 32 lines).
  wait PANE...|--children [--after SEQ | --after PANE=SEQ ...] [--for LIST]
       [--all] [--timeout SECS] [--tail N] [--stall SECS]
                                Block until a worker stops. LIST is a comma list
                                of finished,permission,input,error,exit,attention
                                (default: all of them), or stalled (needs
                                --stall). --stall SECS also ends the wait for a
                                pane whose screen text has not changed for SECS
                                seconds (reason \"stalled\", with \"quietSecs\"),
                                for an agent that does not report. --tail adds each open
                                pane's last N output lines as \"tail\". A timeout
                                prints each pending pane's report count as
                                `baseline`; pass it back as --after PANE=SEQ.
  read PANE [--scrollback] [--max-bytes N]
                                Print PANE's screen (or retained output) text;
                                a cut keeps the end and says so on stderr.
  close PANE...|--children [--any-pane] [--prune]
                                Close worker panes. One that is already gone
                                prints \"gone\":true and is not an error.
                                --prune also removes a closed worker's
                                `spawn --worktree` worktree and its branch
                                (git worktree remove, git branch -d; never
                                forced, so unsaved work stays and is named
                                under \"worktree\":{\"kept\":...}).
  report STATE [[--] DETAIL]    Write an agent-state report (running, permission,
                                input, finished, error, clear) to this terminal.
  board [--of PANE]             Show an orchestrator's workers in this pane
                                (default: this pane's own orchestrator, else
                                every worker). Keys are listed at the bottom.
  board --open [--split right|down | --tab]
                                Open a board on this pane's workers next to it
                                (an open one is printed with \"existing\":true).

Use `--` before a TEXT or DETAIL that starts with `--`. A pane that exits
and stays open ends a wait whatever --for lists, as a closed pane does; one
that had exited before the wait began only ends it when no other pane is left.
`send` and `close` only accept this pane's own workers unless --any-pane is
given. Exit status: 0 ok, 1 error, 2 wait timed out, 3 server unavailable.
";

/// What an argument error prints after its message, instead of [`USAGE`].
const USAGE_HINT: &str = "run \"Noa +ctl --help\" for usage";

// ---------------------------------------------------------------------------
// Arguments

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    SelfPanel,
    Ls {
        children: bool,
    },
    Spawn(SpawnArgs),
    Send(SendArgs),
    Wait(WaitArgs),
    Read {
        pane: u64,
        scrollback: bool,
        max_bytes: Option<usize>,
    },
    Close {
        panes: PaneSet,
        any_pane: bool,
        prune: bool,
    },
    Report {
        state: String,
        detail: String,
    },
    Board {
        of: Option<u64>,
        /// `--open`: start a board pane next to this one instead.
        open: Option<Placement>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaneSet {
    Ids(Vec<u64>),
    Children,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    Auto,
    Right,
    Down,
    Tab,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpawnArgs {
    /// Open this orchestrator's board when none is open (`--no-board`: not).
    pub(crate) board: bool,
    pub(crate) cwd: Option<String>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) label: Option<String>,
    pub(crate) input: Option<String>,
    pub(crate) wait_after: bool,
    pub(crate) focus: bool,
    pub(crate) max_children: usize,
    /// `--worktree`: run in `<repo>-wt/<label>` on branch `orch/<label>`.
    pub(crate) worktree: bool,
    /// `--tier`: the model tier flags to put on the agent command.
    pub(crate) tier: Option<worktree::Tier>,
    pub(crate) command: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TextArg {
    Literal(String),
    Stdin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SendArgs {
    pub(crate) pane: u64,
    pub(crate) text: TextArg,
    pub(crate) enter: bool,
    pub(crate) raw: bool,
    pub(crate) any_pane: bool,
    /// `--answer`: type into a pane that waits for an approval.
    pub(crate) answer: bool,
    /// `--replace`: clear the input line before typing.
    pub(crate) replace: bool,
    /// `--wait`: block like `wait PANE --after <seq before sending>`.
    pub(crate) wait: Option<WaitOptions>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WaitArgs {
    pub(crate) panes: PaneSet,
    /// `--after`: `(pane, report count)` baselines.
    pub(crate) after: Vec<(u64, u64)>,
    pub(crate) options: WaitOptions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WaitOptions {
    pub(crate) reasons: Vec<Reason>,
    pub(crate) all: bool,
    pub(crate) timeout: Option<Duration>,
    /// `--tail N`: add each still-open pane's last N output lines.
    pub(crate) tail: Option<usize>,
    /// `--stall SECS`: also end the wait for a pane whose screen text has not
    /// changed for this long (`Reason::Stalled`).
    pub(crate) stall: Option<Duration>,
}

impl Default for WaitOptions {
    fn default() -> Self {
        Self {
            reasons: Reason::ALL.to_vec(),
            all: false,
            timeout: None,
            tail: None,
            stall: None,
        }
    }
}

/// Why a wait ended for one pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    Finished,
    Permission,
    Input,
    Error,
    Exit,
    Attention,
    /// Only with `--stall`: the screen text stopped changing.
    Stalled,
}

impl Reason {
    const ALL: [Reason; 6] = [
        Reason::Finished,
        Reason::Permission,
        Reason::Input,
        Reason::Error,
        Reason::Exit,
        Reason::Attention,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Permission => "permission",
            Self::Input => "input",
            Self::Error => "error",
            Self::Exit => "exit",
            Self::Attention => "attention",
            Self::Stalled => "stalled",
        }
    }

    /// `stalled` is parsed but is not in `ALL`: it is not a default reason.
    fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .chain([Self::Stalled])
            .find(|reason| reason.name() == name)
    }

    /// The reason an agent report in `state` stands for, if any.
    fn from_agent_state(state: &str) -> Option<Self> {
        match state {
            "finished" => Some(Self::Finished),
            "permission" => Some(Self::Permission),
            "input" => Some(Self::Input),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

const REPORT_STATES: [&str; 6] = [
    "running",
    "permission",
    "input",
    "finished",
    "error",
    "clear",
];

pub(crate) fn parse_args(args: &[String]) -> Result<Command, String> {
    let (name, rest) = args.split_first().ok_or("missing command")?;
    let mut rest = Args::new(rest);
    let command = match name.as_str() {
        "self" => Command::SelfPanel,
        "ls" => Command::Ls {
            children: rest.flag("--children"),
        },
        "spawn" => Command::Spawn(parse_spawn(&mut rest)?),
        "send" => Command::Send(parse_send(&mut rest)?),
        "wait" => {
            let mut afters = Vec::new();
            while let Some(value) = rest.value("--after")? {
                afters.push(value);
            }
            let options = parse_wait_options(&mut rest)?;
            let panes = pane_set(&mut rest)?;
            let after = parse_afters(&afters, &panes)?;
            Command::Wait(WaitArgs {
                panes,
                after,
                options,
            })
        }
        "read" => {
            let scrollback = rest.flag("--scrollback");
            let max_bytes = rest
                .value("--max-bytes")?
                .map(|v| parse_u64("--max-bytes", &v).map(|n| n as usize))
                .transpose()?;
            let pane = single_pane(&mut rest)?;
            Command::Read {
                pane,
                scrollback,
                max_bytes,
            }
        }
        "board" => {
            let opening = rest.flag("--open");
            let placement = parse_placement(&mut rest)?;
            let of = rest
                .value("--of")?
                .map(|v| parse_u64("--of", &v))
                .transpose()?;
            if !opening && placement != Placement::Auto {
                return Err("--split and --tab go with --open".to_string());
            }
            Command::Board {
                of,
                open: opening.then_some(placement),
            }
        }
        "close" => {
            let any_pane = rest.flag("--any-pane");
            let prune = rest.flag("--prune");
            Command::Close {
                panes: pane_set(&mut rest)?,
                any_pane,
                prune,
            }
        }
        "report" => {
            let tail = rest.take_after_double_dash();
            let mut positional = rest.positionals()?;
            positional.extend(tail);
            let (state, detail) = match positional.as_slice() {
                [state] => (state.clone(), String::new()),
                [state, detail] => (state.clone(), detail.clone()),
                _ => return Err("report takes STATE [DETAIL]".to_string()),
            };
            if !REPORT_STATES.contains(&state.as_str()) {
                return Err(format!("unknown state: {state:?}"));
            }
            if state == "clear" && !detail.is_empty() {
                return Err("clear takes no detail".to_string());
            }
            return Ok(Command::Report { state, detail });
        }
        other => return Err(format!("unknown command: {other:?}")),
    };
    rest.finish()?;
    Ok(command)
}

/// `--after SEQ` (the one PANE named) or `--after PANE=SEQ`, repeatable.
fn parse_afters(values: &[String], panes: &PaneSet) -> Result<Vec<(u64, u64)>, String> {
    let mut after = Vec::new();
    for value in values {
        let (pane, seq) = match (value.split_once('='), panes) {
            (Some((pane, seq)), _) => (parse_u64("--after PANE", pane)?, seq),
            (None, PaneSet::Ids(ids)) if ids.len() == 1 && values.len() == 1 => (ids[0], &**value),
            (None, _) => {
                return Err(
                    "--after SEQ takes exactly one PANE; use --after PANE=SEQ for several"
                        .to_string(),
                );
            }
        };
        after.push((pane, parse_u64("--after", seq)?));
    }
    Ok(after)
}

/// `--tab` or `--split auto|right|down` (default `auto`).
fn parse_placement(rest: &mut Args) -> Result<Placement, String> {
    let tab = rest.flag("--tab");
    let split = rest.value("--split")?;
    Ok(match (tab, split.as_deref()) {
        (true, Some(_)) => return Err("--tab and --split are exclusive".to_string()),
        (true, None) => Placement::Tab,
        (false, None | Some("auto")) => Placement::Auto,
        (false, Some("right")) => Placement::Right,
        (false, Some("down")) => Placement::Down,
        (false, Some(other)) => return Err(format!("unknown --split value: {other:?}")),
    })
}

fn parse_spawn(rest: &mut Args) -> Result<SpawnArgs, String> {
    let command = rest.take_after_double_dash();
    if rest.flag("--tab") || rest.value("--split")?.is_some() {
        return Err(
            "workers always open as floating panes over this pane; --split and --tab are gone"
                .to_string(),
        );
    }
    // Accepted from before floats were the only placement; changes nothing.
    rest.flag("--float");
    let board = !rest.flag("--no-board");
    let mut env = Vec::new();
    while let Some(assignment) = rest.value("--env")? {
        let (key, value) = assignment
            .split_once('=')
            .filter(|(key, _)| !key.is_empty())
            .ok_or_else(|| format!("--env expects KEY=VALUE, got {assignment:?}"))?;
        env.push((key.to_string(), value.to_string()));
    }
    let max_children = match rest.value("--max-children")? {
        Some(value) => parse_u64("--max-children", &value)?
            .try_into()
            .ok()
            .filter(|max| *max <= MOST_CHILDREN)
            .ok_or_else(|| {
                format!(
                    "--max-children {value:?} is above {MOST_CHILDREN}: every worker is a \
                     float on this pane's board, which holds {MOST_CHILDREN}"
                )
            })?,
        None => DEFAULT_MAX_CHILDREN,
    };
    let cwd = rest.value("--cwd")?;
    let label = rest.value("--label")?;
    let worktree = rest.flag("--worktree");
    let tier = rest
        .value("--tier")?
        .map(|name| worktree::Tier::parse(&name))
        .transpose()?;
    if worktree {
        if cwd.is_some() {
            return Err("--worktree and --cwd conflict: the worktree is the cwd".to_string());
        }
        let label = label
            .as_deref()
            .ok_or("--worktree needs --label: it names the worktree and its branch")?;
        worktree::check_label(label)?;
    }
    if tier.is_some() {
        worktree::check_tier_command(&command)?;
    }
    Ok(SpawnArgs {
        board,
        cwd,
        env,
        label,
        input: rest.value("--input")?,
        wait_after: !rest.flag("--no-wait-after"),
        focus: rest.flag("--focus"),
        max_children,
        worktree,
        tier,
        command,
    })
}

fn parse_send(rest: &mut Args) -> Result<SendArgs, String> {
    // Everything after `--` is positional, so the text may start with `--`
    // or equal a flag name.
    let tail = rest.take_after_double_dash();
    let enter = rest.flag("--enter");
    let raw = rest.flag("--raw");
    let any_pane = rest.flag("--any-pane");
    let answer = rest.flag("--answer");
    let replace = rest.flag("--replace");
    if replace && answer {
        // The keys that clear an input line would land in the prompt.
        return Err("--replace does not go with --answer: a prompt has no input line".to_string());
    }
    let wait = if rest.flag("--wait") {
        Some(parse_wait_options(rest)?)
    } else {
        None
    };
    let mut positional = rest.positionals()?;
    positional.extend(tail);
    let [pane, text] = positional.as_slice() else {
        return Err("send takes PANE and TEXT (or - for stdin)".to_string());
    };
    Ok(SendArgs {
        pane: parse_u64("PANE", pane)?,
        text: if text == "-" {
            TextArg::Stdin
        } else {
            TextArg::Literal(text.clone())
        },
        enter,
        raw,
        any_pane,
        answer,
        replace,
        wait,
    })
}

/// The most input lines `send --replace` clears.
const REPLACE_MAX_LINES: usize = 32;

/// Ctrl-U keys per write of `send --replace`. Claude Code takes a longer
/// burst of input as a paste and leaves the draft in place (65 bytes in one
/// write did, 33 did not), so the keys go in short writes.
const REPLACE_CHUNK: usize = 16;

/// The pause between two writes of `send --replace`.
const REPLACE_PAUSE: Duration = Duration::from_millis(50);

/// What `send --replace` types before the text, one write each: Ctrl-E (end
/// of line), then two Ctrl-U per line. In Claude Code, Codex and Antigravity
/// CLI, Ctrl-U clears the current line only, and at the start of a line one
/// more joins it to the line above, so a draft of N lines needs up to 2N
/// (checked on Claude Code 2.1.329, Codex 0.170.0, agy 1.5.13, including
/// Claude Code's collapsed "[Pasted text]"; an empty input line is left as it
/// is).
pub(crate) fn replace_keys() -> Vec<String> {
    let mut writes = vec!["\x05".to_string()];
    let mut left = REPLACE_MAX_LINES * 2;
    while left > 0 {
        let n = left.min(REPLACE_CHUNK);
        writes.push("\x15".repeat(n));
        left -= n;
    }
    writes
}

fn parse_wait_options(rest: &mut Args) -> Result<WaitOptions, String> {
    let mut options = WaitOptions {
        all: rest.flag("--all"),
        ..WaitOptions::default()
    };
    if let Some(list) = rest.value("--for")? {
        options.reasons = list
            .split(',')
            .map(|name| {
                Reason::parse(name.trim()).ok_or_else(|| format!("unknown reason: {name:?}"))
            })
            .collect::<Result<_, _>>()?;
    }
    if let Some(value) = rest.value("--timeout")? {
        let seconds: f64 = value
            .parse()
            .ok()
            .filter(|seconds: &f64| seconds.is_finite() && *seconds >= 0.0)
            .ok_or_else(|| format!("--timeout expects seconds, got {value:?}"))?;
        options.timeout = Some(
            Duration::try_from_secs_f64(seconds)
                .map_err(|_| format!("--timeout is out of range: {value:?}"))?,
        );
    }
    if let Some(value) = rest.value("--tail")? {
        options.tail = Some(parse_u64("--tail", &value)? as usize);
    }
    if let Some(value) = rest.value("--stall")? {
        let seconds: f64 = value
            .parse()
            .ok()
            .filter(|seconds: &f64| seconds.is_finite() && *seconds > 0.0)
            .ok_or_else(|| format!("--stall expects seconds above 0, got {value:?}"))?;
        options.stall = Some(
            Duration::try_from_secs_f64(seconds)
                .map_err(|_| format!("--stall is out of range: {value:?}"))?,
        );
        if !options.reasons.contains(&Reason::Stalled) {
            options.reasons.push(Reason::Stalled);
        }
    } else if options.reasons.contains(&Reason::Stalled) {
        return Err("--for stalled needs --stall SECS".to_string());
    }
    Ok(options)
}

fn pane_set(rest: &mut Args) -> Result<PaneSet, String> {
    let children = rest.flag("--children");
    let ids = rest
        .positionals()?
        .iter()
        .map(|pane| parse_u64("PANE", pane))
        .collect::<Result<Vec<_>, _>>()?;
    match (children, ids.is_empty()) {
        (true, true) => Ok(PaneSet::Children),
        (false, false) => Ok(PaneSet::Ids(ids)),
        (true, false) => Err("--children and PANE ids are exclusive".to_string()),
        (false, true) => Err("missing PANE (or --children)".to_string()),
    }
}

fn single_pane(rest: &mut Args) -> Result<u64, String> {
    match rest.positionals()?.as_slice() {
        [pane] => parse_u64("PANE", pane),
        _ => Err("expected exactly one PANE".to_string()),
    }
}

fn parse_u64(what: &str, value: &str) -> Result<u64, String> {
    value.parse().map_err(|_| {
        // `jq -r .paneId` prints `null` for a spawn that failed, and an
        // unset variable expands to nothing.
        let hint = if what.contains("PANE") && matches!(value, "" | "null") {
            " (did the previous spawn fail?)"
        } else {
            ""
        };
        format!("{what} expects a number, got {value:?}{hint}")
    })
}

/// A small consuming argument cursor: flags and `--key VALUE` options are
/// taken out by name, whatever is left must be positional.
struct Args {
    items: Vec<String>,
}

impl Args {
    fn new(items: &[String]) -> Self {
        Self {
            items: items.to_vec(),
        }
    }

    fn flag(&mut self, name: &str) -> bool {
        match self.items.iter().position(|item| item == name) {
            Some(index) => {
                self.items.remove(index);
                true
            }
            None => false,
        }
    }

    /// `--key VALUE` or `--key=VALUE`; repeated calls take later occurrences.
    fn value(&mut self, name: &str) -> Result<Option<String>, String> {
        let prefix = format!("{name}=");
        let Some(index) = self
            .items
            .iter()
            .position(|item| item == name || item.starts_with(&prefix))
        else {
            return Ok(None);
        };
        let item = self.items.remove(index);
        if let Some(value) = item.strip_prefix(&prefix) {
            return Ok(Some(value.to_string()));
        }
        if index < self.items.len() {
            Ok(Some(self.items.remove(index)))
        } else {
            Err(format!("{name} needs a value"))
        }
    }

    fn take_after_double_dash(&mut self) -> Vec<String> {
        match self.items.iter().position(|item| item == "--") {
            Some(index) => {
                let tail = self.items.split_off(index + 1);
                self.items.pop();
                tail
            }
            None => Vec::new(),
        }
    }

    /// Everything left; an unknown `--option` is an error, but a lone `-`
    /// (stdin) is positional.
    fn positionals(&mut self) -> Result<Vec<String>, String> {
        if let Some(unknown) = self
            .items
            .iter()
            .find(|item| item.starts_with("--") && item.len() > 2)
        {
            return Err(format!("unknown option: {unknown:?}"));
        }
        Ok(std::mem::take(&mut self.items))
    }

    fn finish(self) -> Result<(), String> {
        match self.items.first() {
            None => Ok(()),
            Some(item) if item.starts_with("--") => Err(format!("unknown option: {item:?}")),
            Some(item) => Err(format!("unexpected argument: {item:?}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Wait condition

/// One pane a wait watches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Watch {
    pub(crate) pane: u64,
    /// Agent reports at or below this count predate the wait.
    pub(crate) baseline_seq: u64,
    /// `attention` only counts once it has been seen off during the wait, so
    /// a notification left over from an earlier turn does not end it.
    pub(crate) attention_armed: bool,
    /// The pane had already exited (and was held open) when the wait began.
    pub(crate) exited_before: bool,
}

impl Watch {
    pub(crate) fn new(pane: u64, panel: Option<&Panel>, after: Option<u64>) -> Self {
        Self {
            pane,
            baseline_seq: after
                .or_else(|| panel.and_then(|panel| panel.agent.as_ref().map(|agent| agent.seq)))
                .unwrap_or(0),
            attention_armed: !panel.is_some_and(|panel| panel.attention),
            exited_before: panel.is_some_and(|panel| panel.exit.is_some()),
        }
    }
}

/// What ended a wait for one pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) reason: Reason,
    /// The pane is gone (closed, or its child exited without being held).
    pub(crate) closed: bool,
}

/// Whether `panel` (`None`: no longer listed) ends the wait on `watch`.
/// Updates the attention arming as a side effect.
pub(crate) fn evaluate(
    watch: &mut Watch,
    panel: Option<&Panel>,
    reasons: &[Reason],
) -> Option<Outcome> {
    let Some(panel) = panel else {
        // A vanished pane can never satisfy anything else; always report it.
        return Some(Outcome {
            reason: Reason::Exit,
            closed: true,
        });
    };
    // A held-open exited pane will never report again either, so it ends
    // the wait like a vanished pane does, whatever `--for` asked for.
    if panel.exit.is_some() {
        return Some(Outcome {
            reason: Reason::Exit,
            closed: false,
        });
    }
    if let Some(agent) = &panel.agent
        && agent.seq > watch.baseline_seq
        && let Some(reason) = agent.state.as_deref().and_then(Reason::from_agent_state)
        && reasons.contains(&reason)
    {
        return Some(Outcome {
            reason,
            closed: false,
        });
    }
    if !panel.attention {
        watch.attention_armed = true;
    } else if watch.attention_armed && reasons.contains(&Reason::Attention) {
        return Some(Outcome {
            reason: Reason::Attention,
            closed: false,
        });
    }
    None
}

fn outcome_json(
    pane: u64,
    outcome: &Outcome,
    panel: Option<&Panel>,
    quiet: Option<Duration>,
) -> serde_json::Value {
    let agent = panel.and_then(|panel| panel.agent.as_ref());
    let mut value = serde_json::json!({
        "paneId": WireId(pane),
        "reason": outcome.reason.name(),
    });
    let object = value.as_object_mut().expect("object literal");
    if outcome.closed {
        object.insert("closed".into(), true.into());
    }
    if let Some(agent) = agent {
        object.insert("seq".into(), agent.seq.into());
        if let Some(state) = &agent.state {
            object.insert("state".into(), state.clone().into());
        }
        if let Some(detail) = &agent.detail {
            object.insert("detail".into(), detail.clone().into());
        }
    }
    if let Some(code) = panel
        .and_then(|panel| panel.exit.as_ref())
        .and_then(|exit| exit.code)
    {
        object.insert("exitCode".into(), code.into());
    }
    if let Some(quiet) = quiet {
        object.insert("quietSecs".into(), quiet.as_secs().into());
    }
    value
}

// ---------------------------------------------------------------------------
// Report

/// The `OSC 777;noa-agent` sequence for a report, with control characters
/// removed from `detail` and its length bounded like the terminal does.
pub(crate) fn report_sequence(state: &str, detail: &str) -> String {
    let detail: String = detail
        .chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_REPORT_DETAIL_CHARS)
        .collect();
    format!("\x1b]777;noa-agent;{state};{detail}\x1b\\")
}

/// Write a report to the controlling terminal. A process without one (a
/// detached hook) reports nothing and still succeeds, so a hook never fails
/// the agent that runs it.
fn write_report(state: &str, detail: &str) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let tty = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open("/dev/tty");
        if let Ok(mut tty) = tty {
            let _ = tty.write_all(report_sequence(state, detail).as_bytes());
        }
    }
    #[cfg(not(unix))]
    let _ = (state, detail);
    EXIT_OK
}

// ---------------------------------------------------------------------------
// Running

/// Only the command position asks for help: a `--help` later on belongs to
/// the worker's command (`spawn -- claude --help`) or to the text being sent.
pub(crate) fn wants_help(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("-h" | "--help" | "help")
    )
}

/// Run `Noa +ctl` with the arguments after `+ctl`; returns the exit status.
pub(crate) fn run(args: &[String]) -> i32 {
    if args.is_empty() || wants_help(args) {
        print!("{USAGE}");
        return if args.is_empty() { EXIT_ERROR } else { EXIT_OK };
    }
    let command = match parse_args(args) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("Noa +ctl: {message}\n{USAGE_HINT}");
            return EXIT_ERROR;
        }
    };
    if let Command::Report { state, detail } = &command {
        return write_report(state, detail);
    }
    // A spawn that cannot start the worker it describes fails before it
    // reaches Noa, which would otherwise fall back to another directory or
    // report only that the float could not open.
    if let Command::Spawn(spawn) = &command {
        let cwd = std::env::current_dir().ok();
        let env = worker_env(
            spawn.env.clone(),
            !spawn.command.is_empty(),
            std::env::vars(),
        );
        if let Err(message) = check_spawn(spawn, cwd, env.get("PATH").map(String::as_str)) {
            eprintln!("Noa +ctl: {message}");
            return EXIT_ERROR;
        }
    }
    match Session::connect().and_then(|mut session| session.execute(command)) {
        Ok(code) => code,
        Err(failure) => {
            eprintln!("Noa +ctl: {}", failure.message);
            failure.code
        }
    }
}

#[derive(Debug)]
pub(crate) struct Failure {
    code: i32,
    message: String,
}

impl Failure {
    fn error(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_ERROR,
            message: message.into(),
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_UNAVAILABLE,
            message: message.into(),
        }
    }
}

/// The server's JSON-RPC code for a request outside the client's scopes.
const RPC_SCOPE_DENIED: i64 = -32003;
/// The server's JSON-RPC code for a pane id it does not know.
const RPC_UNKNOWN_PANE: i64 = -32002;
/// The server's JSON-RPC code for a pane that takes no more requests: gone,
/// or its process exited.
const RPC_PANE_CLOSED: i64 = -32004;

const SCOPES_HINT: &str = "enable it in Noa's config with `server-enable = true` and `server-scopes = read,control,input`";

impl From<ClientError> for Failure {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::Rpc {
                code: RPC_SCOPE_DENIED,
                message,
            } => Failure::unavailable(format!("{message}; {SCOPES_HINT}")),
            ClientError::Rpc {
                code: RPC_UNKNOWN_PANE,
                ..
            } => Failure::error("unknown pane"),
            // The server went away mid-command (stopped or restarted).
            ClientError::Closed(_) | ClientError::Transport(_) => {
                Failure::unavailable(format!("lost the connection to Noa: {error}"))
            }
            other => Failure::error(other.to_string()),
        }
    }
}

struct Session {
    client: Client,
    /// This process's own panel, when it runs inside a Noa pane.
    own: Option<u64>,
    /// The control port this session reached, for diagnostics.
    port: u16,
    /// Whether a missing own pane was already waited for.
    waited_for_own: bool,
    /// Whether the last listing searched for this pane named no surface ids.
    lacks_surface_ids: bool,
}

/// A worker's environment: `--env` first, then what the orchestrator has that
/// the GUI app's own environment may lack. Its locale (`LANG`, `LC_*`): a
/// Noa started from the Dock has none, and without it a worker's scripts
/// cut multibyte text into bytes. And for a command, which spawns without a
/// shell, its `PATH`.
pub(crate) fn worker_env(
    given: Vec<(String, String)>,
    command: bool,
    orchestrator: impl Iterator<Item = (String, String)>,
) -> std::collections::BTreeMap<String, String> {
    let mut env: std::collections::BTreeMap<String, String> = given.into_iter().collect();
    for (key, value) in orchestrator {
        let wanted = key == "LANG" || key.starts_with("LC_") || (command && key == "PATH");
        if wanted && !env.contains_key(&key) {
            env.insert(key, value);
        }
    }
    env
}

/// Why connecting to `url` failed, with the likely cause when the socket
/// error names one: a shell sandbox (Claude Code's, for one) refuses the
/// loopback connect with `EPERM`, which looks like an unreachable Noa.
pub(crate) fn connect_failure_message(url: &str, error: &ClientError) -> String {
    let address = url.trim_start_matches("ws://").trim_end_matches('/');
    // `std::io::Error`'s text, which the transport error carries.
    let os_error = |code: i32| match error {
        ClientError::Transport(text) => text.contains(&format!("(os error {code})")),
        _ => false,
    };
    let cause = if os_error(libc::EPERM) {
        format!(
            "; this shell's sandbox likely blocks loopback connections to {address}: run \
             `Noa +ctl` outside the sandbox (or allow that address)"
        )
    } else if os_error(libc::ECONNREFUSED) {
        format!(
            "; Noa's control server is not listening on {address} (is Noa running, with \
             `server-enable = true`?)"
        )
    } else {
        String::new()
    };
    format!("cannot reach Noa at {url}: {error}{cause}")
}

/// Check what a spawn asks for before any request: `--cwd` must be an
/// existing directory (Noa would silently use another), and a command named
/// without a slash must be on `path`, the `PATH` the worker gets (`None`:
/// the worker gets Noa's own, which this process cannot see).
pub(crate) fn check_spawn(
    spawn: &SpawnArgs,
    current: Option<std::path::PathBuf>,
    path: Option<&str>,
) -> Result<(), String> {
    let cwd = spawn_cwd(spawn.cwd.clone(), current);
    if let Some(given) = &spawn.cwd {
        let resolved = cwd.as_deref().unwrap_or(given);
        if !std::path::Path::new(resolved).is_dir() {
            let resolved = if resolved == given {
                String::new()
            } else {
                format!(" (resolved to {resolved:?})")
            };
            return Err(format!("--cwd {given:?}{resolved} is not a directory"));
        }
    }
    if let (Some(program), Some(path)) = (spawn.command.first(), path)
        && !program.contains('/')
        && !on_path(program, path, cwd.as_deref().map(std::path::Path::new))
    {
        return Err(format!("command not found on PATH: {program:?}"));
    }
    Ok(())
}

/// Whether `program` names an executable file in a `PATH` entry; a relative
/// entry (an empty one is `.`) is taken from the worker's `cwd`.
fn on_path(program: &str, path: &str, cwd: Option<&std::path::Path>) -> bool {
    std::env::split_paths(path).any(|dir| {
        let dir = match cwd {
            Some(cwd) if dir.is_relative() => cwd.join(dir),
            _ => dir,
        };
        is_executable(&dir.join(program))
    })
}

fn is_executable(file: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(file) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    meta.is_file()
}

/// Whether a spawn from this pane looks for its board first, opening it
/// when none is (D9, D10): an orchestrator's does unless `--no-board`. A
/// worker's does not: the server puts a worker's workers on the board the
/// worker is stowed on (D13), so it has no board of its own, and
/// `--no-board` is refused there.
pub(crate) fn spawn_opens_board(is_worker: bool, board: bool) -> Result<bool, String> {
    match (is_worker, board) {
        (true, false) => Err(
            "--no-board does not apply here: this pane is a worker, and a \
             worker's workers always go on its orchestrator's board"
                .to_string(),
        ),
        (true, true) => Ok(false),
        (false, board) => Ok(board),
    }
}

/// Before a spawn, the pane its worker goes over (D9, D10): `own`'s board,
/// opened when none is, so the worker is stowed there. A board problem is a
/// warning to print, never the spawn's failure: the worker then floats over
/// `own`, as with `--no-board`.
pub(crate) fn board_before_spawn(
    list: impl FnOnce() -> Result<Vec<Panel>, Failure>,
    own: u64,
    open: impl FnOnce() -> Result<u64, Failure>,
) -> (u64, Option<String>) {
    let opened = match list() {
        Ok(panels) => match board_pane(&panels, own) {
            Some(board) => return (board, None),
            None => {
                open().map_err(|failure| format!("could not open the board: {}", failure.message))
            }
        },
        Err(failure) => Err(format!(
            "could not check for the board: {}",
            failure.message
        )),
    };
    match opened {
        Ok(board) => (board, None),
        Err(warning) => (own, Some(warning)),
    }
}

/// The label of `own`'s board pane, which a later spawn looks for.
pub(crate) fn board_label(own: u64) -> String {
    format!("board:{own}")
}

/// `own`'s board pane, when one is open.
pub(crate) fn board_pane(panels: &[Panel], own: u64) -> Option<u64> {
    let label = board_label(own);
    panels
        .iter()
        .find(|panel| panel.label.as_deref() == Some(label.as_str()))
        .map(|panel| panel.pane_id.0)
}

/// This process's panel among `panels`, by its `NOA_SURFACE_ID`.
pub(crate) fn own_pane(panels: &[Panel], surface_id: &str) -> Option<u64> {
    panels
        .iter()
        .find(|panel| panel.surface_id.as_deref() == Some(surface_id))
        .map(|panel| panel.pane_id.0)
}

/// Whether a listing names no surface ids at all: an older Noa, or a server
/// whose listed panes are all remote. Only worth saying when a command needs
/// its own pane; `ls` and `read` work regardless.
pub(crate) fn lacks_surface_ids(panels: &[Panel]) -> bool {
    !panels.is_empty() && panels.iter().all(|panel| panel.surface_id.is_none())
}

/// This process's pane, looked up again while it is not listed yet: a script
/// started with Noa can run before its pane's first listing. `looks` bounds
/// how many listings are taken, `pause` runs between them. Also returns
/// whether the last listing lacked surface ids.
pub(crate) fn wait_for_own(
    mut list: impl FnMut() -> Result<Vec<Panel>, Failure>,
    surface_id: &str,
    looks: usize,
    mut pause: impl FnMut(),
) -> Result<(Option<u64>, bool), Failure> {
    let mut lacks = false;
    for look in 1..=looks.max(1) {
        let panels = list()?;
        if let Some(own) = own_pane(&panels, surface_id) {
            return Ok((Some(own), false));
        }
        lacks = lacks_surface_ids(&panels);
        if look < looks {
            pause();
        }
    }
    Ok((None, lacks))
}

/// Why a command that needs this process's own pane cannot run.
/// `lacks_ids`: the server's listing named no surface ids.
pub(crate) fn not_own_message(surface_id: Option<&str>, port: u16, lacks_ids: bool) -> String {
    match surface_id {
        None => {
            "this process is not running inside a Noa pane (NOA_SURFACE_ID is unset)".to_string()
        }
        Some(_) if lacks_ids => format!(
            "the Noa answering on port {port} does not report surface ids, so it is \
             probably not the Noa this pane runs in: another Noa (an older one, or a \
             second instance) holds the port. {}",
            port_owner_hint(port)
        ),
        Some(surface_id) => format!(
            "this pane (NOA_SURFACE_ID={surface_id}) is not among the panes of the Noa \
             answering on port {port}. If another Noa holds that port, this one could \
             not listen. {} Floating panes that are not workers, and the quick and \
             scratch terminals, cannot orchestrate.",
            port_owner_hint(port)
        ),
    }
}

fn port_owner_hint(port: u16) -> String {
    format!(
        "See which process listens with `lsof -nP -iTCP:{port} -sTCP:LISTEN`; a Noa that \
         could not listen shows \"Bind failed\" under Settings > Server Status."
    )
}

/// The control URL for the configured bind address (an unspecified bind
/// address is reached over loopback).
pub(crate) fn control_url(bind: &str, port: u16) -> String {
    let ip: std::net::IpAddr = bind
        .parse()
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    // An unspecified bind is reached over the loopback of its own family: a
    // v6-only `::` socket refuses IPv4 (like the server's own wake-up).
    let ip = match ip {
        std::net::IpAddr::V4(v4) if v4.is_unspecified() => {
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        }
        std::net::IpAddr::V6(v6) if v6.is_unspecified() => {
            std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
        }
        ip => ip,
    };
    format!("ws://{}/", std::net::SocketAddr::new(ip, port))
}

impl Session {
    fn connect() -> Result<Self, Failure> {
        let (config, _diagnostics) =
            noa_config::load_startup_config(noa_config::ConfigOverrides::default())
                .map_err(|err| Failure::error(format!("cannot load config: {err:#}")))?;
        if !config.server_enable {
            return Err(Failure::unavailable(format!(
                "the Noa control server is disabled; {SCOPES_HINT}"
            )));
        }
        let token = match config.server_token.filter(|token| !token.trim().is_empty()) {
            Some(token) => token,
            None => {
                let path = noa_config::server_token_path()
                    .ok_or_else(|| Failure::unavailable("cannot resolve the token path"))?;
                std::fs::read_to_string(&path)
                    .map(|token| token.trim().to_string())
                    .ok()
                    .filter(|token| !token.is_empty())
                    .ok_or_else(|| {
                        Failure::unavailable(format!(
                            "no server token at {} (is Noa running with the server enabled?)",
                            path.display()
                        ))
                    })?
            }
        };
        let url = control_url(&config.server_bind, config.server_port);
        let client = Client::connect(
            &url,
            &token,
            ScopeSet::from_strings(["read", "control", "input"]),
        )
        .map_err(|err| match err {
            ClientError::Rpc { .. } => Failure::unavailable(err.to_string()),
            other => Failure::unavailable(connect_failure_message(&url, &other)),
        })?;
        let mut session = Self {
            client,
            own: None,
            port: config.server_port,
            waited_for_own: false,
            lacks_surface_ids: false,
        };
        session.own = session.find_own()?;
        Ok(session)
    }

    fn find_own(&mut self) -> Result<Option<u64>, Failure> {
        let Ok(surface_id) = std::env::var(SURFACE_ID_ENV) else {
            return Ok(None);
        };
        let panels = self.client.list_panels()?;
        self.lacks_surface_ids = lacks_surface_ids(&panels);
        Ok(own_pane(&panels, &surface_id))
    }

    /// This process's pane. When the first look missed it, wait up to
    /// [`OWN_WAIT_LOOKS`] listings for it: a script started with Noa can run
    /// before its pane is first listed. Only commands that need the pane wait.
    fn own_now(&mut self) -> Result<Option<u64>, Failure> {
        if self.own.is_none() && !self.waited_for_own {
            self.waited_for_own = true;
            if let Ok(surface_id) = std::env::var(SURFACE_ID_ENV) {
                let client = &mut self.client;
                (self.own, self.lacks_surface_ids) = wait_for_own(
                    || Ok(client.list_panels()?),
                    &surface_id,
                    OWN_WAIT_LOOKS,
                    || std::thread::sleep(OWN_WAIT_PAUSE),
                )?;
            }
        }
        Ok(self.own)
    }

    fn require_own(&mut self) -> Result<u64, Failure> {
        let own = self.own_now()?;
        own.ok_or_else(|| {
            let message = not_own_message(
                std::env::var(SURFACE_ID_ENV).ok().as_deref(),
                self.port,
                self.lacks_surface_ids,
            );
            // A server that names no surface ids is probably another Noa.
            if self.lacks_surface_ids {
                Failure::unavailable(message)
            } else {
                Failure::error(message)
            }
        })
    }

    fn children(&mut self) -> Result<Vec<Panel>, Failure> {
        let own = self.require_own()?;
        Ok(self
            .client
            .list_panels()?
            .into_iter()
            .filter(|panel| panel.parent_pane_id == Some(WireId(own)))
            .collect())
    }

    /// `ids`, or this pane's workers for `--children`.
    fn resolve(&mut self, panes: &PaneSet) -> Result<Vec<u64>, Failure> {
        match panes {
            PaneSet::Ids(ids) => Ok(ids.clone()),
            PaneSet::Children => Ok(self
                .children()?
                .iter()
                .map(|panel| panel.pane_id.0)
                .collect()),
        }
    }

    /// Refuse to type into or close a pane this orchestrator did not spawn,
    /// unless `--any-pane` (a guard against a wrong id, not a security
    /// boundary: D4).
    fn guard(&mut self, panes: &[u64], any_pane: bool) -> Result<(), Failure> {
        if any_pane {
            return Ok(());
        }
        let own = self.guard_own()?;
        let panels = self.client.list_panels()?;
        for pane in panes {
            let child = panels
                .iter()
                .any(|panel| panel.pane_id.0 == *pane && panel.parent_pane_id == Some(WireId(own)));
            if !child {
                return Err(Failure::error(not_worker_message(*pane)));
            }
        }
        Ok(())
    }

    /// This pane, for a command that only targets its own workers.
    fn guard_own(&mut self) -> Result<u64, Failure> {
        self.own_now()?.ok_or_else(|| {
            Failure::error(format!(
                "{} Without a pane of its own there are no workers to target; pass \
                 --any-pane to target a pane anyway.",
                not_own_message(
                    std::env::var(SURFACE_ID_ENV).ok().as_deref(),
                    self.port,
                    self.lacks_surface_ids
                )
            ))
        })
    }

    /// `close`: a pane that is already gone counts as closed, and one
    /// failure does not keep the rest open, so cleanup after a failed run
    /// always finishes.
    fn close(&mut self, panes: &[u64], any_pane: bool, prune: bool) -> Result<i32, Failure> {
        let own = if any_pane {
            None
        } else {
            Some(self.guard_own()?)
        };
        let here = std::env::current_dir().ok();
        let mut prune_label = |label: &str, pane_cwd: &str| {
            if prune {
                worktree::prune(here.as_deref()?, label, std::path::Path::new(pane_cwd))
            } else {
                None
            }
        };
        close_panes(
            &mut self.client,
            panes,
            own,
            &mut prune_label,
            &mut print_json,
        )
    }

    fn execute(&mut self, command: Command) -> Result<i32, Failure> {
        match command {
            Command::SelfPanel => {
                let own = self.require_own()?;
                let panel = self
                    .client
                    .list_panels()?
                    .into_iter()
                    .find(|panel| panel.pane_id.0 == own)
                    .ok_or_else(|| Failure::error("this pane is no longer listed"))?;
                print_panel(panel);
            }
            Command::Ls { children } => {
                let panels = if children {
                    self.children()?
                } else {
                    self.client.list_panels()?
                };
                panels.into_iter().for_each(print_panel);
            }
            Command::Spawn(args) => return self.spawn(args),
            Command::Send(args) => return self.send(args),
            Command::Wait(args) => {
                let panes = self.resolve(&args.panes)?;
                if panes.is_empty() {
                    return Err(Failure::error("no panes to wait for"));
                }
                let named = matches!(args.panes, PaneSet::Ids(_));
                return self.wait(&panes, named, &args.after, &args.options);
            }
            Command::Read {
                pane,
                scrollback,
                max_bytes,
            } => {
                let source = if scrollback {
                    TextSource::Scrollback
                } else {
                    TextSource::Screen
                };
                let result = self.client.get_text(pane, source, max_bytes)?;
                // The screen's rows below the output are blank, not output.
                let text = if scrollback {
                    result.text.as_str()
                } else {
                    trim_trailing_blank_lines(&result.text)
                };
                let mut stdout = std::io::stdout().lock();
                let _ = stdout.write_all(text.as_bytes());
                if !text.ends_with('\n') {
                    let _ = stdout.write_all(b"\n");
                }
                if result.truncated {
                    eprintln!("{}", truncated_message(result.text.len()));
                }
            }
            Command::Close {
                panes,
                any_pane,
                prune,
            } => {
                let panes = self.resolve(&panes)?;
                return self.close(&panes, any_pane, prune);
            }
            Command::Board { of, open } => return self.board(of, open),
            Command::Report { .. } => unreachable!("handled before connecting"),
        }
        Ok(EXIT_OK)
    }

    /// `board`: run the board here, or with `--open` start one next to
    /// this pane. The board pane has no parent, so it never counts as one
    /// of this pane's workers.
    fn board(&mut self, of: Option<u64>, open: Option<Placement>) -> Result<i32, Failure> {
        let Some(placement) = open else {
            let of = match of {
                Some(of) => Some(of),
                None => self.own.and_then(|own| {
                    self.client
                        .list_panels()
                        .ok()?
                        .into_iter()
                        .find(|panel| panel.pane_id.0 == own)?
                        .parent_pane_id
                        .map(|parent| parent.0)
                }),
            };
            return board::run(&mut self.client, of, self.own);
        };
        let own = self.require_own()?;
        if let Some(board) = board_pane(&self.client.list_panels()?, own) {
            print_json(&serde_json::json!({ "paneId": WireId(board), "existing": true }));
            return Ok(EXIT_OK);
        }
        let pane = self.open_board(own, placement)?;
        print_json(&serde_json::json!({ "paneId": WireId(pane) }));
        Ok(EXIT_OK)
    }

    /// Start a board on `own`'s workers next to it (a new tab when there is
    /// no room). The board pane has no parent, so it is not a worker; its
    /// label names the orchestrator, so a later spawn finds it.
    fn open_board(&mut self, own: u64, placement: Placement) -> Result<u64, Failure> {
        let exe = std::env::current_exe()
            .map_err(|err| Failure::error(format!("cannot find this Noa binary: {err}")))?;
        // Its `PATH`, and its locale, which picks the board's language and
        // which a Noa started from the Dock lacks.
        let env = worker_env(Vec::new(), true, std::env::vars());
        let params = SpawnParams {
            config: Some(SpawnConfig {
                command: Some(vec![
                    exe.to_string_lossy().into_owned(),
                    "+ctl".to_string(),
                    "board".to_string(),
                    "--of".to_string(),
                    own.to_string(),
                ]),
                cwd: spawn_cwd(None, std::env::current_dir().ok()),
                env,
                initial_input: None,
                wait_after_command: false,
            }),
            label: Some(board_label(own)),
            parent_pane_id: None,
            focus: Some(false),
        };
        let window = self
            .client
            .list_panels()?
            .into_iter()
            .find(|panel| panel.pane_id.0 == own)
            .map(|panel| panel.window_group_id.0);
        let pane = match placement {
            Placement::Tab => self.client.spawn_tab(window, &params)?,
            // Without a placement, a pane with no room to split opens the
            // board in a new tab, as `spawn`'s auto placement does.
            Placement::Auto => {
                match self
                    .client
                    .spawn_split(own, SplitPlacement::Horizontal, &params)
                {
                    Ok(pane) => pane,
                    Err(ClientError::Rpc { .. }) => self.client.spawn_tab(window, &params)?,
                    Err(err) => return Err(err.into()),
                }
            }
            Placement::Right => {
                self.client
                    .spawn_split(own, SplitPlacement::Horizontal, &params)?
            }
            Placement::Down => self
                .client
                .spawn_split(own, SplitPlacement::Vertical, &params)?,
        };
        Ok(pane)
    }

    fn spawn(&mut self, args: SpawnArgs) -> Result<i32, Failure> {
        let own = self.require_own()?;
        let panels = self.client.list_panels()?;
        let is_worker = panels
            .iter()
            .any(|panel| panel.pane_id.0 == own && panel.parent_pane_id.is_some());
        let opens_board = spawn_opens_board(is_worker, args.board).map_err(Failure::error)?;
        let children: Vec<Panel> = panels
            .into_iter()
            .filter(|panel| panel.parent_pane_id == Some(WireId(own)))
            .collect();
        if children.len() >= args.max_children {
            let exited = children.iter().filter(|panel| panel.exit.is_some()).count();
            return Err(Failure::error(max_children_message(
                children.len(),
                exited,
                args.max_children,
            )));
        }
        let env = worker_env(args.env, !args.command.is_empty(), std::env::vars());
        let mut cwd = spawn_cwd(args.cwd, std::env::current_dir().ok());
        // `--worktree` refuses `--cwd`; the worktree replaces this one.
        let command = match args.tier {
            Some(tier) => worktree::apply_tier(&args.command, tier).map_err(Failure::error)?,
            None => args.command,
        };
        let worktree_label = args.worktree.then(|| args.label.clone()).flatten();
        let label = args.label;
        let (input, wait_after, focus) = (args.input, args.wait_after, args.focus);
        let (pane, made) = spawn_steps(
            self,
            |_| match &worktree_label {
                Some(label) => {
                    let here = std::env::current_dir().map_err(|err| {
                        Failure::error(format!("cannot read the current directory: {err}"))
                    })?;
                    let made = worktree::add(&here, label).map_err(Failure::error)?;
                    Ok(Some((here, made)))
                }
                None => Ok(None),
            },
            // Workers are always floats (D9), stowed on the board when there
            // is one (D10), so the board opens first. A worker's go over the
            // worker itself, and the server stows them on the board it is on
            // (D13).
            |session| {
                if !opens_board {
                    return own;
                }
                let panels = session.client.list_panels().map_err(Failure::from);
                let (host, warning) =
                    board_before_spawn(|| panels, own, || session.open_board(own, Placement::Auto));
                if let Some(warning) = warning {
                    eprintln!("Noa +ctl: {warning}");
                }
                host
            },
            |session, host, made| {
                if let Some((_, made)) = made {
                    cwd = made.path.to_str().map(str::to_string);
                }
                let params = SpawnParams {
                    config: Some(SpawnConfig {
                        command: (!command.is_empty()).then_some(command),
                        cwd,
                        env,
                        initial_input: input,
                        wait_after_command: wait_after,
                    }),
                    label,
                    parent_pane_id: Some(WireId(own)),
                    focus: Some(focus),
                };
                session
                    .client
                    .spawn_split(host, SplitPlacement::Float, &params)
            },
            |_, (here, made), refused| {
                if refused {
                    // Fresh and untouched, so nothing is lost.
                    worktree::remove(here, &made.path, &made.branch);
                } else {
                    eprintln!(
                        "Noa +ctl: kept the worktree {} (branch {}): the spawn's answer was \
                         lost, so the worker may be running there",
                        made.path.display(),
                        made.branch
                    );
                }
            },
        )?;
        let mut line = serde_json::json!({ "paneId": WireId(pane) });
        if let Some((_, made)) = &made {
            line["worktree"] = made.path.to_string_lossy().into_owned().into();
            line["branch"] = made.branch.clone().into();
        }
        if let Some(tier) = args.tier {
            line["tier"] = tier.name().into();
        }
        print_json(&line);
        Ok(EXIT_OK)
    }

    fn send(&mut self, args: SendArgs) -> Result<i32, Failure> {
        self.guard(&[args.pane], args.any_pane)?;
        let text = match args.text {
            TextArg::Literal(text) => text,
            TextArg::Stdin => {
                let mut text = String::new();
                std::io::stdin()
                    .read_to_string(&mut text)
                    .map_err(|err| Failure::error(format!("cannot read stdin: {err}")))?;
                text
            }
        };
        let before = self
            .client
            .list_panels()?
            .into_iter()
            .find(|panel| panel.pane_id.0 == args.pane)
            .ok_or_else(|| Failure::error("unknown pane"))?;
        let seq = before.agent.as_ref().map_or(0, |agent| agent.seq);
        // Enter approves some prompts with their default choice, and picks
        // the first choice of a question (D7), so either is only answered on
        // purpose.
        if !args.answer
            && let Some(awaited) = awaits_answer(&before)
        {
            return Err(Failure::error(format!(
                "pane {} is waiting for {awaited}; nothing was sent. Read it, and pass \
                 --answer to answer it from here.",
                args.pane
            )));
        }
        // Subscribe before typing, so the reply cannot arrive unobserved.
        if args.wait.is_some() {
            self.client
                .subscribe(&[EventKind::StateChanged], Some(&[args.pane]))?;
        }
        if args.replace {
            for keys in replace_keys() {
                self.send_input(args.pane, &keys, false)?;
                std::thread::sleep(REPLACE_PAUSE);
            }
            // Let the TUI take the keys before a paste arrives.
            std::thread::sleep(SUBMIT_DELAY);
        }
        if !text.is_empty() {
            self.send_input(args.pane, &text, !args.raw)?;
        }
        if args.enter {
            if !text.is_empty() {
                std::thread::sleep(SUBMIT_DELAY);
                let asked = self
                    .client
                    .list_panels()?
                    .iter()
                    .find(|panel| panel.pane_id.0 == args.pane)
                    .and_then(awaits_answer);
                if !args.answer
                    && let Some(awaited) = asked
                {
                    return Err(Failure::error(format!(
                        "pane {} began waiting for {awaited} after the text was typed; \
                         Enter was not sent, so the text may still be in its input line.",
                        args.pane
                    )));
                }
            }
            self.send_input(args.pane, "\r", false)?;
        }
        match args.wait {
            Some(options) => {
                self.wait_subscribed(&[args.pane], send_wait_start(before, seq), &options)
            }
            None => {
                print_json(&serde_json::json!({ "paneId": WireId(args.pane), "seq": seq }));
                Ok(EXIT_OK)
            }
        }
    }

    /// `noa.sendText`, with a refusal of a pane whose process has exited
    /// (held open, so still listed) said as such rather than "pane closed".
    fn send_input(&mut self, pane: u64, text: &str, bracketed: bool) -> Result<(), Failure> {
        match self.client.send_text(pane, text, bracketed) {
            Err(ClientError::Rpc {
                code: RPC_PANE_CLOSED,
                message,
            }) => {
                let panels = self.client.list_panels()?;
                let panel = panels.iter().find(|panel| panel.pane_id.0 == pane);
                Err(exited_input_failure(pane, panel).unwrap_or_else(|| {
                    Failure::from(ClientError::Rpc {
                        code: RPC_PANE_CLOSED,
                        message,
                    })
                }))
            }
            result => Ok(result?),
        }
    }

    fn wait(
        &mut self,
        panes: &[u64],
        named: bool,
        after: &[(u64, u64)],
        options: &WaitOptions,
    ) -> Result<i32, Failure> {
        self.client
            .subscribe(&[EventKind::StateChanged], Some(panes))?;
        let after = wait_baselines(panes, after).map_err(Failure::error)?;
        let start = if named {
            WaitStart::Named { after }
        } else {
            WaitStart::Listed { after }
        };
        self.wait_subscribed(panes, start, options)
    }

    fn wait_subscribed(
        &mut self,
        panes: &[u64],
        start: WaitStart,
        options: &WaitOptions,
    ) -> Result<i32, Failure> {
        wait_loop(
            &mut self.client,
            panes,
            start,
            options,
            WAIT_RELIST_INTERVAL,
            &mut print_json,
        )
    }
}

/// A spawn's steps once its checks passed: make the `--worktree` worktree
/// (`worktree`, `None` without one), find the host pane (`host`, which opens
/// the board), start the worker there (`spawn`), and undo the worktree when
/// the start failed (`undo`).
///
/// The worktree comes first: git refusing it (the branch or directory
/// exists) then starts nothing, not even the board. `undo` is told whether
/// the server refused the spawn (an RPC error), the one case where no
/// worker can be running in the worktree; after a lost answer it may be.
pub(crate) fn spawn_steps<S, W>(
    session: &mut S,
    worktree: impl FnOnce(&mut S) -> Result<Option<W>, Failure>,
    host: impl FnOnce(&mut S) -> u64,
    spawn: impl FnOnce(&mut S, u64, Option<&W>) -> Result<u64, ClientError>,
    undo: impl FnOnce(&mut S, &W, bool),
) -> Result<(u64, Option<W>), Failure> {
    let made = worktree(session)?;
    let host = host(session);
    match spawn(session, host, made.as_ref()) {
        Ok(pane) => Ok((pane, made)),
        Err(err) => {
            if let Some(made) = &made {
                undo(session, made, matches!(err, ClientError::Rpc { .. }));
            }
            Err(err.into())
        }
    }
}

/// Where a wait's per-pane baselines come from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum WaitStart {
    /// The first listing inside the wait; `after` overrides the report
    /// count of the panes it names.
    Listed { after: HashMap<u64, u64> },
    /// As `Listed`, for panes named by id: one missing from that first
    /// listing never existed (or is long gone), so the wait fails instead of
    /// reporting it as just closed.
    Named { after: HashMap<u64, u64> },
    /// Panels observed before input was sent (`send --wait`): a change that
    /// lands between sending and the first listing still ends the wait.
    Before {
        panels: Vec<Panel>,
        after: Option<u64>,
    },
}

/// What [`wait_loop`] reads: the pane listing and pushed notifications.
pub(crate) trait WaitSource {
    fn list(&mut self) -> Result<Vec<Panel>, Failure>;
    fn next(&mut self, timeout: Duration) -> Result<Option<Notification>, Failure>;
    /// `pane`'s retained output (`--tail`); `None` once the pane is gone.
    fn scrollback(&mut self, pane: u64) -> Result<Option<String>, Failure>;
    /// `pane`'s visible screen text (`--stall`); `None` once the pane is gone.
    fn screen(&mut self, pane: u64) -> Result<Option<String>, Failure>;
}

impl WaitSource for Client {
    fn list(&mut self) -> Result<Vec<Panel>, Failure> {
        Ok(self.list_panels()?)
    }

    fn next(&mut self, timeout: Duration) -> Result<Option<Notification>, Failure> {
        Ok(self.next_notification(timeout)?)
    }

    fn scrollback(&mut self, pane: u64) -> Result<Option<String>, Failure> {
        client_text(self, pane, TextSource::Scrollback)
    }

    fn screen(&mut self, pane: u64) -> Result<Option<String>, Failure> {
        client_text(self, pane, TextSource::Screen)
    }
}

/// `None` when the pane closed between its outcome and this request.
fn client_text(
    client: &mut Client,
    pane: u64,
    source: TextSource,
) -> Result<Option<String>, Failure> {
    match client.get_text(pane, source, None) {
        Ok(result) => Ok(Some(result.text)),
        Err(ClientError::Rpc {
            code: RPC_UNKNOWN_PANE | RPC_PANE_CLOSED,
            ..
        }) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn watched(panels: Vec<Panel>, panes: &[u64]) -> HashMap<u64, Panel> {
    panels
        .into_iter()
        .filter(|panel| panes.contains(&panel.pane_id.0))
        .map(|panel| (panel.pane_id.0, panel))
        .collect()
}

/// Block until the wait condition holds for one pane (every pane with
/// `--all`) or the timeout passes, writing one JSON line per stopped pane
/// through `emit`. Returns the exit status.
pub(crate) fn wait_loop(
    source: &mut impl WaitSource,
    panes: &[u64],
    start: WaitStart,
    options: &WaitOptions,
    relist_every: Duration,
    emit: &mut dyn FnMut(&serde_json::Value),
) -> Result<i32, Failure> {
    wait_loop_with_clock(
        source,
        panes,
        start,
        options,
        relist_every,
        emit,
        &mut Instant::now,
    )
}

/// What the stall detector remembers of one pane's screen.
struct Quiet {
    text: Option<String>,
    /// The wait's start, or the last time `text` was seen to change.
    since: Instant,
}

/// [`wait_loop`] with the stall detector's time source supplied (the
/// deadline and the re-list schedule keep using the real clock).
pub(crate) fn wait_loop_with_clock(
    source: &mut impl WaitSource,
    panes: &[u64],
    start: WaitStart,
    options: &WaitOptions,
    relist_every: Duration,
    emit: &mut dyn FnMut(&serde_json::Value),
    clock: &mut dyn FnMut() -> Instant,
) -> Result<i32, Failure> {
    // A pane named twice is one pane: `--all` counts each once.
    let mut unique = Vec::with_capacity(panes.len());
    for pane in panes {
        if !unique.contains(pane) {
            unique.push(*pane);
        }
    }
    let panes = unique.as_slice();
    // A timeout too far out to represent as an instant is no deadline.
    let deadline = options
        .timeout
        .and_then(|timeout| Instant::now().checked_add(timeout));
    let mut latest = watched(source.list()?, panes);
    let mut listed_at = Instant::now();
    if let WaitStart::Named { .. } = start {
        let missing: Vec<String> = panes
            .iter()
            .filter(|pane| !latest.contains_key(pane))
            .map(u64::to_string)
            .collect();
        match missing.as_slice() {
            [] => {}
            [pane] => return Err(Failure::error(format!("pane {pane} is not listed"))),
            panes => {
                return Err(Failure::error(format!(
                    "panes {} are not listed",
                    panes.join(", ")
                )));
            }
        }
    }
    let mut watches: Vec<Watch> = match start {
        WaitStart::Listed { after } | WaitStart::Named { after } => panes
            .iter()
            .map(|pane| Watch::new(*pane, latest.get(pane), after.get(pane).copied()))
            .collect(),
        WaitStart::Before { panels, after } => {
            let before = watched(panels, panes);
            panes
                .iter()
                .map(|pane| Watch::new(*pane, before.get(pane), after))
                .collect()
        }
    };
    // A pane that exited before the wait began stopped earlier: waiting for
    // any one pane, it must not end the wait on the panes still running. It
    // only ends a wait that has nothing else to wait for.
    let skip_exited_before = !options.all && watches.iter().any(|watch| !watch.exited_before);
    let mut done: Vec<u64> = Vec::new();
    let mut relist_pending = false;
    let started = clock();
    let mut quiet: HashMap<u64, Quiet> = HashMap::new();
    let mut screens_polled_at: Option<Instant> = None;
    loop {
        for watch in &mut watches {
            if done.contains(&watch.pane) || (skip_exited_before && watch.exited_before) {
                continue;
            }
            let panel = latest.get(&watch.pane);
            if let Some(outcome) = evaluate(watch, panel, &options.reasons) {
                emit_outcome(source, options, watch.pane, &outcome, panel, None, emit)?;
                done.push(watch.pane);
            }
        }
        // A report or exit seen above wins over a stall in the same pass.
        // With a re-list pending, `latest` may be stale (an exit only shows
        // in a listing), so the stall waits for the next pass.
        if let Some(stall) = options.stall
            && !relist_pending
        {
            let now = clock();
            if screens_polled_at.is_none_or(|at| now.saturating_duration_since(at) >= relist_every)
            {
                screens_polled_at = Some(now);
                let mut stalled: Vec<(u64, Duration)> = Vec::new();
                for watch in &watches {
                    if done.contains(&watch.pane) || (skip_exited_before && watch.exited_before) {
                        continue;
                    }
                    let Some(text) = source.screen(watch.pane)? else {
                        continue;
                    };
                    let entry = quiet.entry(watch.pane).or_insert(Quiet {
                        text: None,
                        since: started,
                    });
                    if entry.text.as_ref().is_some_and(|seen| *seen != text) {
                        entry.since = now;
                    }
                    entry.text = Some(text);
                    let quiet_for = now.saturating_duration_since(entry.since);
                    if quiet_for >= stall {
                        stalled.push((watch.pane, quiet_for));
                    }
                }
                if !stalled.is_empty() {
                    // Notifications received while the screens were read wait
                    // in the client's queue: apply them, and judge the
                    // stalled panes on them first.
                    let mut relist = false;
                    while let Some(notification) = source.next(Duration::ZERO)? {
                        relist |= apply_notification(&mut latest, notification, panes);
                    }
                    if relist {
                        latest = watched(source.list()?, panes);
                        listed_at = Instant::now();
                    }
                    for (pane, quiet_for) in stalled {
                        let Some(watch) = watches.iter_mut().find(|watch| watch.pane == pane)
                        else {
                            continue;
                        };
                        let panel = latest.get(&pane);
                        let (outcome, quiet) = match evaluate(watch, panel, &options.reasons) {
                            Some(outcome) => (outcome, None),
                            None => (
                                Outcome {
                                    reason: Reason::Stalled,
                                    closed: false,
                                },
                                Some(quiet_for),
                            ),
                        };
                        emit_outcome(source, options, pane, &outcome, panel, quiet, emit)?;
                        done.push(pane);
                    }
                }
            }
        }
        if (options.all && done.len() == watches.len()) || (!options.all && !done.is_empty()) {
            return Ok(EXIT_OK);
        }
        // Re-list only after the pushed state above was evaluated, so a
        // listing never overwrites a transition before it was seen.
        if relist_pending {
            latest = watched(source.list()?, panes);
            listed_at = Instant::now();
            relist_pending = false;
            continue;
        }
        let remaining = match deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    emit(&timeout_json(&watches, &done, &latest));
                    return Ok(EXIT_TIMEOUT);
                }
                remaining.min(WAIT_RELIST_INTERVAL)
            }
            None => WAIT_RELIST_INTERVAL,
        };
        let relist = match source.next(remaining)? {
            Some(notification) => apply_notification(&mut latest, notification, panes),
            // An idle tick.
            None => true,
        };
        // `stateChanged` has no removal event, so a closed pane is only
        // noticed by listing: re-list on schedule even while other panes keep
        // notifying.
        relist_pending = relist || listed_at.elapsed() >= relist_every;
    }
}

/// Apply a pushed notification to `latest`; returns whether earlier ones
/// were lost (`dropped`), so a re-list is due. A dropped notification's own
/// state is still applied (and evaluated first).
fn apply_notification(
    latest: &mut HashMap<u64, Panel>,
    notification: Notification,
    panes: &[u64],
) -> bool {
    match notification {
        Notification::StateChanged { panels, dropped } => {
            for panel in panels {
                if panes.contains(&panel.pane_id.0) {
                    latest.insert(panel.pane_id.0, panel);
                }
            }
            dropped
        }
        Notification::Other { dropped, .. } => dropped,
    }
}

/// Print the line for a pane whose wait ended, with `--tail` if asked.
fn emit_outcome(
    source: &mut impl WaitSource,
    options: &WaitOptions,
    pane: u64,
    outcome: &Outcome,
    panel: Option<&Panel>,
    quiet: Option<Duration>,
    emit: &mut dyn FnMut(&serde_json::Value),
) -> Result<(), Failure> {
    let mut line = outcome_json(pane, outcome, panel, quiet);
    if let Some(lines) = options.tail
        && !outcome.closed
        && let Some(text) = source.scrollback(pane)?
    {
        line["tail"] = tail_lines(&text, lines).into();
    }
    emit(&line);
    Ok(())
}

/// The line a timed-out wait prints. A pane that had exited before the wait
/// (and so was never waited on) is under `exited`, not `pending`. The
/// baselines, with each pane's agent state, let a later `wait --after`
/// carry on without missing a report that lands in between.
fn timeout_json(
    watches: &[Watch],
    done: &[u64],
    latest: &HashMap<u64, Panel>,
) -> serde_json::Value {
    let left: Vec<&Watch> = watches
        .iter()
        .filter(|watch| !done.contains(&watch.pane))
        .collect();
    let ids = |exited: bool| {
        left.iter()
            .filter(|watch| watch.exited_before == exited)
            .map(|watch| WireId(watch.pane))
            .collect::<Vec<_>>()
    };
    let mut value = serde_json::json!({
        "timeout": true,
        "pending": ids(false),
        "baseline": left
            .iter()
            .map(|watch| {
                let mut entry = serde_json::json!({
                    "paneId": WireId(watch.pane),
                    "seq": watch.baseline_seq,
                });
                if let Some(state) = latest
                    .get(&watch.pane)
                    .and_then(|panel| panel.agent.as_ref()?.state.clone())
                {
                    entry["state"] = state.into();
                }
                entry
            })
            .collect::<Vec<_>>(),
    });
    let exited = ids(true);
    if !exited.is_empty() {
        value["exited"] = serde_json::json!(exited);
    }
    value
}

/// How `send --wait` starts its wait, given the panel it saw before typing
/// and that panel's report count.
pub(crate) fn send_wait_start(before: Panel, seq: u64) -> WaitStart {
    WaitStart::Before {
        panels: vec![before],
        after: Some(seq),
    }
}

/// The `--after` baselines of a wait on `panes` (already resolved, so
/// `--children` is checked too).
pub(crate) fn wait_baselines(
    panes: &[u64],
    after: &[(u64, u64)],
) -> Result<HashMap<u64, u64>, String> {
    if let Some((pane, _)) = after.iter().find(|(pane, _)| !panes.contains(pane)) {
        return Err(format!("--after names pane {pane}, which is not waited on"));
    }
    Ok(after.iter().copied().collect())
}

fn not_worker_message(pane: u64) -> String {
    format!("pane {pane} is not a worker of this pane (pass --any-pane to target it)")
}

/// What `panel`'s agent reported it waits for, if it is an approval
/// (`permission`), as the noun phrase a refusal names. `input` is not
/// refused: it also stands for an agent idle at its prompt (the hook
/// adapter's `idle_prompt`), where a follow-up send is the normal case.
pub(crate) fn awaits_answer(panel: &Panel) -> Option<&'static str> {
    match panel.agent.as_ref()?.state.as_deref()? {
        "permission" => Some("an approval"),
        _ => None,
    }
}

/// The refusal of input to `pane` when its listed `panel` shows that its
/// process has exited; `None` when it has not (the pane really is gone).
pub(crate) fn exited_input_failure(pane: u64, panel: Option<&Panel>) -> Option<Failure> {
    let exit = panel?.exit.as_ref()?;
    let code = exit
        .code
        .map_or_else(String::new, |code| format!(" (code {code})"));
    Some(Failure::error(format!(
        "pane {pane}'s process has exited{code}; it takes no input — read it, then close it"
    )))
}

/// Why a spawn is refused at the `--max-children` limit. Exited workers
/// held open keep their slot until closed; the board's own cap means the
/// limit can never be raised past [`MOST_CHILDREN`].
pub(crate) fn max_children_message(live: usize, exited: usize, max: usize) -> String {
    let exited = if exited > 0 {
        format!(", {exited} of them exited")
    } else {
        String::new()
    };
    let raise = if max < MOST_CHILDREN {
        format!(", or raise --max-children (at most {MOST_CHILDREN})")
    } else {
        String::new()
    };
    format!(
        "this pane already has {live} workers{exited} (limit {max}); close finished ones \
         after reading them{raise}"
    )
}

/// `text` without the blank (empty or whitespace-only) lines at its end.
pub(crate) fn trim_trailing_blank_lines(text: &str) -> &str {
    let mut end = 0;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        if !line.trim().is_empty() {
            end = offset;
        }
    }
    &text[..end]
}

/// The last `lines` lines of `text`, trailing blank lines left out first.
pub(crate) fn tail_lines(text: &str, lines: usize) -> String {
    let mut all: Vec<&str> = text.lines().collect();
    // Noa's own banner under an exited command, and the blank rows around
    // it, are not the worker's output.
    while all
        .last()
        .is_some_and(|line| line.trim().is_empty() || line.starts_with(HOLD_BANNER))
    {
        all.pop();
    }
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// How Noa's `wait-after-command` banner under an exited command starts
/// (`app/child_exited.rs`).
pub(crate) const HOLD_BANNER: &str = "[Process exited.";

/// The stderr line for a `getText` the server cut: it keeps the end.
pub(crate) fn truncated_message(kept: usize) -> String {
    format!("Noa +ctl: output truncated; kept the last {kept} bytes")
}

/// The panes `close` acts on, each once, with whether it is still listed
/// (an unlisted one is already gone). `own` is the orchestrator whose
/// workers are the only panes accepted; `None` (`--any-pane`) accepts any.
pub(crate) fn close_targets(
    panels: &[Panel],
    panes: &[u64],
    own: Option<u64>,
) -> Result<Vec<(u64, bool)>, String> {
    let mut targets: Vec<(u64, bool)> = Vec::with_capacity(panes.len());
    for pane in panes {
        if targets.iter().any(|(seen, _)| seen == pane) {
            continue;
        }
        let panel = panels.iter().find(|panel| panel.pane_id.0 == *pane);
        if let (Some(panel), Some(own)) = (panel, own)
            && panel.parent_pane_id != Some(WireId(own))
        {
            return Err(not_worker_message(*pane));
        }
        targets.push((*pane, panel.is_some()));
    }
    Ok(targets)
}

/// What [`close_panes`] talks to: the pane listing and `noa.closePane`.
pub(crate) trait PaneCloser {
    fn list(&mut self) -> Result<Vec<Panel>, ClientError>;
    fn close(&mut self, pane: u64) -> Result<(), ClientError>;
}

impl PaneCloser for Client {
    fn list(&mut self) -> Result<Vec<Panel>, ClientError> {
        self.list_panels()
    }

    fn close(&mut self, pane: u64) -> Result<(), ClientError> {
        self.close_pane(pane)
    }
}

/// `close`: a pane that is already gone counts as closed, and a pane that
/// could not be closed does not keep the rest open, so cleanup after a
/// failed run always finishes. Writes one JSON line per closed pane through
/// `emit`; returns the exit status.
pub(crate) fn close_panes(
    source: &mut impl PaneCloser,
    panes: &[u64],
    own: Option<u64>,
    prune: &mut dyn FnMut(&str, &str) -> Option<serde_json::Value>,
    emit: &mut dyn FnMut(&serde_json::Value),
) -> Result<i32, Failure> {
    let panels = source.list()?;
    let mut failed = false;
    for (pane, listed) in close_targets(&panels, panes, own).map_err(Failure::error)? {
        let label = panels
            .iter()
            .find(|panel| panel.pane_id.0 == pane)
            .and_then(|panel| Some((panel.label.clone()?, panel.cwd.clone())));
        let gone = !listed
            || match source.close(pane) {
                Ok(()) => false,
                // Refused: fine if the pane went away since the listing. A
                // missing scope refuses every pane, so it fails the command.
                Err(ClientError::Rpc { code, message }) if code != RPC_SCOPE_DENIED => {
                    let listed = source.list()?;
                    if listed.iter().any(|panel| panel.pane_id.0 == pane) {
                        eprintln!("Noa +ctl: pane {pane} was not closed: {message}");
                        failed = true;
                        continue;
                    }
                    true
                }
                Err(err) => return Err(err.into()),
            };
        let mut line = serde_json::json!({ "paneId": WireId(pane), "closed": true });
        if gone {
            line["gone"] = true.into();
        } else if let Some((label, pane_cwd)) = label {
            // Only after the pane is closed, so its process no longer
            // holds the worktree; a refusal keeps it and does not fail.
            if let Some(info) = prune(&label, &pane_cwd) {
                line["worktree"] = info;
            }
        }
        emit(&line);
    }
    Ok(if failed { EXIT_ERROR } else { EXIT_OK })
}

/// The worker's cwd: `--cwd` (a relative one taken from this process's
/// cwd, since Noa resolves the path in its own process), or this process's
/// cwd.
pub(crate) fn spawn_cwd(
    requested: Option<String>,
    current: Option<std::path::PathBuf>,
) -> Option<String> {
    let path = match (requested, current) {
        (Some(dir), Some(current)) => current.join(dir),
        (Some(dir), None) => std::path::PathBuf::from(dir),
        (None, current) => current?,
    };
    path.into_os_string().into_string().ok()
}

fn print_panel(mut panel: Panel) {
    // The preview is for UIs; a shell reads output with `read`.
    panel.preview.clear();
    print_json(&serde_json::to_value(panel).expect("Panel serializes"));
}

fn print_json(value: &serde_json::Value) {
    println!("{value}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_ipc::{PanelAgent, PanelExit};

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn panel(seq: u64, state: Option<&str>, attention: bool) -> Panel {
        Panel {
            window_group_id: WireId(1),
            window_id: WireId(1),
            pane_id: WireId(8),
            name: "codex".to_string(),
            cwd: "/repo".to_string(),
            branch: None,
            process: Some("codex".to_string()),
            busy: false,
            attention,
            attachable: true,
            preview: Vec::new(),
            surface_id: None,
            agent: Some(PanelAgent {
                kind: Some("codex".to_string()),
                state: state.map(str::to_string),
                detail: None,
                seq,
            }),
            exit: None,
            parent_pane_id: Some(WireId(3)),
            label: None,
        }
    }

    #[test]
    fn spawn_parses_placement_options_and_the_command_after_double_dash() {
        let command = parse_args(&args(&[
            "spawn",
            "--cwd",
            "/repo",
            "--env",
            "A=1",
            "--env",
            "B=x=y",
            "--label",
            "review",
            "--no-wait-after",
            "--",
            "codex",
            "--label",
            "x",
        ]))
        .unwrap();

        assert_eq!(
            command,
            Command::Spawn(SpawnArgs {
                board: true,
                cwd: Some("/repo".to_string()),
                env: vec![
                    ("A".to_string(), "1".to_string()),
                    ("B".to_string(), "x=y".to_string())
                ],
                label: Some("review".to_string()),
                input: None,
                wait_after: false,
                focus: false,
                max_children: DEFAULT_MAX_CHILDREN,
                worktree: false,
                tier: None,
                command: args(&["codex", "--label", "x"]),
            })
        );
    }

    #[test]
    fn spawn_defaults_keep_finished_panes_open_and_focus_where_it_was() {
        let Command::Spawn(spawn) = parse_args(&args(&["spawn", "--", "claude"])).unwrap() else {
            panic!("spawn expected");
        };

        assert!(spawn.wait_after);
        assert!(!spawn.focus);
    }

    #[test]
    fn spawn_rejects_conflicting_or_malformed_options() {
        for bad in [
            &["spawn", "--tab", "--split", "auto"][..],
            &["spawn", "--split", "left"],
            &["spawn", "--env", "=x"],
            &["spawn", "--bogus"],
            &["spawn", "--cwd"],
        ] {
            assert!(parse_args(&args(bad)).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn spawn_worktree_needs_a_plain_label_and_no_cwd() {
        let Command::Spawn(spawn) = parse_args(&args(&[
            "spawn",
            "--worktree",
            "--label",
            "fix-1",
            "--",
            "claude",
        ]))
        .unwrap() else {
            panic!("spawn expected");
        };
        assert!(spawn.worktree);
        assert_eq!(spawn.label.as_deref(), Some("fix-1"));

        for bad in [
            &["spawn", "--worktree"][..],
            &["spawn", "--worktree", "--label", ".x"],
            &["spawn", "--worktree", "--label", "-x"],
            &["spawn", "--worktree", "--label", "a/b"],
            &["spawn", "--worktree", "--label", "a", "--cwd", "/repo"],
        ] {
            assert!(parse_args(&args(bad)).is_err(), "{bad:?} must be rejected");
        }
        // The label rules apply to --worktree only.
        assert!(parse_args(&args(&["spawn", "--label", "a b"])).is_ok());
    }

    #[test]
    fn spawn_tier_needs_an_agent_command_that_picks_no_model() {
        let Command::Spawn(spawn) =
            parse_args(&args(&["spawn", "--tier", "deep", "--", "codex", "exec"])).unwrap()
        else {
            panic!("spawn expected");
        };
        assert_eq!(spawn.tier, Some(worktree::Tier::Deep));
        // The command stays as given: the PATH check sees the program, and
        // the flags are added when the worker starts.
        assert_eq!(spawn.command, args(&["codex", "exec"]));

        for bad in [
            &["spawn", "--tier", "deep"][..],
            &["spawn", "--tier", "deep", "--", "vim"],
            &["spawn", "--tier", "max", "--", "claude"],
            &[
                "spawn", "--tier", "light", "--", "claude", "--model", "opus",
            ],
            &["spawn", "--tier", "light", "--", "codex", "-m", "x"],
            &["spawn", "--tier", "light", "--", "agy", "--model", "x"],
        ] {
            assert!(parse_args(&args(bad)).is_err(), "{bad:?} must be rejected");
        }
    }

    /// What `run_spawn_steps` returns: the result and the steps taken.
    type StepsRun = (Result<(u64, Option<&'static str>), Failure>, Vec<String>);

    /// `spawn_steps` with each step logged; `worktree` fails when
    /// `worktree_fails`, `spawn` fails with `spawn_error`.
    fn run_spawn_steps(worktree_fails: bool, spawn_error: Option<ClientError>) -> StepsRun {
        let mut log: Vec<String> = Vec::new();
        let result = spawn_steps(
            &mut log,
            |log| {
                log.push("worktree".to_string());
                if worktree_fails {
                    Err(Failure::error(
                        "fatal: a branch named 'orch/x' already exists",
                    ))
                } else {
                    Ok(Some("wt"))
                }
            },
            |log| {
                log.push("board".to_string());
                7
            },
            |log, host, made| {
                log.push(format!("spawn on {host} in {made:?}"));
                match spawn_error {
                    Some(err) => Err(err),
                    None => Ok(9),
                }
            },
            |log, made, refused| log.push(format!("undo {made} refused={refused}")),
        );
        (result, log)
    }

    // Review finding: the board opened before the worktree was made, so a
    // `spawn --worktree` that git refused still left a board pane behind.
    #[test]
    fn a_failed_worktree_opens_no_board() {
        let (result, log) = run_spawn_steps(true, None);
        assert!(result.is_err());
        assert_eq!(log, ["worktree"]);

        let (result, log) = run_spawn_steps(false, None);
        assert_eq!(result.ok(), Some((9, Some("wt"))));
        assert_eq!(log, ["worktree", "board", "spawn on 7 in Some(\"wt\")"]);
    }

    // Review finding: any spawn error removed the worktree, also a lost
    // answer to a spawn the server had carried out, taking the running
    // worker's directory away.
    #[test]
    fn only_a_refused_spawn_removes_its_worktree() {
        let (result, log) = run_spawn_steps(
            false,
            Some(ClientError::Rpc {
                code: -32602,
                message: "nesting goes one level deep".to_string(),
            }),
        );
        assert!(result.is_err());
        assert_eq!(log.last().map(String::as_str), Some("undo wt refused=true"));

        let (result, log) = run_spawn_steps(
            false,
            Some(ClientError::Transport("connection reset".to_string())),
        );
        assert!(result.is_err());
        assert_eq!(
            log.last().map(String::as_str),
            Some("undo wt refused=false")
        );
    }

    #[test]
    fn close_takes_prune_with_or_without_any_pane() {
        assert_eq!(
            parse_args(&args(&["close", "--prune", "--any-pane", "8"])).unwrap(),
            Command::Close {
                panes: PaneSet::Ids(vec![8]),
                any_pane: true,
                prune: true,
            }
        );
        assert_eq!(
            parse_args(&args(&["close", "8"])).unwrap(),
            Command::Close {
                panes: PaneSet::Ids(vec![8]),
                any_pane: false,
                prune: false,
            }
        );
    }

    struct OkCloser(Vec<Panel>);

    impl PaneCloser for OkCloser {
        fn list(&mut self) -> Result<Vec<Panel>, ClientError> {
            Ok(self.0.clone())
        }

        fn close(&mut self, _pane: u64) -> Result<(), ClientError> {
            Ok(())
        }
    }

    #[test]
    fn prune_runs_after_a_close_for_labelled_listed_panes_only() {
        let mut labelled = worker(8, 1);
        labelled.label = Some("job".to_string());
        let mut source = OkCloser(vec![labelled, worker(9, 1)]);
        let mut asked = Vec::new();
        let mut lines = Vec::new();
        let code = close_panes(
            &mut source,
            &[8, 9, 7],
            Some(1),
            &mut |label, _cwd| {
                asked.push(label.to_string());
                Some(serde_json::json!({ "path": "/r-wt/job", "removed": true }))
            },
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_OK));
        // No label (9) and already gone (7) are not asked about.
        assert_eq!(asked, ["job"]);
        assert_eq!(lines[0]["worktree"]["removed"], true);
        assert!(lines[1].get("worktree").is_none());
        assert_eq!(lines[2]["gone"], true);
        assert!(lines[2].get("worktree").is_none());
    }

    #[test]
    fn send_reads_text_or_stdin_and_its_wait_options() {
        let command = parse_args(&args(&[
            "send",
            "8",
            "--enter",
            "--wait",
            "--timeout",
            "30",
            "--for",
            "finished,error",
            "fix the failing test",
        ]))
        .unwrap();

        assert_eq!(
            command,
            Command::Send(SendArgs {
                pane: 8,
                text: TextArg::Literal("fix the failing test".to_string()),
                enter: true,
                raw: false,
                any_pane: false,
                answer: false,
                replace: false,
                wait: Some(WaitOptions {
                    reasons: vec![Reason::Finished, Reason::Error],
                    all: false,
                    timeout: Some(Duration::from_secs(30)),
                    tail: None,
                    stall: None,
                }),
            })
        );
        assert!(matches!(
            parse_args(&args(&["send", "8", "-"])).unwrap(),
            Command::Send(SendArgs {
                text: TextArg::Stdin,
                ..
            })
        ));
    }

    #[test]
    fn wait_takes_panes_or_children_and_a_baseline_per_pane() {
        assert_eq!(
            parse_args(&args(&["wait", "--children", "--all"])).unwrap(),
            Command::Wait(WaitArgs {
                panes: PaneSet::Children,
                after: Vec::new(),
                options: WaitOptions {
                    all: true,
                    ..WaitOptions::default()
                },
            })
        );
        let after = |items: &[&str]| match parse_args(&args(items)) {
            Ok(Command::Wait(wait)) => Some(wait.after),
            _ => None,
        };
        assert_eq!(after(&["wait", "8", "--after", "4"]), Some(vec![(8, 4)]));
        assert_eq!(after(&["wait", "8", "9", "--after", "4"]), None);
        assert_eq!(
            after(&["wait", "8", "9", "--after", "8=4", "--after=9=2"]),
            Some(vec![(8, 4), (9, 2)])
        );
        assert_eq!(
            after(&["wait", "--children", "--after", "8=4"]),
            Some(vec![(8, 4)])
        );
        assert_eq!(after(&["wait", "--children", "--after", "4"]), None);
        assert_eq!(after(&["wait", "8", "--after", "8=x"]), None);
        assert!(parse_args(&args(&["wait", "--children", "8"])).is_err());
        assert!(parse_args(&args(&["wait"])).is_err());
        assert!(parse_args(&args(&["wait", "8", "--for", "done"])).is_err());
        assert!(parse_args(&args(&["wait", "8", "--timeout", "-1"])).is_err());
    }

    #[test]
    fn report_accepts_only_the_protocol_states() {
        assert_eq!(
            parse_args(&args(&["report", "permission", "Bash"])).unwrap(),
            Command::Report {
                state: "permission".to_string(),
                detail: "Bash".to_string()
            }
        );
        assert!(parse_args(&args(&["report", "approved"])).is_err());
        assert!(parse_args(&args(&["report", "clear", "x"])).is_err());
    }

    #[test]
    fn report_sequence_strips_controls_and_bounds_the_detail() {
        assert_eq!(
            report_sequence("running", "a\x1b]b\x07c"),
            "\x1b]777;noa-agent;running;a]bc\x1b\\"
        );
        let long = report_sequence("input", &"あ".repeat(300));
        assert_eq!(long.matches('あ').count(), MAX_REPORT_DETAIL_CHARS);
    }

    #[test]
    fn a_report_that_predates_the_wait_does_not_end_it() {
        let stale = panel(5, Some("finished"), false);
        let mut watch = Watch::new(8, Some(&stale), None);

        assert_eq!(evaluate(&mut watch, Some(&stale), &Reason::ALL), None);
        assert_eq!(
            evaluate(
                &mut watch,
                Some(&panel(6, Some("running"), false)),
                &Reason::ALL
            ),
            None
        );
        assert_eq!(
            evaluate(
                &mut watch,
                Some(&panel(7, Some("finished"), false)),
                &Reason::ALL
            ),
            Some(Outcome {
                reason: Reason::Finished,
                closed: false
            })
        );
    }

    #[test]
    fn after_overrides_the_observed_baseline() {
        // `send` captured seq 5 before typing; the reply already arrived.
        let current = panel(6, Some("permission"), false);
        let mut watch = Watch::new(8, Some(&current), Some(5));

        assert_eq!(
            evaluate(&mut watch, Some(&current), &Reason::ALL).map(|o| o.reason),
            Some(Reason::Permission)
        );
    }

    #[test]
    fn unrequested_states_keep_waiting() {
        let mut watch = Watch::new(8, None, Some(0));

        assert_eq!(
            evaluate(
                &mut watch,
                Some(&panel(1, Some("permission"), false)),
                &[Reason::Finished]
            ),
            None
        );
    }

    #[test]
    fn attention_counts_only_after_it_was_seen_cleared() {
        let lingering = panel(0, None, true);
        let mut watch = Watch::new(8, Some(&lingering), None);

        assert_eq!(evaluate(&mut watch, Some(&lingering), &Reason::ALL), None);
        assert_eq!(
            evaluate(&mut watch, Some(&panel(0, None, false)), &Reason::ALL),
            None
        );
        assert_eq!(
            evaluate(&mut watch, Some(&panel(0, None, true)), &Reason::ALL).map(|o| o.reason),
            Some(Reason::Attention)
        );
    }

    #[test]
    fn exit_and_vanished_panes_end_the_wait() {
        let mut held = panel(0, None, false);
        held.exit = Some(PanelExit { code: Some(2) });
        let mut watch = Watch::new(8, Some(&held), None);

        let outcome = evaluate(&mut watch, Some(&held), &Reason::ALL).unwrap();
        assert_eq!(outcome.reason, Reason::Exit);
        assert_eq!(
            outcome_json(8, &outcome, Some(&held), None)["exitCode"],
            serde_json::json!(2)
        );
        // A pane that disappeared ends the wait whatever was asked for.
        assert_eq!(
            evaluate(&mut watch, None, &[Reason::Finished]),
            Some(Outcome {
                reason: Reason::Exit,
                closed: true
            })
        );
    }

    #[test]
    fn own_pane_is_found_by_surface_id_and_an_old_server_is_named() {
        let mut mine = panel(0, None, false);
        mine.surface_id = Some("0x01".to_string());
        let mut other = panel(0, None, false);
        other.pane_id = WireId(9);
        other.surface_id = Some("0x02".to_string());

        assert_eq!(own_pane(&[other.clone(), mine], "0x01"), Some(8));
        assert_eq!(own_pane(&[other.clone()], "0x01"), None);
        assert!(!lacks_surface_ids(&[other]));
        assert!(lacks_surface_ids(&[panel(0, None, false)]));
        assert!(!lacks_surface_ids(&[]));
    }

    // Owner decision (agent-orchestration D9): a worker is always a float,
    // and the first spawn opens the orchestrator's board.
    #[test]
    fn spawn_always_places_a_float_and_opens_the_board() {
        let Ok(Command::Spawn(spawn)) = parse_args(&args(&["spawn", "--", "claude"])) else {
            panic!("spawn did not parse");
        };
        assert!(spawn.board);
        // `--float` from before still parses; it changes nothing.
        assert_eq!(
            parse_args(&args(&["spawn", "--float", "--", "claude"])),
            Ok(Command::Spawn(spawn))
        );
        let Ok(Command::Spawn(quiet)) = parse_args(&args(&["spawn", "--no-board"])) else {
            panic!("spawn --no-board did not parse");
        };
        assert!(!quiet.board);
        for gone in [
            &["spawn", "--tab"][..],
            &["spawn", "--split", "right"],
            &["spawn", "--split", "auto"],
        ] {
            let error = parse_args(&args(gone)).unwrap_err();
            assert!(error.contains("float"), "{gone:?}: {error}");
        }
        assert!(parse_args(&args(&["board", "--open", "--float"])).is_err());
    }

    // Review finding: the board check listed the panes with `?`, so a
    // failed listing failed the spawn. It is a warning, and the worker then
    // floats over the orchestrator (D10).
    #[test]
    fn a_failed_board_check_is_a_warning_and_the_worker_floats_over_its_orchestrator() {
        let (host, warning) = board_before_spawn(
            || Err(Failure::unavailable("lost the connection to Noa")),
            1,
            || panic!("no board opens without a listing"),
        );
        assert_eq!(host, 1);
        assert!(
            warning
                .as_deref()
                .is_some_and(|w| w.contains("lost the connection")),
            "{warning:?}"
        );

        let (host, warning) =
            board_before_spawn(|| Ok(Vec::new()), 1, || Err(Failure::error("no room")));
        assert_eq!(host, 1);
        assert!(warning.is_some_and(|w| w.contains("could not open the board")));
    }

    // agent-orchestration D13: a worker spawning has no board of its own to
    // find or open (the server stows its workers on the board it is on),
    // and cannot opt out of that board.
    #[test]
    fn a_workers_spawn_opens_no_board_and_refuses_no_board() {
        assert_eq!(spawn_opens_board(false, true), Ok(true));
        assert_eq!(spawn_opens_board(false, false), Ok(false));
        assert_eq!(spawn_opens_board(true, true), Ok(false));
        let error = spawn_opens_board(true, false).unwrap_err();
        assert!(
            error.contains("a worker's workers always go on its orchestrator's board"),
            "{error}"
        );
    }

    // D10: the worker is stowed on the board, which opens first when needed.
    #[test]
    fn a_worker_goes_over_its_orchestrators_board() {
        let mut board = panel(0, None, false);
        board.pane_id = WireId(5);
        board.label = Some(board_label(1));
        assert_eq!(
            board_before_spawn(|| Ok(vec![board]), 1, || panic!("already open")),
            (5, None)
        );
        assert_eq!(
            board_before_spawn(|| Ok(Vec::new()), 1, || Ok(9)),
            (9, None)
        );
    }

    // Reported with a screenshot, reproduced in a Noa started without LANG:
    // a worker got no locale, so its status line script (`tr ' ' '█'`) cut
    // the bar into single bytes and the pane showed replacement characters.
    #[test]
    fn a_worker_inherits_the_orchestrators_locale() {
        let orchestrator = || {
            [
                ("LANG", "ja_JP.UTF-8"),
                ("LC_CTYPE", "UTF-8"),
                ("PATH", "/usr/bin"),
                ("HOME", "/Users/me"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
        };
        let env = worker_env(Vec::new(), true, orchestrator());
        assert_eq!(env.get("LANG").map(String::as_str), Some("ja_JP.UTF-8"));
        assert_eq!(env.get("LC_CTYPE").map(String::as_str), Some("UTF-8"));
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert!(!env.contains_key("HOME"));

        // A shell worker gets the locale too, but its shell sets PATH.
        let shell = worker_env(Vec::new(), false, orchestrator());
        assert_eq!(shell.get("LANG").map(String::as_str), Some("ja_JP.UTF-8"));
        assert!(!shell.contains_key("PATH"));

        // `--env` wins.
        let given = vec![("LANG".to_string(), "C".to_string())];
        assert_eq!(
            worker_env(given, true, orchestrator())
                .get("LANG")
                .map(String::as_str),
            Some("C")
        );
    }

    #[test]
    fn an_orchestrators_board_is_found_by_its_label() {
        let mut board = panel(0, None, false);
        board.pane_id = WireId(5);
        board.label = Some(board_label(1));
        let mut other = panel(0, None, false);
        other.label = Some(board_label(2));
        assert_eq!(board_pane(&[other.clone(), board], 1), Some(5));
        assert_eq!(board_pane(&[other], 1), None);
    }

    #[test]
    fn board_runs_here_or_opens_next_to_this_pane() {
        assert_eq!(
            parse_args(&args(&["board"])),
            Ok(Command::Board {
                of: None,
                open: None
            })
        );
        assert_eq!(
            parse_args(&args(&["board", "--of", "3"])),
            Ok(Command::Board {
                of: Some(3),
                open: None
            })
        );
        assert_eq!(
            parse_args(&args(&["board", "--open", "--split", "down"])),
            Ok(Command::Board {
                of: None,
                open: Some(Placement::Down)
            })
        );
        assert_eq!(
            parse_args(&args(&["board", "--open"])),
            Ok(Command::Board {
                of: None,
                open: Some(Placement::Auto)
            })
        );
        assert!(parse_args(&args(&["board", "--tab"])).is_err());
    }

    // Review finding: remote panes have no surface id, so a server whose
    // listed panes were all remote looked like an older Noa, and even `ls`
    // and `read` from a floating pane (never listed) failed.
    #[test]
    fn a_listing_without_surface_ids_fails_only_commands_that_need_the_pane() {
        let remote = panel(0, None, false);
        assert_eq!(own_pane(std::slice::from_ref(&remote), "0x01"), None);
        assert!(lacks_surface_ids(&[remote]));

        let message = not_own_message(Some("0x01"), 61771, true);
        assert!(message.contains("does not report surface ids"), "{message}");
        assert!(
            message.contains("lsof -nP -iTCP:61771 -sTCP:LISTEN"),
            "{message}"
        );
    }

    // Review finding: a script started with Noa (`Noa -e …`) ran `+ctl self`
    // before its pane was first listed and was told it was not in Noa.
    #[test]
    fn a_pane_not_listed_yet_is_looked_up_again() {
        let mut mine = panel(0, None, false);
        mine.surface_id = Some("0x01".to_string());
        let mut other = panel(0, None, false);
        other.pane_id = WireId(9);
        other.surface_id = Some("0x02".to_string());
        let mut listings = vec![
            vec![other.clone()],
            vec![other.clone()],
            vec![other.clone(), mine],
        ]
        .into_iter();
        let mut pauses = 0;

        let own = wait_for_own(
            || Ok(listings.next().unwrap_or_default()),
            "0x01",
            30,
            || pauses += 1,
        );

        assert_eq!(own.ok(), Some((Some(8), false)));
        assert_eq!(pauses, 2);

        // A pane that never shows up gives up after the bound.
        let mut looked = 0;
        let own = wait_for_own(
            || {
                looked += 1;
                Ok(vec![other.clone()])
            },
            "0x01",
            5,
            || {},
        );
        assert_eq!(own.ok(), Some((None, false)));
        assert_eq!(looked, 5);
    }

    // Feedback: an older Noa still held the port, so +ctl reached it and told
    // the user to restart with a version that has +ctl, which they had done.
    #[test]
    fn a_server_without_surface_ids_names_the_port_and_how_to_find_its_owner() {
        let message = not_own_message(Some("0x01"), 61771, true);

        assert!(message.contains("61771"), "{message}");
        assert!(message.contains("another Noa"), "{message}");
        assert!(
            message.contains("lsof -nP -iTCP:61771 -sTCP:LISTEN"),
            "{message}"
        );
    }

    // The same situation with a server that does report surface ids said
    // "not running inside a Noa pane", though NOA_SURFACE_ID was set.
    #[test]
    fn a_pane_missing_from_the_listing_is_not_called_outside_noa() {
        let listed_elsewhere = not_own_message(Some("0x01"), 61771, false);
        assert!(
            !listed_elsewhere.contains("not running inside"),
            "{listed_elsewhere}"
        );
        assert!(listed_elsewhere.contains("0x01"), "{listed_elsewhere}");
        assert!(
            listed_elsewhere.contains("lsof -nP -iTCP:61771 -sTCP:LISTEN"),
            "{listed_elsewhere}"
        );

        assert!(not_own_message(None, 61771, false).contains("NOA_SURFACE_ID is unset"));
    }

    /// A scripted server: `lists` are answered in order (the last one
    /// repeats), and `next` replays `notifications`, then reports idle.
    struct FakeSource {
        lists: Vec<Vec<Panel>>,
        notifications: std::collections::VecDeque<Notification>,
        list_calls: usize,
        /// Each pane's scrollback; a pane missing here is gone.
        texts: HashMap<u64, String>,
    }

    impl WaitSource for FakeSource {
        fn list(&mut self) -> Result<Vec<Panel>, Failure> {
            let index = self.list_calls.min(self.lists.len() - 1);
            self.list_calls += 1;
            Ok(self.lists[index].clone())
        }

        fn next(&mut self, _timeout: Duration) -> Result<Option<Notification>, Failure> {
            match self.notifications.pop_front() {
                Some(notification) => Ok(Some(notification)),
                None => Err(Failure::error("fake source exhausted")),
            }
        }

        fn scrollback(&mut self, pane: u64) -> Result<Option<String>, Failure> {
            Ok(self.texts.get(&pane).cloned())
        }

        fn screen(&mut self, _pane: u64) -> Result<Option<String>, Failure> {
            Ok(None)
        }
    }

    fn with_pane(mut panel: Panel, pane: u64) -> Panel {
        panel.pane_id = WireId(pane);
        panel
    }

    // Review finding: with several panes, a steady stream of notifications
    // for one kept the wait from ever re-listing, so another that vanished
    // (closed, or exited without being held) was never reported.
    #[test]
    fn a_vanished_pane_is_reported_while_another_keeps_notifying() {
        let busy = with_pane(panel(1, Some("running"), false), 1);
        let gone = with_pane(panel(1, Some("running"), false), 2);
        let mut source = FakeSource {
            lists: vec![vec![busy.clone(), gone], vec![busy.clone()]],
            notifications: (0..20)
                .map(|_| Notification::StateChanged {
                    panels: vec![busy.clone()],
                    dropped: false,
                })
                .collect(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();

        let code = wait_loop(
            &mut source,
            &[1, 2],
            WaitStart::Listed {
                after: HashMap::new(),
            },
            &WaitOptions::default(),
            Duration::ZERO,
            &mut |line| lines.push(line.clone()),
        );

        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines[0]["paneId"], serde_json::json!("2"));
        assert_eq!(lines[0]["closed"], serde_json::json!(true));
    }

    // Review finding: a scheduled re-list ran before the notification it
    // followed was evaluated, so a pushed `attention: false` that a later
    // listing already showed as true again never armed the wait.
    #[test]
    fn a_pushed_state_is_evaluated_before_a_relist_overwrites_it() {
        let raised = with_pane(panel(0, None, true), 8);
        let lowered = with_pane(panel(0, None, false), 8);
        let mut source = FakeSource {
            lists: vec![vec![raised.clone()]],
            notifications: [lowered, raised.clone()]
                .into_iter()
                .map(|panel| Notification::StateChanged {
                    panels: vec![panel],
                    dropped: false,
                })
                .collect(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();

        let code = wait_loop(
            &mut source,
            &[8],
            WaitStart::Listed {
                after: HashMap::new(),
            },
            &WaitOptions::default(),
            Duration::ZERO,
            &mut |line| lines.push(line.clone()),
        );

        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines[0]["reason"], serde_json::json!("attention"));
    }

    // Review finding: a notification flagged `dropped` was ignored whole,
    // though only earlier ones were lost; its own `attention: false` was
    // overwritten by the re-list and the wait never armed.
    #[test]
    fn a_dropped_flagged_notification_still_carries_its_own_state() {
        let raised = with_pane(panel(0, None, true), 8);
        let lowered = with_pane(panel(0, None, false), 8);
        let mut source = FakeSource {
            lists: vec![vec![raised.clone()]],
            notifications: [(lowered, true), (raised.clone(), false)]
                .into_iter()
                .map(|(panel, dropped)| Notification::StateChanged {
                    panels: vec![panel],
                    dropped,
                })
                .collect(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();

        let code = wait_loop(
            &mut source,
            &[8],
            WaitStart::Listed {
                after: HashMap::new(),
            },
            &WaitOptions::default(),
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );

        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines[0]["reason"], serde_json::json!("attention"));
    }

    // Review finding: `send --wait` built its baseline from the listing taken
    // after typing, so an `attention` the input raised before that listing
    // looked pre-existing and never ended the wait.
    #[test]
    fn send_wait_counts_attention_raised_right_after_sending() {
        let before = with_pane(panel(5, Some("finished"), false), 8);
        let after = with_pane(panel(5, Some("finished"), true), 8);
        let mut source = FakeSource {
            lists: vec![vec![after]],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();

        let code = wait_loop(
            &mut source,
            &[8],
            send_wait_start(before, 5),
            &WaitOptions {
                timeout: Some(Duration::ZERO),
                ..WaitOptions::default()
            },
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );

        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines[0]["reason"], serde_json::json!("attention"));
    }

    // Review finding: a held-open exited worker never reports again, so a
    // wait without `exit` in `--for` blocked forever, unlike a vanished pane.
    #[test]
    fn a_held_exited_pane_ends_the_wait_whatever_was_asked_for() {
        let mut held = panel(3, Some("running"), false);
        held.exit = Some(PanelExit { code: Some(137) });
        let mut watch = Watch::new(8, Some(&held), None);

        assert_eq!(
            evaluate(&mut watch, Some(&held), &[Reason::Finished, Reason::Error]),
            Some(Outcome {
                reason: Reason::Exit,
                closed: false
            })
        );
    }

    // Review finding: a worker that had already exited (held open) ended
    // every later `wait --children` at once, so the others were never waited on.
    #[test]
    fn a_pane_already_exited_does_not_end_a_wait_on_others() {
        let mut held = with_pane(panel(1, Some("running"), false), 1);
        held.exit = Some(PanelExit { code: Some(0) });
        let busy = with_pane(panel(1, Some("running"), false), 2);
        let finished = with_pane(panel(2, Some("finished"), false), 2);
        let mut source = FakeSource {
            lists: vec![vec![held.clone(), busy]],
            notifications: [Notification::StateChanged {
                panels: vec![held, finished],
                dropped: false,
            }]
            .into(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();

        let code = wait_loop(
            &mut source,
            &[1, 2],
            WaitStart::Listed {
                after: HashMap::new(),
            },
            &WaitOptions::default(),
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );

        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["paneId"], "2");
        assert_eq!(lines[0]["reason"], "finished");
    }

    // With nothing else left to wait for, an already exited pane still ends
    // the wait rather than leaving it to block forever.
    #[test]
    fn panes_that_all_exited_before_the_wait_still_end_it() {
        let mut held = with_pane(panel(1, Some("running"), false), 1);
        held.exit = Some(PanelExit { code: Some(3) });
        let mut source = FakeSource {
            lists: vec![vec![held]],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();

        let code = wait_loop(
            &mut source,
            &[1],
            WaitStart::Listed {
                after: HashMap::new(),
            },
            &WaitOptions::default(),
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );

        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["reason"], "exit");
        assert_eq!(lines[0]["exitCode"], 3);
    }

    // Review finding: `wait 8 8 --all` watched pane 8 twice but counted it
    // done once, so it never completed.
    #[test]
    fn a_pane_named_twice_is_waited_on_once() {
        let finished = with_pane(panel(2, Some("finished"), false), 8);
        let mut source = FakeSource {
            lists: vec![vec![finished]],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();

        let code = wait_loop(
            &mut source,
            &[8, 8],
            WaitStart::Listed {
                after: HashMap::from([(8, 1)]),
            },
            &WaitOptions {
                all: true,
                ..WaitOptions::default()
            },
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );

        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines.len(), 1);
    }

    // Review finding: a finite but huge `--timeout` panicked, either in the
    // seconds conversion or when adding it to `Instant::now()`.
    #[test]
    fn a_huge_timeout_is_an_error_or_no_deadline_but_never_a_panic() {
        assert!(parse_args(&args(&["wait", "8", "--timeout", "1e20"])).is_err());

        let Command::Wait(wait) = parse_args(&args(&["wait", "8", "--timeout", "1e18"])).unwrap()
        else {
            panic!("wait expected");
        };
        let mut source = FakeSource {
            lists: vec![vec![with_pane(panel(2, Some("finished"), false), 8)]],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let code = wait_loop(
            &mut source,
            &[8],
            WaitStart::Listed {
                after: HashMap::from([(8, 1)]),
            },
            &wait.options,
            WAIT_RELIST_INTERVAL,
            &mut |_| {},
        );
        assert_eq!(code.ok(), Some(EXIT_OK));
    }

    // A wait that timed out is carried on with `--after PANE=SEQ`: the
    // timeout line names each pending pane's baseline, and a report that
    // landed between the two waits ends the second one.
    #[test]
    fn a_timeout_prints_baselines_that_a_later_wait_carries_on_from() {
        let running = |pane| with_pane(panel(3, Some("running"), false), pane);
        let mut source = FakeSource {
            lists: vec![vec![running(8), running(9)]],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();
        let code = wait_loop(
            &mut source,
            &[8, 9],
            WaitStart::Listed {
                after: HashMap::from([(8, 2)]),
            },
            &WaitOptions {
                timeout: Some(Duration::ZERO),
                ..WaitOptions::default()
            },
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );

        assert_eq!(code.ok(), Some(EXIT_TIMEOUT));
        assert_eq!(
            lines,
            vec![serde_json::json!({
                "timeout": true,
                "pending": ["8", "9"],
                "baseline": [
                    {"paneId": "8", "seq": 2, "state": "running"},
                    {"paneId": "9", "seq": 3, "state": "running"},
                ],
            })]
        );

        // Pane 9 finished before the second wait listed it.
        let mut source = FakeSource {
            lists: vec![vec![
                running(8),
                with_pane(panel(4, Some("finished"), false), 9),
            ]],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();
        let code = wait_loop(
            &mut source,
            &[8, 9],
            WaitStart::Listed {
                after: HashMap::from([(8, 2), (9, 3)]),
            },
            &WaitOptions::default(),
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines[0]["paneId"], serde_json::json!("9"));
    }

    // Review finding: `wait --children --after 9=4` with only pane 8 as a
    // child ignored the baseline for 9, so a mistyped id silently waited on 8
    // from its current report count and missed a report already in.
    #[test]
    fn a_baseline_for_a_pane_not_waited_on_is_refused_after_children_resolve() {
        assert!(wait_baselines(&[8], &[(9, 4)]).is_err());
        assert_eq!(
            wait_baselines(&[8, 9], &[(9, 4)]),
            Ok(HashMap::from([(9, 4)]))
        );
    }

    struct FakeCloser {
        panels: Vec<Panel>,
        refuse_code: i64,
    }

    impl PaneCloser for FakeCloser {
        fn list(&mut self) -> Result<Vec<Panel>, ClientError> {
            Ok(self.panels.clone())
        }

        fn close(&mut self, _pane: u64) -> Result<(), ClientError> {
            Err(ClientError::Rpc {
                code: self.refuse_code,
                message: "refused".to_string(),
            })
        }
    }

    // Review finding: a server without the control scope refused closePane
    // with -32003, which close reported as a failed pane (exit 1) instead of
    // the missing scope (exit 3 and the config hint).
    #[test]
    fn a_missing_scope_on_close_is_unavailable_not_a_failed_pane() {
        let mut source = FakeCloser {
            panels: vec![worker(8, 1)],
            refuse_code: -32003,
        };
        let failure =
            close_panes(&mut source, &[8], Some(1), &mut |_, _| None, &mut |_| {}).unwrap_err();
        assert_eq!(failure.code, EXIT_UNAVAILABLE);

        // Any other refusal of a pane that is still listed fails that pane only.
        source.refuse_code = -32000;
        let mut lines = Vec::new();
        let code = close_panes(&mut source, &[8], Some(1), &mut |_, _| None, &mut |line| {
            lines.push(line.clone())
        });
        assert_eq!(code.ok(), Some(EXIT_ERROR));
        assert!(lines.is_empty());
    }

    // Review finding: the docs said a wait carried on from `baseline`
    // misses nothing, but a notification raised between the two waits is
    // already on when the second starts, so it does not end that wait:
    // `--after` covers reports only.
    #[test]
    fn a_notification_raised_between_two_waits_does_not_end_the_second() {
        let mut source = FakeSource {
            lists: vec![vec![with_pane(panel(3, None, true), 8)]],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let code = wait_loop(
            &mut source,
            &[8],
            WaitStart::Listed {
                after: HashMap::from([(8, 3)]),
            },
            &WaitOptions {
                timeout: Some(Duration::ZERO),
                ..WaitOptions::default()
            },
            WAIT_RELIST_INTERVAL,
            &mut |_| {},
        );
        assert_eq!(code.ok(), Some(EXIT_TIMEOUT));
    }

    fn worker(pane: u64, parent: u64) -> Panel {
        let mut worker = with_pane(panel(0, None, false), pane);
        worker.parent_pane_id = Some(WireId(parent));
        worker
    }

    #[test]
    fn close_skips_panes_that_are_gone_and_refuses_live_strangers() {
        let panels = vec![worker(8, 1), worker(9, 1), worker(5, 2)];

        // 7 is gone (closed by hand, or it exited unheld): not an error.
        assert_eq!(
            close_targets(&panels, &[8, 7, 9, 8], Some(1)),
            Ok(vec![(8, true), (7, false), (9, true)])
        );
        // A live pane of another orchestrator is still a wrong id.
        assert!(close_targets(&panels, &[8, 5], Some(1)).is_err());
        assert_eq!(
            close_targets(&panels, &[5, 7], None),
            Ok(vec![(5, true), (7, false)])
        );
    }

    // Only `permission` is refused: `input` also means an agent idle at its
    // prompt (`scripts/noa-agent-hook.py` maps `idle_prompt` to it).
    #[test]
    fn only_a_reported_permission_awaits_an_answer() {
        assert_eq!(
            awaits_answer(&panel(2, Some("permission"), false)),
            Some("an approval")
        );
        assert_eq!(awaits_answer(&panel(2, Some("input"), false)), None);
        assert_eq!(awaits_answer(&panel(2, Some("running"), true)), None);
        assert_eq!(awaits_answer(&panel(2, Some("finished"), false)), None);
        assert_eq!(awaits_answer(&panel(0, None, false)), None);
        assert!(matches!(
            parse_args(&args(&["send", "8", "--answer", "--enter", "y"])).unwrap(),
            Command::Send(SendArgs { answer: true, .. })
        ));
    }

    #[test]
    fn replace_clears_the_input_line_first_and_not_a_prompt() {
        assert!(matches!(
            parse_args(&args(&["send", "8", "--replace", "--enter", "go"])).unwrap(),
            Command::Send(SendArgs { replace: true, .. })
        ));
        let err = parse_args(&args(&["send", "8", "--replace", "--answer", "y"])).unwrap_err();
        assert!(err.contains("--answer"), "{err}");

        let writes = replace_keys();
        assert_eq!(writes[0], "\x05");
        assert!(writes[1..].iter().all(|keys| {
            !keys.is_empty() && keys.len() <= REPLACE_CHUNK && keys.chars().all(|c| c == '\x15')
        }));
        let clears: usize = writes[1..].iter().map(String::len).sum();
        assert_eq!(clears, 2 * REPLACE_MAX_LINES);
    }

    // Review finding: text that starts with `--` or equals a flag name could
    // not be sent; `--` now ends the options.
    #[test]
    fn text_after_double_dash_is_sent_verbatim() {
        let Command::Send(send) = parse_args(&args(&["send", "8", "--", "--enter"])).unwrap()
        else {
            panic!("send expected");
        };
        assert_eq!(send.text, TextArg::Literal("--enter".to_string()));
        assert!(!send.enter);

        let Command::Send(send) =
            parse_args(&args(&["send", "8", "--enter", "--", "--fix the tests"])).unwrap()
        else {
            panic!("send expected");
        };
        assert_eq!(send.text, TextArg::Literal("--fix the tests".to_string()));
        assert!(send.enter);

        assert_eq!(
            parse_args(&args(&["report", "input", "--", "--verbose?"])).unwrap(),
            Command::Report {
                state: "input".to_string(),
                detail: "--verbose?".to_string()
            }
        );
    }

    // Review finding: losing the server mid-command (it stopped or
    // restarted during a wait) exited 1 like a bad argument, not 3.
    #[test]
    fn a_lost_connection_exits_as_unavailable() {
        for error in [
            ClientError::Closed("server shutdown".to_string()),
            ClientError::Transport("connection reset".to_string()),
        ] {
            assert_eq!(Failure::from(error).code, EXIT_UNAVAILABLE);
        }
        assert_eq!(
            Failure::from(ClientError::Rpc {
                code: -32602,
                message: "invalid params".to_string()
            })
            .code,
            EXIT_ERROR
        );
    }

    // Review finding: a relative `--cwd` reached the GUI process as is, and
    // was resolved against Noa's own cwd (`/` for an app bundle) instead of
    // the orchestrator's.
    #[test]
    fn a_relative_cwd_is_resolved_against_the_orchestrators_cwd() {
        let here = Some(std::path::PathBuf::from("/repo"));

        assert_eq!(
            spawn_cwd(Some("crates/noa-vt".to_string()), here.clone()).as_deref(),
            Some("/repo/crates/noa-vt")
        );
        assert_eq!(
            spawn_cwd(Some("/abs".to_string()), here.clone()).as_deref(),
            Some("/abs")
        );
        assert_eq!(spawn_cwd(None, here).as_deref(), Some("/repo"));
    }

    fn transport(code: i32) -> ClientError {
        ClientError::Transport(std::io::Error::from_raw_os_error(code).to_string())
    }

    // Review finding (F4/skill F2): a shell sandbox refuses the loopback
    // connect with EPERM, and the message read like a disabled server.
    #[test]
    fn a_refused_connect_names_the_likely_cause() {
        let url = "ws://127.0.0.1:61771/";
        let sandboxed = connect_failure_message(url, &transport(libc::EPERM));
        assert!(sandboxed.contains("sandbox"), "{sandboxed}");
        assert!(sandboxed.contains("127.0.0.1:61771"), "{sandboxed}");
        assert!(sandboxed.contains("outside the sandbox"), "{sandboxed}");

        let refused = connect_failure_message(url, &transport(libc::ECONNREFUSED));
        assert!(refused.contains("not listening"), "{refused}");
        assert!(refused.contains("server-enable"), "{refused}");
        assert!(!refused.contains("sandbox"), "{refused}");

        let other = connect_failure_message(url, &transport(libc::ETIMEDOUT));
        assert!(!other.contains("sandbox") && !other.contains("not listening"));
    }

    fn spawn_args(cwd: Option<&str>, command: &[&str]) -> SpawnArgs {
        SpawnArgs {
            board: true,
            cwd: cwd.map(str::to_string),
            env: Vec::new(),
            label: None,
            input: None,
            wait_after: true,
            focus: false,
            max_children: DEFAULT_MAX_CHILDREN,
            worktree: false,
            tier: None,
            command: args(command),
        }
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("noa-ctl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Review finding (F1): a `--cwd` that did not exist ran the worker in
    // `$HOME` with exit 0.
    #[test]
    fn a_cwd_that_is_not_a_directory_is_refused_before_spawning() {
        let here = temp_dir("cwd");
        std::fs::create_dir(here.join("sub")).unwrap();
        std::fs::write(here.join("file"), "").unwrap();

        assert_eq!(
            check_spawn(&spawn_args(Some("sub"), &[]), Some(here.clone()), None),
            Ok(())
        );
        assert_eq!(
            check_spawn(&spawn_args(None, &[]), Some(here.clone()), None),
            Ok(())
        );
        let missing = check_spawn(
            &spawn_args(Some("/nonexistent/dir"), &[]),
            Some(here.clone()),
            None,
        )
        .unwrap_err();
        assert_eq!(missing, r#"--cwd "/nonexistent/dir" is not a directory"#);
        let file =
            check_spawn(&spawn_args(Some("file"), &[]), Some(here.clone()), None).unwrap_err();
        assert!(file.starts_with(r#"--cwd "file" (resolved to "#), "{file}");
        assert!(file.ends_with("is not a directory"), "{file}");
        let _ = std::fs::remove_dir_all(here);
    }

    // Review finding (F4): a command missing from the worker's PATH came
    // back as "internal error: the float could not open".
    #[cfg(unix)]
    #[test]
    fn a_command_missing_from_the_workers_path_is_refused_before_spawning() {
        use std::os::unix::fs::PermissionsExt;
        let bin = temp_dir("path");
        let tool = bin.join("tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(bin.join("data"), "").unwrap();
        let path = format!("/nonexistent:{}", bin.display());
        let check = |command: &[&str], path: Option<&str>| {
            check_spawn(&spawn_args(None, command), Some(bin.clone()), path)
        };

        assert_eq!(check(&["tool", "--flag"], Some(&path)), Ok(()));
        assert_eq!(
            check(&["definitely-not-a-cmd-xyz"], Some(&path)),
            Err(r#"command not found on PATH: "definitely-not-a-cmd-xyz""#.to_string())
        );
        // Not executable.
        assert!(check(&["data"], Some(&path)).is_err());
        // A path is the worker's to resolve, and so is a PATH this process
        // does not send (the worker gets Noa's own).
        assert_eq!(check(&["./anything"], Some(&path)), Ok(()));
        assert_eq!(check(&["definitely-not-a-cmd-xyz"], None), Ok(()));
        // A relative PATH entry is taken from the worker's cwd.
        assert_eq!(check(&["tool"], Some(".")), Ok(()));
        // A shell worker has no command to look up.
        assert_eq!(check(&[], Some("/nonexistent")), Ok(()));
        let _ = std::fs::remove_dir_all(bin);
    }

    // Review finding (F7/skill F5): exited workers held open keep their slot,
    // and the error suggested raising --max-children past the board's size
    // (20 since D12).
    #[test]
    fn the_worker_limit_counts_exited_workers_and_stops_at_the_boards_size() {
        let at_most = max_children_message(20, 5, 20);
        assert!(
            at_most.contains("20 workers, 5 of them exited"),
            "{at_most}"
        );
        assert!(
            at_most.contains("close finished ones after reading them"),
            "{at_most}"
        );
        assert!(!at_most.contains("raise"), "{at_most}");

        let lowered = max_children_message(2, 0, 2);
        assert!(!lowered.contains("exited"), "{lowered}");
        assert!(
            lowered.contains("raise --max-children (at most 20)"),
            "{lowered}"
        );

        let parsed = |value: &str| match parse_args(&args(&["spawn", "--max-children", value])) {
            Ok(Command::Spawn(spawn)) => Ok(spawn.max_children),
            Ok(_) => panic!("spawn expected"),
            Err(error) => Err(error),
        };
        assert_eq!(parsed("20"), Ok(20));
        assert_eq!(parsed("3"), Ok(3));
        let above = parsed("21").unwrap_err();
        assert!(above.contains("holds 20"), "{above}");
    }

    // Review finding (F8/skill F8): input to an exited, held-open worker was
    // refused as "pane closed", though its output is still there to read.
    #[test]
    fn input_to_an_exited_pane_says_it_exited() {
        let mut held = panel(0, None, false);
        held.exit = Some(PanelExit { code: Some(3) });
        let failure = exited_input_failure(8, Some(&held)).unwrap();
        assert_eq!(failure.code, EXIT_ERROR);
        assert_eq!(
            failure.message,
            "pane 8's process has exited (code 3); it takes no input — read it, then close it"
        );
        held.exit = Some(PanelExit { code: None });
        assert!(
            exited_input_failure(8, Some(&held))
                .unwrap()
                .message
                .starts_with("pane 8's process has exited; ")
        );
        // Still running, or gone: the server's own answer stands.
        assert!(exited_input_failure(8, Some(&panel(0, None, false))).is_none());
        assert!(exited_input_failure(8, None).is_none());
    }

    // Review finding (F9/skill F11): `read` printed the screen's blank rows
    // below the output.
    #[test]
    fn trailing_blank_lines_are_trimmed_and_a_tail_keeps_the_last_lines() {
        assert_eq!(
            trim_trailing_blank_lines("out\n\n[exited]\n   \n\n\n"),
            "out\n\n[exited]\n"
        );
        assert_eq!(trim_trailing_blank_lines("a\nb"), "a\nb");
        assert_eq!(trim_trailing_blank_lines("\n  \n"), "");
        assert_eq!(tail_lines("1\n2\n3\n4\n\n  \n", 2), "3\n4");
        assert_eq!(tail_lines("1\n2\n", 5), "1\n2");
        assert_eq!(tail_lines("1\n2\n", 0), "");
        assert_eq!(
            tail_lines(
                "one\ntwo\n\n[Process exited. Press any key to close the terminal.]\n",
                2
            ),
            "one\ntwo"
        );
        assert_eq!(
            truncated_message(65536),
            "Noa +ctl: output truncated; kept the last 65536 bytes"
        );
    }

    // Review finding (F10): an empty id (a failed spawn) printed `got ` and
    // the whole usage, so the cause was invisible.
    #[test]
    fn argument_errors_quote_the_value_and_hint_at_a_failed_spawn() {
        for empty in ["", "null"] {
            let error = parse_args(&args(&["wait", empty, "--timeout", "5"])).unwrap_err();
            assert_eq!(
                error,
                format!("PANE expects a number, got {empty:?} (did the previous spawn fail?)")
            );
        }
        assert_eq!(
            parse_args(&args(&["read", "x"])).unwrap_err(),
            r#"PANE expects a number, got "x""#
        );
        assert_eq!(
            parse_args(&args(&["wait", "8", "--timeout", "soon"])).unwrap_err(),
            r#"--timeout expects seconds, got "soon""#
        );
        assert_eq!(
            parse_args(&args(&["frob"])).unwrap_err(),
            r#"unknown command: "frob""#
        );
        assert!(!USAGE_HINT.contains('\n'));
        assert!(USAGE_HINT.contains("Noa +ctl --help"));
    }

    #[test]
    fn wait_and_send_wait_take_a_tail() {
        let Command::Wait(wait) = parse_args(&args(&["wait", "8", "--tail", "20"])).unwrap() else {
            panic!("wait expected");
        };
        assert_eq!(wait.options.tail, Some(20));
        let Command::Send(send) =
            parse_args(&args(&["send", "8", "--wait", "--tail", "5", "go"])).unwrap()
        else {
            panic!("send expected");
        };
        assert_eq!(send.wait.and_then(|wait| wait.tail), Some(5));
        assert!(parse_args(&args(&["send", "8", "--tail", "5", "go"])).is_err());
        assert!(parse_args(&args(&["wait", "8", "--tail", "x"])).is_err());
    }

    // Review finding (F2): `wait 999999` returned at once with exit 0 and
    // `reason:"exit"`, though the pane never existed.
    #[test]
    fn a_named_pane_missing_from_the_first_listing_fails_the_wait() {
        let running = with_pane(panel(1, Some("running"), false), 8);
        let source = || FakeSource {
            lists: vec![vec![running.clone()]],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();
        let failure = wait_loop(
            &mut source(),
            &[8, 999999],
            WaitStart::Named {
                after: HashMap::new(),
            },
            &WaitOptions::default(),
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        )
        .unwrap_err();
        assert_eq!(failure.code, EXIT_ERROR);
        assert_eq!(failure.message, "pane 999999 is not listed");
        assert!(lines.is_empty());

        // `--children` resolved the pane from a listing just before, so one
        // gone by the wait's first listing closed in between.
        let code = wait_loop(
            &mut source(),
            &[8, 999999],
            WaitStart::Listed {
                after: HashMap::new(),
            },
            &WaitOptions::default(),
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines[0]["closed"], true);
    }

    // A named pane that vanishes during the wait still ends it as closed.
    #[test]
    fn a_named_pane_that_vanishes_later_is_reported_closed() {
        let running = with_pane(panel(1, Some("running"), false), 8);
        let mut source = FakeSource {
            lists: vec![vec![running], Vec::new()],
            notifications: [Notification::Other {
                method: "noa.other".to_string(),
                dropped: true,
            }]
            .into(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();
        let code = wait_loop(
            &mut source,
            &[8],
            WaitStart::Named {
                after: HashMap::new(),
            },
            &WaitOptions::default(),
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines[0]["reason"], "exit");
        assert_eq!(lines[0]["closed"], true);
    }

    // Review finding (F11): a pane that had exited before the wait was
    // listed as `pending`, and no entry said what state a pane was in.
    #[test]
    fn a_timeout_lists_panes_exited_before_the_wait_apart_with_each_state() {
        let mut held = with_pane(panel(0, None, false), 151);
        held.exit = Some(PanelExit { code: Some(0) });
        let mut source = FakeSource {
            lists: vec![vec![
                with_pane(panel(1, Some("finished"), false), 149),
                held,
            ]],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();
        let code = wait_loop(
            &mut source,
            &[149, 151],
            WaitStart::Listed {
                after: HashMap::new(),
            },
            &WaitOptions {
                timeout: Some(Duration::ZERO),
                ..WaitOptions::default()
            },
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_TIMEOUT));
        assert_eq!(
            lines,
            vec![serde_json::json!({
                "timeout": true,
                "pending": ["149"],
                "exited": ["151"],
                "baseline": [
                    {"paneId": "149", "seq": 1, "state": "finished"},
                    {"paneId": "151", "seq": 0},
                ],
            })]
        );
    }

    // Review finding (F8): every outcome needed a separate `read` to be
    // useful; `--tail` adds the output on the same connection.
    #[test]
    fn a_tail_is_added_to_each_open_panes_outcome() {
        let finished = with_pane(panel(2, Some("finished"), false), 8);
        let mut source = FakeSource {
            lists: vec![vec![finished.clone()], Vec::new()],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::from([(8, "a\nb\nc\n\n\n".to_string())]),
        };
        let mut lines = Vec::new();
        let options = WaitOptions {
            tail: Some(2),
            ..WaitOptions::default()
        };
        let code = wait_loop(
            &mut source,
            &[8],
            WaitStart::Listed {
                after: HashMap::from([(8, 1)]),
            },
            &options,
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines[0]["tail"], "b\nc");

        // A closed pane has nothing left to read.
        let mut source = FakeSource {
            lists: vec![Vec::new()],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::from([(8, "stale".to_string())]),
        };
        let mut lines = Vec::new();
        let code = wait_loop(
            &mut source,
            &[8],
            WaitStart::Listed {
                after: HashMap::new(),
            },
            &options,
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines[0]["closed"], true);
        assert!(lines[0].get("tail").is_none());
    }

    #[test]
    fn help_is_only_read_in_the_command_position() {
        assert!(wants_help(&args(&["--help"])));
        assert!(wants_help(&args(&["help"])));
        assert!(!wants_help(&args(&["spawn", "--", "claude", "--help"])));
        assert!(!wants_help(&args(&["send", "8", "-h"])));
    }

    #[test]
    fn control_url_reaches_an_unspecified_bind_over_loopback() {
        assert_eq!(control_url("0.0.0.0", 61771), "ws://127.0.0.1:61771/");
        assert_eq!(control_url("127.0.0.1", 9), "ws://127.0.0.1:9/");
        assert_eq!(control_url("::1", 9), "ws://[::1]:9/");
        assert_eq!(control_url("::", 9), "ws://[::1]:9/");
    }

    /// A listener on `[::]` that refuses IPv4 (`IPV6_V6ONLY`, the default
    /// under Linux `net.ipv6.bindv6only=1`).
    #[cfg(unix)]
    fn v6only_unspecified_listener() -> std::net::TcpListener {
        use std::os::fd::FromRawFd;
        // SAFETY: plain socket syscalls on a fresh fd that this function owns
        // and hands to `TcpListener`; every return value is checked.
        unsafe {
            let fd = libc::socket(libc::AF_INET6, libc::SOCK_STREAM, 0);
            assert!(fd >= 0, "socket");
            let on: libc::c_int = 1;
            assert_eq!(
                libc::setsockopt(
                    fd,
                    libc::IPPROTO_IPV6,
                    libc::IPV6_V6ONLY,
                    (&on as *const libc::c_int).cast(),
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                ),
                0
            );
            let mut addr: libc::sockaddr_in6 = std::mem::zeroed();
            addr.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            assert_eq!(
                libc::bind(
                    fd,
                    (&addr as *const libc::sockaddr_in6).cast(),
                    std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                ),
                0
            );
            assert_eq!(libc::listen(fd, 1), 0);
            std::net::TcpListener::from_raw_fd(fd)
        }
    }

    // Review finding: `server-bind = ::` was reached over 127.0.0.1, which a
    // v6-only socket does not accept, so every `+ctl` failed with exit 3.
    #[cfg(unix)]
    #[test]
    fn an_ipv6_unspecified_bind_is_reached_over_ipv6_loopback() {
        let listener = v6only_unspecified_listener();
        let port = listener.local_addr().unwrap().port();

        let url = control_url("::", port);
        let host = url.trim_start_matches("ws://").trim_end_matches('/');
        let addr: std::net::SocketAddr = host.parse().unwrap();

        assert!(
            std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok(),
            "{url} does not reach a v6-only listener on [::]:{port}"
        );
    }

    // `--stall`: a source whose idle ticks advance a fake clock by a second
    // and whose screen text is a function of that clock.
    struct StallSource {
        inner: FakeSource,
        now: std::rc::Rc<std::cell::Cell<u64>>,
        /// The screen at second N (the last entry repeats).
        screens: Vec<&'static str>,
        /// Notifications already received, returned before any wait.
        queued: std::collections::VecDeque<Notification>,
        /// Received while the screen is read at or after second N.
        during_screen: Option<(u64, Notification)>,
    }

    impl WaitSource for StallSource {
        fn list(&mut self) -> Result<Vec<Panel>, Failure> {
            self.inner.list()
        }

        fn next(&mut self, timeout: Duration) -> Result<Option<Notification>, Failure> {
            if let Some(notification) = self.queued.pop_front() {
                return Ok(Some(notification));
            }
            if timeout.is_zero() {
                return Ok(None);
            }
            if self.now.get() >= 100 {
                return Err(Failure::error("never stalled"));
            }
            self.now.set(self.now.get() + 1);
            Ok(None)
        }

        fn scrollback(&mut self, pane: u64) -> Result<Option<String>, Failure> {
            self.inner.scrollback(pane)
        }

        fn screen(&mut self, _pane: u64) -> Result<Option<String>, Failure> {
            if self
                .during_screen
                .as_ref()
                .is_some_and(|(at, _)| self.now.get() >= *at)
                && let Some((_, notification)) = self.during_screen.take()
            {
                self.queued.push_back(notification);
            }
            let index = (self.now.get() as usize).min(self.screens.len() - 1);
            Ok(Some(self.screens[index].to_string()))
        }
    }

    fn run_stall_wait(
        lists: Vec<Vec<Panel>>,
        screens: Vec<&'static str>,
        stall_secs: u64,
    ) -> (Result<i32, Failure>, Vec<serde_json::Value>) {
        run_stall_wait_from(lists, screens, stall_secs, HashMap::new(), 0)
    }

    /// `after` is the report baseline; `jump` seconds pass between the
    /// wait's start and its first look at the screen.
    fn run_stall_wait_from(
        lists: Vec<Vec<Panel>>,
        screens: Vec<&'static str>,
        stall_secs: u64,
        after: HashMap<u64, u64>,
        jump: u64,
    ) -> (Result<i32, Failure>, Vec<serde_json::Value>) {
        run_stall_wait_with(lists, screens, stall_secs, after, jump, None)
    }

    /// `during_screen`: a notification that arrives while the screen is
    /// read at or after that second.
    fn run_stall_wait_with(
        lists: Vec<Vec<Panel>>,
        screens: Vec<&'static str>,
        stall_secs: u64,
        after: HashMap<u64, u64>,
        jump: u64,
        during_screen: Option<(u64, Notification)>,
    ) -> (Result<i32, Failure>, Vec<serde_json::Value>) {
        let now = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut source = StallSource {
            inner: FakeSource {
                lists,
                notifications: Default::default(),
                list_calls: 0,
                texts: HashMap::new(),
            },
            now: now.clone(),
            screens,
            queued: Default::default(),
            during_screen,
        };
        let base = Instant::now();
        let mut calls = 0;
        let mut lines = Vec::new();
        let options = WaitOptions {
            reasons: vec![Reason::Finished, Reason::Stalled],
            stall: Some(Duration::from_secs(stall_secs)),
            ..WaitOptions::default()
        };
        let code = wait_loop_with_clock(
            &mut source,
            &[8],
            WaitStart::Listed { after },
            &options,
            Duration::ZERO,
            &mut |line| lines.push(line.clone()),
            &mut || {
                let skew = if calls > 0 { jump } else { 0 };
                calls += 1;
                base + Duration::from_secs(now.get() + skew)
            },
        );
        (code, lines)
    }

    #[test]
    fn an_unchanging_screen_ends_the_wait_as_stalled_after_the_stall_time() {
        let (code, lines) = run_stall_wait(
            vec![vec![panel(0, Some("running"), false)]],
            vec!["same"],
            3,
        );

        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["reason"], serde_json::json!("stalled"));
        assert_eq!(lines[0]["quietSecs"], serde_json::json!(3));
        assert_eq!(lines[0]["paneId"], serde_json::json!("8"));
    }

    #[test]
    fn a_changing_screen_restarts_the_quiet_time() {
        // Changes at second 2, so the quiet time is 3s only at second 5.
        let (code, lines) = run_stall_wait(
            vec![vec![panel(0, Some("running"), false)]],
            vec!["a", "b", "c"],
            3,
        );

        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines[0]["reason"], serde_json::json!("stalled"));
        assert_eq!(lines[0]["quietSecs"], serde_json::json!(3));

        // A screen that never stops changing never stalls.
        let screens: Vec<&'static str> = (0..200)
            .map(|n| if n % 2 == 0 { "x" } else { "y" })
            .collect();
        let (code, lines) =
            run_stall_wait(vec![vec![panel(0, Some("running"), false)]], screens, 3);
        assert!(code.is_err());
        assert!(lines.is_empty());
    }

    #[test]
    fn a_report_wins_over_a_stall_in_the_same_pass() {
        // The report is already listed when the screen is first looked at,
        // and by then the quiet time is long past.
        let (code, lines) = run_stall_wait_from(
            vec![vec![panel(1, Some("finished"), false)]],
            vec!["same"],
            1,
            HashMap::from([(8, 0)]),
            10,
        );

        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["reason"], serde_json::json!("finished"));
        assert!(lines[0].get("quietSecs").is_none());
    }

    // Review finding: an idle tick schedules a re-list, but the stall was
    // judged on the stale listing first, so a worker that had exited in
    // the meantime read as stalled.
    #[test]
    fn an_exit_seen_by_the_pending_relist_wins_over_a_stall() {
        let mut exited = panel(0, Some("running"), false);
        exited.exit = Some(PanelExit { code: Some(0) });
        let (code, lines) = run_stall_wait(
            vec![vec![panel(0, Some("running"), false)], vec![exited]],
            vec!["same"],
            1,
        );

        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["reason"], serde_json::json!("exit"));
    }

    // Review finding: a report received while the screens were read sat in
    // the client's queue, and the stall was printed from the older state.
    #[test]
    fn a_report_received_while_reading_the_screen_wins_over_a_stall() {
        let finished = panel(1, Some("finished"), false);
        let (code, lines) = run_stall_wait_with(
            vec![vec![panel(0, Some("running"), false)]],
            vec!["same"],
            3,
            HashMap::new(),
            0,
            Some((
                3,
                Notification::StateChanged {
                    panels: vec![finished],
                    dropped: false,
                },
            )),
        );

        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["reason"], serde_json::json!("finished"));
        assert_eq!(lines[0]["seq"], serde_json::json!(1));
    }

    #[test]
    fn stall_is_parsed_and_stalled_needs_it() {
        let wait = |items: &[&str]| match parse_args(&args(items)) {
            Ok(Command::Wait(wait)) => Ok(wait.options),
            Ok(other) => panic!("not a wait: {other:?}"),
            Err(message) => Err(message),
        };

        let options = wait(&["wait", "8", "--stall", "2.5"]).unwrap();
        assert_eq!(options.stall, Some(Duration::from_millis(2500)));
        assert!(options.reasons.contains(&Reason::Stalled));
        assert!(options.reasons.contains(&Reason::Finished));

        // `--for` does not leave it out, and listing it too adds no copy.
        let options = wait(&["wait", "8", "--for", "finished", "--stall", "1"]).unwrap();
        assert_eq!(options.reasons, vec![Reason::Finished, Reason::Stalled]);
        let options = wait(&["wait", "8", "--for", "stalled", "--stall", "1"]).unwrap();
        assert_eq!(options.reasons, vec![Reason::Stalled]);

        assert_eq!(wait(&["wait", "8"]).unwrap().stall, None);
        assert!(
            !wait(&["wait", "8"])
                .unwrap()
                .reasons
                .contains(&Reason::Stalled)
        );
        let refused = wait(&["wait", "8", "--for", "stalled"]).unwrap_err();
        assert!(refused.contains("--stall"), "{refused}");
        assert!(wait(&["wait", "8", "--stall", "0"]).is_err());
        assert!(wait(&["wait", "8", "--stall", "-1"]).is_err());
        assert!(wait(&["wait", "8", "--stall", "soon"]).is_err());

        let Ok(Command::Send(send)) =
            parse_args(&args(&["send", "8", "--wait", "--stall", "4", "go"]))
        else {
            panic!("send --wait --stall should parse");
        };
        assert_eq!(send.wait.unwrap().stall, Some(Duration::from_secs(4)));
    }
}
