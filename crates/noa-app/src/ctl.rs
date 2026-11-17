//! `Noa +ctl`: the shell-callable orchestration client
//! (`docs/specs/agent-orchestration.md` D3/D4/D6). An orchestrator running
//! in one pane spawns worker panes, sends them input, waits for their agent
//! state, reads their output, and closes them — each as one command that
//! prints one JSON object per line.
//!
//! Argument parsing and the wait condition are pure functions so they are
//! unit-testable without a server; [`run`] is the thin I/O shell around them.

mod board;

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

/// Default cap on live workers one orchestrator may hold (D4).
const DEFAULT_MAX_CHILDREN: usize = 8;
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
  spawn [placement] [options] [-- COMMAND...]
                                Start a worker pane; prints {\"paneId\":...}.
      placement: --split auto|right|down (default auto) | --tab | --float
      --cwd DIR  --env KEY=VALUE  --label TEXT  --input TEXT
      --no-wait-after  --focus  --max-children N (default 8)
  send PANE [--enter] [--raw] [--wait [wait options]] [--any-pane] [--] TEXT|-
                                Type TEXT (or stdin) into PANE; prints the
                                report count to pass to `wait --after`.
  wait PANE...|--children [--after SEQ] [--for LIST] [--all] [--timeout SECS]
                                Block until a worker stops. LIST is a comma list
                                of finished,permission,input,error,exit,attention
                                (default: all of them).
  read PANE [--scrollback] [--max-bytes N]
                                Print PANE's screen (or retained output) text.
  close PANE...|--children [--any-pane]
                                Close worker panes.
  report STATE [[--] DETAIL]    Write an agent-state report (running, permission,
                                input, finished, error, clear) to this terminal.
  board [--of PANE]             Show an orchestrator's workers in this pane
                                (default: this pane's own orchestrator, else
                                every worker). Keys are listed at the bottom.
  board --open [--split right|down | --tab]
                                Open a board on this pane's workers next to it.

Use `--` before a TEXT or DETAIL that starts with `--`. A pane that exits
and stays open ends a wait whatever --for lists, as a closed pane does; one
that had exited before the wait began only ends it when no other pane is left.
`send` and `close` only accept this pane's own workers unless --any-pane is
given. Exit status: 0 ok, 1 error, 2 wait timed out, 3 server unavailable.
";

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
    /// A floating pane over the orchestrator, collapsed to its title strip.
    Float,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpawnArgs {
    pub(crate) placement: Placement,
    pub(crate) cwd: Option<String>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) label: Option<String>,
    pub(crate) input: Option<String>,
    pub(crate) wait_after: bool,
    pub(crate) focus: bool,
    pub(crate) max_children: usize,
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
    /// `--wait`: block like `wait PANE --after <seq before sending>`.
    pub(crate) wait: Option<WaitOptions>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WaitArgs {
    pub(crate) panes: PaneSet,
    pub(crate) after: Option<u64>,
    pub(crate) options: WaitOptions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WaitOptions {
    pub(crate) reasons: Vec<Reason>,
    pub(crate) all: bool,
    pub(crate) timeout: Option<Duration>,
}

impl Default for WaitOptions {
    fn default() -> Self {
        Self {
            reasons: Reason::ALL.to_vec(),
            all: false,
            timeout: None,
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
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.name() == name)
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
            let after = rest
                .value("--after")?
                .map(|v| parse_u64("--after", &v))
                .transpose()?;
            let options = parse_wait_options(&mut rest)?;
            let panes = pane_set(&mut rest)?;
            if after.is_some() && !matches!(&panes, PaneSet::Ids(ids) if ids.len() == 1) {
                return Err("--after takes exactly one PANE".to_string());
            }
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
            if placement == Placement::Float {
                return Err("a board opens in a split or a tab, not a float".to_string());
            }
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
            Command::Close {
                panes: pane_set(&mut rest)?,
                any_pane,
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
                return Err(format!("unknown state: {state}"));
            }
            if state == "clear" && !detail.is_empty() {
                return Err("clear takes no detail".to_string());
            }
            return Ok(Command::Report { state, detail });
        }
        other => return Err(format!("unknown command: {other}")),
    };
    rest.finish()?;
    Ok(command)
}

/// `--tab`, `--float` or `--split auto|right|down` (default `auto`).
fn parse_placement(rest: &mut Args) -> Result<Placement, String> {
    let tab = rest.flag("--tab");
    let float = rest.flag("--float");
    let split = rest.value("--split")?;
    if float {
        return match (tab, split) {
            (false, None) => Ok(Placement::Float),
            _ => Err("--float, --tab and --split are exclusive".to_string()),
        };
    }
    Ok(match (tab, split.as_deref()) {
        (true, Some(_)) => return Err("--tab and --split are exclusive".to_string()),
        (true, None) => Placement::Tab,
        (false, None | Some("auto")) => Placement::Auto,
        (false, Some("right")) => Placement::Right,
        (false, Some("down")) => Placement::Down,
        (false, Some(other)) => return Err(format!("unknown --split value: {other}")),
    })
}

fn parse_spawn(rest: &mut Args) -> Result<SpawnArgs, String> {
    let command = rest.take_after_double_dash();
    let placement = parse_placement(rest)?;
    let mut env = Vec::new();
    while let Some(assignment) = rest.value("--env")? {
        let (key, value) = assignment
            .split_once('=')
            .filter(|(key, _)| !key.is_empty())
            .ok_or_else(|| format!("--env expects KEY=VALUE, got {assignment}"))?;
        env.push((key.to_string(), value.to_string()));
    }
    let max_children = match rest.value("--max-children")? {
        Some(value) => parse_u64("--max-children", &value)? as usize,
        None => DEFAULT_MAX_CHILDREN,
    };
    Ok(SpawnArgs {
        placement,
        cwd: rest.value("--cwd")?,
        env,
        label: rest.value("--label")?,
        input: rest.value("--input")?,
        wait_after: !rest.flag("--no-wait-after"),
        focus: rest.flag("--focus"),
        max_children,
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
        wait,
    })
}

fn parse_wait_options(rest: &mut Args) -> Result<WaitOptions, String> {
    let mut options = WaitOptions {
        all: rest.flag("--all"),
        ..WaitOptions::default()
    };
    if let Some(list) = rest.value("--for")? {
        options.reasons = list
            .split(',')
            .map(|name| Reason::parse(name.trim()).ok_or_else(|| format!("unknown reason: {name}")))
            .collect::<Result<_, _>>()?;
    }
    if let Some(value) = rest.value("--timeout")? {
        let seconds: f64 = value
            .parse()
            .ok()
            .filter(|seconds: &f64| seconds.is_finite() && *seconds >= 0.0)
            .ok_or_else(|| format!("--timeout expects seconds, got {value}"))?;
        options.timeout = Some(
            Duration::try_from_secs_f64(seconds)
                .map_err(|_| format!("--timeout is out of range: {value}"))?,
        );
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
    value
        .parse()
        .map_err(|_| format!("{what} expects a number, got {value}"))
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
            return Err(format!("unknown option: {unknown}"));
        }
        Ok(std::mem::take(&mut self.items))
    }

    fn finish(self) -> Result<(), String> {
        match self.items.first() {
            None => Ok(()),
            Some(item) if item.starts_with("--") => Err(format!("unknown option: {item}")),
            Some(item) => Err(format!("unexpected argument: {item}")),
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

fn outcome_json(pane: u64, outcome: &Outcome, panel: Option<&Panel>) -> serde_json::Value {
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
            eprintln!("Noa +ctl: {message}\n\n{USAGE}");
            return EXIT_ERROR;
        }
    };
    if let Command::Report { state, detail } = &command {
        return write_report(state, detail);
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

const SCOPES_HINT: &str = "enable it in Noa's config with `server-enable = true` and `server-scopes = read,control,input`";

impl From<ClientError> for Failure {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::Rpc {
                code: -32003,
                message,
            } => Failure::unavailable(format!("{message}; {SCOPES_HINT}")),
            ClientError::Rpc { code: -32002, .. } => Failure::error("unknown pane"),
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
             not listen. {} Floating panes and the quick and scratch terminals cannot \
             orchestrate.",
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
            other => Failure::unavailable(format!("cannot reach Noa at {url}: {other}")),
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
        let own = self.own_now()?.ok_or_else(|| {
            Failure::error(format!(
                "{} Without a pane of its own there are no workers to target; pass \
                 --any-pane to target a pane anyway.",
                not_own_message(
                    std::env::var(SURFACE_ID_ENV).ok().as_deref(),
                    self.port,
                    self.lacks_surface_ids
                )
            ))
        })?;
        let panels = self.client.list_panels()?;
        for pane in panes {
            let child = panels
                .iter()
                .any(|panel| panel.pane_id.0 == *pane && panel.parent_pane_id == Some(WireId(own)));
            if !child {
                return Err(Failure::error(format!(
                    "pane {pane} is not a worker of this pane (pass --any-pane to target it)"
                )));
            }
        }
        Ok(())
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
                return self.wait(&panes, args.after, &args.options);
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
                let mut stdout = std::io::stdout().lock();
                let _ = stdout.write_all(result.text.as_bytes());
                if !result.text.ends_with('\n') {
                    let _ = stdout.write_all(b"\n");
                }
            }
            Command::Close { panes, any_pane } => {
                let panes = self.resolve(&panes)?;
                self.guard(&panes, any_pane)?;
                for pane in panes {
                    self.client.close_pane(pane)?;
                    print_json(&serde_json::json!({ "paneId": WireId(pane), "closed": true }));
                }
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
        let exe = std::env::current_exe()
            .map_err(|err| Failure::error(format!("cannot find this Noa binary: {err}")))?;
        let mut env = std::collections::BTreeMap::new();
        if let Ok(path) = std::env::var("PATH") {
            env.insert("PATH".to_string(), path);
        }
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
            label: Some("board".to_string()),
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
            Placement::Float => {
                return Err(Failure::error(
                    "a board opens in a split or a tab, not a float",
                ));
            }
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
        print_json(&serde_json::json!({ "paneId": WireId(pane) }));
        Ok(EXIT_OK)
    }

    fn spawn(&mut self, args: SpawnArgs) -> Result<i32, Failure> {
        let own = self.require_own()?;
        let live = self.children()?.len();
        if live >= args.max_children {
            return Err(Failure::error(format!(
                "this pane already has {live} workers (limit {}; raise it with --max-children)",
                args.max_children
            )));
        }
        let mut env: std::collections::BTreeMap<String, String> = args.env.into_iter().collect();
        // A command spawns without a shell, so it is looked up on the
        // orchestrator's own PATH rather than the GUI app's minimal one.
        if !args.command.is_empty()
            && !env.contains_key("PATH")
            && let Ok(path) = std::env::var("PATH")
        {
            env.insert("PATH".to_string(), path);
        }
        let cwd = spawn_cwd(args.cwd, std::env::current_dir().ok());
        let params = SpawnParams {
            config: Some(SpawnConfig {
                command: (!args.command.is_empty()).then_some(args.command),
                cwd,
                env,
                initial_input: args.input,
                wait_after_command: args.wait_after,
            }),
            label: args.label,
            parent_pane_id: Some(WireId(own)),
            focus: Some(args.focus),
        };
        let pane = match args.placement {
            Placement::Tab => {
                let window = self
                    .client
                    .list_panels()?
                    .into_iter()
                    .find(|panel| panel.pane_id.0 == own)
                    .map(|panel| panel.window_group_id.0);
                self.client.spawn_tab(window, &params)?
            }
            Placement::Auto => self
                .client
                .spawn_split(own, SplitPlacement::Auto, &params)?,
            Placement::Right => {
                self.client
                    .spawn_split(own, SplitPlacement::Horizontal, &params)?
            }
            Placement::Down => self
                .client
                .spawn_split(own, SplitPlacement::Vertical, &params)?,
            Placement::Float => self
                .client
                .spawn_split(own, SplitPlacement::Float, &params)?,
        };
        print_json(&serde_json::json!({ "paneId": WireId(pane) }));
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
        // Subscribe before typing, so the reply cannot arrive unobserved.
        if args.wait.is_some() {
            self.client
                .subscribe(&[EventKind::StateChanged], Some(&[args.pane]))?;
        }
        if !text.is_empty() {
            self.client.send_text(args.pane, &text, !args.raw)?;
        }
        if args.enter {
            if !text.is_empty() {
                std::thread::sleep(SUBMIT_DELAY);
            }
            self.client.send_text(args.pane, "\r", false)?;
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

    fn wait(
        &mut self,
        panes: &[u64],
        after: Option<u64>,
        options: &WaitOptions,
    ) -> Result<i32, Failure> {
        self.client
            .subscribe(&[EventKind::StateChanged], Some(panes))?;
        self.wait_subscribed(panes, WaitStart::Listed { after }, options)
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

/// Where a wait's per-pane baselines come from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum WaitStart {
    /// The first listing inside the wait; `after` overrides the report count.
    Listed { after: Option<u64> },
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
}

impl WaitSource for Client {
    fn list(&mut self) -> Result<Vec<Panel>, Failure> {
        Ok(self.list_panels()?)
    }

    fn next(&mut self, timeout: Duration) -> Result<Option<Notification>, Failure> {
        Ok(self.next_notification(timeout)?)
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
    let mut watches: Vec<Watch> = match start {
        WaitStart::Listed { after } => panes
            .iter()
            .map(|pane| Watch::new(*pane, latest.get(pane), after))
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
    loop {
        for watch in &mut watches {
            if done.contains(&watch.pane) || (skip_exited_before && watch.exited_before) {
                continue;
            }
            let panel = latest.get(&watch.pane);
            if let Some(outcome) = evaluate(watch, panel, &options.reasons) {
                emit(&outcome_json(watch.pane, &outcome, panel));
                done.push(watch.pane);
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
                    let pending: Vec<_> = watches
                        .iter()
                        .filter(|watch| !done.contains(&watch.pane))
                        .map(|watch| WireId(watch.pane))
                        .collect();
                    emit(&serde_json::json!({ "timeout": true, "pending": pending }));
                    return Ok(EXIT_TIMEOUT);
                }
                remaining.min(WAIT_RELIST_INTERVAL)
            }
            None => WAIT_RELIST_INTERVAL,
        };
        let relist = match source.next(remaining)? {
            // `dropped` means earlier notifications were lost, not this one:
            // its state is applied (and evaluated first), then re-listed.
            Some(Notification::StateChanged { panels, dropped }) => {
                for panel in panels {
                    if panes.contains(&panel.pane_id.0) {
                        latest.insert(panel.pane_id.0, panel);
                    }
                }
                dropped
            }
            Some(Notification::Other { dropped, .. }) => dropped,
            // An idle tick.
            None => true,
        };
        // `stateChanged` has no removal event, so a closed pane is only
        // noticed by listing: re-list on schedule even while other panes keep
        // notifying.
        relist_pending = relist || listed_at.elapsed() >= relist_every;
    }
}

/// How `send --wait` starts its wait, given the panel it saw before typing
/// and that panel's report count.
pub(crate) fn send_wait_start(before: Panel, seq: u64) -> WaitStart {
    WaitStart::Before {
        panels: vec![before],
        after: Some(seq),
    }
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
            "--split",
            "down",
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
                placement: Placement::Down,
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
                command: args(&["codex", "--label", "x"]),
            })
        );
    }

    #[test]
    fn spawn_defaults_to_auto_placement_and_keeps_finished_panes_open() {
        let Command::Spawn(spawn) = parse_args(&args(&["spawn", "--", "claude"])).unwrap() else {
            panic!("spawn expected");
        };

        assert_eq!(spawn.placement, Placement::Auto);
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
                wait: Some(WaitOptions {
                    reasons: vec![Reason::Finished, Reason::Error],
                    all: false,
                    timeout: Some(Duration::from_secs(30)),
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
    fn wait_takes_panes_or_children_and_after_only_for_one_pane() {
        assert_eq!(
            parse_args(&args(&["wait", "--children", "--all"])).unwrap(),
            Command::Wait(WaitArgs {
                panes: PaneSet::Children,
                after: None,
                options: WaitOptions {
                    all: true,
                    ..WaitOptions::default()
                },
            })
        );
        assert!(parse_args(&args(&["wait", "8", "--after", "4"])).is_ok());
        assert!(parse_args(&args(&["wait", "8", "9", "--after", "4"])).is_err());
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
            outcome_json(8, &outcome, Some(&held))["exitCode"],
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

    #[test]
    fn spawn_can_place_a_worker_in_a_float() {
        let Ok(Command::Spawn(spawn)) = parse_args(&args(&["spawn", "--float", "--", "claude"]))
        else {
            panic!("spawn --float did not parse");
        };
        assert_eq!(spawn.placement, Placement::Float);
        assert!(parse_args(&args(&["spawn", "--float", "--tab"])).is_err());
        assert!(parse_args(&args(&["spawn", "--float", "--split", "right"])).is_err());
        assert!(parse_args(&args(&["board", "--open", "--float"])).is_err());
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
        };
        let mut lines = Vec::new();

        let code = wait_loop(
            &mut source,
            &[1, 2],
            WaitStart::Listed { after: None },
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
        };
        let mut lines = Vec::new();

        let code = wait_loop(
            &mut source,
            &[8],
            WaitStart::Listed { after: None },
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
        };
        let mut lines = Vec::new();

        let code = wait_loop(
            &mut source,
            &[8],
            WaitStart::Listed { after: None },
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
        };
        let mut lines = Vec::new();

        let code = wait_loop(
            &mut source,
            &[1, 2],
            WaitStart::Listed { after: None },
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
        };
        let mut lines = Vec::new();

        let code = wait_loop(
            &mut source,
            &[1],
            WaitStart::Listed { after: None },
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
        };
        let mut lines = Vec::new();

        let code = wait_loop(
            &mut source,
            &[8, 8],
            WaitStart::Listed { after: Some(1) },
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
        };
        let code = wait_loop(
            &mut source,
            &[8],
            WaitStart::Listed { after: Some(1) },
            &wait.options,
            WAIT_RELIST_INTERVAL,
            &mut |_| {},
        );
        assert_eq!(code.ok(), Some(EXIT_OK));
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
}
